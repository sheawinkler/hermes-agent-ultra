//! Discord Bot API adapter.
//!
//! Implements the `PlatformAdapter` trait for Discord using the REST API
//! for message operations and the Gateway WebSocket for receiving events.
//! Supports message splitting at 2000 characters, file uploads via
//! multipart form data, embeds, threads, reactions, slash commands, and
//! Gateway event handling (IDENTIFY, HEARTBEAT, RESUME, READY,
//! MESSAGE_CREATE, MESSAGE_UPDATE, INTERACTION_CREATE, VOICE_STATE_UPDATE,
//! MESSAGE_REACTION_ADD, MESSAGE_REACTION_REMOVE).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter, SendMessageOptions};

use crate::adapter::{describe_secret, AdapterProxyConfig, BasePlatformAdapter};
use crate::pairing::{PairingManager, PairingState};

/// Maximum message length for Discord (2000 characters).
const MAX_MESSAGE_LENGTH: usize = 2000;

/// Discord API base URL.
const DISCORD_API_BASE: &str = "https://discord.com/api/v10";
const DISCORD_APPLICATION_COMMAND_LIMIT: usize = 100;
const DISCORD_NONCONVERSATIONAL_STATE_FILENAME: &str = "discord_nonconversational_messages.json";

// ---------------------------------------------------------------------------
// DiscordConfig
// ---------------------------------------------------------------------------

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

/// Discord bot-message acceptance policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordBotMessagePolicy {
    /// Reject other bot/webhook senders.
    None,
    /// Accept bot/webhook senders only when they mention this bot.
    Mentions,
    /// Accept all bot/webhook senders.
    All,
}

impl DiscordBotMessagePolicy {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            Some(value) if value.eq_ignore_ascii_case("all") => Self::All,
            Some(value) if value.eq_ignore_ascii_case("mentions") => Self::Mentions,
            _ => Self::None,
        }
    }

    pub fn from_lookup<F>(mut lookup: F) -> Self
    where
        F: FnMut(&str) -> Option<String>,
    {
        Self::parse(lookup(DISCORD_ALLOW_BOTS_ENV).as_deref())
    }

    pub fn bypasses_gateway_allowlist(self) -> bool {
        matches!(self, Self::Mentions | Self::All)
    }
}

fn discord_message_type_is_user_visible(message_type: u8) -> bool {
    matches!(message_type, 0 | 19)
}

pub fn discord_flatten_clarify_choice(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::Null => None,
        serde_json::Value::String(raw) => {
            let trimmed = raw.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        serde_json::Value::Object(map) => ["label", "description", "text", "title"]
            .into_iter()
            .filter_map(|key| map.get(key).and_then(serde_json::Value::as_str))
            .map(str::trim)
            .find(|value| !value.is_empty())
            .map(ToOwned::to_owned),
        serde_json::Value::Array(values) => {
            let joined = values
                .iter()
                .filter_map(discord_flatten_clarify_choice)
                .collect::<Vec<_>>()
                .join(" ");
            (!joined.is_empty()).then_some(joined)
        }
        other => {
            let rendered = other.to_string();
            let trimmed = rendered.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
    }
}

pub fn discord_normalize_clarify_choices(
    values: impl IntoIterator<Item = serde_json::Value>,
) -> Vec<String> {
    values
        .into_iter()
        .filter_map(|value| discord_flatten_clarify_choice(&value))
        .collect()
}

pub fn discord_clarify_button_label(index: usize, choice: &str) -> String {
    let prefix = format!("{}. ", index + 1);
    let budget = 80usize.saturating_sub(prefix.chars().count()).max(1);
    let choice_len = choice.chars().count();
    let label_body = if choice_len <= budget {
        choice.to_string()
    } else {
        let mut chars = choice
            .chars()
            .take(budget.saturating_sub(1))
            .collect::<String>();
        while chars.chars().last().is_some_and(char::is_whitespace) {
            chars.pop();
        }

        let cut_at = {
            let char_vec = chars.chars().collect::<Vec<_>>();
            let trailing_half = budget / 2;
            let space_cut = char_vec
                .iter()
                .rposition(|ch| *ch == ' ')
                .filter(|pos| *pos >= trailing_half);
            space_cut.or_else(|| {
                char_vec
                    .iter()
                    .rposition(|ch| matches!(*ch, '-' | ',' | '.' | ')'))
                    .filter(|pos| *pos >= trailing_half)
                    .map(|pos| pos + 1)
            })
        };

        if let Some(cut_at) = cut_at.filter(|pos| *pos > 0) {
            chars = chars.chars().take(cut_at).collect();
        }
        while chars.chars().last().is_some_and(char::is_whitespace) {
            chars.pop();
        }
        format!("{chars}…")
    };
    format!("{prefix}{label_body}")
}

fn discord_non_conversational_patterns() -> &'static [Regex] {
    static PATTERNS: OnceLock<Vec<Regex>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        [
            r"(?i)^\s*💾\s*Self-improvement review:\s+\S[\s\S]*$",
            r#"(?i)^\s*💾\s+Skill\s+['"].+?['"]\s+(?:created|updated|improved|patched)\.?\s*$"#,
            r"(?i)^\s*⏳\s+Working\s+—\s+\d+\s+min(?:\s|$)",
            r"(?i)^\s*\[Background process\s+\S+\s+(?:finished with exit code|is still running~)[\s\S]*\]\s*$",
            r"(?i)^\s*(?:✅|❌)\s+Hermes update\s+(?:finished|failed|timed out)[\s\S]*$",
            r"(?i)^\s*♻️?\s+Gateway\s+(?:restarted successfully|online\b)[\s\S]*$",
        ]
        .into_iter()
        .map(|pattern| Regex::new(pattern).expect("valid Discord non-conversational pattern"))
        .collect()
    })
}

pub fn discord_looks_like_non_conversational_history_message(content: &str) -> bool {
    discord_non_conversational_patterns()
        .iter()
        .any(|pattern| pattern.is_match(content))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordHistoryMessage {
    pub id: String,
    pub author_name: Option<String>,
    pub author_is_bot: bool,
    pub author_is_self: bool,
    pub message_type: u8,
    pub content: String,
    pub has_attachments: bool,
}

impl DiscordHistoryMessage {
    pub fn new(
        id: impl Into<String>,
        author_name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            author_name: Some(author_name.into()),
            author_is_bot: false,
            author_is_self: false,
            message_type: 0,
            content: content.into(),
            has_attachments: false,
        }
    }

    pub fn self_message(id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            author_name: Some("Hermes".into()),
            author_is_bot: true,
            author_is_self: true,
            message_type: 0,
            content: content.into(),
            has_attachments: false,
        }
    }

    pub fn bot_message(
        id: impl Into<String>,
        author_name: impl Into<String>,
        content: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            author_name: Some(author_name.into()),
            author_is_bot: true,
            author_is_self: false,
            message_type: 0,
            content: content.into(),
            has_attachments: false,
        }
    }
}

fn discord_history_message_is_non_conversational(
    message: &DiscordHistoryMessage,
    non_conversational_ids: &BTreeSet<String>,
) -> bool {
    let id = message.id.trim();
    (!id.is_empty() && non_conversational_ids.contains(id))
        || discord_looks_like_non_conversational_history_message(&message.content)
}

fn discord_history_line(
    message: &DiscordHistoryMessage,
    include_other_bots: bool,
    non_conversational_ids: &BTreeSet<String>,
) -> Option<String> {
    if !discord_message_type_is_user_visible(message.message_type)
        || discord_history_message_is_non_conversational(message, non_conversational_ids)
    {
        return None;
    }
    if message.author_is_bot && !message.author_is_self && !include_other_bots {
        return None;
    }

    let content = match message.content.trim() {
        "" if message.has_attachments => "(attachment)".to_string(),
        "" => return None,
        text => text.to_string(),
    };
    let mut name = message
        .author_name
        .as_deref()
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("unknown")
        .to_string();
    if message.author_is_bot {
        name.push_str(" [bot]");
    }
    Some(format!("[{name}] {content}"))
}

pub fn discord_format_channel_context(
    primary_newest_first: &[DiscordHistoryMessage],
    reply_newest_first: &[DiscordHistoryMessage],
    include_other_bots: bool,
    reply_target_id: Option<&str>,
    non_conversational_ids: &BTreeSet<String>,
) -> String {
    let mut collected = Vec::<(String, String)>::new();
    let mut seen_ids = BTreeSet::<String>::new();

    for message in primary_newest_first {
        if discord_history_message_is_non_conversational(message, non_conversational_ids) {
            continue;
        }
        if message.author_is_self {
            break;
        }
        let Some(line) = discord_history_line(message, include_other_bots, non_conversational_ids)
        else {
            continue;
        };
        let id = message.id.trim().to_string();
        if !id.is_empty() {
            seen_ids.insert(id.clone());
        }
        collected.push((id, line));
    }

    let reply_target_id = reply_target_id.map(str::trim).filter(|id| !id.is_empty());
    let mut reply_collected = Vec::<(String, String)>::new();
    if reply_target_id.is_some_and(|target_id| !seen_ids.contains(target_id)) {
        for message in reply_newest_first {
            let id = message.id.trim().to_string();
            if !id.is_empty() && seen_ids.contains(&id) {
                continue;
            }
            let Some(line) =
                discord_history_line(message, include_other_bots, non_conversational_ids)
            else {
                continue;
            };
            if !id.is_empty() {
                seen_ids.insert(id.clone());
            }
            reply_collected.push((id, line));
        }
    }

    let mut blocks = Vec::new();
    if !reply_collected.is_empty() {
        reply_collected.reverse();
        blocks.push(format!(
            "[Context around the replied-to message]\n{}",
            reply_collected
                .into_iter()
                .map(|(_, line)| line)
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if !collected.is_empty() {
        collected.reverse();
        blocks.push(format!(
            "[Recent channel messages]\n{}",
            collected
                .into_iter()
                .map(|(_, line)| line)
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    blocks.join("\n\n")
}

pub fn discord_should_fetch_channel_context(
    require_mention: bool,
    is_free_channel: bool,
    in_bot_thread: bool,
    context: &DiscordChannelContext,
    auto_threaded_channel: bool,
) -> bool {
    if context.is_dm || auto_threaded_channel {
        return false;
    }
    let has_mention_gap = require_mention && !is_free_channel && !in_bot_thread;
    has_mention_gap || context.is_thread || context.is_reply
}

/// Parse Discord reaction lifecycle opt-in values. Default is enabled.
pub fn discord_reactions_enabled_from_raw(raw: Option<&str>) -> bool {
    match raw.map(str::trim).filter(|s| !s.is_empty()) {
        Some(value) => parse_allowed_mention_bool(value, true),
        None => true,
    }
}

// ---------------------------------------------------------------------------
// Discord channel policy
// ---------------------------------------------------------------------------

fn scalar_json_to_discord_id(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(s) => {
            let trimmed = s.trim();
            (!trimmed.is_empty()).then(|| trimmed.to_string())
        }
        serde_json::Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn discord_id_set_from_csv(raw: &str) -> BTreeSet<String> {
    raw.split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

fn discord_id_set_from_json(value: Option<&serde_json::Value>) -> BTreeSet<String> {
    let Some(value) = value else {
        return BTreeSet::new();
    };
    match value {
        serde_json::Value::String(raw) => discord_id_set_from_csv(raw),
        serde_json::Value::Array(values) => values
            .iter()
            .filter_map(scalar_json_to_discord_id)
            .collect::<BTreeSet<_>>(),
        other => scalar_json_to_discord_id(other).into_iter().collect(),
    }
}

fn bool_from_json(value: Option<&serde_json::Value>, default: bool) -> bool {
    match value {
        Some(serde_json::Value::Bool(v)) => *v,
        Some(serde_json::Value::Number(n)) => n.as_i64().map(|v| v != 0).unwrap_or(default),
        Some(serde_json::Value::String(raw)) => parse_allowed_mention_bool(raw, default),
        _ => default,
    }
}

fn channel_matches(
    ids: &BTreeSet<String>,
    channel_id: &str,
    parent_channel_id: Option<&str>,
) -> bool {
    if ids.iter().any(|id| id.trim() == "*") {
        return true;
    }
    let channel_id = channel_id.trim();
    let parent_channel_id = parent_channel_id.map(str::trim).filter(|s| !s.is_empty());
    (!channel_id.is_empty() && ids.contains(channel_id))
        || parent_channel_id
            .map(|parent| ids.contains(parent))
            .unwrap_or(false)
}

/// Discord channel-level policy controls.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordChannelControls {
    /// Server channel IDs whose messages are always dropped.
    #[serde(default)]
    pub ignored_channels: BTreeSet<String>,
    /// Server channel IDs where automatic thread creation is suppressed.
    #[serde(default)]
    pub no_thread_channels: BTreeSet<String>,
    /// Server channel IDs where mention-free responses are allowed.
    #[serde(default)]
    pub free_response_channels: BTreeSet<String>,
    /// Global auto-thread toggle. Defaults to true to match upstream behavior.
    #[serde(default = "default_true_channel_control")]
    pub auto_thread: bool,
    /// Require explicit mentions even in participated/free-response threads.
    #[serde(default)]
    pub thread_require_mention: bool,
}

fn default_true_channel_control() -> bool {
    true
}

impl Default for DiscordChannelControls {
    fn default() -> Self {
        Self {
            ignored_channels: BTreeSet::new(),
            no_thread_channels: BTreeSet::new(),
            free_response_channels: BTreeSet::new(),
            auto_thread: true,
            thread_require_mention: false,
        }
    }
}

impl DiscordChannelControls {
    pub fn from_extra(extra: &std::collections::HashMap<String, serde_json::Value>) -> Self {
        Self {
            ignored_channels: discord_id_set_from_json(extra.get("ignored_channels")),
            no_thread_channels: discord_id_set_from_json(extra.get("no_thread_channels")),
            free_response_channels: discord_id_set_from_json(extra.get("free_response_channels")),
            auto_thread: bool_from_json(extra.get("auto_thread"), true),
            thread_require_mention: bool_from_json(extra.get("thread_require_mention"), false),
        }
    }

    pub fn is_ignored(&self, context: &DiscordChannelContext) -> bool {
        if context.is_dm {
            return false;
        }
        channel_matches(
            &self.ignored_channels,
            &context.channel_id,
            context.parent_channel_id.as_deref(),
        )
    }

    pub fn allows_free_response(&self, context: &DiscordChannelContext) -> bool {
        if context.is_dm {
            return true;
        }
        context.voice_linked_text_channel
            || channel_matches(
                &self.free_response_channels,
                &context.channel_id,
                context.parent_channel_id.as_deref(),
            )
    }

    pub fn should_auto_thread(&self, context: &DiscordChannelContext) -> bool {
        if !self.auto_thread
            || context.is_dm
            || context.is_thread
            || context.is_reply
            || context.voice_linked_text_channel
            || self.allows_free_response(context)
        {
            return false;
        }

        !channel_matches(
            &self.no_thread_channels,
            &context.channel_id,
            context.parent_channel_id.as_deref(),
        )
    }
}

/// Discord channel context used by pure Rust policy checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordChannelContext {
    pub channel_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_channel_id: Option<String>,
    #[serde(default)]
    pub is_dm: bool,
    #[serde(default)]
    pub is_thread: bool,
    #[serde(default)]
    pub is_reply: bool,
    #[serde(default)]
    pub voice_linked_text_channel: bool,
}

impl DiscordChannelContext {
    pub fn server(channel_id: impl Into<String>) -> Self {
        Self {
            channel_id: channel_id.into(),
            parent_channel_id: None,
            is_dm: false,
            is_thread: false,
            is_reply: false,
            voice_linked_text_channel: false,
        }
    }

    pub fn thread(channel_id: impl Into<String>, parent_channel_id: impl Into<String>) -> Self {
        Self {
            channel_id: channel_id.into(),
            parent_channel_id: Some(parent_channel_id.into()),
            is_dm: false,
            is_thread: true,
            is_reply: false,
            voice_linked_text_channel: false,
        }
    }

    pub fn dm(channel_id: impl Into<String>) -> Self {
        Self {
            channel_id: channel_id.into(),
            parent_channel_id: None,
            is_dm: true,
            is_thread: false,
            is_reply: false,
            voice_linked_text_channel: false,
        }
    }
}

fn id_matches_any(candidate: &str, allowed: &BTreeSet<String>) -> bool {
    let candidate = candidate.trim();
    if candidate.is_empty() {
        return false;
    }
    let candidate_no_at = candidate.strip_prefix('@').unwrap_or(candidate);
    allowed.iter().any(|entry| {
        let allowed = entry.trim();
        if allowed.is_empty() {
            return false;
        }
        let allowed_no_at = allowed.strip_prefix('@').unwrap_or(allowed);
        allowed.eq_ignore_ascii_case(candidate)
            || allowed.eq_ignore_ascii_case(candidate_no_at)
            || allowed_no_at.eq_ignore_ascii_case(candidate)
            || allowed_no_at.eq_ignore_ascii_case(candidate_no_at)
    })
}

/// Discord user/member data relevant to slash and component authorization.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscordInteractionSubject {
    pub user_id: Option<String>,
    pub role_ids: BTreeSet<String>,
    /// Guild that the resolved role list belongs to.
    ///
    /// Component interactions carry the resolved member role list directly and
    /// do not need this field. Slash/on-message role checks use it to avoid
    /// trusting roles from a different mutual guild.
    pub role_guild_id: Option<String>,
}

impl DiscordInteractionSubject {
    pub fn user(user_id: impl Into<String>) -> Self {
        Self {
            user_id: Some(user_id.into()),
            role_ids: BTreeSet::new(),
            role_guild_id: None,
        }
    }

    pub fn member(
        user_id: impl Into<String>,
        role_ids: impl IntoIterator<Item = impl Into<String>>,
        role_guild_id: impl Into<String>,
    ) -> Self {
        Self {
            user_id: Some(user_id.into()),
            role_ids: role_ids.into_iter().map(Into::into).collect(),
            role_guild_id: Some(role_guild_id.into()),
        }
    }

    fn has_role_match(&self, allowed_role_ids: &BTreeSet<String>) -> bool {
        self.role_ids
            .iter()
            .any(|role_id| id_matches_any(role_id, allowed_role_ids))
    }
}

/// Slash/component authorization policy matching Discord's Python gate shape.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscordInteractionAuthPolicy {
    pub allowed_user_ids: BTreeSet<String>,
    pub allowed_role_ids: BTreeSet<String>,
    pub allowed_channels: BTreeSet<String>,
    pub ignored_channels: BTreeSet<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordAuthDecision {
    Allow,
    Deny(DiscordAuthDenyReason),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordAuthDenyReason {
    AllowedUsersOrRoles,
    AllowedChannels,
    IgnoredChannels,
}

impl DiscordInteractionAuthPolicy {
    pub fn has_identity_policy(&self) -> bool {
        !self.allowed_user_ids.is_empty() || !self.allowed_role_ids.is_empty()
    }

    pub fn component_allows(&self, subject: &DiscordInteractionSubject) -> bool {
        if !self.has_identity_policy() {
            return true;
        }
        subject
            .user_id
            .as_deref()
            .map(|user_id| id_matches_any(user_id, &self.allowed_user_ids))
            .unwrap_or(false)
            || subject.has_role_match(&self.allowed_role_ids)
    }

    fn slash_role_allows(
        &self,
        subject: &DiscordInteractionSubject,
        guild_id: Option<&str>,
        is_dm: bool,
        dm_role_auth_guild: Option<&str>,
    ) -> bool {
        if self.allowed_role_ids.is_empty() || !subject.has_role_match(&self.allowed_role_ids) {
            return false;
        }

        let Some(role_guild_id) = subject
            .role_guild_id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty())
        else {
            return false;
        };

        if is_dm {
            return dm_role_auth_guild
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(|trusted| trusted == role_guild_id)
                .unwrap_or(false);
        }

        guild_id
            .map(str::trim)
            .filter(|id| !id.is_empty())
            .map(|origin| origin == role_guild_id)
            .unwrap_or(false)
    }

    pub fn authorize_slash(
        &self,
        subject: &DiscordInteractionSubject,
        channel_context: Option<&DiscordChannelContext>,
        guild_id: Option<&str>,
        dm_role_auth_guild: Option<&str>,
    ) -> DiscordAuthDecision {
        let is_dm = channel_context
            .map(|ctx| ctx.is_dm)
            .unwrap_or(guild_id.is_none());
        if !is_dm {
            let Some(context) = channel_context else {
                if !self.allowed_channels.is_empty() {
                    return DiscordAuthDecision::Deny(DiscordAuthDenyReason::AllowedChannels);
                }
                if !self.ignored_channels.is_empty() {
                    return DiscordAuthDecision::Deny(DiscordAuthDenyReason::IgnoredChannels);
                }
                return self.authorize_slash_identity(subject, guild_id, is_dm, dm_role_auth_guild);
            };

            if channel_matches(
                &self.ignored_channels,
                &context.channel_id,
                context.parent_channel_id.as_deref(),
            ) {
                return DiscordAuthDecision::Deny(DiscordAuthDenyReason::IgnoredChannels);
            }

            if !self.allowed_channels.is_empty()
                && !channel_matches(
                    &self.allowed_channels,
                    &context.channel_id,
                    context.parent_channel_id.as_deref(),
                )
            {
                return DiscordAuthDecision::Deny(DiscordAuthDenyReason::AllowedChannels);
            }
        }

        if !self.has_identity_policy() {
            return DiscordAuthDecision::Allow;
        }

        self.authorize_slash_identity(subject, guild_id, is_dm, dm_role_auth_guild)
    }

    fn authorize_slash_identity(
        &self,
        subject: &DiscordInteractionSubject,
        guild_id: Option<&str>,
        is_dm: bool,
        dm_role_auth_guild: Option<&str>,
    ) -> DiscordAuthDecision {
        if !self.has_identity_policy() {
            return DiscordAuthDecision::Allow;
        }

        let user_allowed = subject
            .user_id
            .as_deref()
            .map(|user_id| id_matches_any(user_id, &self.allowed_user_ids))
            .unwrap_or(false);
        if user_allowed || self.slash_role_allows(subject, guild_id, is_dm, dm_role_auth_guild) {
            DiscordAuthDecision::Allow
        } else {
            DiscordAuthDecision::Deny(DiscordAuthDenyReason::AllowedUsersOrRoles)
        }
    }
}

/// Component button authorization with pairing-store fallback.
///
/// Allowlist/role policy remains authoritative. When that fails, an explicitly
/// approved pairing entry authorizes the same Discord user id, matching the
/// gateway-level pairing path without relaxing fail-closed behavior for unknown
/// users.
pub fn discord_component_allows_with_pairing(
    policy: &DiscordInteractionAuthPolicy,
    subject: &DiscordInteractionSubject,
    pairing: Option<&PairingManager>,
) -> bool {
    if policy.component_allows(subject) {
        return true;
    }
    let Some(user_id) = subject
        .user_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    else {
        return false;
    };
    pairing
        .and_then(|manager| manager.state(user_id))
        .is_some_and(|state| state == PairingState::Approved)
}

/// Determine whether a Discord message may be routed without an explicit bot mention.
pub fn discord_allows_message_without_mention(
    require_mention: bool,
    controls: &DiscordChannelControls,
    context: &DiscordChannelContext,
    bot_participated_in_thread: bool,
    bot_mentioned: bool,
) -> bool {
    if bot_mentioned || !require_mention || context.is_dm || controls.allows_free_response(context)
    {
        return true;
    }
    context.is_thread && bot_participated_in_thread && !controls.thread_require_mention
}

/// Discord SendResult-style success handling for unauthorized slash notifications.
pub fn discord_notify_result_counts_delivered(success: Option<bool>) -> bool {
    success.unwrap_or(true)
}

/// Catalog entry used by the flat `/skill` Discord command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordSkillCommandEntry {
    pub name: String,
    pub description: String,
    pub command_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscordSkillCommandDecision {
    Unauthorized,
    UnknownSkill { requested_name: String },
    Dispatch { text: String },
}

#[derive(Debug, Clone, Copy)]
pub struct DiscordSkillCommandRequest<'a> {
    pub requested_name: &'a str,
    pub args: &'a str,
}

pub fn discord_skill_autocomplete_choices(
    policy: &DiscordInteractionAuthPolicy,
    subject: &DiscordInteractionSubject,
    channel_context: Option<&DiscordChannelContext>,
    guild_id: Option<&str>,
    dm_role_auth_guild: Option<&str>,
    entries: &[DiscordSkillCommandEntry],
    current: &str,
) -> Vec<String> {
    if policy.authorize_slash(subject, channel_context, guild_id, dm_role_auth_guild)
        != DiscordAuthDecision::Allow
    {
        return Vec::new();
    }

    let needle = current.trim().to_ascii_lowercase();
    entries
        .iter()
        .filter(|entry| {
            needle.is_empty()
                || entry.name.to_ascii_lowercase().contains(&needle)
                || entry.description.to_ascii_lowercase().contains(&needle)
        })
        .take(25)
        .map(|entry| entry.name.clone())
        .collect()
}

pub fn discord_skill_command_decision(
    policy: &DiscordInteractionAuthPolicy,
    subject: &DiscordInteractionSubject,
    channel_context: Option<&DiscordChannelContext>,
    guild_id: Option<&str>,
    dm_role_auth_guild: Option<&str>,
    entries: &[DiscordSkillCommandEntry],
    request: DiscordSkillCommandRequest<'_>,
) -> DiscordSkillCommandDecision {
    if policy.authorize_slash(subject, channel_context, guild_id, dm_role_auth_guild)
        != DiscordAuthDecision::Allow
    {
        return DiscordSkillCommandDecision::Unauthorized;
    }

    let requested = request.requested_name.trim();
    let Some(entry) = entries
        .iter()
        .find(|entry| entry.name.eq_ignore_ascii_case(requested))
    else {
        return DiscordSkillCommandDecision::UnknownSkill {
            requested_name: requested.to_string(),
        };
    };

    let args = request.args.trim();
    let text = if args.is_empty() {
        entry.command_key.clone()
    } else {
        format!("{} {}", entry.command_key, args)
    };
    DiscordSkillCommandDecision::Dispatch { text }
}

// ---------------------------------------------------------------------------
// Discord gateway parity helpers
// ---------------------------------------------------------------------------

fn discord_user_identifier_requires_member_lookup(raw: &str) -> bool {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return false;
    }
    let candidate = trimmed
        .strip_prefix('@')
        .unwrap_or(trimmed)
        .trim()
        .trim_matches(|c| c == '<' || c == '>');
    !candidate.is_empty() && !candidate.chars().all(|c| c.is_ascii_digit())
}

/// Whether Discord connect must request the privileged members intent.
pub fn discord_members_intent_required(
    allowed_users: impl IntoIterator<Item = impl AsRef<str>>,
) -> bool {
    allowed_users
        .into_iter()
        .any(|user| discord_user_identifier_requires_member_lookup(user.as_ref()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordClientReentryAction {
    ReuseFreshSlot,
    ClosePreviousClient,
}

/// Re-entering connect with an open client must close the old websocket first.
pub fn discord_client_reentry_action(previous_client_open: bool) -> DiscordClientReentryAction {
    if previous_client_open {
        DiscordClientReentryAction::ClosePreviousClient
    } else {
        DiscordClientReentryAction::ReuseFreshSlot
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordSlashSyncPolicy {
    Off,
    Diff,
    Bulk,
}

impl DiscordSlashSyncPolicy {
    pub fn parse(raw: Option<&str>) -> Self {
        match raw.map(str::trim).filter(|s| !s.is_empty()) {
            Some(value) if value.eq_ignore_ascii_case("off") => Self::Off,
            Some(value) if value.eq_ignore_ascii_case("bulk") => Self::Bulk,
            _ => Self::Diff,
        }
    }

    pub fn should_register(self, slash_commands_enabled: bool) -> bool {
        slash_commands_enabled && self != Self::Off
    }
}

/// Resolve a Discord channel prompt, preferring exact thread/channel IDs over parents.
pub fn discord_resolve_channel_prompt<'a>(
    prompts: &'a BTreeMap<String, String>,
    channel_id: &str,
    parent_channel_id: Option<&str>,
) -> Option<&'a str> {
    let channel_id = channel_id.trim();
    if !channel_id.is_empty() {
        if let Some(prompt) = prompts
            .get(channel_id)
            .map(String::as_str)
            .map(str::trim)
            .filter(|prompt| !prompt.is_empty())
        {
            return Some(prompt);
        }
    }

    parent_channel_id
        .map(str::trim)
        .filter(|parent| !parent.is_empty())
        .and_then(|parent| prompts.get(parent))
        .map(String::as_str)
        .map(str::trim)
        .filter(|prompt| !prompt.is_empty())
}

/// Compose per-run system prompt layers in Python gateway order.
pub fn discord_compose_ephemeral_system_prompt(
    context_prompt: Option<&str>,
    channel_prompt: Option<&str>,
    global_prompt: Option<&str>,
) -> Option<String> {
    let parts = [context_prompt, channel_prompt, global_prompt]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(String::from)
        .collect::<Vec<_>>();
    (!parts.is_empty()).then(|| parts.join("\n\n"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordModelPickerEdit {
    pub title: String,
    pub description: String,
    pub clears_view: bool,
}

pub fn discord_model_picker_switch_edits(
    model_id: &str,
    switch_result: &str,
) -> (DiscordModelPickerEdit, DiscordModelPickerEdit) {
    (
        DiscordModelPickerEdit {
            title: "Switching Model".into(),
            description: format!("Switching to `{}`...", model_id.trim()),
            clears_view: true,
        },
        DiscordModelPickerEdit {
            title: "Model Switched".into(),
            description: switch_result.to_string(),
            clears_view: true,
        },
    )
}

fn strip_discord_mentions(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '<' && matches!(chars.peek(), Some('@' | '#' | '&')) {
            let mut consumed_marker = false;
            for next in chars.by_ref() {
                if next == '>' {
                    consumed_marker = true;
                    break;
                }
            }
            if consumed_marker {
                out.push(' ');
                continue;
            }
        }
        out.push(ch);
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

pub fn discord_auto_thread_name(content: &str) -> String {
    let stripped = strip_discord_mentions(content);
    let candidate = stripped.trim();
    let candidate = if candidate.is_empty() {
        "Hermes"
    } else {
        candidate
    };

    let mut name = candidate.chars().take(80).collect::<String>();
    if candidate.chars().count() > 80 {
        while name.chars().count() > 77 {
            name.pop();
        }
        name.push_str("...");
    }
    name
}

pub fn discord_thread_create_success_message(thread_id: &str) -> String {
    format!("Created thread <#{}>.", thread_id.trim())
}

pub fn discord_thread_create_failure_message(error: &str) -> String {
    format!("Failed to create thread: {}", error.trim())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscordAttachmentKind {
    Image,
    Audio,
    Document,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordAttachmentHandling {
    pub kind: DiscordAttachmentKind,
    pub prefer_bot_session_read: bool,
    pub fallback_uses_ssrf_gate: bool,
    pub inject_text_content: bool,
}

pub fn discord_attachment_handling(
    filename: &str,
    content_type: Option<&str>,
    size_bytes: u64,
) -> DiscordAttachmentHandling {
    let ext = Path::new(filename)
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let content_type = content_type.unwrap_or_default().to_ascii_lowercase();
    let kind = if content_type.starts_with("image/")
        || matches!(ext.as_str(), "png" | "jpg" | "jpeg" | "gif" | "webp")
    {
        DiscordAttachmentKind::Image
    } else if content_type.starts_with("audio/")
        || matches!(ext.as_str(), "mp3" | "wav" | "ogg" | "m4a" | "flac")
    {
        DiscordAttachmentKind::Audio
    } else if !filename.trim().is_empty() {
        DiscordAttachmentKind::Document
    } else {
        DiscordAttachmentKind::Other
    };
    let inject_text_content = kind == DiscordAttachmentKind::Document
        && size_bytes <= 100 * 1024
        && (content_type.starts_with("text/")
            || matches!(ext.as_str(), "txt" | "md" | "markdown" | "log"));

    DiscordAttachmentHandling {
        kind,
        prefer_bot_session_read: matches!(
            kind,
            DiscordAttachmentKind::Image
                | DiscordAttachmentKind::Audio
                | DiscordAttachmentKind::Document
        ),
        fallback_uses_ssrf_gate: !matches!(kind, DiscordAttachmentKind::Other),
        inject_text_content,
    }
}

pub fn discord_inject_document_text(caption: &str, filename: &str, document_text: &str) -> String {
    let injected = format!(
        "[Content of {}]:\n{}",
        filename.trim(),
        document_text.trim_end()
    );
    let caption = caption.trim();
    if caption.is_empty() {
        injected
    } else {
        format!("{}\n\n{}", injected, caption)
    }
}

pub fn discord_opus_library_candidates(
    platform: &str,
    find_library_result: Option<&str>,
) -> Vec<String> {
    if let Some(found) = find_library_result
        .map(str::trim)
        .filter(|found| !found.is_empty())
    {
        return vec![found.to_string()];
    }

    if platform.eq_ignore_ascii_case("darwin") || platform.eq_ignore_ascii_case("macos") {
        vec![
            "/opt/homebrew/lib/libopus.dylib".into(),
            "/usr/local/lib/libopus.dylib".into(),
        ]
    } else {
        Vec::new()
    }
}

pub fn discord_should_log_opus_decode_error(error: Option<&str>) -> bool {
    error.map(str::trim).filter(|err| !err.is_empty()).is_some()
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscordVoiceJoinAction {
    Connect,
    MoveExisting,
    AlreadyConnected,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct DiscordVoiceJoinTracker {
    connected_guilds: BTreeSet<String>,
    inflight_guilds: BTreeSet<String>,
}

impl DiscordVoiceJoinTracker {
    pub fn begin_join(&mut self, guild_id: impl Into<String>) -> DiscordVoiceJoinAction {
        let guild_id = guild_id.into();
        if self.connected_guilds.contains(&guild_id) {
            return DiscordVoiceJoinAction::AlreadyConnected;
        }
        if self.inflight_guilds.contains(&guild_id) {
            return DiscordVoiceJoinAction::MoveExisting;
        }
        self.inflight_guilds.insert(guild_id);
        DiscordVoiceJoinAction::Connect
    }

    pub fn complete_join(&mut self, guild_id: impl AsRef<str>, connected: bool) {
        let guild_id = guild_id.as_ref();
        self.inflight_guilds.remove(guild_id);
        if connected {
            self.connected_guilds.insert(guild_id.to_string());
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordSlashRegistrationSpec {
    pub name: String,
    pub description: String,
    pub args_hint: Option<String>,
    pub command_text: String,
}

impl DiscordSlashRegistrationSpec {
    pub fn new(
        name: impl Into<String>,
        description: impl Into<String>,
        args_hint: Option<impl Into<String>>,
        command_text: impl Into<String>,
    ) -> Self {
        let args_hint = args_hint
            .map(Into::into)
            .map(|hint: String| hint.trim().to_string())
            .filter(|hint| !hint.is_empty());
        Self {
            name: name.into(),
            description: description.into(),
            args_hint,
            command_text: command_text.into(),
        }
    }

    pub fn to_slash_command(&self) -> SlashCommand {
        let options = self.args_hint.as_ref().map(|hint| {
            vec![SlashCommandOption {
                name: "args".into(),
                description: hint.chars().take(100).collect(),
                option_type: 3,
                required: Some(false),
                choices: None,
            }]
        });
        SlashCommand {
            name: self.name.clone(),
            description: self.description.chars().take(100).collect(),
            options,
            default_member_permissions: None,
            dm_permission: Some(true),
            nsfw: Some(false),
            contexts: None,
            integration_types: None,
            command_type: 1,
        }
    }

    pub fn dispatch_text(&self, args: Option<&str>) -> String {
        let args = args.map(str::trim).filter(|args| !args.is_empty());
        match args {
            Some(args) => format!("{} {}", self.command_text.trim(), args),
            None => self.command_text.trim().to_string(),
        }
    }
}

mod command_sync;
pub use command_sync::{
    discord_auto_registered_commands, discord_command_fingerprint, plan_discord_command_sync,
    DiscordCommandSyncMutation, DiscordCommandSyncSummary,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscordCommandSyncStateEntry {
    pub fingerprint: Option<String>,
    pub last_success_at: Option<u64>,
    pub last_attempt_at: Option<u64>,
    pub retry_after_until: Option<u64>,
    pub retry_after: Option<u64>,
}

impl DiscordCommandSyncStateEntry {
    pub fn should_attempt(&self, fingerprint: &str, now_epoch_secs: u64) -> bool {
        if self
            .retry_after_until
            .map(|until| until > now_epoch_secs)
            .unwrap_or(false)
        {
            return false;
        }
        self.fingerprint.as_deref() != Some(fingerprint)
    }

    pub fn record_attempt(&mut self, now_epoch_secs: u64) {
        self.last_attempt_at = Some(now_epoch_secs);
    }

    pub fn record_success(&mut self, fingerprint: impl Into<String>, now_epoch_secs: u64) {
        self.fingerprint = Some(fingerprint.into());
        self.last_success_at = Some(now_epoch_secs);
        self.retry_after = None;
        self.retry_after_until = None;
    }

    pub fn record_rate_limit(&mut self, retry_after_secs: u64, now_epoch_secs: u64) {
        self.retry_after = Some(retry_after_secs);
        self.retry_after_until = Some(now_epoch_secs.saturating_add(retry_after_secs));
    }
}

/// Channel-bound skill binding parsed from Python-style Discord config.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiscordChannelSkillBinding {
    pub id: String,
    pub skills: Vec<String>,
}

impl DiscordChannelSkillBinding {
    pub fn from_json(value: &serde_json::Value) -> Option<Self> {
        let obj = value.as_object()?;
        let id = obj.get("id").and_then(scalar_json_to_discord_id)?;
        let skills_value = obj.get("skills").or_else(|| obj.get("skill"))?;
        let mut skills = Vec::new();
        match skills_value {
            serde_json::Value::Array(values) => {
                for value in values {
                    if let Some(skill) = scalar_json_to_discord_id(value) {
                        if !skills.contains(&skill) {
                            skills.push(skill);
                        }
                    }
                }
            }
            value => {
                if let Some(skill) = scalar_json_to_discord_id(value) {
                    skills.push(skill);
                }
            }
        }
        (!skills.is_empty()).then_some(Self { id, skills })
    }

    pub fn list_from_json(value: Option<&serde_json::Value>) -> Vec<Self> {
        match value {
            Some(serde_json::Value::Array(values)) => {
                values.iter().filter_map(Self::from_json).collect()
            }
            Some(value) => Self::from_json(value).into_iter().collect(),
            None => Vec::new(),
        }
    }
}

fn resolve_channel_skills_from_bindings(
    bindings: &[DiscordChannelSkillBinding],
    channel_id: &str,
    parent_id: Option<&str>,
) -> Option<Vec<String>> {
    let channel_id = channel_id.trim();
    let parent_id = parent_id.map(str::trim).filter(|id| !id.is_empty());

    bindings
        .iter()
        .find(|binding| binding.id.trim() == channel_id)
        .or_else(|| {
            parent_id.and_then(|parent| bindings.iter().find(|binding| binding.id.trim() == parent))
        })
        .map(|binding| binding.skills.clone())
}

// ---------------------------------------------------------------------------
// Discord thread participation persistence
// ---------------------------------------------------------------------------

/// Persistent ordered set of Discord threads the bot has participated in.
#[derive(Debug, Clone)]
pub struct DiscordThreadParticipationTracker {
    path: PathBuf,
    threads: VecDeque<String>,
    max_tracked: usize,
}

impl DiscordThreadParticipationTracker {
    pub const DEFAULT_MAX_TRACKED: usize = 2048;

    pub fn new(platform: &str) -> Self {
        let filename = format!("{}_threads.json", platform.trim());
        Self::from_path(
            hermes_config::hermes_home().join(filename),
            Self::DEFAULT_MAX_TRACKED,
        )
    }

    pub fn from_path(path: impl Into<PathBuf>, max_tracked: usize) -> Self {
        let path = path.into();
        let mut tracker = Self {
            path,
            threads: VecDeque::new(),
            max_tracked: max_tracked.max(1),
        };
        tracker.load();
        tracker
    }

    pub fn set_max_tracked(&mut self, max_tracked: usize) {
        self.max_tracked = max_tracked.max(1);
        self.enforce_capacity();
    }

    pub fn contains(&self, thread_id: &str) -> bool {
        let thread_id = thread_id.trim();
        !thread_id.is_empty() && self.threads.iter().any(|existing| existing == thread_id)
    }

    pub fn mark(&mut self, thread_id: impl Into<String>) -> std::io::Result<bool> {
        let thread_id = thread_id.into();
        let thread_id = thread_id.trim();
        if thread_id.is_empty() || self.contains(thread_id) {
            return Ok(false);
        }

        self.threads.push_back(thread_id.to_string());
        self.enforce_capacity();
        self.save()?;
        Ok(true)
    }

    pub fn len(&self) -> usize {
        self.threads.len()
    }

    pub fn is_empty(&self) -> bool {
        self.threads.is_empty()
    }

    pub fn entries(&self) -> Vec<String> {
        self.threads.iter().cloned().collect()
    }

    fn load(&mut self) {
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return;
        };
        let Ok(values) = serde_json::from_str::<Vec<String>>(&raw) else {
            return;
        };

        let mut seen = BTreeSet::new();
        for value in values {
            let trimmed = value.trim();
            if !trimmed.is_empty() && seen.insert(trimmed.to_string()) {
                self.threads.push_back(trimmed.to_string());
            }
        }
        self.enforce_capacity();
    }

    fn enforce_capacity(&mut self) {
        while self.threads.len() > self.max_tracked {
            self.threads.pop_front();
        }
    }

    fn save(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let values: Vec<&str> = self.threads.iter().map(String::as_str).collect();
        let body = serde_json::to_string(&values).expect("thread id list serializes");
        std::fs::write(&self.path, body)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

pub type ThreadParticipationTracker = DiscordThreadParticipationTracker;

// ---------------------------------------------------------------------------
// Discord non-conversational message persistence
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct DiscordNonConversationalMessageTracker {
    path: PathBuf,
    ids: VecDeque<String>,
    max_tracked: usize,
}

impl DiscordNonConversationalMessageTracker {
    pub const DEFAULT_MAX_TRACKED: usize = 2000;

    pub fn new(platform: &str) -> Self {
        let platform = platform.trim();
        let filename = if platform.is_empty() || platform.eq_ignore_ascii_case("discord") {
            DISCORD_NONCONVERSATIONAL_STATE_FILENAME.to_string()
        } else {
            format!("{}_{}", platform, DISCORD_NONCONVERSATIONAL_STATE_FILENAME)
        };
        Self::from_path(
            hermes_config::hermes_home().join("gateway").join(filename),
            Self::DEFAULT_MAX_TRACKED,
        )
    }

    pub fn from_path(path: impl Into<PathBuf>, max_tracked: usize) -> Self {
        let path = path.into();
        let mut tracker = Self {
            path,
            ids: VecDeque::new(),
            max_tracked: max_tracked.max(1),
        };
        tracker.load();
        tracker
    }

    pub fn contains(&self, message_id: &str) -> bool {
        let message_id = message_id.trim();
        !message_id.is_empty() && self.ids.iter().any(|existing| existing == message_id)
    }

    pub fn mark_many(
        &mut self,
        message_ids: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> std::io::Result<bool> {
        let mut changed = false;
        for message_id in message_ids {
            let message_id = message_id.as_ref().trim();
            if !message_id.is_empty() && !self.contains(message_id) {
                self.ids.push_back(message_id.to_string());
                changed = true;
            }
        }
        if changed {
            self.enforce_capacity();
            self.save()?;
        }
        Ok(changed)
    }

    pub fn entries(&self) -> Vec<String> {
        self.ids.iter().cloned().collect()
    }

    fn load(&mut self) {
        let Ok(raw) = std::fs::read_to_string(&self.path) else {
            return;
        };
        let Ok(values) = serde_json::from_str::<Vec<String>>(&raw) else {
            return;
        };
        let mut seen = BTreeSet::new();
        for value in values {
            let trimmed = value.trim();
            if !trimmed.is_empty() && seen.insert(trimmed.to_string()) {
                self.ids.push_back(trimmed.to_string());
            }
        }
        self.enforce_capacity();
    }

    fn enforce_capacity(&mut self) {
        while self.ids.len() > self.max_tracked {
            self.ids.pop_front();
        }
    }

    fn save(&self) -> std::io::Result<()> {
        if let Some(parent) = self.path.parent().filter(|p| !p.as_os_str().is_empty()) {
            std::fs::create_dir_all(parent)?;
        }
        let values: Vec<&str> = self.ids.iter().map(String::as_str).collect();
        let body = serde_json::to_string(&values).expect("discord status id list serializes");
        std::fs::write(&self.path, body)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

// ---------------------------------------------------------------------------
// Discord Gateway opcodes & payload
// ---------------------------------------------------------------------------

include!("discord/gateway_state.rs");

// ---------------------------------------------------------------------------
// Typed dispatch events
// ---------------------------------------------------------------------------

/// A strongly-typed dispatch event produced by [`DiscordAdapter::parse_dispatch`].
#[derive(Debug, Clone)]
pub enum DispatchEvent {
    MessageCreate(IncomingDiscordMessage),
    MessageUpdate(MessageUpdateEvent),
    InteractionCreate(InteractionData),
    ReactionAdd(ReactionEvent),
    ReactionRemove(ReactionEvent),
    VoiceStateUpdate(VoiceState),
}

// ---------------------------------------------------------------------------
// PlatformAdapter trait impl
// ---------------------------------------------------------------------------

#[async_trait]
impl PlatformAdapter for DiscordAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "Discord adapter starting (token: {})",
            describe_secret(&self.config.token)
        );
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Discord adapter stopping");
        self.base.mark_stopped();
        self.stop_signal.notify_one();
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.send_text(chat_id, text).await?;
        Ok(())
    }

    async fn send_message_with_options(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
        options: SendMessageOptions,
    ) -> Result<(), GatewayError> {
        let metadata = discord_metadata_from_send_options(&options);
        self.send_text_with_metadata(chat_id, text, metadata.as_ref())
            .await?;
        Ok(())
    }

    async fn send_or_update_status(
        &self,
        chat_id: &str,
        _status_key: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        let metadata = DiscordSendMetadata::non_conversational();
        self.send_text_with_metadata(chat_id, text, Some(&metadata))
            .await?;
        Ok(())
    }

    async fn edit_message(
        &self,
        chat_id: &str,
        message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        self.edit_text(chat_id, message_id, text).await
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.upload_file(chat_id, file_path, caption).await?;
        Ok(())
    }

    async fn send_file_with_options(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
        options: SendMessageOptions,
    ) -> Result<(), GatewayError> {
        let metadata = discord_metadata_from_send_options(&options);
        self.upload_file_with_metadata(chat_id, file_path, caption, metadata.as_ref())
            .await?;
        Ok(())
    }

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.send_image_url_with_metadata(chat_id, image_url, caption, None)
            .await?;
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }

    fn splits_long_messages(&self) -> bool {
        true
    }

    fn platform_name(&self) -> &str {
        "discord"
    }
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

/// Split a message into chunks that fit within the given max length.
fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut start = 0;

    while start < text.len() {
        let end = (start + max_len).min(text.len());

        if end >= text.len() {
            chunks.push(text[start..].to_string());
            break;
        }

        let break_at = text[start..end]
            .rfind('\n')
            .map(|pos| start + pos + 1)
            .unwrap_or(end);

        chunks.push(text[start..break_at].to_string());
        start = break_at;
    }

    chunks
}

/// URL-encode a unicode emoji for use in reaction endpoints.
pub fn encode_emoji(emoji: &str) -> String {
    percent_encode_emoji(emoji)
}

fn percent_encode_emoji(s: &str) -> String {
    let mut out = String::new();
    for byte in s.as_bytes() {
        if byte.is_ascii_alphanumeric() || *byte == b'-' || *byte == b'_' || *byte == b':' {
            out.push(*byte as char);
        } else {
            out.push_str(&format!("%{:02X}", byte));
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
