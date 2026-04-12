//! Platform-specific configuration.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// UnauthorizedDmBehavior
// ---------------------------------------------------------------------------

/// How to handle unauthorized direct messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UnauthorizedDmBehavior {
    /// Pair the user with the bot (create a session).
    Pair,
    /// Silently ignore the message.
    Ignore,
}

impl Default for UnauthorizedDmBehavior {
    fn default() -> Self {
        Self::Ignore
    }
}

// ---------------------------------------------------------------------------
// PlatformConfig
// ---------------------------------------------------------------------------

/// Configuration for a specific platform (e.g. discord, slack).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlatformConfig {
    /// Whether this platform adapter is enabled.
    #[serde(default)]
    pub enabled: bool,

    /// Bot authentication token (may be an env-var reference).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,

    /// Optional webhook URL for incoming events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub webhook_url: Option<String>,

    /// Whether the bot must be @mentioned in group channels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_mention: Option<bool>,

    /// How to handle DMs from unauthorized users.
    #[serde(default)]
    pub unauthorized_dm_behavior: UnauthorizedDmBehavior,

    /// Whether each user gets their own group session context.
    #[serde(default)]
    pub group_sessions_per_user: bool,

    /// Home / landing channel for the bot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home_channel: Option<String>,

    /// Users explicitly allowed to interact with the bot.
    #[serde(default)]
    pub allowed_users: Vec<String>,

    /// Users with administrative privileges.
    #[serde(default)]
    pub admin_users: Vec<String>,

    /// Keys not declared above (Python Hermes platform blocks often use many
    /// adapter-specific names); they deserialize here so YAML stays compatible.
    #[serde(default, flatten)]
    pub extra: HashMap<String, serde_json::Value>,
}

impl Default for PlatformConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            token: None,
            webhook_url: None,
            require_mention: None,
            unauthorized_dm_behavior: UnauthorizedDmBehavior::default(),
            group_sessions_per_user: false,
            home_channel: None,
            allowed_users: Vec::new(),
            admin_users: Vec::new(),
            extra: HashMap::new(),
        }
    }
}

impl PlatformConfig {
    /// Deep-merge a JSON overlay into this configuration.
    ///
    /// Values present in `overlay` take precedence; missing keys in the
    /// overlay are left unchanged. The `extra` map is merged recursively.
    pub fn merge_with_json(&mut self, overlay: &serde_json::Value) {
        if let serde_json::Value::Object(map) = overlay {
            // Standard fields
            if let Some(v) = map.get("enabled").and_then(|v| v.as_bool()) {
                self.enabled = v;
            }
            if let Some(v) = map.get("token").and_then(|v| v.as_str()) {
                self.token = Some(v.to_string());
            }
            if let Some(v) = map.get("webhook_url").and_then(|v| v.as_str()) {
                self.webhook_url = Some(v.to_string());
            }
            if let Some(v) = map.get("require_mention").and_then(|v| v.as_bool()) {
                self.require_mention = Some(v);
            }
            if let Some(v) = map.get("unauthorized_dm_behavior").and_then(|v| v.as_str()) {
                if let Ok(behavior) = serde_json::from_value::<UnauthorizedDmBehavior>(
                    serde_json::Value::String(v.to_string()),
                ) {
                    self.unauthorized_dm_behavior = behavior;
                }
            }
            if let Some(v) = map.get("group_sessions_per_user").and_then(|v| v.as_bool()) {
                self.group_sessions_per_user = v;
            }
            if let Some(v) = map.get("home_channel").and_then(|v| v.as_str()) {
                self.home_channel = Some(v.to_string());
            }
            if let Some(v) = map.get("allowed_users") {
                if let Ok(users) = serde_json::from_value::<Vec<String>>(v.clone()) {
                    self.allowed_users = users;
                }
            }
            if let Some(v) = map.get("admin_users") {
                if let Ok(users) = serde_json::from_value::<Vec<String>>(v.clone()) {
                    self.admin_users = users;
                }
            }
            // Merge extra map recursively
            if let Some(serde_json::Value::Object(extra_overlay)) = map.get("extra") {
                for (k, v) in extra_overlay {
                    self.extra.insert(k.clone(), v.clone());
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn platform_config_default() {
        let pc = PlatformConfig::default();
        assert!(!pc.enabled);
        assert!(pc.token.is_none());
        assert_eq!(pc.unauthorized_dm_behavior, UnauthorizedDmBehavior::Ignore);
    }

    #[test]
    fn unauthorized_dm_behavior_serde() {
        let b = UnauthorizedDmBehavior::Pair;
        let json = serde_json::to_string(&b).unwrap();
        assert_eq!(json, "\"pair\"");
        let back: UnauthorizedDmBehavior = serde_json::from_str(&json).unwrap();
        assert_eq!(back, UnauthorizedDmBehavior::Pair);
    }

    #[test]
    fn merge_with_json_overlay() {
        let mut pc = PlatformConfig::default();
        let overlay = serde_json::json!({
            "enabled": true,
            "token": "abc123",
            "extra": {
                "custom_field": "value"
            }
        });
        pc.merge_with_json(&overlay);
        assert!(pc.enabled);
        assert_eq!(pc.token.as_deref(), Some("abc123"));
        assert_eq!(
            pc.extra.get("custom_field").unwrap().as_str().unwrap(),
            "value"
        );
    }
}