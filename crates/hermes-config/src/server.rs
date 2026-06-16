//! Remote LLM server configuration (Flowy `/claw` API + auth).

use serde::{Deserialize, Serialize};

/// FlowyClaw `resolveWeChatFlowyServerBase()` — WeChat OAuth always uses the domestic API root.
pub const DEFAULT_WECHAT_FLOWY_SERVER_BASE: &str = "https://server.flowyaipc.cn/claw";

/// Built-in default when `server.llm.default_model` is empty (see docs/new-client-api-llm-chat.md).
pub const DEFAULT_SERVER_LLM_MODEL: &str = "AIPC-glm-4.7";

/// Supported remote server login methods.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ServerLoginMethod {
    #[default]
    WechatQr,
    EmailOtp,
}

impl ServerLoginMethod {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::WechatQr => "wechat_qr",
            Self::EmailOtp => "email_otp",
        }
    }
}

/// Top-level remote server settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[derive(Default)]
pub struct ServerConfig {
    /// When true, Hermes uses the remote server for LLM calls (after login).
    #[serde(default)]
    pub enabled: bool,

    /// Flowy API root, e.g. `https://server.flowyaipc.cn/claw`.
    #[serde(default)]
    pub base_url: String,

    /// WeChat OAuth / MP API root; defaults to `base_url` when empty.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub wechat_base_url: String,

    /// Brand channel for user isolation (default `flowy`).
    #[serde(default = "default_channel")]
    pub channel: String,

    /// Client app identifier sent on login (`flowymes`, `aipc`, …).
    #[serde(default = "default_app")]
    pub app: String,

    /// Optional invite code (max 16 chars).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub invite_code: String,

    #[serde(default)]
    pub auth: ServerAuthConfig,

    #[serde(default)]
    pub llm: ServerLlmConfig,
}

impl ServerConfig {
    pub fn effective_wechat_base_url(&self) -> String {
        let trimmed = self.wechat_base_url.trim();
        if trimmed.is_empty() {
            DEFAULT_WECHAT_FLOWY_SERVER_BASE.to_string()
        } else {
            trimmed.trim_end_matches('/').to_string()
        }
    }

    pub fn api_ready(&self) -> bool {
        !self.base_url.trim().is_empty()
    }

    pub fn effective_llm_base_url(&self) -> String {
        let base = self.base_url.trim().trim_end_matches('/');
        let prefix = self.llm.path_prefix.trim();
        if prefix.is_empty() || prefix == "/" {
            format!("{base}/v1")
        } else if prefix.starts_with('/') {
            format!("{base}{prefix}")
        } else {
            format!("{base}/{prefix}")
        }
    }

    pub fn effective_wechat_app_id(&self) -> String {
        let trimmed = self.auth.wechat_app_id.trim();
        if is_valid_wechat_open_app_id(trimmed) {
            return trimmed.to_string();
        }
        default_wechat_app_id_for_channel(&self.channel)
    }

    pub fn effective_default_llm_model(&self) -> String {
        let trimmed = self.llm.default_model.trim();
        if trimmed.is_empty() {
            DEFAULT_SERVER_LLM_MODEL.to_string()
        } else {
            trimmed.to_string()
        }
    }
}

/// WeChat Open Platform website app id (`wx` + 16 hex chars).
pub fn is_valid_wechat_open_app_id(value: &str) -> bool {
    let v = value.trim();
    v.len() == 18
        && v.starts_with("wx")
        && v.as_bytes()[2..].iter().all(|b| b.is_ascii_hexdigit())
}

pub fn default_wechat_app_id_for_channel(channel: &str) -> String {
    match channel.trim().to_ascii_lowercase().as_str() {
        "gmk" => "wx413de9863ef7ea1c".to_string(),
        _ => "wxc7a38fe55e162569".to_string(),
    }
}

pub fn is_known_brand_wechat_app_id(value: &str) -> bool {
    let v = value.trim();
    v == default_wechat_app_id_for_channel("flowy") || v == default_wechat_app_id_for_channel("gmk")
}

/// When channel changes, refresh stored app id unless the user set a custom valid wx id.
pub fn sync_wechat_app_id_for_channel(auth: &mut ServerAuthConfig, channel: &str) {
    let stored = auth.wechat_app_id.trim();
    let should_sync = stored.is_empty()
        || !is_valid_wechat_open_app_id(stored)
        || is_known_brand_wechat_app_id(stored);
    if should_sync {
        auth.wechat_app_id = default_wechat_app_id_for_channel(channel);
    }
}

/// Login settings for the remote server account.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerAuthConfig {
    #[serde(default)]
    pub preferred_method: ServerLoginMethod,

    #[serde(default = "default_poll_interval_ms")]
    pub poll_interval_ms: u64,

    #[serde(default = "default_otp_ttl_seconds")]
    pub otp_ttl_seconds: u64,

    /// Seconds between presence heartbeats when a long-running process enables it.
    #[serde(default = "default_heartbeat_interval_secs")]
    pub heartbeat_interval_secs: u64,

    /// WeChat Open Platform app id (`wx...`); empty uses channel default.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub wechat_app_id: String,
}

impl Default for ServerAuthConfig {
    fn default() -> Self {
        Self {
            preferred_method: ServerLoginMethod::default(),
            poll_interval_ms: default_poll_interval_ms(),
            otp_ttl_seconds: default_otp_ttl_seconds(),
            heartbeat_interval_secs: default_heartbeat_interval_secs(),
            wechat_app_id: String::new(),
        }
    }
}

/// LLM gateway path and timeout settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ServerLlmConfig {
    #[serde(default = "default_llm_path_prefix")]
    pub path_prefix: String,

    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub default_model: String,

    #[serde(default = "default_llm_request_timeout_seconds")]
    pub request_timeout_seconds: u64,
}

impl Default for ServerLlmConfig {
    fn default() -> Self {
        Self {
            path_prefix: default_llm_path_prefix(),
            default_model: String::new(),
            request_timeout_seconds: default_llm_request_timeout_seconds(),
        }
    }
}

fn default_channel() -> String {
    "flowy".to_string()
}

fn default_app() -> String {
    "flowymes".to_string()
}

fn default_poll_interval_ms() -> u64 {
    2000
}

fn default_otp_ttl_seconds() -> u64 {
    300
}

fn default_heartbeat_interval_secs() -> u64 {
    60
}

fn default_llm_path_prefix() -> String {
    "/v1".to_string()
}

fn default_llm_request_timeout_seconds() -> u64 {
    120
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_config_defaults_off() {
        let cfg = ServerConfig::default();
        assert!(!cfg.enabled);
        assert_eq!(cfg.channel, "flowy");
        assert_eq!(cfg.app, "flowymes");
    }

    #[test]
    fn wechat_base_defaults_domestic_when_unset() {
        let cfg = ServerConfig {
            base_url: "https://test.example/claw".into(),
            ..Default::default()
        };
        assert_eq!(
            cfg.effective_wechat_base_url(),
            DEFAULT_WECHAT_FLOWY_SERVER_BASE
        );
    }

    #[test]
    fn wechat_base_honors_explicit_override() {
        let cfg = ServerConfig {
            wechat_base_url: "https://test.flowyaipc.cn/claw".into(),
            ..Default::default()
        };
        assert_eq!(
            cfg.effective_wechat_base_url(),
            "https://test.flowyaipc.cn/claw"
        );
    }

    #[test]
    fn wechat_app_id_defaults_by_channel() {
        assert_eq!(
            default_wechat_app_id_for_channel("flowy"),
            "wxc7a38fe55e162569"
        );
        assert_eq!(
            default_wechat_app_id_for_channel("gmk"),
            "wx413de9863ef7ea1c"
        );
        assert_eq!(
            default_wechat_app_id_for_channel("GMK"),
            "wx413de9863ef7ea1c"
        );
    }

    #[test]
    fn effective_wechat_app_id_ignores_invalid_override() {
        let cfg = ServerConfig {
            channel: "flowy".into(),
            auth: ServerAuthConfig {
                wechat_app_id: "flowymes".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(cfg.effective_wechat_app_id(), "wxc7a38fe55e162569");
    }

    #[test]
    fn effective_wechat_app_id_honors_valid_override() {
        let cfg = ServerConfig {
            channel: "flowy".into(),
            auth: ServerAuthConfig {
                wechat_app_id: "wx413de9863ef7ea1c".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(cfg.effective_wechat_app_id(), "wx413de9863ef7ea1c");
    }

    #[test]
    fn effective_default_llm_model_uses_builtin_when_empty() {
        let cfg = ServerConfig::default();
        assert_eq!(cfg.effective_default_llm_model(), DEFAULT_SERVER_LLM_MODEL);
    }

    #[test]
    fn effective_default_llm_model_honors_override() {
        let cfg = ServerConfig {
            llm: ServerLlmConfig {
                default_model: "custom-model".into(),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(cfg.effective_default_llm_model(), "custom-model");
    }

    #[test]
    fn server_config_yaml_roundtrip() {
        let yaml = r#"
enabled: true
base_url: https://server.flowyaipc.cn/claw
channel: flowy
app: flowymes
auth:
  preferred_method: email_otp
"#;
        let cfg: ServerConfig = serde_yaml::from_str(yaml).expect("parse");
        assert!(cfg.enabled);
        assert_eq!(cfg.auth.preferred_method, ServerLoginMethod::EmailOtp);
    }
}
