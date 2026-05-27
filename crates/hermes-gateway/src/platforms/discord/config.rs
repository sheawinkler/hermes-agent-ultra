//! Discord adapter configuration.

use serde::{Deserialize, Serialize};

use crate::adapter::AdapterProxyConfig;

/// Maximum message length for Discord (2000 characters).
pub const MAX_MESSAGE_LENGTH: usize = 2000;

/// Discord API base URL.
pub const DISCORD_API_BASE: &str = "https://discord.com/api/v10";

/// Configuration for the Discord adapter.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordConfig {
    /// Discord bot token.
    pub token: String,

    /// Application ID for interactions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub application_id: Option<String>,

    /// Proxy configuration for outbound requests.
    #[serde(default)]
    pub proxy: AdapterProxyConfig,

    /// Whether the bot must be @mentioned in group channels.
    #[serde(default = "default_require_mention")]
    pub require_mention: bool,

    /// Gateway intents bitmask.
    #[serde(default = "default_intents")]
    pub intents: u64,
}

fn default_require_mention() -> bool {
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
    fn default_intents_includes_dm_and_message_content() {
        let i = default_intents();
        assert_ne!(i & (1 << 1), 0, "GUILD_MEMBERS");
        assert_ne!(i & (1 << 12), 0, "DIRECT_MESSAGES");
        assert_ne!(i & (1 << 15), 0, "MESSAGE_CONTENT");
    }

    #[test]
    fn default_require_mention_is_true() {
        let cfg = DiscordConfig {
            token: "t".into(),
            application_id: None,
            proxy: AdapterProxyConfig::default(),
            require_mention: default_require_mention(),
            intents: default_intents(),
        };
        assert!(cfg.require_mention);
    }
}
