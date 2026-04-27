//! Tool policy engine for centralized pre-dispatch governance.
//!
//! This module provides a lightweight policy gate used by `ToolRegistry` to
//! audit or block tool calls before handler execution.

use std::collections::HashSet;
use std::path::Path;

use regex::Regex;
use serde::Deserialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPolicyMode {
    Off,
    Audit,
    Enforce,
}

impl ToolPolicyMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Audit => "audit",
            Self::Enforce => "enforce",
        }
    }

    fn parse(raw: &str) -> Self {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" => Self::Off,
            "audit" => Self::Audit,
            _ => Self::Enforce,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ToolPolicyEngine {
    mode: ToolPolicyMode,
    allowlist: HashSet<String>,
    denylist: HashSet<String>,
    deny_param_patterns: Vec<Regex>,
    max_param_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct ToolPolicyDecision {
    pub allow: bool,
    pub reason: Option<String>,
    pub audited_only: bool,
    pub mode: ToolPolicyMode,
    pub code: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct ToolPolicyFileConfig {
    mode: Option<String>,
    allowlist: Option<Vec<String>>,
    denylist: Option<Vec<String>>,
    deny_param_patterns: Option<Vec<String>>,
    max_param_bytes: Option<usize>,
}

impl ToolPolicyEngine {
    pub fn new(mode: ToolPolicyMode) -> Self {
        Self {
            mode,
            allowlist: HashSet::new(),
            denylist: HashSet::new(),
            deny_param_patterns: Vec::new(),
            max_param_bytes: 256 * 1024,
        }
    }

    pub fn from_env() -> Self {
        let mode = std::env::var("HERMES_TOOL_POLICY_MODE")
            .map(|v| ToolPolicyMode::parse(&v))
            .unwrap_or(ToolPolicyMode::Enforce);

        let allowlist = std::env::var("HERMES_TOOL_POLICY_ALLOWLIST")
            .ok()
            .map(|v| parse_csv_set(&v))
            .unwrap_or_default();
        let denylist = std::env::var("HERMES_TOOL_POLICY_DENYLIST")
            .ok()
            .map(|v| parse_csv_set(&v))
            .unwrap_or_default();
        let deny_param_patterns = std::env::var("HERMES_TOOL_POLICY_DENY_PARAM_PATTERNS")
            .ok()
            .map(|v| parse_csv_list(&v))
            .unwrap_or_default();
        let max_param_bytes = std::env::var("HERMES_TOOL_POLICY_MAX_PARAM_BYTES")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(256 * 1024);

        let mut policy = Self {
            mode,
            allowlist,
            denylist,
            deny_param_patterns: compile_patterns(&deny_param_patterns),
            max_param_bytes,
        };

        if let Ok(path) = std::env::var("HERMES_TOOL_POLICY_FILE") {
            if let Ok(from_file) = Self::from_file(path.trim()) {
                policy = from_file;
            }
        }
        policy
    }

    pub fn from_file(path: impl AsRef<Path>) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path.as_ref())
            .map_err(|e| format!("read policy file {}: {}", path.as_ref().display(), e))?;
        let cfg: ToolPolicyFileConfig = serde_json::from_str(&raw)
            .map_err(|e| format!("parse policy file {}: {}", path.as_ref().display(), e))?;
        Ok(Self::from_file_config(cfg))
    }

    fn from_file_config(cfg: ToolPolicyFileConfig) -> Self {
        let mode = cfg
            .mode
            .as_deref()
            .map(ToolPolicyMode::parse)
            .unwrap_or(ToolPolicyMode::Enforce);
        let allowlist = cfg
            .allowlist
            .unwrap_or_default()
            .into_iter()
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty())
            .collect();
        let denylist = cfg
            .denylist
            .unwrap_or_default()
            .into_iter()
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|v| !v.is_empty())
            .collect();
        let deny_param_patterns = compile_patterns(&cfg.deny_param_patterns.unwrap_or_default());
        let max_param_bytes = cfg.max_param_bytes.filter(|v| *v > 0).unwrap_or(256 * 1024);
        Self {
            mode,
            allowlist,
            denylist,
            deny_param_patterns,
            max_param_bytes,
        }
    }

    pub fn with_allowlist(mut self, names: &[&str]) -> Self {
        self.allowlist = names
            .iter()
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        self
    }

    pub fn with_denylist(mut self, names: &[&str]) -> Self {
        self.denylist = names
            .iter()
            .map(|v| v.trim().to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        self
    }

    pub fn with_deny_param_patterns(mut self, patterns: &[&str]) -> Self {
        let raw: Vec<String> = patterns
            .iter()
            .map(|v| v.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        self.deny_param_patterns = compile_patterns(&raw);
        self
    }

    pub fn with_max_param_bytes(mut self, max_param_bytes: usize) -> Self {
        if max_param_bytes > 0 {
            self.max_param_bytes = max_param_bytes;
        }
        self
    }

    pub fn evaluate(&self, tool_name: &str, params: &Value) -> ToolPolicyDecision {
        if matches!(self.mode, ToolPolicyMode::Off) {
            return ToolPolicyDecision {
                allow: true,
                reason: None,
                audited_only: false,
                mode: self.mode,
                code: None,
            };
        }

        let normalized_name = tool_name.trim().to_ascii_lowercase();
        let mut deny_reason = None;
        let mut deny_code = None;

        if self.denylist.contains(&normalized_name) {
            deny_reason = Some(format!("tool '{}' is denylisted", tool_name));
            deny_code = Some("tool_denylisted".to_string());
        } else if !self.allowlist.is_empty() && !self.allowlist.contains(&normalized_name) {
            deny_reason = Some(format!("tool '{}' is not in policy allowlist", tool_name));
            deny_code = Some("tool_not_allowlisted".to_string());
        } else if !params.is_null() {
            match serde_json::to_vec(params) {
                Ok(buf) => {
                    if buf.len() > self.max_param_bytes {
                        deny_reason = Some(format!(
                            "tool params exceed max bytes: {} > {}",
                            buf.len(),
                            self.max_param_bytes
                        ));
                        deny_code = Some("params_too_large".to_string());
                    } else if !self.deny_param_patterns.is_empty() {
                        if let Ok(params_str) = serde_json::to_string(params) {
                            if let Some(pattern) = self
                                .deny_param_patterns
                                .iter()
                                .find(|p| p.is_match(&params_str))
                            {
                                deny_reason = Some(format!(
                                    "tool params matched deny pattern '{}'",
                                    pattern.as_str()
                                ));
                                deny_code = Some("params_pattern_denied".to_string());
                            }
                        }
                    }
                }
                Err(_) => {
                    deny_reason =
                        Some("tool params could not be serialized for policy check".to_string());
                    deny_code = Some("params_not_serializable".to_string());
                }
            }
        }

        match deny_reason {
            None => ToolPolicyDecision {
                allow: true,
                reason: None,
                audited_only: false,
                mode: self.mode,
                code: None,
            },
            Some(reason) => {
                if matches!(self.mode, ToolPolicyMode::Audit) {
                    ToolPolicyDecision {
                        allow: true,
                        reason: Some(reason),
                        audited_only: true,
                        mode: self.mode,
                        code: deny_code,
                    }
                } else {
                    ToolPolicyDecision {
                        allow: false,
                        reason: Some(reason),
                        audited_only: false,
                        mode: self.mode,
                        code: deny_code,
                    }
                }
            }
        }
    }
}

fn parse_csv_set(raw: &str) -> HashSet<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.to_ascii_lowercase())
        .collect()
}

fn parse_csv_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect()
}

fn compile_patterns(raw: &[String]) -> Vec<Regex> {
    raw.iter()
        .filter_map(|entry| Regex::new(entry).ok())
        .collect()
}

pub fn annotate_policy_audit(result: &str, reason: &str) -> String {
    if let Ok(mut value) = serde_json::from_str::<Value>(result) {
        if let Some(obj) = value.as_object_mut() {
            obj.insert(
                "_tool_policy_warning".to_string(),
                Value::String(reason.to_string()),
            );
            return value.to_string();
        }
    }
    serde_json::json!({
        "result": result,
        "_tool_policy_warning": reason,
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn policy_denies_denylisted_tool() {
        let policy = ToolPolicyEngine::new(ToolPolicyMode::Enforce)
            .with_denylist(&["terminal"])
            .with_max_param_bytes(1024);
        let decision = policy.evaluate("terminal", &serde_json::json!({"cmd":"ls"}));
        assert!(!decision.allow);
        assert!(!decision.audited_only);
        assert_eq!(decision.mode, ToolPolicyMode::Enforce);
        assert_eq!(decision.code.as_deref(), Some("tool_denylisted"));
        assert!(decision
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("denylisted"));
    }

    #[test]
    fn policy_audit_mode_allows_with_warning() {
        let policy = ToolPolicyEngine::new(ToolPolicyMode::Audit)
            .with_allowlist(&["read_file"])
            .with_max_param_bytes(1024);
        let decision = policy.evaluate("write_file", &serde_json::json!({"path":"a"}));
        assert!(decision.allow);
        assert!(decision.audited_only);
        assert_eq!(decision.mode, ToolPolicyMode::Audit);
        assert_eq!(decision.code.as_deref(), Some("tool_not_allowlisted"));
        assert!(decision.reason.is_some());
    }

    #[test]
    fn policy_denies_regex_param_match() {
        let policy = ToolPolicyEngine::new(ToolPolicyMode::Enforce)
            .with_deny_param_patterns(&["(?i)rm\\s+-rf", "sk-[A-Za-z0-9]{8,}"]);
        let decision = policy.evaluate("terminal", &serde_json::json!({"cmd":"rm -rf /tmp/x"}));
        assert!(!decision.allow);
        assert_eq!(decision.code.as_deref(), Some("params_pattern_denied"));
        assert!(decision
            .reason
            .as_deref()
            .unwrap_or("")
            .contains("deny pattern"));
    }

    #[test]
    fn policy_from_file_loads_mode_and_patterns() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("policy.json");
        std::fs::write(
            &file,
            r#"{
                "mode": "audit",
                "allowlist": ["read_file"],
                "deny_param_patterns": ["(?i)secret=.+$"],
                "max_param_bytes": 128
            }"#,
        )
        .expect("write policy file");
        let policy = ToolPolicyEngine::from_file(&file).expect("load policy");
        let decision = policy.evaluate("write_file", &serde_json::json!({"q":"secret=abc"}));
        assert!(decision.allow);
        assert!(decision.audited_only);
        assert_eq!(decision.mode, ToolPolicyMode::Audit);
    }

    #[test]
    fn annotate_policy_audit_appends_warning() {
        let out = annotate_policy_audit(r#"{"ok":true}"#, "policy audit");
        let parsed: Value = serde_json::from_str(&out).expect("valid json");
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["_tool_policy_warning"], "policy audit");
    }
}
