//! 将 Python Hermes 使用的 **同名** 平台环境变量写入 [`GatewayConfig::platforms`]，
//! 与 `gateway/platforms/weixin.py`、`dingtalk.py` 中 `os.getenv(...)` 的键一致。
//!
//! 在 [`crate::loader::apply_env_overrides`] 末尾调用，优先级高于 YAML（与现有 env 覆盖链一致）。

use serde_json::{json, Value};

use crate::config::GatewayConfig;
use crate::platform::PlatformConfig;

fn env_nonempty(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn comma_list_to_strings(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn set_extra(pc: &mut PlatformConfig, key: &str, val: Value) {
    pc.extra.insert(key.to_string(), val);
}

fn apply_weixin_env(config: &mut GatewayConfig) {
    let wx = config
        .platforms
        .entry("weixin".into())
        .or_insert_with(PlatformConfig::default);

    if let Some(t) = env_nonempty("WEIXIN_TOKEN") {
        wx.token = Some(t);
    }
    if let Some(v) = env_nonempty("WEIXIN_ACCOUNT_ID") {
        set_extra(wx, "account_id", json!(v));
    }
    if let Some(v) = env_nonempty("WEIXIN_BASE_URL") {
        set_extra(wx, "base_url", json!(v));
    }
    if let Some(v) = env_nonempty("WEIXIN_CDN_BASE_URL") {
        set_extra(wx, "cdn_base_url", json!(v));
    }
    if let Some(v) = env_nonempty("WEIXIN_DM_POLICY") {
        set_extra(wx, "dm_policy", json!(v));
    }
    if let Some(v) = env_nonempty("WEIXIN_GROUP_POLICY") {
        set_extra(wx, "group_policy", json!(v));
    }
    if let Some(v) = env_nonempty("WEIXIN_ALLOWED_USERS") {
        let list = comma_list_to_strings(&v);
        set_extra(wx, "allow_from", json!(list));
    }
    if let Some(v) = env_nonempty("WEIXIN_GROUP_ALLOWED_USERS") {
        let list = comma_list_to_strings(&v);
        set_extra(wx, "group_allow_from", json!(list));
    }
    if let Some(v) = env_nonempty("WEIXIN_HOME_CHANNEL") {
        wx.home_channel = Some(v);
    }
    if let Some(v) = env_nonempty("WEIXIN_HOME_CHANNEL_NAME") {
        set_extra(wx, "home_channel_name", json!(v));
    }
    if let Some(v) = env_nonempty("WEIXIN_ALLOW_ALL_USERS") {
        let flag = matches!(v.to_lowercase().as_str(), "1" | "true" | "yes" | "on");
        set_extra(wx, "allow_all_users", json!(flag));
    }
}

fn extra_string_list(pc: &PlatformConfig, key: &str) -> Vec<String> {
    pc.extra
        .get(key)
        .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
        .unwrap_or_default()
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn set_env_if_unset(key: &str, value: &str) {
    if std::env::var(key)
        .ok()
        .map(|s| s.trim().to_string())
        .is_none_or(|s| s.is_empty())
    {
        // SAFETY: config load path only; mirrors Python `load_gateway_config` env bridge.
        unsafe { std::env::set_var(key, value) };
    }
}

fn apply_telegram_env(config: &mut GatewayConfig) {
    let tg = config
        .platforms
        .entry("telegram".into())
        .or_insert_with(PlatformConfig::default);

    if tg.token.is_none() {
        if let Some(t) = env_nonempty("TELEGRAM_BOT_TOKEN") {
            tg.token = Some(t);
        }
    }

    if env_nonempty("TELEGRAM_ALLOWED_USERS").is_none() {
        let mut users = tg
            .allowed_users
            .iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        if users.is_empty() {
            users = extra_string_list(tg, "allow_from");
        }
        if !users.is_empty() {
            set_env_if_unset("TELEGRAM_ALLOWED_USERS", &users.join(","));
        }
    }

    if env_nonempty("TELEGRAM_GROUP_ALLOWED_USERS").is_none() {
        let users = extra_string_list(tg, "group_allow_from");
        if !users.is_empty() {
            set_env_if_unset("TELEGRAM_GROUP_ALLOWED_USERS", &users.join(","));
        }
    }

    if env_nonempty("TELEGRAM_GROUP_ALLOWED_CHATS").is_none() {
        let chats = extra_string_list(tg, "group_allowed_chats");
        if !chats.is_empty() {
            set_env_if_unset("TELEGRAM_GROUP_ALLOWED_CHATS", &chats.join(","));
        }
    }

    if env_nonempty("TELEGRAM_WEBHOOK_URL").is_none() {
        if let Some(url) = tg.webhook_url.as_deref().filter(|s| !s.trim().is_empty()) {
            set_env_if_unset("TELEGRAM_WEBHOOK_URL", url.trim());
        }
    }

    if env_nonempty("TELEGRAM_WEBHOOK_SECRET").is_none() {
        if let Some(secret) = tg
            .extra
            .get("webhook_secret")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            set_env_if_unset("TELEGRAM_WEBHOOK_SECRET", secret);
        }
    }

    if env_nonempty("TELEGRAM_WEBHOOK_PORT").is_none() {
        if let Some(port) = tg.extra.get("webhook_port").and_then(|v| v.as_u64()) {
            set_env_if_unset("TELEGRAM_WEBHOOK_PORT", &port.to_string());
        }
    }
}

fn apply_dingtalk_env(config: &mut GatewayConfig) {
    let dt = config
        .platforms
        .entry("dingtalk".into())
        .or_insert_with(PlatformConfig::default);

    if let Some(v) = env_nonempty("DINGTALK_CLIENT_ID") {
        set_extra(dt, "client_id", json!(v));
    }
    if let Some(v) = env_nonempty("DINGTALK_CLIENT_SECRET") {
        set_extra(dt, "client_secret", json!(v));
    }
    if let Some(v) = env_nonempty("DINGTALK_OPENAPI_ENDPOINT") {
        set_extra(dt, "openapi_endpoint", json!(v));
    }
}

/// 应用与 Python 文档一致的 `WEIXIN_*` / `DINGTALK_*` / `TELEGRAM_*` 环境变量到 `platforms`，
/// 并将 YAML 中的 Telegram 白名单 / webhook 设置桥接到进程环境（与 Python `load_gateway_config` 一致）。
pub fn apply_python_named_platform_env(config: &mut GatewayConfig) {
    apply_weixin_env(config);
    apply_dingtalk_env(config);
    apply_telegram_env(config);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn weixin_env_sets_platform_extra_and_token() {
        unsafe {
            std::env::set_var("WEIXIN_ACCOUNT_ID", "acc_x");
            std::env::set_var("WEIXIN_TOKEN", "tok_y");
            std::env::set_var("WEIXIN_DM_POLICY", "allowlist");
            std::env::set_var("WEIXIN_ALLOWED_USERS", " u1 , u2 ");
        }
        let mut cfg = GatewayConfig::default();
        apply_python_named_platform_env(&mut cfg);
        let wx = cfg.platforms.get("weixin").expect("weixin block");
        assert_eq!(wx.token.as_deref(), Some("tok_y"));
        assert_eq!(
            wx.extra.get("account_id").and_then(|v| v.as_str()),
            Some("acc_x")
        );
        assert_eq!(
            wx.extra.get("dm_policy").and_then(|v| v.as_str()),
            Some("allowlist")
        );
        let af = wx
            .extra
            .get("allow_from")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let ids: Vec<&str> = af.iter().filter_map(|x| x.as_str()).collect();
        assert_eq!(ids, vec!["u1", "u2"]);
        unsafe {
            std::env::remove_var("WEIXIN_ACCOUNT_ID");
            std::env::remove_var("WEIXIN_TOKEN");
            std::env::remove_var("WEIXIN_DM_POLICY");
            std::env::remove_var("WEIXIN_ALLOWED_USERS");
        }
    }

    #[test]
    fn telegram_yaml_bridges_allowlists_to_env() {
        let mut cfg = GatewayConfig::default();
        let tg = cfg
            .platforms
            .entry("telegram".into())
            .or_insert_with(PlatformConfig::default);
        tg.allowed_users = vec!["111".into(), "222".into()];
        tg.extra.insert(
            "group_allow_from".into(),
            json!(["333"]),
        );
        tg.extra.insert(
            "group_allowed_chats".into(),
            json!(["-100"]),
        );
        tg.webhook_url = Some("https://example.com/telegram".into());
        tg.extra.insert("webhook_secret".into(), json!("sec"));
        tg.extra.insert("webhook_port".into(), json!(9443));

        unsafe {
            std::env::remove_var("TELEGRAM_ALLOWED_USERS");
            std::env::remove_var("TELEGRAM_GROUP_ALLOWED_USERS");
            std::env::remove_var("TELEGRAM_GROUP_ALLOWED_CHATS");
            std::env::remove_var("TELEGRAM_WEBHOOK_URL");
            std::env::remove_var("TELEGRAM_WEBHOOK_SECRET");
            std::env::remove_var("TELEGRAM_WEBHOOK_PORT");
        }

        apply_python_named_platform_env(&mut cfg);

        assert_eq!(
            std::env::var("TELEGRAM_ALLOWED_USERS").ok().as_deref(),
            Some("111,222")
        );
        assert_eq!(
            std::env::var("TELEGRAM_GROUP_ALLOWED_USERS").ok().as_deref(),
            Some("333")
        );
        assert_eq!(
            std::env::var("TELEGRAM_GROUP_ALLOWED_CHATS").ok().as_deref(),
            Some("-100")
        );
        assert_eq!(
            std::env::var("TELEGRAM_WEBHOOK_URL").ok().as_deref(),
            Some("https://example.com/telegram")
        );
        assert_eq!(
            std::env::var("TELEGRAM_WEBHOOK_SECRET").ok().as_deref(),
            Some("sec")
        );
        assert_eq!(
            std::env::var("TELEGRAM_WEBHOOK_PORT").ok().as_deref(),
            Some("9443")
        );

        unsafe {
            std::env::remove_var("TELEGRAM_ALLOWED_USERS");
            std::env::remove_var("TELEGRAM_GROUP_ALLOWED_USERS");
            std::env::remove_var("TELEGRAM_GROUP_ALLOWED_CHATS");
            std::env::remove_var("TELEGRAM_WEBHOOK_URL");
            std::env::remove_var("TELEGRAM_WEBHOOK_SECRET");
            std::env::remove_var("TELEGRAM_WEBHOOK_PORT");
        }
    }

    #[test]
    fn dingtalk_env_sets_client_fields() {
        unsafe {
            std::env::set_var("DINGTALK_CLIENT_ID", "cid");
            std::env::set_var("DINGTALK_CLIENT_SECRET", "sec");
            std::env::set_var("DINGTALK_OPENAPI_ENDPOINT", "https://api.example.com");
        }
        let mut cfg = GatewayConfig::default();
        apply_python_named_platform_env(&mut cfg);
        let dt = cfg.platforms.get("dingtalk").expect("dingtalk block");
        assert_eq!(
            dt.extra.get("client_id").and_then(|v| v.as_str()),
            Some("cid")
        );
        assert_eq!(
            dt.extra.get("client_secret").and_then(|v| v.as_str()),
            Some("sec")
        );
        assert_eq!(
            dt.extra.get("openapi_endpoint").and_then(|v| v.as_str()),
            Some("https://api.example.com")
        );
        unsafe {
            std::env::remove_var("DINGTALK_CLIENT_ID");
            std::env::remove_var("DINGTALK_CLIENT_SECRET");
            std::env::remove_var("DINGTALK_OPENAPI_ENDPOINT");
        }
    }
}
