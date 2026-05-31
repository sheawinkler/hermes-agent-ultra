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

fn json_array_or_csv(raw: &str) -> Vec<String> {
    serde_json::from_str::<Vec<String>>(raw)
        .unwrap_or_else(|_| comma_list_to_strings(raw))
        .into_iter()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn env_bool(raw: &str) -> bool {
    matches!(raw.to_lowercase().as_str(), "1" | "true" | "yes" | "on")
}

fn reply_to_mode(raw: &str) -> Option<&'static str> {
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

fn apply_telegram_env(config: &mut GatewayConfig) {
    let token = env_nonempty("TELEGRAM_BOT_TOKEN");
    let webhook_url = env_nonempty("TELEGRAM_WEBHOOK_URL");
    let webhook_secret = env_nonempty("TELEGRAM_WEBHOOK_SECRET");
    let reply_mode = env_nonempty("TELEGRAM_REPLY_TO_MODE").and_then(|v| reply_to_mode(&v));
    let reactions = env_nonempty("TELEGRAM_REACTIONS");
    let fallback_ips = env_nonempty("TELEGRAM_FALLBACK_IPS");
    let require_mention = env_nonempty("TELEGRAM_REQUIRE_MENTION");
    let guest_mode = env_nonempty("TELEGRAM_GUEST_MODE");
    let exclusive_bot_mentions = env_nonempty("TELEGRAM_EXCLUSIVE_BOT_MENTIONS");
    let observe_unmentioned = env_nonempty("TELEGRAM_OBSERVE_UNMENTIONED_GROUP_MESSAGES");
    let mention_patterns = env_nonempty("TELEGRAM_MENTION_PATTERNS");
    let free_response_chats = env_nonempty("TELEGRAM_FREE_RESPONSE_CHATS");
    let allowed_chats = env_nonempty("TELEGRAM_ALLOWED_CHATS");
    let group_allowed_chats = env_nonempty("TELEGRAM_GROUP_ALLOWED_CHATS");
    let allowed_topics = env_nonempty("TELEGRAM_ALLOWED_TOPICS");
    let ignored_threads = env_nonempty("TELEGRAM_IGNORED_THREADS");
    let allowed_users = env_nonempty("TELEGRAM_ALLOWED_USERS");
    let group_allowed_users = env_nonempty("TELEGRAM_GROUP_ALLOWED_USERS");

    if token.is_none()
        && webhook_url.is_none()
        && webhook_secret.is_none()
        && reply_mode.is_none()
        && reactions.is_none()
        && fallback_ips.is_none()
        && require_mention.is_none()
        && guest_mode.is_none()
        && exclusive_bot_mentions.is_none()
        && observe_unmentioned.is_none()
        && mention_patterns.is_none()
        && free_response_chats.is_none()
        && allowed_chats.is_none()
        && group_allowed_chats.is_none()
        && allowed_topics.is_none()
        && ignored_threads.is_none()
        && allowed_users.is_none()
        && group_allowed_users.is_none()
    {
        return;
    }

    let telegram = config
        .platforms
        .entry("telegram".into())
        .or_insert_with(PlatformConfig::default);

    if let Some(token) = token {
        let enabled_was_explicit = telegram
            .extra
            .remove("_enabled_explicit")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !telegram.enabled && !enabled_was_explicit {
            telegram.enabled = true;
        }
        telegram.token = Some(token);
    }
    if let Some(v) = webhook_url {
        telegram.webhook_url = Some(v);
    }
    if let Some(v) = webhook_secret {
        set_extra(telegram, "webhook_secret", json!(v));
    }
    if let Some(mode) = reply_mode {
        set_extra(telegram, "reply_to_mode", json!(mode));
    }
    if let Some(v) = reactions {
        set_extra(telegram, "reactions", json!(env_bool(&v)));
    }
    if let Some(v) = fallback_ips {
        let ips = comma_list_to_strings(&v);
        if !ips.is_empty() {
            set_extra(telegram, "fallback_ips", json!(ips));
        }
    }
    if let Some(v) = require_mention {
        telegram.require_mention = Some(env_bool(&v));
        set_extra(telegram, "require_mention", json!(env_bool(&v)));
    }
    if let Some(v) = guest_mode {
        set_extra(telegram, "guest_mode", json!(env_bool(&v)));
    }
    if let Some(v) = exclusive_bot_mentions {
        set_extra(telegram, "exclusive_bot_mentions", json!(env_bool(&v)));
    }
    if let Some(v) = observe_unmentioned {
        set_extra(
            telegram,
            "observe_unmentioned_group_messages",
            json!(env_bool(&v)),
        );
    }
    if let Some(v) = mention_patterns {
        set_extra(telegram, "mention_patterns", json!(json_array_or_csv(&v)));
    }
    if let Some(v) = free_response_chats {
        set_extra(
            telegram,
            "free_response_chats",
            json!(comma_list_to_strings(&v)),
        );
    }
    if let Some(v) = allowed_chats {
        set_extra(telegram, "allowed_chats", json!(comma_list_to_strings(&v)));
    }
    if let Some(v) = group_allowed_chats {
        set_extra(
            telegram,
            "group_allowed_chats",
            json!(comma_list_to_strings(&v)),
        );
    }
    if let Some(v) = allowed_topics {
        set_extra(telegram, "allowed_topics", json!(comma_list_to_strings(&v)));
    }
    if let Some(v) = ignored_threads {
        set_extra(
            telegram,
            "ignored_threads",
            json!(comma_list_to_strings(&v)),
        );
    }
    if let Some(v) = allowed_users {
        telegram.allowed_users = comma_list_to_strings(&v);
    }
    if let Some(v) = group_allowed_users {
        set_extra(
            telegram,
            "group_allow_from",
            json!(comma_list_to_strings(&v)),
        );
    }
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
    if let Some(v) = env_nonempty("DINGTALK_ALLOWED_CHATS") {
        set_extra(dt, "allowed_chats", json!(comma_list_to_strings(&v)));
    }
}

fn apply_mattermost_env(config: &mut GatewayConfig) {
    let server_url = env_nonempty("MATTERMOST_SERVER_URL");
    let token = env_nonempty("MATTERMOST_TOKEN");
    let team_id = env_nonempty("MATTERMOST_TEAM_ID");
    let allowed_channels = env_nonempty("MATTERMOST_ALLOWED_CHANNELS");

    if server_url.is_none() && token.is_none() && team_id.is_none() && allowed_channels.is_none() {
        return;
    }

    let mattermost = config
        .platforms
        .entry("mattermost".into())
        .or_insert_with(PlatformConfig::default);
    if let Some(v) = server_url {
        set_extra(mattermost, "server_url", json!(v));
    }
    if let Some(v) = token {
        mattermost.token = Some(v);
        let enabled_was_explicit = mattermost
            .extra
            .remove("_enabled_explicit")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !mattermost.enabled && !enabled_was_explicit {
            mattermost.enabled = true;
        }
    }
    if let Some(v) = team_id {
        set_extra(mattermost, "team_id", json!(v));
    }
    if let Some(v) = allowed_channels {
        set_extra(
            mattermost,
            "allowed_channels",
            json!(comma_list_to_strings(&v)),
        );
    }
}

fn apply_matrix_env(config: &mut GatewayConfig) {
    let homeserver_url = env_nonempty("MATRIX_HOMESERVER_URL");
    let user_id = env_nonempty("MATRIX_USER_ID");
    let access_token = env_nonempty("MATRIX_ACCESS_TOKEN");
    let room_id = env_nonempty("MATRIX_ROOM_ID").or_else(|| env_nonempty("MATRIX_HOME_ROOM"));
    let allowed_rooms = env_nonempty("MATRIX_ALLOWED_ROOMS");

    if homeserver_url.is_none()
        && user_id.is_none()
        && access_token.is_none()
        && room_id.is_none()
        && allowed_rooms.is_none()
    {
        return;
    }

    let matrix = config
        .platforms
        .entry("matrix".into())
        .or_insert_with(PlatformConfig::default);
    if let Some(v) = homeserver_url {
        set_extra(matrix, "homeserver_url", json!(v));
    }
    if let Some(v) = user_id {
        set_extra(matrix, "user_id", json!(v));
    }
    if let Some(v) = access_token {
        matrix.token = Some(v.clone());
        set_extra(matrix, "access_token", json!(v));
        let enabled_was_explicit = matrix
            .extra
            .remove("_enabled_explicit")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !matrix.enabled && !enabled_was_explicit {
            matrix.enabled = true;
        }
    }
    if let Some(v) = room_id {
        set_extra(matrix, "room_id", json!(v));
    }
    if let Some(v) = allowed_rooms {
        set_extra(matrix, "allowed_rooms", json!(comma_list_to_strings(&v)));
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
        if let Some(mode) = reply_to_mode(&v) {
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
    apply_telegram_env(config);
    apply_discord_env(config);
    apply_weixin_env(config);
    apply_dingtalk_env(config);
    apply_mattermost_env(config);
    apply_matrix_env(config);
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
    fn telegram_env_sets_reply_reactions_webhook_secret_and_token() {
        let _env = env_lock();
        unsafe {
            std::env::set_var("TELEGRAM_BOT_TOKEN", "telegram-token");
            std::env::set_var("TELEGRAM_WEBHOOK_URL", "https://hooks.example.com/tg");
            std::env::set_var("TELEGRAM_WEBHOOK_SECRET", "telegram-secret");
            std::env::set_var("TELEGRAM_REPLY_TO_MODE", "ALL");
            std::env::set_var("TELEGRAM_REACTIONS", "1");
            std::env::set_var("TELEGRAM_FALLBACK_IPS", "149.154.167.220,149.154.167.221");
            std::env::set_var("TELEGRAM_REQUIRE_MENTION", "true");
            std::env::set_var("TELEGRAM_GUEST_MODE", "yes");
            std::env::set_var("TELEGRAM_EXCLUSIVE_BOT_MENTIONS", "on");
            std::env::set_var("TELEGRAM_OBSERVE_UNMENTIONED_GROUP_MESSAGES", "1");
            std::env::set_var(
                "TELEGRAM_MENTION_PATTERNS",
                r#"["^\\s*chompy\\b","@hermes"]"#,
            );
            std::env::set_var("TELEGRAM_FREE_RESPONSE_CHATS", "-100,-101");
            std::env::set_var("TELEGRAM_ALLOWED_CHATS", "-200");
            std::env::set_var("TELEGRAM_GROUP_ALLOWED_CHATS", "-300,-301");
            std::env::set_var("TELEGRAM_ALLOWED_TOPICS", "8,0");
            std::env::set_var("TELEGRAM_IGNORED_THREADS", "31, 32");
            std::env::set_var("TELEGRAM_ALLOWED_USERS", "u1,u2");
            std::env::set_var("TELEGRAM_GROUP_ALLOWED_USERS", "g1,g2");
        }
        let mut cfg = GatewayConfig::default();
        apply_python_named_platform_env(&mut cfg);
        let telegram = cfg.platforms.get("telegram").expect("telegram block");
        assert!(telegram.enabled);
        assert_eq!(telegram.token.as_deref(), Some("telegram-token"));
        assert_eq!(
            telegram.webhook_url.as_deref(),
            Some("https://hooks.example.com/tg")
        );
        assert_eq!(
            telegram
                .extra
                .get("webhook_secret")
                .and_then(|v| v.as_str()),
            Some("telegram-secret")
        );
        assert_eq!(
            telegram.extra.get("reply_to_mode").and_then(|v| v.as_str()),
            Some("all")
        );
        assert_eq!(
            telegram.extra.get("reactions").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(telegram.require_mention, Some(true));
        assert_eq!(telegram.allowed_users, vec!["u1", "u2"]);
        assert_eq!(
            telegram
                .extra
                .get("fallback_ips")
                .and_then(|v| v.as_array())
                .map(|items| items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .collect::<Vec<_>>()),
            Some(vec!["149.154.167.220", "149.154.167.221"])
        );
        assert_eq!(
            telegram
                .extra
                .get("require_mention")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            telegram.extra.get("guest_mode").and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            telegram
                .extra
                .get("exclusive_bot_mentions")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            telegram
                .extra
                .get("observe_unmentioned_group_messages")
                .and_then(|v| v.as_bool()),
            Some(true)
        );
        assert_eq!(
            telegram
                .extra
                .get("mention_patterns")
                .and_then(|v| v.as_array())
                .map(|items| items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .collect::<Vec<_>>()),
            Some(vec![r"^\s*chompy\b", "@hermes"])
        );
        assert_eq!(
            telegram
                .extra
                .get("free_response_chats")
                .and_then(|v| v.as_array())
                .map(|items| items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .collect::<Vec<_>>()),
            Some(vec!["-100", "-101"])
        );
        assert_eq!(
            telegram
                .extra
                .get("allowed_chats")
                .and_then(|v| v.as_array())
                .map(|items| items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .collect::<Vec<_>>()),
            Some(vec!["-200"])
        );
        assert_eq!(
            telegram
                .extra
                .get("group_allowed_chats")
                .and_then(|v| v.as_array())
                .map(|items| items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .collect::<Vec<_>>()),
            Some(vec!["-300", "-301"])
        );
        assert_eq!(
            telegram
                .extra
                .get("allowed_topics")
                .and_then(|v| v.as_array())
                .map(|items| items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .collect::<Vec<_>>()),
            Some(vec!["8", "0"])
        );
        assert_eq!(
            telegram
                .extra
                .get("ignored_threads")
                .and_then(|v| v.as_array())
                .map(|items| items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .collect::<Vec<_>>()),
            Some(vec!["31", "32"])
        );
        assert_eq!(
            telegram
                .extra
                .get("group_allow_from")
                .and_then(|v| v.as_array())
                .map(|items| items
                    .iter()
                    .filter_map(|item| item.as_str())
                    .collect::<Vec<_>>()),
            Some(vec!["g1", "g2"])
        );
        unsafe {
            std::env::remove_var("TELEGRAM_BOT_TOKEN");
            std::env::remove_var("TELEGRAM_WEBHOOK_URL");
            std::env::remove_var("TELEGRAM_WEBHOOK_SECRET");
            std::env::remove_var("TELEGRAM_REPLY_TO_MODE");
            std::env::remove_var("TELEGRAM_REACTIONS");
            std::env::remove_var("TELEGRAM_FALLBACK_IPS");
            std::env::remove_var("TELEGRAM_REQUIRE_MENTION");
            std::env::remove_var("TELEGRAM_GUEST_MODE");
            std::env::remove_var("TELEGRAM_EXCLUSIVE_BOT_MENTIONS");
            std::env::remove_var("TELEGRAM_OBSERVE_UNMENTIONED_GROUP_MESSAGES");
            std::env::remove_var("TELEGRAM_MENTION_PATTERNS");
            std::env::remove_var("TELEGRAM_FREE_RESPONSE_CHATS");
            std::env::remove_var("TELEGRAM_ALLOWED_CHATS");
            std::env::remove_var("TELEGRAM_GROUP_ALLOWED_CHATS");
            std::env::remove_var("TELEGRAM_ALLOWED_TOPICS");
            std::env::remove_var("TELEGRAM_IGNORED_THREADS");
            std::env::remove_var("TELEGRAM_ALLOWED_USERS");
            std::env::remove_var("TELEGRAM_GROUP_ALLOWED_USERS");
        }
    }

    #[test]
    fn telegram_reply_to_mode_env_ignores_invalid_values() {
        let _env = env_lock();
        unsafe {
            std::env::set_var("TELEGRAM_REPLY_TO_MODE", "banana");
        }
        let mut cfg = GatewayConfig::default();
        cfg.platforms
            .entry("telegram".into())
            .or_default()
            .extra
            .insert("reply_to_mode".into(), json!("off"));

        apply_python_named_platform_env(&mut cfg);
        let telegram = cfg.platforms.get("telegram").expect("telegram block");
        assert_eq!(
            telegram.extra.get("reply_to_mode").and_then(|v| v.as_str()),
            Some("off")
        );

        unsafe {
            std::env::remove_var("TELEGRAM_REPLY_TO_MODE");
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
            std::env::set_var("DINGTALK_ALLOWED_CHATS", "cidABC,cidDEF");
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
        assert_eq!(
            dt.extra
                .get("allowed_chats")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>()),
            Some(vec!["cidABC", "cidDEF"])
        );
        unsafe {
            std::env::remove_var("DINGTALK_CLIENT_ID");
            std::env::remove_var("DINGTALK_CLIENT_SECRET");
            std::env::remove_var("DINGTALK_OPENAPI_ENDPOINT");
            std::env::remove_var("DINGTALK_ALLOWED_CHATS");
        }
    }

    #[test]
    fn mattermost_and_matrix_allowed_channel_envs_are_imported() {
        let _env = env_lock();
        unsafe {
            std::env::set_var("MATTERMOST_ALLOWED_CHANNELS", "chanABC,chanDEF");
            std::env::set_var("MATRIX_ALLOWED_ROOMS", "!room1:srv,!room2:srv");
        }
        let mut cfg = GatewayConfig::default();
        apply_python_named_platform_env(&mut cfg);
        let mattermost = cfg.platforms.get("mattermost").expect("mattermost block");
        assert_eq!(
            mattermost
                .extra
                .get("allowed_channels")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>()),
            Some(vec!["chanABC", "chanDEF"])
        );
        let matrix = cfg.platforms.get("matrix").expect("matrix block");
        assert_eq!(
            matrix
                .extra
                .get("allowed_rooms")
                .and_then(|v| v.as_array())
                .map(|items| items.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>()),
            Some(vec!["!room1:srv", "!room2:srv"])
        );
        unsafe {
            std::env::remove_var("MATTERMOST_ALLOWED_CHANNELS");
            std::env::remove_var("MATRIX_ALLOWED_ROOMS");
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
