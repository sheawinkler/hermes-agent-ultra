//! Discord adapter configuration.

use std::collections::HashSet;

use hermes_config::PlatformConfig;

use crate::adapter::AdapterProxyConfig;

/// Maximum message length for Discord (2000 characters).
pub const MAX_MESSAGE_LENGTH: usize = 2000;

/// Discord API base URL.
pub const DISCORD_API_BASE: &str = "https://discord.com/api/v10";

/// A set of Discord channel snowflakes, with optional `"*"` wildcard.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ChannelIdSet {
    ids: HashSet<String>,
    wildcard: bool,
}

impl ChannelIdSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Parse comma-separated IDs or a JSON string / array value.
    pub fn parse(raw: Option<&str>) -> Self {
        let mut set = Self::new();
        let Some(text) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
            return set;
        };
        set.extend_tokens(text.split(','));
        set
    }

    pub fn extend_tokens<'a, I>(&mut self, tokens: I)
    where
        I: IntoIterator<Item = &'a str>,
    {
        for token in tokens {
            let trimmed = token.trim();
            if trimmed.is_empty() {
                continue;
            }
            if trimmed == "*" {
                self.wildcard = true;
            } else {
                self.ids.insert(trimmed.to_string());
            }
        }
    }

    pub fn merge_json(&mut self, value: Option<&serde_json::Value>) {
        let Some(value) = value else {
            return;
        };
        if let Some(text) = value.as_str() {
            self.extend_tokens(text.split(','));
            return;
        }
        if let Some(arr) = value.as_array() {
            for item in arr {
                if let Some(text) = item.as_str() {
                    self.extend_tokens(std::iter::once(text));
                } else if let Some(n) = item.as_u64() {
                    self.ids.insert(n.to_string());
                }
            }
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.wildcard && self.ids.is_empty()
    }

    pub fn is_restrictive(&self) -> bool {
        !self.wildcard && !self.ids.is_empty()
    }

    pub fn contains(&self, channel_id: &str) -> bool {
        self.wildcard || self.ids.contains(channel_id)
    }
}

/// Configuration for the Discord adapter.
#[derive(Debug, Clone)]
pub struct DiscordConfig {
    /// Discord bot token.
    pub token: String,

    /// Application ID for interactions.
    pub application_id: Option<String>,

    /// Proxy configuration for outbound requests.
    pub proxy: AdapterProxyConfig,

    /// Whether the bot must be @mentioned in group channels.
    pub require_mention: bool,

    /// Gateway intents bitmask.
    pub intents: u64,

    /// Channels where @mention is not required (guild only).
    pub free_response_channels: ChannelIdSet,

    /// When non-empty, bot only responds in these guild channels (DMs unaffected).
    pub allowed_channels: ChannelIdSet,

    /// Highest priority: bot never responds in these channels.
    pub ignored_channels: ChannelIdSet,

    /// Add 👀/✅/❌ reactions during processing (P1).
    pub reactions_enabled: bool,

    /// Register MVP slash commands on adapter start (requires `application_id`).
    pub slash_commands_enabled: bool,

    /// When set, register slash commands at guild scope for faster iteration.
    pub slash_guild_id: Option<String>,

    /// Max bytes per inbound attachment (0 = use default 25 MiB).
    pub max_attachment_bytes: u64,

    /// Allowed role snowflakes (OR with allowed_users at inbound auth).
    pub allowed_roles: ChannelIdSet,

    /// Allowed user snowflakes for Discord-layer auth (mirrors platform allowlist).
    pub allowed_users: ChannelIdSet,

    /// Guild id for DM role authorization when set.
    pub dm_role_auth_guild: Option<String>,

    /// Auto-create a thread on guild @mentions.
    pub auto_thread: bool,

    /// Channels where auto-thread is disabled.
    pub no_thread_channels: ChannelIdSet,
}

impl DiscordConfig {
    pub fn with_channel_lists(
        mut self,
        free_response: ChannelIdSet,
        allowed: ChannelIdSet,
        ignored: ChannelIdSet,
    ) -> Self {
        self.free_response_channels = free_response;
        self.allowed_channels = allowed;
        self.ignored_channels = ignored;
        self
    }

    /// Build a channel set from env (comma-separated) and/or `platforms.discord.extra` JSON.
    pub fn channel_set_from_sources(
        platform_extra: &std::collections::HashMap<String, serde_json::Value>,
        extra_key: &str,
        env_value: Option<&str>,
        yaml_inline: Option<&str>,
    ) -> ChannelIdSet {
        let mut set = ChannelIdSet::new();
        if let Some(raw) = env_value {
            set.extend_tokens(raw.split(','));
        }
        if let Some(raw) = yaml_inline {
            set.extend_tokens(raw.split(','));
        }
        set.merge_json(platform_extra.get(extra_key));
        set
    }

    /// Build adapter config from gateway `PlatformConfig` + resolved bot token.
    pub fn from_platform(platform_cfg: &PlatformConfig, token: String) -> Self {
        let reactions_enabled = parse_reactions_enabled(platform_cfg);
        Self {
            token,
            application_id: extra_string(platform_cfg, "application_id"),
            proxy: AdapterProxyConfig::default(),
            require_mention: platform_cfg.require_mention.unwrap_or(true),
            intents: platform_cfg
                .extra
                .get("intents")
                .and_then(|v| v.as_u64())
                .unwrap_or_else(default_intents),
            free_response_channels: Self::channel_set_from_sources(
                &platform_cfg.extra,
                "free_response_channels",
                env_channel_list("DISCORD_FREE_RESPONSE_CHANNELS").as_deref(),
                extra_string(platform_cfg, "free_response_channels").as_deref(),
            ),
            allowed_channels: Self::channel_set_from_sources(
                &platform_cfg.extra,
                "allowed_channels",
                env_channel_list("DISCORD_ALLOWED_CHANNELS").as_deref(),
                extra_string(platform_cfg, "allowed_channels").as_deref(),
            ),
            ignored_channels: Self::channel_set_from_sources(
                &platform_cfg.extra,
                "ignored_channels",
                env_channel_list("DISCORD_IGNORED_CHANNELS").as_deref(),
                extra_string(platform_cfg, "ignored_channels").as_deref(),
            ),
            reactions_enabled,
            slash_commands_enabled: parse_slash_commands_enabled(platform_cfg),
            slash_guild_id: extra_string(platform_cfg, "guild_id")
                .or_else(|| env_guild_id()),
            max_attachment_bytes: parse_max_attachment_bytes(platform_cfg),
            allowed_roles: Self::channel_set_from_sources(
                &platform_cfg.extra,
                "allowed_roles",
                env_channel_list("DISCORD_ALLOWED_ROLES").as_deref(),
                extra_string(platform_cfg, "allowed_roles").as_deref(),
            ),
            allowed_users: parse_allowed_users(platform_cfg),
            dm_role_auth_guild: extra_string(platform_cfg, "dm_role_auth_guild")
                .or_else(|| env_dm_role_auth_guild()),
            auto_thread: parse_auto_thread(platform_cfg),
            no_thread_channels: Self::channel_set_from_sources(
                &platform_cfg.extra,
                "no_thread_channels",
                env_channel_list("DISCORD_NO_THREAD_CHANNELS").as_deref(),
                extra_string(platform_cfg, "no_thread_channels").as_deref(),
            ),
        }
    }

    #[cfg(test)]
    pub fn for_test(token: &str) -> Self {
        Self {
            token: token.into(),
            application_id: None,
            proxy: AdapterProxyConfig::default(),
            require_mention: default_require_mention(),
            intents: default_intents(),
            free_response_channels: ChannelIdSet::new(),
            allowed_channels: ChannelIdSet::new(),
            ignored_channels: ChannelIdSet::new(),
            reactions_enabled: true,
            slash_commands_enabled: false,
            slash_guild_id: None,
            max_attachment_bytes: super::media::DEFAULT_MAX_ATTACHMENT_BYTES,
            allowed_roles: ChannelIdSet::new(),
            allowed_users: ChannelIdSet::new(),
            dm_role_auth_guild: None,
            auto_thread: false,
            no_thread_channels: ChannelIdSet::new(),
        }
    }
}

fn parse_max_attachment_bytes(platform_cfg: &PlatformConfig) -> u64 {
    platform_cfg
        .extra
        .get("max_attachment_bytes")
        .and_then(|v| v.as_u64())
        .or_else(|| {
            std::env::var("DISCORD_MAX_ATTACHMENT_BYTES")
                .ok()
                .and_then(|v| v.parse().ok())
        })
        .unwrap_or(super::media::DEFAULT_MAX_ATTACHMENT_BYTES)
}

fn parse_allowed_users(platform_cfg: &PlatformConfig) -> ChannelIdSet {
    let mut set = ChannelIdSet::new();
    for user in &platform_cfg.allowed_users {
        set.extend_tokens(std::iter::once(user.as_str()));
    }
    for user in &platform_cfg.admin_users {
        set.extend_tokens(std::iter::once(user.as_str()));
    }
    set.merge_json(platform_cfg.extra.get("allowed_users"));
    if let Ok(env) = std::env::var("DISCORD_ALLOWED_USERS") {
        set.extend_tokens(env.split(','));
    }
    set
}

fn parse_auto_thread(platform_cfg: &PlatformConfig) -> bool {
    if let Some(v) = platform_cfg.extra.get("auto_thread") {
        if let Some(b) = v.as_bool() {
            return b;
        }
        if let Some(s) = v.as_str() {
            return !matches!(s.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off");
        }
    }
    std::env::var("DISCORD_AUTO_THREAD")
        .ok()
        .map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(false)
}

fn env_dm_role_auth_guild() -> Option<String> {
    extra_string_from_env("DISCORD_DM_ROLE_AUTH_GUILD")
}

fn extra_string_from_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn parse_slash_commands_enabled(platform_cfg: &PlatformConfig) -> bool {
    if let Some(v) = platform_cfg.extra.get("slash_commands") {
        if let Some(b) = v.as_bool() {
            return b;
        }
        if let Some(s) = v.as_str() {
            return !matches!(s.trim().to_ascii_lowercase().as_str(), "0" | "false" | "no" | "off");
        }
    }
    std::env::var("DISCORD_SLASH_COMMANDS")
        .ok()
        .map(|v| {
            !matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "no" | "off"
            )
        })
        .unwrap_or(true)
}

fn env_guild_id() -> Option<String> {
    std::env::var("DISCORD_GUILD_ID")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn default_require_mention() -> bool {
    true
}

fn extra_string(platform_cfg: &PlatformConfig, key: &str) -> Option<String> {
    platform_cfg
        .extra
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn env_channel_list(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn parse_reactions_enabled(platform_cfg: &PlatformConfig) -> bool {
    if let Some(v) = platform_cfg.extra.get("reactions").and_then(|v| v.as_bool()) {
        return v;
    }
    if let Some(text) = extra_string(platform_cfg, "reactions") {
        let normalized = text.to_ascii_lowercase();
        if matches!(normalized.as_str(), "0" | "false" | "no" | "off") {
            return false;
        }
        if matches!(normalized.as_str(), "1" | "true" | "yes" | "on") {
            return true;
        }
    }
    if let Ok(text) = std::env::var("DISCORD_REACTIONS") {
        let normalized = text.trim().to_ascii_lowercase();
        if matches!(normalized.as_str(), "0" | "false" | "no" | "off") {
            return false;
        }
        if matches!(normalized.as_str(), "1" | "true" | "yes" | "on") {
            return true;
        }
    }
    true
}

/// GUILDS | GUILD_MEMBERS | MESSAGE_CONTENT | DIRECT_MESSAGES (no deprecated GUILD_MESSAGES).
pub fn default_intents() -> u64 {
    (1 << 0) | (1 << 1) | (1 << 15) | (1 << 12)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_set_wildcard_matches_all() {
        let set = ChannelIdSet::parse(Some("*"));
        assert!(set.contains("123"));
        assert!(!set.is_restrictive());
    }

    #[test]
    fn channel_set_exact_match() {
        let set = ChannelIdSet::parse(Some("111,222"));
        assert!(set.contains("111"));
        assert!(!set.contains("333"));
        assert!(set.is_restrictive());
    }

    #[test]
    fn channel_set_empty_allows_none_via_contains() {
        let set = ChannelIdSet::new();
        assert!(!set.contains("111"));
        assert!(!set.is_restrictive());
    }

    #[test]
    fn default_intents_includes_dm_and_message_content() {
        let i = default_intents();
        assert_ne!(i & (1 << 1), 0, "GUILD_MEMBERS");
        assert_ne!(i & (1 << 12), 0, "DIRECT_MESSAGES");
        assert_ne!(i & (1 << 15), 0, "MESSAGE_CONTENT");
    }
}
