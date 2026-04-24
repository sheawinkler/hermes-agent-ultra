//! Tool policy engine for centralized pre-dispatch governance.
//!
//! This module provides a lightweight policy gate used by `ToolRegistry` to
//! audit or block tool calls before handler execution.

use std::collections::HashSet;

use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolPolicyMode {
    Off,
    Audit,
    Enforce,
}

impl ToolPolicyMode {
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
    max_param_bytes: usize,
}

#[derive(Debug, Clone)]
pub struct ToolPolicyDecision {
    pub allow: bool,
    pub reason: Option<String>,
    pub audited_only: bool,
}

impl ToolPolicyEngine {
    pub fn new(mode: ToolPolicyMode) -> Self {
        Self {
            mode,
            allowlist: HashSet::new(),
            denylist: HashSet::new(),
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
        let max_param_bytes = std::env::var("HERMES_TOOL_POLICY_MAX_PARAM_BYTES")
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .filter(|v| *v > 0)
            .unwrap_or(256 * 1024);

        Self {
            mode,
            allowlist,
            denylist,
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
            };
        }

        let normalized_name = tool_name.trim().to_ascii_lowercase();
        let mut deny_reason = None;

        if self.denylist.contains(&normalized_name) {
            deny_reason = Some(format!("tool '{}' is denylisted", tool_name));
        } else if !self.allowlist.is_empty() && !self.allowlist.contains(&normalized_name) {
            deny_reason = Some(format!("tool '{}' is not in policy allowlist", tool_name));
        } else if !params.is_null() {
            match serde_json::to_vec(params) {
                Ok(buf) => {
                    if buf.len() > self.max_param_bytes {
                        deny_reason = Some(format!(
                            "tool params exceed max bytes: {} > {}",
                            buf.len(),
                            self.max_param_bytes
                        ));
                    }
                }
                Err(_) => {
                    deny_reason =
                        Some("tool params could not be serialized for policy check".to_string());
                }
            }
        }

        match deny_reason {
            None => ToolPolicyDecision {
                allow: true,
                reason: None,
                audited_only: false,
            },
            Some(reason) => {
                if matches!(self.mode, ToolPolicyMode::Audit) {
                    ToolPolicyDecision {
                        allow: true,
                        reason: Some(reason),
                        audited_only: true,
                    }
                } else {
                    ToolPolicyDecision {
                        allow: false,
                        reason: Some(reason),
                        audited_only: false,
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
        assert!(decision.reason.is_some());
    }

    #[test]
    fn annotate_policy_audit_appends_warning() {
        let out = annotate_policy_audit(r#"{"ok":true}"#, "policy audit");
        let parsed: Value = serde_json::from_str(&out).expect("valid json");
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["_tool_policy_warning"], "policy audit");
    }
}
