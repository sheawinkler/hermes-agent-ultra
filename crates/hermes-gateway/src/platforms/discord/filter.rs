//! P0 inbound message filtering (pure functions).

use super::parse::RawDiscordMessage;

/// Discord default message type.
pub const MESSAGE_TYPE_DEFAULT: u8 = 0;
/// Reply message type.
pub const MESSAGE_TYPE_REPLY: u8 = 19;

/// Inbound filter configuration.
#[derive(Debug, Clone)]
pub struct DiscordInboundConfig {
    pub require_mention: bool,
    pub bot_user_id: Option<String>,
}

/// Whether a parsed MESSAGE_CREATE should be forwarded to the gateway.
pub fn should_accept_message(raw: &RawDiscordMessage, cfg: &DiscordInboundConfig) -> bool {
    if raw.content.trim().is_empty() {
        return false;
    }

    if !is_allowed_message_type(raw.message_type) {
        return false;
    }

    if let Some(bot_id) = cfg.bot_user_id.as_deref() {
        if raw.user_id.as_deref() == Some(bot_id) {
            return false;
        }
    }

    if raw.is_bot {
        return false;
    }

    if raw.guild_id.is_some() {
        if cfg.require_mention {
            let bot_id = match cfg.bot_user_id.as_deref() {
                Some(id) => id,
                None => return false,
            };
            let mentioned = raw.mentions.iter().any(|m| m == bot_id)
                || content_mentions_bot(&raw.content, bot_id);
            if !mentioned {
                return false;
            }
        }
    }

    true
}

fn is_allowed_message_type(t: u8) -> bool {
    t == MESSAGE_TYPE_DEFAULT || t == MESSAGE_TYPE_REPLY
}

fn content_mentions_bot(content: &str, bot_id: &str) -> bool {
    content.contains(&format!("<@{bot_id}>")) || content.contains(&format!("<@!{bot_id}>"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn human_guild(content: &str, mentions: Vec<&str>) -> RawDiscordMessage {
        RawDiscordMessage {
            channel_id: "ch1".into(),
            message_id: "m1".into(),
            user_id: Some("user1".into()),
            username: None,
            content: content.into(),
            is_bot: false,
            guild_id: Some("guild1".into()),
            mentions: mentions.into_iter().map(String::from).collect(),
            message_type: MESSAGE_TYPE_DEFAULT,
        }
    }

    fn cfg(mention: bool, bot: Option<&str>) -> DiscordInboundConfig {
        DiscordInboundConfig {
            require_mention: mention,
            bot_user_id: bot.map(String::from),
        }
    }

    #[test]
    fn f01_reject_self_bot_author() {
        let raw = RawDiscordMessage {
            user_id: Some("bot99".into()),
            is_bot: true,
            guild_id: None,
            ..human_guild("hi", vec![])
        };
        assert!(!should_accept_message(
            &raw,
            &cfg(false, Some("bot99"))
        ));
    }

    #[test]
    fn f02_reject_other_bot() {
        let raw = RawDiscordMessage {
            is_bot: true,
            user_id: Some("other-bot".into()),
            ..human_guild("hi", vec![])
        };
        assert!(!should_accept_message(&raw, &cfg(false, Some("bot99"))));
    }

    #[test]
    fn f03_accept_human_dm() {
        let raw = RawDiscordMessage {
            guild_id: None,
            ..human_guild("hello", vec![])
        };
        assert!(should_accept_message(&raw, &cfg(true, Some("bot99"))));
        assert!(should_accept_message(&raw, &cfg(false, Some("bot99"))));
    }

    #[test]
    fn f04_reject_guild_without_mention() {
        let raw = human_guild("hello", vec![]);
        assert!(!should_accept_message(&raw, &cfg(true, Some("bot99"))));
    }

    #[test]
    fn f05_accept_guild_with_bot_mention() {
        let raw = human_guild("hello", vec!["bot99"]);
        assert!(should_accept_message(&raw, &cfg(true, Some("bot99"))));
    }

    #[test]
    fn f06_accept_guild_when_mention_not_required() {
        let raw = human_guild("hello", vec![]);
        assert!(should_accept_message(&raw, &cfg(false, Some("bot99"))));
    }

    #[test]
    fn f07_reject_empty_content() {
        let raw = human_guild("   ", vec![]);
        assert!(!should_accept_message(&raw, &cfg(false, Some("bot99"))));
    }

    #[test]
    fn f08_reject_system_message_type() {
        let raw = RawDiscordMessage {
            message_type: 6,
            ..human_guild("pin", vec![])
        };
        assert!(!should_accept_message(&raw, &cfg(false, Some("bot99"))));
    }

    #[test]
    fn f09_reject_guild_mention_other_not_bot() {
        let raw = human_guild("hi", vec!["other-user"]);
        assert!(!should_accept_message(&raw, &cfg(true, Some("bot99"))));
    }

    #[test]
    fn f10_accept_guild_mention_in_content_without_mentions_array() {
        let raw = human_guild("hey <@!bot99> ping", vec![]);
        assert!(should_accept_message(&raw, &cfg(true, Some("bot99"))));
    }
}
