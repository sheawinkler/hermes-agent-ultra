//! Website/domain blocklist policy for web-facing tools.

use std::fmt;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};
use url::Url;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebsitePolicyError {
    message: String,
}

impl WebsitePolicyError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for WebsitePolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for WebsitePolicyError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebsiteBlockRule {
    pub pattern: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebsiteBlocklistPolicy {
    pub enabled: bool,
    pub rules: Vec<WebsiteBlockRule>,
}

impl WebsiteBlocklistPolicy {
    pub fn to_json(&self) -> Value {
        json!({
            "enabled": self.enabled,
            "rules": self.rules.iter().map(|rule| json!({"pattern": rule.pattern})).collect::<Vec<_>>(),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebsiteAccessBlock {
    pub url: String,
    pub host: String,
    pub rule: String,
}

impl WebsiteAccessBlock {
    pub fn to_json(&self) -> Value {
        json!({
            "url": self.url,
            "host": self.host,
            "rule": self.rule,
        })
    }
}

pub fn load_website_blocklist(
    config_path: Option<&Path>,
) -> Result<WebsiteBlocklistPolicy, WebsitePolicyError> {
    let path = config_path
        .map(Path::to_path_buf)
        .unwrap_or_else(hermes_config::config_path);
    if !path.exists() {
        return Ok(WebsiteBlocklistPolicy {
            enabled: false,
            rules: Vec::new(),
        });
    }

    let raw = std::fs::read_to_string(&path)
        .map_err(|e| WebsitePolicyError::new(format!("Failed to read config YAML: {e}")))?;
    let root: serde_yaml::Value = serde_yaml::from_str(&raw)
        .map_err(|e| WebsitePolicyError::new(format!("Invalid config YAML: {e}")))?;
    let serde_yaml::Value::Mapping(root) = root else {
        return Err(WebsitePolicyError::new("config root must be a mapping"));
    };

    let Some(security) = mapping_get(&root, "security") else {
        return Ok(WebsiteBlocklistPolicy {
            enabled: false,
            rules: Vec::new(),
        });
    };
    let serde_yaml::Value::Mapping(security) = security else {
        return Err(WebsitePolicyError::new("security must be a mapping"));
    };
    let Some(blocklist) = mapping_get(security, "website_blocklist") else {
        return Ok(WebsiteBlocklistPolicy {
            enabled: false,
            rules: Vec::new(),
        });
    };
    let serde_yaml::Value::Mapping(blocklist) = blocklist else {
        return Err(WebsitePolicyError::new(
            "security.website_blocklist must be a mapping",
        ));
    };

    let enabled = match mapping_get(blocklist, "enabled") {
        Some(serde_yaml::Value::Bool(value)) => *value,
        Some(_) => {
            return Err(WebsitePolicyError::new(
                "security.website_blocklist.enabled must be a boolean",
            ))
        }
        None => false,
    };

    let mut rules = Vec::new();
    if let Some(domains) = mapping_get(blocklist, "domains") {
        let serde_yaml::Value::Sequence(domains) = domains else {
            return Err(WebsitePolicyError::new(
                "security.website_blocklist.domains must be a list",
            ));
        };
        for domain in domains {
            if let Some(rule) = domain.as_str().and_then(normalize_domain_rule) {
                rules.push(WebsiteBlockRule { pattern: rule });
            }
        }
    }

    if let Some(shared_files) = mapping_get(blocklist, "shared_files") {
        let serde_yaml::Value::Sequence(shared_files) = shared_files else {
            return Err(WebsitePolicyError::new(
                "security.website_blocklist.shared_files must be a list",
            ));
        };
        for shared in shared_files {
            let Some(path) = shared.as_str().map(PathBuf::from) else {
                continue;
            };
            match std::fs::read_to_string(&path) {
                Ok(raw) => {
                    for line in raw.lines() {
                        let line = line.split('#').next().unwrap_or("").trim();
                        if let Some(rule) = normalize_domain_rule(line) {
                            rules.push(WebsiteBlockRule { pattern: rule });
                        }
                    }
                }
                Err(err) => {
                    tracing::warn!(
                        "Skipping unreadable website blocklist {}: {err}",
                        path.display()
                    );
                }
            }
        }
    }

    dedupe_rules(&mut rules);
    Ok(WebsiteBlocklistPolicy { enabled, rules })
}

pub fn check_website_access(
    url: &str,
    config_path: Option<&Path>,
) -> Result<Option<WebsiteAccessBlock>, WebsitePolicyError> {
    let policy = load_website_blocklist(config_path)?;
    Ok(check_website_access_with_policy(url, &policy))
}

pub fn check_website_access_with_policy(
    url: &str,
    policy: &WebsiteBlocklistPolicy,
) -> Option<WebsiteAccessBlock> {
    if !policy.enabled {
        return None;
    }
    let host = url_host(url)?;
    for rule in &policy.rules {
        if domain_rule_matches(&host, &rule.pattern) {
            return Some(WebsiteAccessBlock {
                url: url.to_string(),
                host,
                rule: rule.pattern.clone(),
            });
        }
    }
    None
}

fn mapping_get<'a>(map: &'a serde_yaml::Mapping, key: &str) -> Option<&'a serde_yaml::Value> {
    map.get(serde_yaml::Value::String(key.to_string()))
}

fn normalize_domain_rule(input: &str) -> Option<String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return None;
    }
    let host = if trimmed.starts_with("http://") || trimmed.starts_with("https://") {
        Url::parse(trimmed)
            .ok()
            .and_then(|url| url.host_str().map(ToString::to_string))?
    } else {
        trimmed
            .trim_start_matches("//")
            .split('/')
            .next()
            .unwrap_or(trimmed)
            .to_string()
    };
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

fn url_host(input: &str) -> Option<String> {
    Url::parse(input)
        .ok()
        .and_then(|url| url.host_str().map(ToString::to_string))
        .map(|host| host.trim().trim_end_matches('.').to_ascii_lowercase())
        .filter(|host| !host.is_empty())
}

fn domain_rule_matches(host: &str, rule: &str) -> bool {
    if let Some(suffix) = rule.strip_prefix("*.") {
        return host.ends_with(&format!(".{suffix}")) && host != suffix;
    }
    host == rule || host.ends_with(&format!(".{rule}"))
}

fn dedupe_rules(rules: &mut Vec<WebsiteBlockRule>) {
    let mut seen = std::collections::BTreeSet::new();
    rules.retain(|rule| seen.insert(rule.pattern.clone()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_website_blocklist_merges_config_and_shared_file() {
        let tmp = tempfile::tempdir().unwrap();
        let shared = tmp.path().join("community-blocklist.txt");
        std::fs::write(&shared, "# comment\nexample.org\nsub.bad.net\n").unwrap();
        let config = tmp.path().join("config.yaml");
        std::fs::write(
            &config,
            format!(
                r#"
security:
  website_blocklist:
    enabled: true
    domains:
      - example.com
      - https://www.evil.test/path
    shared_files:
      - {}
"#,
                shared.display()
            ),
        )
        .unwrap();

        let policy = load_website_blocklist(Some(&config)).unwrap();
        assert!(policy.enabled);
        let rules = policy
            .rules
            .iter()
            .map(|rule| rule.pattern.as_str())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            rules,
            ["evil.test", "example.com", "example.org", "sub.bad.net"]
                .into_iter()
                .collect()
        );
    }

    #[test]
    fn check_website_access_matches_parent_domain_subdomains() {
        let policy = WebsiteBlocklistPolicy {
            enabled: true,
            rules: vec![WebsiteBlockRule {
                pattern: "example.com".to_string(),
            }],
        };
        let blocked =
            check_website_access_with_policy("https://docs.example.com/page", &policy).unwrap();
        assert_eq!(blocked.host, "docs.example.com");
        assert_eq!(blocked.rule, "example.com");
    }

    #[test]
    fn check_website_access_supports_wildcard_subdomains_only() {
        let policy = WebsiteBlocklistPolicy {
            enabled: true,
            rules: vec![WebsiteBlockRule {
                pattern: "*.tracking.example".to_string(),
            }],
        };
        assert!(check_website_access_with_policy("https://a.tracking.example", &policy).is_some());
        assert!(
            check_website_access_with_policy("https://www.tracking.example", &policy).is_some()
        );
        assert!(check_website_access_with_policy("https://tracking.example", &policy).is_none());
    }

    #[test]
    fn load_website_blocklist_uses_disabled_default_when_section_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config.yaml");
        std::fs::write(&config, "display:\n  tool_progress: all\n").unwrap();
        let policy = load_website_blocklist(Some(&config)).unwrap();
        assert!(!policy.enabled);
        assert!(policy.rules.is_empty());
    }

    #[test]
    fn load_website_blocklist_rejects_invalid_shapes_cleanly() {
        let tmp = tempfile::tempdir().unwrap();
        let cases = [
            ("[]", "config root must be a mapping"),
            ("security: []\n", "security must be a mapping"),
            (
                "security:\n  website_blocklist: block everything\n",
                "security.website_blocklist must be a mapping",
            ),
            (
                "security:\n  website_blocklist:\n    enabled: \"false\"\n",
                "security.website_blocklist.enabled must be a boolean",
            ),
            (
                "security:\n  website_blocklist:\n    domains: example.com\n",
                "security.website_blocklist.domains must be a list",
            ),
            (
                "security:\n  website_blocklist:\n    shared_files: community.txt\n",
                "security.website_blocklist.shared_files must be a list",
            ),
        ];
        for (idx, (yaml, message)) in cases.iter().enumerate() {
            let config = tmp.path().join(format!("case-{idx}.yaml"));
            std::fs::write(&config, yaml).unwrap();
            let err = load_website_blocklist(Some(&config)).unwrap_err();
            assert!(err.to_string().contains(message), "{yaml:?} produced {err}");
        }
    }

    #[test]
    fn load_website_blocklist_rejects_malformed_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let config = tmp.path().join("config.yaml");
        std::fs::write(&config, "security: [oops\n").unwrap();
        let err = load_website_blocklist(Some(&config)).unwrap_err();
        assert!(err.to_string().contains("Invalid config YAML"));
    }
}
