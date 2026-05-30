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

fn env_bool(raw: &str) -> bool {
    matches!(raw.to_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

fn discord_reply_to_mode(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "off" => Some("off"),
        "first" => Some("first"),
        "all" => Some("all"),
        _ => None,
    }
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

fn apply_ntfy_env(config: &mut GatewayConfig) {
    let Some(topic) = env_nonempty("NTFY_TOPIC") else {
        return;
    };
    let ntfy = config
        .platforms
        .entry("ntfy".into())
        .or_insert_with(PlatformConfig::default);
    let enabled_was_explicit = ntfy
        .extra
        .remove("_enabled_explicit")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if !ntfy.enabled && !enabled_was_explicit {
        ntfy.enabled = true;
    }
    set_extra(ntfy, "topic", json!(topic));
    if let Some(v) = env_nonempty("NTFY_SERVER_URL") {
        set_extra(ntfy, "server", json!(v));
    }
    if let Some(v) = env_nonempty("NTFY_PUBLISH_TOPIC") {
        set_extra(ntfy, "publish_topic", json!(v));
    }
    if let Some(v) = env_nonempty("NTFY_TOKEN") {
        ntfy.token = Some(v);
    }
    if let Some(v) = env_nonempty("NTFY_MARKDOWN") {
        set_extra(
            ntfy,
            "markdown",
            json!(matches!(
                v.to_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )),
        );
    }
    if let Some(v) = env_nonempty("NTFY_HOME_CHANNEL") {
        ntfy.home_channel = Some(v);
    }
    if let Some(v) = env_nonempty("NTFY_HOME_CHANNEL_NAME") {
        set_extra(ntfy, "home_channel_name", json!(v));
    }
}

fn apply_discord_env(config: &mut GatewayConfig) {
    let discord = config
        .platforms
        .entry("discord".into())
        .or_insert_with(PlatformConfig::default);

    if let Some(token) = env_nonempty("DISCORD_BOT_TOKEN") {
        let enabled_was_explicit = discord
            .extra
            .remove("_enabled_explicit")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !discord.enabled && !enabled_was_explicit {
            discord.enabled = true;
        }
        discord.token = Some(token);
    }
    if let Some(v) = env_nonempty("DISCORD_APPLICATION_ID") {
        set_extra(discord, "application_id", json!(v));
    }
    if let Some(v) = env_nonempty("DISCORD_ALLOW_BOTS") {
        set_extra(discord, "allow_bots", json!(v));
    }
    if let Some(v) = env_nonempty("DISCORD_ALLOWED_USERS") {
        discord.allowed_users = comma_list_to_strings(&v);
    }
    if let Some(v) = env_nonempty("DISCORD_ALLOWED_ROLES") {
        set_extra(discord, "allowed_roles", json!(comma_list_to_strings(&v)));
    }
    if let Some(v) = env_nonempty("DISCORD_ALLOWED_CHANNELS") {
        set_extra(
            discord,
            "allowed_channels",
            json!(comma_list_to_strings(&v)),
        );
    }
    if let Some(v) = env_nonempty("DISCORD_REACTIONS") {
        set_extra(discord, "reactions", json!(env_bool(&v)));
    }
    if let Some(v) = env_nonempty("DISCORD_REPLY_TO_MODE") {
        if let Some(mode) = discord_reply_to_mode(&v) {
            set_extra(discord, "reply_to_mode", json!(mode));
        }
    }
    if let Some(v) = env_nonempty("DISCORD_REQUIRE_MENTION") {
        discord.require_mention = Some(env_bool(&v));
    }
    if let Some(v) = env_nonempty("DISCORD_IGNORED_CHANNELS") {
        set_extra(
            discord,
            "ignored_channels",
            json!(comma_list_to_strings(&v)),
        );
    }
    if let Some(v) = env_nonempty("DISCORD_NO_THREAD_CHANNELS") {
        set_extra(
            discord,
            "no_thread_channels",
            json!(comma_list_to_strings(&v)),
        );
    }
    if let Some(v) = env_nonempty("DISCORD_FREE_RESPONSE_CHANNELS") {
        set_extra(
            discord,
            "free_response_channels",
            json!(comma_list_to_strings(&v)),
        );
    }
    if let Some(v) = env_nonempty("DISCORD_AUTO_THREAD") {
        set_extra(discord, "auto_thread", json!(env_bool(&v)));
    }
    if let Some(v) = env_nonempty("DISCORD_THREAD_REQUIRE_MENTION") {
        set_extra(discord, "thread_require_mention", json!(env_bool(&v)));
    }
}

/// 应用与 Python 文档一致的平台环境变量到 `platforms`。
pub fn apply_python_named_platform_env(config: &mut GatewayConfig) {
    apply_discord_env(config);
    apply_weixin_env(config);
    apply_dingtalk_env(config);
    apply_ntfy_env(config);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK.lock().unwrap_or_else(|err| err.into_inner())
    }

    #[test]
    fn weixin_env_sets_platform_extra_and_token() {
        let _env = env_lock();
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
    fn discord_env_sets_bot_policy_and_reaction_fields() {
        let _env = env_lock();
        unsafe {
            std::env::set_var("DISCORD_BOT_TOKEN", "discord-token");
            std::env::set_var("DISCORD_APPLICATION_ID", "app-123");
            std::env::set_var("DISCORD_ALLOW_BOTS", "mentions");
            std::env::set_var("DISCORD_ALLOWED_USERS", "100, 200");
            std::env::set_var("DISCORD_ALLOWED_ROLES", "300,400");
            std::env::set_var("DISCORD_ALLOWED_CHANNELS", "500,*");
            std::env::set_var("DISCORD_REACTIONS", "false");
            std::env::set_var("DISCORD_REPLY_TO_MODE", "ALL");
            std::env::set_var("DISCORD_REQUIRE_MENTION", "true");
            std::env::set_var("DISCORD_IGNORED_CHANNELS", "111, 222");
            std::env::set_var("DISCORD_NO_THREAD_CHANNELS", "333");
            std::env::set_var("DISCORD_FREE_RESPONSE_CHANNELS", "444,555");
            std::env::set_var("DISCORD_AUTO_THREAD", "false");
            std::env::set_var("DISCORD_THREAD_REQUIRE_MENTION", "yes");
        }
        let mut cfg = GatewayConfig::default();
        apply_python_named_platform_env(&mut cfg);
        let discord = cfg.platforms.get("discord").expect("discord block");
        assert!(discord.enabled);
        assert_eq!(discord.token.as_deref(), Some("discord-token"));
        assert_eq!(discord.require_mention, Some(true));
        assert_eq!(
            discord.extra.get("application_id").and_then(|v| v.as_str()),
            Some("app-123")
        );
        assert_eq!(
            discord.extra.get("allow_bots").and_then(|v| v.as_str()),
            Some("mentions")
        );
        assert_eq!(discord.allowed_users, vec!["100", "200"]);
        assert_eq!(
            discord
                .extra
                .get("allowed_roles")
                .and_then(|v| v.as_array())
                .map(|items| { items.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>() }),
            Some(vec!["300", "400"])
        );
        assert_eq!(
            discord
                .extra
                .get("allowed_channels")
                .and_then(|v| v.as_array())
                .map(|items| { items.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>() }),
            Some(vec!["500", "*"])
        );
        assert_eq!(
            discord.extra.get("reactions").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            discord.extra.get("reply_to_mode").and_then(|v| v.as_str()),
            Some("all")
        );
        assert_eq!(
            discord
                .extra
                .get("ignored_channels")
                .and_then(|v| v.as_array())
                .map(|items| { items.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>() }),
            Some(vec!["111", "222"])
        );
        assert_eq!(
            discord
                .extra
                .get("no_thread_channels")
                .and_then(|v| v.as_array())
                .map(|items| { items.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>() }),
            Some(vec!["333"])
        );
        assert_eq!(
            discord
                .extra
                .get("free_response_channels")
                .and_then(|v| v.as_array())
                .map(|items| { items.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>() }),
            Some(vec!["444", "555"])
        );
        assert_eq!(
            discord.extra.get("auto_thread").and_then(|v| v.as_bool()),
            Some(false)
        );
        assert_eq!(
            discord
                .extra
                .get("thread_require_mention")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        unsafe {
            std::env::remove_var("DISCORD_BOT_TOKEN");
            std::env::remove_var("DISCORD_APPLICATION_ID");
            std::env::remove_var("DISCORD_ALLOW_BOTS");
            std::env::remove_var("DISCORD_ALLOWED_USERS");
            std::env::remove_var("DISCORD_ALLOWED_ROLES");
            std::env::remove_var("DISCORD_ALLOWED_CHANNELS");
            std::env::remove_var("DISCORD_REACTIONS");
            std::env::remove_var("DISCORD_REPLY_TO_MODE");
            std::env::remove_var("DISCORD_REQUIRE_MENTION");
            std::env::remove_var("DISCORD_IGNORED_CHANNELS");
            std::env::remove_var("DISCORD_NO_THREAD_CHANNELS");
            std::env::remove_var("DISCORD_FREE_RESPONSE_CHANNELS");
            std::env::remove_var("DISCORD_AUTO_THREAD");
            std::env::remove_var("DISCORD_THREAD_REQUIRE_MENTION");
        }
    }

    #[test]
    fn discord_reply_to_mode_env_ignores_invalid_values() {
        let _env = env_lock();
        unsafe {
            std::env::set_var("DISCORD_REPLY_TO_MODE", "banana");
        }
        let mut cfg = GatewayConfig::default();
        cfg.platforms
            .entry("discord".into())
            .or_default()
            .extra
            .insert("reply_to_mode".into(), json!("off"));

        apply_python_named_platform_env(&mut cfg);
        let discord = cfg.platforms.get("discord").expect("discord block");
        assert_eq!(
            discord.extra.get("reply_to_mode").and_then(|v| v.as_str()),
            Some("off")
        );

        unsafe {
            std::env::remove_var("DISCORD_REPLY_TO_MODE");
        }
    }

    #[test]
    fn dingtalk_env_sets_client_fields() {
        let _env = env_lock();
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

    #[test]
    fn ntfy_env_sets_topic_and_auto_enables() {
        let _env = env_lock();
        unsafe {
            std::env::set_var("NTFY_TOPIC", "hermes-in");
            std::env::set_var("NTFY_SERVER_URL", "https://ntfy.example.com");
            std::env::set_var("NTFY_PUBLISH_TOPIC", "hermes-out");
            std::env::set_var("NTFY_TOKEN", "token");
            std::env::set_var("NTFY_MARKDOWN", "true");
        }
        let mut cfg = GatewayConfig::default();
        apply_python_named_platform_env(&mut cfg);
        let ntfy = cfg.platforms.get("ntfy").expect("ntfy block");
        assert!(ntfy.enabled);
        assert_eq!(ntfy.token.as_deref(), Some("token"));
        assert_eq!(
            ntfy.extra.get("topic").and_then(|v| v.as_str()),
            Some("hermes-in")
        );
        assert_eq!(
            ntfy.extra.get("server").and_then(|v| v.as_str()),
            Some("https://ntfy.example.com")
        );
        assert_eq!(
            ntfy.extra.get("publish_topic").and_then(|v| v.as_str()),
            Some("hermes-out")
        );
        assert_eq!(
            ntfy.extra.get("markdown").and_then(|v| v.as_bool()),
            Some(true)
        );
        unsafe {
            std::env::remove_var("NTFY_TOPIC");
            std::env::remove_var("NTFY_SERVER_URL");
            std::env::remove_var("NTFY_PUBLISH_TOPIC");
            std::env::remove_var("NTFY_TOKEN");
            std::env::remove_var("NTFY_MARKDOWN");
        }
    }
}
