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
    #[serde(default)]
    pub require_mention: bool,

    /// Gateway intents bitmask (default: GUILDS | GUILD_MESSAGES | MESSAGE_CONTENT).
    #[serde(default = "default_intents")]
    pub intents: u64,

    /// How outgoing chunks should reply-reference the original Discord message.
    #[serde(default = "default_reply_to_mode")]
    pub reply_to_mode: String,

    /// Channel-level inbound and auto-thread policy.
    #[serde(default)]
    pub channel_controls: DiscordChannelControls,

    /// Channel-bound skills injected for Discord sessions.
    #[serde(default)]
    pub channel_skill_bindings: Vec<DiscordChannelSkillBinding>,
}

fn default_intents() -> u64 {
    // GUILDS (1<<0) | GUILD_MESSAGES (1<<9) | MESSAGE_CONTENT (1<<15)
    (1 << 0) | (1 << 9) | (1 << 15)
}

fn default_reply_to_mode() -> String {
    "first".to_string()
}

/// Optional Discord send metadata carried by higher-level gateway helpers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordSendMetadata {
    /// Discord thread channel ID to target instead of the parent channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    /// Original Discord message ID to reply-reference.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to_message_id: Option<String>,
    /// Marks lifecycle/status sends that must not act as channel-history boundaries.
    #[serde(default, skip_serializing_if = "bool_is_false")]
    pub non_conversational: bool,
}

fn bool_is_false(value: &bool) -> bool {
    !*value
}

impl DiscordSendMetadata {
    pub fn with_thread_id(thread_id: impl Into<String>) -> Self {
        Self {
            thread_id: Some(thread_id.into()),
            reply_to_message_id: None,
            non_conversational: false,
        }
    }

    pub fn with_reply_to_message_id(message_id: impl Into<String>) -> Self {
        Self {
            thread_id: None,
            reply_to_message_id: Some(message_id.into()),
            non_conversational: false,
        }
    }

    pub fn with_thread_and_reply(
        thread_id: impl Into<String>,
        message_id: impl Into<String>,
    ) -> Self {
        Self {
            thread_id: Some(thread_id.into()),
            reply_to_message_id: Some(message_id.into()),
            non_conversational: false,
        }
    }

    pub fn non_conversational() -> Self {
        Self {
            thread_id: None,
            reply_to_message_id: None,
            non_conversational: true,
        }
    }

    pub fn with_non_conversational(mut self, non_conversational: bool) -> Self {
        self.non_conversational = non_conversational;
        self
    }

    pub fn target_channel_id<'a>(&'a self, fallback_channel_id: &'a str) -> &'a str {
        self.thread_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .unwrap_or(fallback_channel_id)
    }

    pub fn reply_to_message_id(&self) -> Option<&str> {
        self.reply_to_message_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
    }

    pub fn marks_non_conversational(&self) -> bool {
        self.non_conversational
    }
}

fn target_channel_id_for_metadata<'a>(
    channel_id: &'a str,
    metadata: Option<&'a DiscordSendMetadata>,
) -> &'a str {
    metadata
        .map(|m| m.target_channel_id(channel_id))
        .unwrap_or(channel_id)
}

fn reply_to_message_id_for_metadata(metadata: Option<&DiscordSendMetadata>) -> Option<&str> {
    metadata.and_then(DiscordSendMetadata::reply_to_message_id)
}

fn metadata_marks_non_conversational(metadata: Option<&DiscordSendMetadata>) -> bool {
    metadata
        .map(DiscordSendMetadata::marks_non_conversational)
        .unwrap_or(false)
}

fn discord_metadata_from_send_options(options: &SendMessageOptions) -> Option<DiscordSendMetadata> {
    if options.thread_id.is_none() && !options.non_conversational {
        return None;
    }
    Some(DiscordSendMetadata {
        thread_id: options.thread_id.clone(),
        reply_to_message_id: None,
        non_conversational: options.non_conversational,
    })
}

/// Effective behavior for Discord reply references across split chunks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordReplyToMode {
    Off,
    First,
    All,
}

impl DiscordReplyToMode {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            Some(value) if value.eq_ignore_ascii_case("off") => Self::Off,
            Some(value) if value.eq_ignore_ascii_case("all") => Self::All,
            _ => Self::First,
        }
    }

    pub fn references_chunk(self, chunk_index: usize) -> bool {
        match self {
            Self::Off => false,
            Self::First => chunk_index == 0,
            Self::All => true,
        }
    }
}

const DISCORD_ALLOW_MENTION_EVERYONE_ENV: &str = "DISCORD_ALLOW_MENTION_EVERYONE";
const DISCORD_ALLOW_MENTION_ROLES_ENV: &str = "DISCORD_ALLOW_MENTION_ROLES";
const DISCORD_ALLOW_MENTION_USERS_ENV: &str = "DISCORD_ALLOW_MENTION_USERS";
const DISCORD_ALLOW_MENTION_REPLIED_USER_ENV: &str = "DISCORD_ALLOW_MENTION_REPLIED_USER";
const DISCORD_ALLOW_BOTS_ENV: &str = "DISCORD_ALLOW_BOTS";

/// Discord REST `allowed_mentions` payload.
///
/// Safe defaults block broad server pings while preserving direct user and
/// reply-reference pings, matching the upstream gateway adapter contract.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordAllowedMentions {
    pub parse: Vec<String>,
    pub replied_user: bool,
}

impl DiscordAllowedMentions {
    pub fn from_flags(everyone: bool, roles: bool, users: bool, replied_user: bool) -> Self {
        let mut parse = Vec::new();
        if everyone {
            parse.push("everyone".to_string());
        }
        if roles {
            parse.push("roles".to_string());
        }
        if users {
            parse.push("users".to_string());
        }

        Self {
            parse,
            replied_user,
        }
    }
}

fn parse_allowed_mention_bool(raw: &str, default: bool) -> bool {
    match raw.trim().to_ascii_lowercase().as_str() {
        "true" | "1" | "yes" | "on" => true,
        "false" | "0" | "no" | "off" => false,
        "" => default,
        _ => default,
    }
}

fn discord_allowed_mentions_from_lookup<F>(mut lookup: F) -> DiscordAllowedMentions
where
    F: FnMut(&str) -> Option<String>,
{
    let allow_everyone = lookup(DISCORD_ALLOW_MENTION_EVERYONE_ENV)
        .map(|raw| parse_allowed_mention_bool(&raw, false))
        .unwrap_or(false);
    let allow_roles = lookup(DISCORD_ALLOW_MENTION_ROLES_ENV)
        .map(|raw| parse_allowed_mention_bool(&raw, false))
        .unwrap_or(false);
    let allow_users = lookup(DISCORD_ALLOW_MENTION_USERS_ENV)
        .map(|raw| parse_allowed_mention_bool(&raw, true))
        .unwrap_or(true);
    let allow_replied_user = lookup(DISCORD_ALLOW_MENTION_REPLIED_USER_ENV)
        .map(|raw| parse_allowed_mention_bool(&raw, true))
        .unwrap_or(true);

    DiscordAllowedMentions::from_flags(allow_everyone, allow_roles, allow_users, allow_replied_user)
}

fn default_discord_allowed_mentions() -> DiscordAllowedMentions {
    discord_allowed_mentions_from_lookup(|name| std::env::var(name).ok())
}

fn with_allowed_mentions(
    mut body: serde_json::Value,
    allowed_mentions: DiscordAllowedMentions,
) -> serde_json::Value {
    body["allowed_mentions"] =
        serde_json::to_value(allowed_mentions).expect("DiscordAllowedMentions serializes");
    body
}

fn with_default_allowed_mentions(body: serde_json::Value) -> serde_json::Value {
    with_allowed_mentions(body, default_discord_allowed_mentions())
}

fn with_reply_reference(mut body: serde_json::Value, message_id: &str) -> serde_json::Value {
    let message_id = message_id.trim();
    if !message_id.is_empty() {
        body["message_reference"] = serde_json::json!({
            "message_id": message_id,
            "fail_if_not_exists": false,
        });
    }
    body
}

fn discord_message_body(
    content: &str,
    reply_to_message_id: Option<&str>,
    allowed_mentions: DiscordAllowedMentions,
) -> serde_json::Value {
    let body = with_allowed_mentions(serde_json::json!({ "content": content }), allowed_mentions);
    match reply_to_message_id {
        Some(message_id) => with_reply_reference(body, message_id),
        None => body,
    }
}

fn discord_reply_reference_error_allows_retry(raw_error: &str) -> bool {
    let normalized = raw_error.to_ascii_lowercase();
    normalized.contains("cannot reply to a system message")
        || normalized.contains("unknown message")
        || normalized.contains("error code: 10008")
}

fn forum_thread_name(content: Option<&str>, file_name: Option<&str>) -> String {
    let candidate = content
        .and_then(|content| content.lines().map(str::trim).find(|line| !line.is_empty()))
        .or_else(|| file_name.map(str::trim).filter(|name| !name.is_empty()))
        .unwrap_or("Hermes");

    candidate.chars().take(100).collect()
}

fn forum_thread_message_body(content: &str) -> serde_json::Value {
    with_default_allowed_mentions(serde_json::json!({ "content": content }))
}

fn forum_thread_payload(
    content: &str,
    file_name: Option<&str>,
    auto_archive_duration: Option<u32>,
) -> serde_json::Value {
    let mut body = serde_json::json!({
        "name": forum_thread_name(Some(content), file_name),
        "message": forum_thread_message_body(content),
    });
    if let Some(duration) = auto_archive_duration {
        body["auto_archive_duration"] = serde_json::Value::Number(duration.into());
    }
    body
}

pub fn discord_channel_type_is_forum_parent(channel_type: Option<u8>) -> bool {
    matches!(channel_type, Some(15))
}

