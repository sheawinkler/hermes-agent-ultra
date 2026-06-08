//! URL safety and website policy engine.
//!
//! Provides:
//! 1. **UrlSafetyHandler** — tool the LLM invokes to check if a URL is safe
//! 2. **WebsitePolicy** — configurable URL whitelist/blacklist engine with
//!    domain, path, and regex matching. Loaded from `~/.hermes/website_policy.yaml`.
//! 3. **Pre-check interface** for `web_tools` and `browser` to call before
//!    accessing any URL.
//!
//! Policy file format (`~/.hermes/website_policy.yaml`):
//! ```yaml
//! default_action: allow  # or "deny"
//! rules:
//!   - domain: "*.malware.com"
//!     action: deny
//!     reason: "Known malware domain"
//!   - domain: "internal.corp.com"
//!     action: allow
//!   - path_regex: "/admin/.*"
//!     action: deny
//!     reason: "Admin paths blocked"
//!   - domain: "*.gov"
//!     action: allow
//! ```

use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use async_trait::async_trait;
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

// ---------------------------------------------------------------------------
// Policy types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum PolicyAction {
    #[default]
    Allow,
    Deny,
    Warn,
}

/// A single policy rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PolicyRule {
    /// Domain glob pattern (e.g. "*.malware.com", "example.com").
    #[serde(default)]
    pub domain: Option<String>,
    /// Path regex pattern (e.g. "/admin/.*").
    #[serde(default)]
    pub path_regex: Option<String>,
    /// Full URL regex pattern.
    #[serde(default)]
    pub url_regex: Option<String>,
    /// Action to take when matched.
    pub action: PolicyAction,
    /// Human-readable reason.
    #[serde(default)]
    pub reason: Option<String>,
}

/// Website policy configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebsitePolicyConfig {
    /// Default action when no rules match.
    #[serde(default)]
    pub default_action: PolicyAction,
    /// Ordered list of rules (first match wins).
    #[serde(default)]
    pub rules: Vec<PolicyRule>,
}

impl Default for WebsitePolicyConfig {
    fn default() -> Self {
        Self {
            default_action: PolicyAction::Allow,
            rules: vec![
                // Built-in safety rules
                PolicyRule {
                    domain: None,
                    path_regex: None,
                    url_regex: Some(r"^http://".into()),
                    action: PolicyAction::Warn,
                    reason: Some("Non-HTTPS URL".into()),
                },
            ],
        }
    }
}

/// Result of a URL policy check.
#[derive(Debug, Clone, Serialize)]
pub struct PolicyCheckResult {
    pub url: String,
    pub action: PolicyAction,
    pub safe: bool,
    pub reason: String,
    pub matched_rule: Option<usize>,
}

// ---------------------------------------------------------------------------
// WebsitePolicy engine
// ---------------------------------------------------------------------------

/// Website policy engine with hot-reload support.
pub struct WebsitePolicy {
    config: Arc<RwLock<WebsitePolicyConfig>>,
    config_path: Option<PathBuf>,
    last_modified: Arc<RwLock<Option<SystemTime>>>,
}

impl WebsitePolicy {
    /// Create with default built-in rules.
    pub fn new() -> Self {
        Self {
            config: Arc::new(RwLock::new(WebsitePolicyConfig::default())),
            config_path: None,
            last_modified: Arc::new(RwLock::new(None)),
        }
    }

    /// Create from a config file path. Loads immediately and supports hot-reload.
    pub fn from_file(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let config = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => serde_yaml::from_str(&content).unwrap_or_default(),
                Err(_) => WebsitePolicyConfig::default(),
            }
        } else {
            WebsitePolicyConfig::default()
        };

        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();

        Self {
            config: Arc::new(RwLock::new(config)),
            config_path: Some(path),
            last_modified: Arc::new(RwLock::new(mtime)),
        }
    }

    /// Create from an explicit config (for testing).
    pub fn from_config(config: WebsitePolicyConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            config_path: None,
            last_modified: Arc::new(RwLock::new(None)),
        }
    }

    /// Check if the config file has been modified and reload if so.
    pub fn maybe_reload(&self) {
        let Some(ref path) = self.config_path else {
            return;
        };

        let current_mtime = match std::fs::metadata(path).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => return,
        };

        let needs_reload = {
            let last = self.last_modified.read().unwrap();
            match *last {
                Some(prev) => current_mtime > prev,
                None => true,
            }
        };

        if needs_reload {
            if let Ok(content) = std::fs::read_to_string(path) {
                if let Ok(new_config) = serde_yaml::from_str::<WebsitePolicyConfig>(&content) {
                    if let Ok(mut cfg) = self.config.write() {
                        *cfg = new_config;
                    }
                    if let Ok(mut last) = self.last_modified.write() {
                        *last = Some(current_mtime);
                    }
                    tracing::info!("Reloaded website policy from {:?}", path);
                }
            }
        }
    }

    /// Check a URL against the policy.
    pub fn check_url(&self, url: &str) -> PolicyCheckResult {
        self.maybe_reload();

        let config = self.config.read().unwrap();
        let parsed = url::Url::parse(url);

        for (i, rule) in config.rules.iter().enumerate() {
            if rule_matches(rule, url, parsed.as_ref().ok()) {
                let action = rule.action;
                return PolicyCheckResult {
                    url: url.to_string(),
                    action,
                    safe: action != PolicyAction::Deny,
                    reason: rule
                        .reason
                        .clone()
                        .unwrap_or_else(|| format!("Matched rule #{}", i + 1)),
                    matched_rule: Some(i),
                };
            }
        }

        // No rule matched — use default
        PolicyCheckResult {
            url: url.to_string(),
            action: config.default_action,
            safe: config.default_action != PolicyAction::Deny,
            reason: "No matching rule; using default policy".into(),
            matched_rule: None,
        }
    }

    /// Pre-check interface for web/browser tools. Returns `Err` if denied.
    pub fn pre_check(&self, url: &str) -> Result<PolicyCheckResult, ToolError> {
        let result = self.check_url(url);
        if result.action == PolicyAction::Deny {
            return Err(ToolError::ExecutionFailed(format!(
                "URL blocked by policy: {} (reason: {})",
                url, result.reason
            )));
        }
        if result.action == PolicyAction::Warn {
            tracing::warn!(url = url, reason = %result.reason, "URL policy warning");
        }
        Ok(result)
    }
}

impl Default for WebsitePolicy {
    fn default() -> Self {
        Self::new()
    }
}

/// Check if a rule matches a URL.
fn rule_matches(rule: &PolicyRule, url: &str, parsed: Option<&url::Url>) -> bool {
    // Check domain pattern
    if let Some(ref domain_pattern) = rule.domain {
        if let Some(parsed) = parsed {
            if let Some(host) = parsed.host_str() {
                if !domain_glob_matches(domain_pattern, host) {
                    return false;
                }
            } else {
                return false;
            }
        } else {
            return false;
        }
        // If only domain is specified and it matched, return true
        if rule.path_regex.is_none() && rule.url_regex.is_none() {
            return true;
        }
    }

    // Check path regex
    if let Some(ref path_pattern) = rule.path_regex {
        if let Some(parsed) = parsed {
            if let Ok(re) = regex::Regex::new(path_pattern) {
                if !re.is_match(parsed.path()) {
                    return false;
                }
            }
        } else {
            return false;
        }
        if rule.url_regex.is_none() {
            return true;
        }
    }

    // Check full URL regex
    if let Some(ref url_pattern) = rule.url_regex {
        if let Ok(re) = regex::Regex::new(url_pattern) {
            return re.is_match(url);
        }
        return false;
    }

    // No patterns specified — rule doesn't match anything
    false
}

/// Simple glob matching for domain patterns.
/// Supports `*` as wildcard for a single domain segment and `*.` prefix for subdomains.
fn domain_glob_matches(pattern: &str, domain: &str) -> bool {
    let pattern = pattern.to_lowercase();
    let domain = domain.to_lowercase();

    if pattern == domain {
        return true;
    }

    // *.example.com matches sub.example.com and deep.sub.example.com
    if let Some(suffix) = pattern.strip_prefix("*.") {
        return domain.ends_with(suffix)
            && (domain.len() > suffix.len())
            && domain.as_bytes()[domain.len() - suffix.len() - 1] == b'.';
    }

    false
}

// ---------------------------------------------------------------------------
// Tirith Security Policy Engine
// ---------------------------------------------------------------------------

/// Tirith security policy — tool-call-level pre-check engine.
///
/// Evaluates rules before any tool call to decide whether to allow, block,
/// or require approval. Rules are loaded from `~/.hermes/tirith_rules.yaml`.
///
/// Rule format:
/// ```yaml
/// rules:
///   - tool: "terminal"
///     pattern: "rm -rf"
///     action: block
///     reason: "Destructive command"
///   - tool: "send_message"
///     condition: "platform == 'production'"
///     action: require_approval
///   - tool: "*"
///     action: allow
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TirithRule {
    /// Tool name to match (glob: "*" matches all).
    pub tool: String,
    /// Regex pattern to match against the tool's parameters (serialized as JSON).
    #[serde(default)]
    pub pattern: Option<String>,
    /// Action: allow, block, require_approval, warn.
    pub action: TirithAction,
    /// Human-readable reason.
    #[serde(default)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TirithAction {
    Allow,
    Block,
    RequireApproval,
    Warn,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TirithConfig {
    #[serde(default)]
    pub rules: Vec<TirithRule>,
}

/// Result of a Tirith security check.
#[derive(Debug, Clone, Serialize)]
pub struct TirithCheckResult {
    pub tool_name: String,
    pub action: TirithAction,
    pub allowed: bool,
    pub reason: String,
    pub matched_rule: Option<usize>,
}

/// Tirith security engine.
pub struct TirithSecurity {
    config: Arc<RwLock<TirithConfig>>,
    config_path: Option<PathBuf>,
    last_modified: Arc<RwLock<Option<SystemTime>>>,
}

impl TirithSecurity {
    pub fn new() -> Self {
        Self {
            config: Arc::new(RwLock::new(TirithConfig::default())),
            config_path: None,
            last_modified: Arc::new(RwLock::new(None)),
        }
    }

    pub fn from_file(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let config = if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => serde_yaml::from_str(&content).unwrap_or_default(),
                Err(_) => TirithConfig::default(),
            }
        } else {
            TirithConfig::default()
        };

        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();

        Self {
            config: Arc::new(RwLock::new(config)),
            config_path: Some(path),
            last_modified: Arc::new(RwLock::new(mtime)),
        }
    }

    pub fn from_config(config: TirithConfig) -> Self {
        Self {
            config: Arc::new(RwLock::new(config)),
            config_path: None,
            last_modified: Arc::new(RwLock::new(None)),
        }
    }

    /// Hot-reload from file if modified.
    pub fn maybe_reload(&self) {
        let Some(ref path) = self.config_path else {
            return;
        };
        let current_mtime = match std::fs::metadata(path).and_then(|m| m.modified()) {
            Ok(t) => t,
            Err(_) => return,
        };
        let needs_reload = {
            let last = self.last_modified.read().unwrap();
            match *last {
                Some(prev) => current_mtime > prev,
                None => true,
            }
        };
        if needs_reload {
            if let Ok(content) = std::fs::read_to_string(path) {
                if let Ok(new_config) = serde_yaml::from_str::<TirithConfig>(&content) {
                    if let Ok(mut cfg) = self.config.write() {
                        *cfg = new_config;
                    }
                    if let Ok(mut last) = self.last_modified.write() {
                        *last = Some(current_mtime);
                    }
                    tracing::info!("Reloaded Tirith security rules from {:?}", path);
                }
            }
        }
    }

    /// Check a tool call against security rules.
    pub fn check_tool_call(&self, tool_name: &str, params: &Value) -> TirithCheckResult {
        self.maybe_reload();

        let config = self.config.read().unwrap();
        let params_str = serde_json::to_string(params).unwrap_or_default();

        for (i, rule) in config.rules.iter().enumerate() {
            // Match tool name (glob)
            if rule.tool != "*" && rule.tool != tool_name {
                continue;
            }

            // Match pattern against params
            if let Some(ref pattern) = rule.pattern {
                if let Ok(re) = regex::Regex::new(pattern) {
                    if !re.is_match(&params_str) {
                        continue;
                    }
                }
            }

            // Rule matched
            return TirithCheckResult {
                tool_name: tool_name.to_string(),
                action: rule.action,
                allowed: rule.action != TirithAction::Block,
                reason: rule
                    .reason
                    .clone()
                    .unwrap_or_else(|| format!("Matched Tirith rule #{}", i + 1)),
                matched_rule: Some(i),
            };
        }

        // No rule matched — allow by default
        TirithCheckResult {
            tool_name: tool_name.to_string(),
            action: TirithAction::Allow,
            allowed: true,
            reason: "No matching security rule".into(),
            matched_rule: None,
        }
    }

    /// Pre-check interface. Returns `Err` if blocked.
    pub fn pre_check(
        &self,
        tool_name: &str,
        params: &Value,
    ) -> Result<TirithCheckResult, ToolError> {
        let result = self.check_tool_call(tool_name, params);
        if result.action == TirithAction::Block {
            return Err(ToolError::ExecutionFailed(format!(
                "Tool call blocked by security policy: {} (reason: {})",
                tool_name, result.reason
            )));
        }
        if result.action == TirithAction::Warn {
            tracing::warn!(
                tool = tool_name,
                reason = %result.reason,
                "Tirith security warning"
            );
        }
        Ok(result)
    }
}

impl Default for TirithSecurity {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// UrlSafetyHandler — tool the LLM invokes
// ---------------------------------------------------------------------------

/// Tool handler for URL safety checks. Uses the `WebsitePolicy` engine.
pub struct UrlSafetyHandler {
    policy: Arc<WebsitePolicy>,
}

impl UrlSafetyHandler {
    pub fn new(policy: Arc<WebsitePolicy>) -> Self {
        Self { policy }
    }
}

impl Default for UrlSafetyHandler {
    fn default() -> Self {
        // Try to load from default path
        let policy_path = hermes_config::hermes_home().join("website_policy.yaml");

        let policy = if policy_path.exists() {
            WebsitePolicy::from_file(policy_path)
        } else {
            WebsitePolicy::new()
        };

        Self {
            policy: Arc::new(policy),
        }
    }
}

#[async_trait]
impl ToolHandler for UrlSafetyHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        let url = params.get("url").and_then(|v| v.as_str()).unwrap_or("");
        if url.is_empty() {
            return Err(ToolError::InvalidParams("Missing 'url'".into()));
        }

        let result = self.policy.check_url(url);

        Ok(json!({
            "url": result.url,
            "safe": result.safe,
            "action": format!("{:?}", result.action).to_lowercase(),
            "reason": result.reason,
            "matched_rule": result.matched_rule,
        })
        .to_string())
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "url".into(),
            json!({
                "type": "string",
                "description": "URL to check against the website safety policy"
            }),
        );
        tool_schema(
            "url_safety",
            "Check whether a URL is safe to access based on the website policy engine. \
             Evaluates domain whitelist/blacklist, path patterns, and regex rules. \
             Policy is loaded from <hermes_home>/website_policy.yaml with hot-reload support.",
            JsonSchema::object(props, vec!["url".into()]),
        )
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // -- Domain glob matching ------------------------------------------------

    #[test]
    fn domain_glob_exact_match() {
        assert!(domain_glob_matches("example.com", "example.com"));
        assert!(!domain_glob_matches("example.com", "other.com"));
    }

    #[test]
    fn domain_glob_wildcard_subdomain() {
        assert!(domain_glob_matches("*.example.com", "sub.example.com"));
        assert!(domain_glob_matches("*.example.com", "deep.sub.example.com"));
        assert!(!domain_glob_matches("*.example.com", "example.com"));
        assert!(!domain_glob_matches("*.example.com", "notexample.com"));
    }

    #[test]
    fn domain_glob_case_insensitive() {
        assert!(domain_glob_matches("Example.COM", "example.com"));
        assert!(domain_glob_matches("*.Example.COM", "sub.example.com"));
    }

    // -- WebsitePolicy tests -------------------------------------------------

    #[test]
    fn policy_default_allows_https() {
        let policy = WebsitePolicy::new();
        let result = policy.check_url("https://example.com/page");
        assert!(result.safe);
        assert_eq!(result.action, PolicyAction::Allow);
    }

    #[test]
    fn policy_default_warns_http() {
        let policy = WebsitePolicy::new();
        let result = policy.check_url("http://example.com/page");
        assert!(result.safe); // warn is still safe
        assert_eq!(result.action, PolicyAction::Warn);
    }

    #[test]
    fn policy_custom_deny_domain() {
        let config = WebsitePolicyConfig {
            default_action: PolicyAction::Allow,
            rules: vec![PolicyRule {
                domain: Some("*.malware.com".into()),
                path_regex: None,
                url_regex: None,
                action: PolicyAction::Deny,
                reason: Some("Known malware".into()),
            }],
        };
        let policy = WebsitePolicy::from_config(config);

        let result = policy.check_url("https://evil.malware.com/payload");
        assert!(!result.safe);
        assert_eq!(result.action, PolicyAction::Deny);
        assert!(result.reason.contains("malware"));

        let result = policy.check_url("https://safe.example.com");
        assert!(result.safe);
    }

    #[test]
    fn policy_path_regex() {
        let config = WebsitePolicyConfig {
            default_action: PolicyAction::Allow,
            rules: vec![PolicyRule {
                domain: None,
                path_regex: Some(r"/admin/.*".into()),
                url_regex: None,
                action: PolicyAction::Deny,
                reason: Some("Admin paths blocked".into()),
            }],
        };
        let policy = WebsitePolicy::from_config(config);

        let result = policy.check_url("https://example.com/admin/settings");
        assert!(!result.safe);

        let result = policy.check_url("https://example.com/public/page");
        assert!(result.safe);
    }

    #[test]
    fn policy_default_deny() {
        let config = WebsitePolicyConfig {
            default_action: PolicyAction::Deny,
            rules: vec![PolicyRule {
                domain: Some("allowed.com".into()),
                path_regex: None,
                url_regex: None,
                action: PolicyAction::Allow,
                reason: None,
            }],
        };
        let policy = WebsitePolicy::from_config(config);

        let result = policy.check_url("https://allowed.com/page");
        assert!(result.safe);

        let result = policy.check_url("https://blocked.com/page");
        assert!(!result.safe);
    }

    #[test]
    fn policy_pre_check_blocks() {
        let config = WebsitePolicyConfig {
            default_action: PolicyAction::Allow,
            rules: vec![PolicyRule {
                domain: Some("blocked.com".into()),
                path_regex: None,
                url_regex: None,
                action: PolicyAction::Deny,
                reason: Some("Blocked".into()),
            }],
        };
        let policy = WebsitePolicy::from_config(config);

        let err = policy.pre_check("https://blocked.com/page").unwrap_err();
        assert!(err.to_string().contains("blocked by policy"));

        assert!(policy.pre_check("https://safe.com/page").is_ok());
    }

    // -- Tirith security tests -----------------------------------------------

    #[test]
    fn tirith_no_rules_allows() {
        let engine = TirithSecurity::new();
        let result = engine.check_tool_call("terminal", &json!({"command": "ls"}));
        assert!(result.allowed);
        assert_eq!(result.action, TirithAction::Allow);
    }

    #[test]
    fn tirith_block_rule() {
        let config = TirithConfig {
            rules: vec![TirithRule {
                tool: "terminal".into(),
                pattern: Some(r"rm\s+-rf".into()),
                action: TirithAction::Block,
                reason: Some("Destructive command".into()),
            }],
        };
        let engine = TirithSecurity::from_config(config);

        let result = engine.check_tool_call("terminal", &json!({"command": "rm -rf /"}));
        assert!(!result.allowed);
        assert_eq!(result.action, TirithAction::Block);
        assert!(result.reason.contains("Destructive"));

        // Non-matching command should be allowed
        let result = engine.check_tool_call("terminal", &json!({"command": "ls -la"}));
        assert!(result.allowed);
    }

    #[test]
    fn tirith_wildcard_tool() {
        let config = TirithConfig {
            rules: vec![TirithRule {
                tool: "*".into(),
                pattern: Some(r"password".into()),
                action: TirithAction::Warn,
                reason: Some("Sensitive data".into()),
            }],
        };
        let engine = TirithSecurity::from_config(config);

        let result = engine.check_tool_call("any_tool", &json!({"data": "contains password"}));
        assert_eq!(result.action, TirithAction::Warn);
        assert!(result.allowed); // warn doesn't block
    }

    #[test]
    fn tirith_require_approval() {
        let config = TirithConfig {
            rules: vec![TirithRule {
                tool: "send_message".into(),
                pattern: None,
                action: TirithAction::RequireApproval,
                reason: Some("All messages need approval".into()),
            }],
        };
        let engine = TirithSecurity::from_config(config);

        let result = engine.check_tool_call("send_message", &json!({}));
        assert_eq!(result.action, TirithAction::RequireApproval);
        assert!(result.allowed); // require_approval doesn't block

        // Different tool should be allowed
        let result = engine.check_tool_call("read_file", &json!({}));
        assert_eq!(result.action, TirithAction::Allow);
    }

    #[test]
    fn tirith_pre_check_blocks() {
        let config = TirithConfig {
            rules: vec![TirithRule {
                tool: "terminal".into(),
                pattern: Some(r"dangerous".into()),
                action: TirithAction::Block,
                reason: Some("Blocked".into()),
            }],
        };
        let engine = TirithSecurity::from_config(config);

        let err = engine
            .pre_check("terminal", &json!({"command": "dangerous_cmd"}))
            .unwrap_err();
        assert!(err.to_string().contains("blocked by security"));

        assert!(engine
            .pre_check("terminal", &json!({"command": "safe_cmd"}))
            .is_ok());
    }

    #[test]
    fn tirith_first_match_wins() {
        let config = TirithConfig {
            rules: vec![
                TirithRule {
                    tool: "terminal".into(),
                    pattern: Some(r"safe_cmd".into()),
                    action: TirithAction::Allow,
                    reason: Some("Explicitly allowed".into()),
                },
                TirithRule {
                    tool: "*".into(),
                    pattern: None,
                    action: TirithAction::Block,
                    reason: Some("Block everything else".into()),
                },
            ],
        };
        let engine = TirithSecurity::from_config(config);

        // First rule matches — allowed
        let result = engine.check_tool_call("terminal", &json!({"command": "safe_cmd"}));
        assert!(result.allowed);

        // Second rule matches — blocked
        let result = engine.check_tool_call("terminal", &json!({"command": "other_cmd"}));
        assert!(!result.allowed);
    }

    // -- UrlSafetyHandler tests ----------------------------------------------

    #[tokio::test]
    async fn handler_checks_url() {
        let handler = UrlSafetyHandler::new(Arc::new(WebsitePolicy::new()));
        let result = handler
            .execute(json!({"url": "https://example.com"}))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["safe"], true);
    }

    #[tokio::test]
    async fn handler_warns_http() {
        let handler = UrlSafetyHandler::new(Arc::new(WebsitePolicy::new()));
        let result = handler
            .execute(json!({"url": "http://example.com"}))
            .await
            .unwrap();
        let v: Value = serde_json::from_str(&result).unwrap();
        assert_eq!(v["action"], "warn");
        assert_eq!(v["safe"], true); // warn is still safe
    }

    #[tokio::test]
    async fn handler_missing_url() {
        let handler = UrlSafetyHandler::new(Arc::new(WebsitePolicy::new()));
        let err = handler.execute(json!({})).await.unwrap_err();
        assert!(err.to_string().contains("Missing"));
    }

    #[tokio::test]
    async fn handler_schema() {
        let handler = UrlSafetyHandler::default();
        let schema = handler.schema();
        assert_eq!(schema.name, "url_safety");
        assert!(schema.description.contains("policy"));
    }

    // -- YAML config round-trip ----------------------------------------------

    #[test]
    fn website_policy_yaml_roundtrip() {
        let config = WebsitePolicyConfig {
            default_action: PolicyAction::Allow,
            rules: vec![
                PolicyRule {
                    domain: Some("*.malware.com".into()),
                    path_regex: None,
                    url_regex: None,
                    action: PolicyAction::Deny,
                    reason: Some("Malware".into()),
                },
                PolicyRule {
                    domain: None,
                    path_regex: Some(r"/admin/.*".into()),
                    url_regex: None,
                    action: PolicyAction::Deny,
                    reason: Some("Admin blocked".into()),
                },
            ],
        };
        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: WebsitePolicyConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.rules.len(), 2);
        assert_eq!(parsed.default_action, PolicyAction::Allow);
    }

    #[test]
    fn tirith_yaml_roundtrip() {
        let config = TirithConfig {
            rules: vec![TirithRule {
                tool: "terminal".into(),
                pattern: Some(r"rm -rf".into()),
                action: TirithAction::Block,
                reason: Some("Destructive".into()),
            }],
        };
        let yaml = serde_yaml::to_string(&config).unwrap();
        let parsed: TirithConfig = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(parsed.rules.len(), 1);
        assert_eq!(parsed.rules[0].action, TirithAction::Block);
    }
}
