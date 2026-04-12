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

/// 应用与 Python 文档一致的 `WEIXIN_*` / `DINGTALK_*` 环境变量到 `platforms`。
pub fn apply_python_named_platform_env(config: &mut GatewayConfig) {
    apply_weixin_env(config);
    apply_dingtalk_env(config);
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
