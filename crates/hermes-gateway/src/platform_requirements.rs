//! Single source of truth for static gateway platform credential checks.
//!
//! See `.kiro/specs/unified-runtime-architecture/gateway-requirements-single-source-rfc.md`.

use hermes_config::GatewayConfig;
#[cfg(any(
    feature = "telegram",
    feature = "weixin",
    feature = "discord",
    feature = "slack",
    feature = "matrix",
    feature = "mattermost",
    feature = "signal",
    feature = "whatsapp",
    feature = "dingtalk",
    feature = "feishu",
    feature = "wecom",
    feature = "wecom-callback",
    feature = "qqbot",
    feature = "bluebubbles",
    feature = "email",
    feature = "sms",
    feature = "homeassistant",
    feature = "webhook"
))]
use hermes_config::{extra_string, platform_token_or_extra};

/// Call site / UX context for requirement evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequirementScope {
    /// Runtime / `hermes serve` style startup — fatals block boot.
    RuntimeStart,
    /// `hermes doctor` — surfaces fatals and optional warns.
    Doctor,
    /// CLI live-messaging tool — fatals skip per-platform registration only.
    LiveMessaging,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RequirementSeverity {
    Fatal,
    Warn,
}

/// Structured requirement issue (stable `code`, human `message`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequirementIssue {
    pub platform: String,
    pub code: &'static str,
    pub message: String,
    pub severity: RequirementSeverity,
}

fn push_fatal(
    issues: &mut Vec<RequirementIssue>,
    platform: &str,
    code: &'static str,
    message: impl Into<String>,
) {
    issues.push(RequirementIssue {
        platform: platform.to_string(),
        code,
        message: message.into(),
        severity: RequirementSeverity::Fatal,
    });
}

fn push_warn(
    issues: &mut Vec<RequirementIssue>,
    platform: &str,
    code: &'static str,
    message: impl Into<String>,
) {
    issues.push(RequirementIssue {
        platform: platform.to_string(),
        code,
        message: message.into(),
        severity: RequirementSeverity::Warn,
    });
}

/// Evaluate enabled-platform credential requirements.
///
/// Fatal issues mirror legacy [`crate::gateway_requirement_issues`] strings.
/// Scope currently affects optional warns (e.g. partial adapter implementations).
#[allow(unused_mut, unused_variables)]
pub fn evaluate_gateway_requirements(
    config: &GatewayConfig,
    scope: RequirementScope,
) -> Vec<RequirementIssue> {
    let mut issues = Vec::new();
    let check = |enabled: bool, cond: bool| enabled && !cond;

    #[cfg(feature = "telegram")]
    if let Some(p) = config.platforms.get("telegram") {
        if check(p.enabled, platform_token_or_extra(p).is_some()) {
            push_fatal(
                &mut issues,
                "telegram",
                "missing_token",
                "telegram.enabled=true 但缺少 token",
            );
        }
    }

    #[cfg(feature = "weixin")]
    if let Some(p) = config.platforms.get("weixin") {
        let account_id = extra_string(p, "account_id").is_some();
        let token = platform_token_or_extra(p).is_some();
        if check(p.enabled, account_id && token) {
            push_fatal(
                &mut issues,
                "weixin",
                "missing_account_id_or_token",
                "weixin.enabled=true 但缺少 account_id 或 token",
            );
        }
    }

    #[cfg(feature = "discord")]
    if let Some(p) = config.platforms.get("discord") {
        if check(p.enabled, platform_token_or_extra(p).is_some()) {
            push_fatal(
                &mut issues,
                "discord",
                "missing_token",
                "discord.enabled=true 但缺少 token",
            );
        }
    }

    #[cfg(feature = "slack")]
    if let Some(p) = config.platforms.get("slack") {
        if check(p.enabled, platform_token_or_extra(p).is_some()) {
            push_fatal(
                &mut issues,
                "slack",
                "missing_token",
                "slack.enabled=true 但缺少 token",
            );
        }
    }

    #[cfg(feature = "matrix")]
    if let Some(p) = config.platforms.get("matrix") {
        if p.enabled {
            let homeserver = extra_string(p, "homeserver_url")
                .or_else(|| extra_string(p, "homeserver"))
                .is_some();
            let user_id = extra_string(p, "user_id").is_some();
            let access = platform_token_or_extra(p)
                .or_else(|| extra_string(p, "access_token"))
                .is_some();
            if check(p.enabled, homeserver && user_id && access) {
                push_fatal(
                    &mut issues,
                    "matrix",
                    "missing_matrix_credentials",
                    "matrix.enabled=true 但缺少 homeserver_url（或 homeserver）/user_id/access_token（或 token）",
                );
            }
        }
    }

    #[cfg(feature = "mattermost")]
    if let Some(p) = config.platforms.get("mattermost") {
        if p.enabled {
            let url = extra_string(p, "server_url")
                .or_else(|| extra_string(p, "url"))
                .is_some();
            let token = platform_token_or_extra(p).is_some();
            if check(p.enabled, url && token) {
                push_fatal(
                    &mut issues,
                    "mattermost",
                    "missing_server_or_token",
                    "mattermost.enabled=true 但缺少 server_url（或 url）或 token",
                );
            }
        }
    }

    #[cfg(feature = "signal")]
    if let Some(p) = config.platforms.get("signal") {
        if p.enabled {
            let phone = extra_string(p, "phone_number")
                .or_else(|| extra_string(p, "account"))
                .is_some();
            if check(p.enabled, phone) {
                push_fatal(
                    &mut issues,
                    "signal",
                    "missing_phone_number",
                    "signal.enabled=true 但缺少 phone_number（或 account）",
                );
            }
        }
    }

    #[cfg(feature = "whatsapp")]
    if let Some(p) = config.platforms.get("whatsapp") {
        if check(p.enabled, platform_token_or_extra(p).is_some()) {
            push_fatal(
                &mut issues,
                "whatsapp",
                "missing_token",
                "whatsapp.enabled=true 但缺少 token",
            );
        }
    }

    #[cfg(feature = "dingtalk")]
    if let Some(p) = config.platforms.get("dingtalk") {
        if p.enabled {
            let id = extra_string(p, "client_id").is_some();
            let secret = extra_string(p, "client_secret").is_some();
            if check(p.enabled, id && secret) {
                push_fatal(
                    &mut issues,
                    "dingtalk",
                    "missing_client_credentials",
                    "dingtalk.enabled=true 但缺少 client_id 或 client_secret",
                );
            }
        }
    }

    #[cfg(feature = "feishu")]
    if let Some(p) = config.platforms.get("feishu") {
        if p.enabled {
            let app_id = extra_string(p, "app_id").is_some();
            let app_secret = extra_string(p, "app_secret").is_some();
            if check(p.enabled, app_id && app_secret) {
                push_fatal(
                    &mut issues,
                    "feishu",
                    "missing_app_credentials",
                    "feishu.enabled=true 但缺少 app_id 或 app_secret",
                );
            }
        }
    }

    #[cfg(feature = "wecom")]
    if let Some(p) = config.platforms.get("wecom") {
        if p.enabled {
            let env_non_empty = |key: &str| {
                std::env::var(key)
                    .ok()
                    .map(|v| v.trim().to_string())
                    .filter(|v| !v.is_empty())
                    .is_some()
            };
            // WeCom AI Bot websocket mode uses bot_id + secret.
            // Keep corp_id as compatibility fallback for older configs.
            let bot_id = extra_string(p, "bot_id")
                .or_else(|| extra_string(p, "corp_id"))
                .or_else(|| {
                    if env_non_empty("WECOM_BOT_ID") {
                        Some(String::new())
                    } else {
                        None
                    }
                })
                .is_some();
            let secret = extra_string(p, "secret")
                .or_else(|| {
                    if env_non_empty("WECOM_SECRET") {
                        Some(String::new())
                    } else {
                        None
                    }
                })
                .is_some();
            let ready = bot_id && secret;
            if check(p.enabled, ready) {
                push_fatal(
                    &mut issues,
                    "wecom",
                    "missing_wecom_credentials",
                    "wecom.enabled=true 但缺少 bot_id（兼容 corp_id）或 secret",
                );
            }
        }
    }

    #[cfg(feature = "wecom-callback")]
    if let Some(p) = config.platforms.get("wecom_callback") {
        let ready = extra_string(p, "corp_id").is_some()
            && extra_string(p, "corp_secret").is_some()
            && extra_string(p, "agent_id").is_some()
            && platform_token_or_extra(p)
                .or_else(|| extra_string(p, "token"))
                .is_some()
            && extra_string(p, "encoding_aes_key").is_some();
        if check(p.enabled, ready) {
            push_fatal(
                &mut issues,
                "wecom_callback",
                "missing_wecom_callback_credentials",
                "wecom_callback.enabled=true 但缺少 corp_id/corp_secret/agent_id/token/encoding_aes_key",
            );
        } else if p.enabled && ready && scope != RequirementScope::LiveMessaging {
            push_warn(
                &mut issues,
                "wecom_callback",
                "adapter_partial_implementation",
                "wecom_callback 凭证已齐，但本构建中适配器仍为简化实现；注册阶段可能返回实现级错误。",
            );
        }
    }

    #[cfg(feature = "qqbot")]
    if let Some(p) = config
        .platforms
        .get("qqbot")
        .or_else(|| config.platforms.get("qq"))
    {
        let app_id = extra_string(p, "app_id").is_some();
        let secret = extra_string(p, "client_secret").is_some();
        if check(p.enabled, app_id && secret) {
            push_fatal(
                &mut issues,
                "qqbot",
                "missing_qqbot_credentials",
                "qqbot.enabled=true 但缺少 app_id 或 client_secret",
            );
        }
    }

    #[cfg(feature = "bluebubbles")]
    if let Some(p) = config.platforms.get("bluebubbles") {
        if p.enabled {
            let ready =
                extra_string(p, "server_url").is_some() && extra_string(p, "password").is_some();
            if check(p.enabled, ready) {
                push_fatal(
                    &mut issues,
                    "bluebubbles",
                    "missing_server_or_password",
                    "bluebubbles.enabled=true 但缺少 server_url 或 password",
                );
            }
        }
    }

    #[cfg(feature = "email")]
    if let Some(p) = config.platforms.get("email") {
        if p.enabled {
            let ready = extra_string(p, "imap_host").is_some()
                && extra_string(p, "smtp_host").is_some()
                && extra_string(p, "username").is_some()
                && extra_string(p, "password").is_some();
            if check(p.enabled, ready) {
                push_fatal(
                    &mut issues,
                    "email",
                    "missing_email_credentials",
                    "email.enabled=true 但缺少 imap_host/smtp_host/username/password",
                );
            }
        }
    }

    #[cfg(feature = "sms")]
    if let Some(p) = config.platforms.get("sms") {
        if p.enabled {
            let ready = extra_string(p, "account_sid").is_some()
                && extra_string(p, "auth_token").is_some()
                && extra_string(p, "from_number").is_some();
            if check(p.enabled, ready) {
                push_fatal(
                    &mut issues,
                    "sms",
                    "missing_sms_credentials",
                    "sms.enabled=true 但缺少 account_sid/auth_token/from_number",
                );
            }
        }
    }

    #[cfg(feature = "homeassistant")]
    if let Some(p) = config.platforms.get("homeassistant") {
        if p.enabled {
            let token = platform_token_or_extra(p)
                .or_else(|| extra_string(p, "long_lived_token"))
                .is_some();
            let base = extra_string(p, "base_url").is_some();
            if check(p.enabled, base && token) {
                push_fatal(
                    &mut issues,
                    "homeassistant",
                    "missing_homeassistant_credentials",
                    "homeassistant.enabled=true 但缺少 base_url 或 token（long_lived_token）",
                );
            }
        }
    }

    #[cfg(feature = "webhook")]
    if let Some(p) = config.platforms.get("webhook") {
        if check(p.enabled, extra_string(p, "secret").is_some()) {
            push_fatal(
                &mut issues,
                "webhook",
                "missing_secret",
                "webhook.enabled=true 但缺少 secret",
            );
        }
    }

    issues
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(feature = "wecom")]
    use crate::test_env;
    use hermes_config::PlatformConfig;
    use std::sync::{Mutex, OnceLock};

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    fn make_platform(enabled: bool, token: Option<&str>) -> PlatformConfig {
        let mut cfg = PlatformConfig {
            enabled,
            ..Default::default()
        };
        if let Some(t) = token {
            cfg.token = Some(t.to_string());
        }
        cfg
    }

    #[test]
    fn gateway_requirement_issues_matches_fatal_messages() {
        use crate::gateway_requirement_issues;
        let mut config = GatewayConfig::default();
        let mut m = make_platform(true, None);
        m.extra.insert(
            "homeserver_url".to_string(),
            serde_json::json!("https://m.org"),
        );
        config.platforms.insert("matrix".to_string(), m);

        let legacy = gateway_requirement_issues(&config);
        let fatal: Vec<String> =
            evaluate_gateway_requirements(&config, RequirementScope::RuntimeStart)
                .into_iter()
                .filter(|i| i.severity == RequirementSeverity::Fatal)
                .map(|i| i.message)
                .collect();
        assert_eq!(
            legacy, fatal,
            "fatal messages must match legacy gateway_requirement_issues"
        );
    }

    #[test]
    #[cfg(feature = "wecom-callback")]
    fn wecom_callback_warn_emitted_for_doctor_not_live_messaging() {
        let mut config = GatewayConfig::default();
        let mut p = PlatformConfig {
            enabled: true,
            ..Default::default()
        };
        p.extra
            .insert("corp_id".to_string(), serde_json::json!("c"));
        p.extra
            .insert("corp_secret".to_string(), serde_json::json!("s"));
        p.extra
            .insert("agent_id".to_string(), serde_json::json!("1"));
        p.token = Some("tok".to_string());
        p.extra.insert(
            "encoding_aes_key".to_string(),
            serde_json::json!("0123456789012345678901234567890123456789012345"),
        );
        config.platforms.insert("wecom_callback".to_string(), p);

        let doctor = evaluate_gateway_requirements(&config, RequirementScope::Doctor);
        assert!(
            doctor
                .iter()
                .any(|i| i.code == "adapter_partial_implementation"),
            "{doctor:?}"
        );

        let live = evaluate_gateway_requirements(&config, RequirementScope::LiveMessaging);
        assert!(!live
            .iter()
            .any(|i| i.code == "adapter_partial_implementation"));
    }

    #[test]
    #[cfg(feature = "wecom")]
    fn wecom_websocket_credentials_use_bot_id_secret_with_corp_id_fallback() {
        let mut config = GatewayConfig::default();
        let mut wecom = make_platform(true, None);
        wecom
            .extra
            .insert("bot_id".to_string(), serde_json::json!("wb_123"));
        wecom
            .extra
            .insert("secret".to_string(), serde_json::json!("sec_123"));
        config.platforms.insert("wecom".to_string(), wecom);
        let issues = evaluate_gateway_requirements(&config, RequirementScope::RuntimeStart);
        assert!(
            !issues
                .iter()
                .any(|i| i.platform == "wecom" && i.severity == RequirementSeverity::Fatal),
            "{issues:?}"
        );

        let mut legacy = make_platform(true, None);
        legacy
            .extra
            .insert("corp_id".to_string(), serde_json::json!("legacy-corp"));
        legacy
            .extra
            .insert("secret".to_string(), serde_json::json!("legacy-secret"));
        config.platforms.insert("wecom".to_string(), legacy);
        let issues = evaluate_gateway_requirements(&config, RequirementScope::RuntimeStart);
        assert!(
            !issues
                .iter()
                .any(|i| i.platform == "wecom" && i.severity == RequirementSeverity::Fatal),
            "{issues:?}"
        );
    }

    #[test]
    #[cfg(feature = "wecom")]
    fn wecom_websocket_credentials_allow_env_fallback() {
        let _guard = env_lock().lock().expect("env lock poisoned");
        let mut config = GatewayConfig::default();
        let wecom = make_platform(true, None);
        config.platforms.insert("wecom".to_string(), wecom);

        test_env::set_var("WECOM_BOT_ID", "env-bot");
        test_env::set_var("WECOM_SECRET", "env-secret");

        let issues = evaluate_gateway_requirements(&config, RequirementScope::RuntimeStart);
        assert!(
            !issues
                .iter()
                .any(|i| i.platform == "wecom" && i.severity == RequirementSeverity::Fatal),
            "{issues:?}"
        );

        test_env::remove_var("WECOM_BOT_ID");
        test_env::remove_var("WECOM_SECRET");
    }

    #[test]
    #[cfg(feature = "wecom")]
    fn wecom_websocket_credentials_do_not_use_wechaty_service_token_as_bot_id() {
        let _guard = env_lock().lock().expect("env lock poisoned");
        let mut config = GatewayConfig::default();
        let wecom = make_platform(true, None);
        config.platforms.insert("wecom".to_string(), wecom);

        test_env::set_var("WECOM_BOT_ID", "");
        test_env::set_var("WECHATY_PUPPET_SERVICE_TOKEN", "legacy-wechaty-token");
        test_env::set_var("WECOM_SECRET", "env-secret");

        let issues = evaluate_gateway_requirements(&config, RequirementScope::RuntimeStart);
        assert!(
            issues
                .iter()
                .any(|i| i.platform == "wecom" && i.severity == RequirementSeverity::Fatal),
            "{issues:?}"
        );

        test_env::remove_var("WECOM_BOT_ID");
        test_env::remove_var("WECHATY_PUPPET_SERVICE_TOKEN");
        test_env::remove_var("WECOM_SECRET");
    }
}
