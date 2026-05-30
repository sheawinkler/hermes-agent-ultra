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
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{describe_secret, AdapterProxyConfig, BasePlatformAdapter};

/// Maximum message length for Discord (2000 characters).
const MAX_MESSAGE_LENGTH: usize = 2000;

/// Discord API base URL.
const DISCORD_API_BASE: &str = "https://discord.com/api/v10";

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
}

impl DiscordSendMetadata {
    pub fn with_thread_id(thread_id: impl Into<String>) -> Self {
        Self {
            thread_id: Some(thread_id.into()),
            reply_to_message_id: None,
        }
    }

    pub fn with_reply_to_message_id(message_id: impl Into<String>) -> Self {
        Self {
            thread_id: None,
            reply_to_message_id: Some(message_id.into()),
        }
    }

    pub fn with_thread_and_reply(
        thread_id: impl Into<String>,
        message_id: impl Into<String>,
    ) -> Self {
        Self {
            thread_id: Some(thread_id.into()),
            reply_to_message_id: Some(message_id.into()),
        }
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
    requested_name: &str,
    args: &str,
) -> DiscordSkillCommandDecision {
    if policy.authorize_slash(subject, channel_context, guild_id, dm_role_auth_guild)
        != DiscordAuthDecision::Allow
    {
        return DiscordSkillCommandDecision::Unauthorized;
    }

    let requested = requested_name.trim();
    let Some(entry) = entries
        .iter()
        .find(|entry| entry.name.eq_ignore_ascii_case(requested))
    else {
        return DiscordSkillCommandDecision::UnknownSkill {
            requested_name: requested.to_string(),
        };
    };

    let args = args.trim();
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

pub fn discord_auto_registered_commands(
    explicit_names: impl IntoIterator<Item = impl AsRef<str>>,
    gateway_specs: impl IntoIterator<Item = DiscordSlashRegistrationSpec>,
    plugin_specs: impl IntoIterator<Item = DiscordSlashRegistrationSpec>,
) -> Vec<DiscordSlashRegistrationSpec> {
    let mut registered = explicit_names
        .into_iter()
        .map(|name| {
            name.as_ref()
                .trim()
                .trim_start_matches('/')
                .to_ascii_lowercase()
        })
        .filter(|name| !name.is_empty())
        .collect::<BTreeSet<_>>();
    let mut out = Vec::new();
    for spec in gateway_specs.into_iter().chain(plugin_specs) {
        let key = spec
            .name
            .trim()
            .trim_start_matches('/')
            .to_ascii_lowercase();
        if key.is_empty() || registered.contains(&key) {
            continue;
        }
        registered.insert(key);
        out.push(spec);
    }
    out
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DiscordCommandSyncSummary {
    pub total: usize,
    pub unchanged: usize,
    pub updated: usize,
    pub recreated: usize,
    pub created: usize,
    pub deleted: usize,
    pub mutations: Vec<DiscordCommandSyncMutation>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiscordCommandSyncMutation {
    Create { name: String },
    Update { name: String },
    Recreate { name: String },
    Delete { name: String },
}

fn json_to_sorted(value: &serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let sorted = map
                .iter()
                .map(|(key, value)| (key.clone(), json_to_sorted(value)))
                .collect::<serde_json::Map<_, _>>();
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(json_to_sorted).collect())
        }
        other => other.clone(),
    }
}

fn command_key(payload: &serde_json::Value) -> Option<(String, u64)> {
    let name = payload.get("name")?.as_str()?.trim().to_ascii_lowercase();
    if name.is_empty() {
        return None;
    }
    let command_type = payload.get("type").and_then(|v| v.as_u64()).unwrap_or(1);
    Some((name, command_type))
}

fn normalize_command_payload(payload: &serde_json::Value) -> serde_json::Value {
    let mut normalized = match payload {
        serde_json::Value::Object(map) => map.clone(),
        _ => serde_json::Map::new(),
    };
    for key in [
        "id",
        "application_id",
        "version",
        "name_localizations",
        "description_localizations",
    ] {
        normalized.remove(key);
    }
    normalized.retain(|_, value| !value.is_null());
    normalized
        .entry("type")
        .or_insert_with(|| serde_json::json!(1));
    normalized
        .entry("options")
        .or_insert_with(|| serde_json::json!([]));
    normalized
        .entry("nsfw")
        .or_insert_with(|| serde_json::json!(false));
    normalized
        .entry("dm_permission")
        .or_insert_with(|| serde_json::json!(true));
    json_to_sorted(&serde_json::Value::Object(normalized))
}

fn command_patchable_view(payload: &serde_json::Value) -> serde_json::Value {
    let mut map = match normalize_command_payload(payload) {
        serde_json::Value::Object(map) => map,
        _ => serde_json::Map::new(),
    };
    for key in [
        "nsfw",
        "dm_permission",
        "default_member_permissions",
        "contexts",
        "integration_types",
    ] {
        map.remove(key);
    }
    serde_json::Value::Object(map)
}

fn command_requires_recreate(desired: &serde_json::Value, existing: &serde_json::Value) -> bool {
    let desired = normalize_command_payload(desired);
    let existing = normalize_command_payload(existing);
    [
        "nsfw",
        "dm_permission",
        "default_member_permissions",
        "contexts",
        "integration_types",
    ]
    .into_iter()
    .any(|key| desired.get(key) != existing.get(key))
}

pub fn plan_discord_command_sync(
    desired: &[serde_json::Value],
    existing: &[serde_json::Value],
) -> DiscordCommandSyncSummary {
    let mut existing_by_key = existing
        .iter()
        .filter_map(|payload| command_key(payload).map(|key| (key, payload)))
        .collect::<BTreeMap<_, _>>();
    let mut summary = DiscordCommandSyncSummary {
        total: desired.len(),
        ..DiscordCommandSyncSummary::default()
    };

    for desired_payload in desired {
        let Some((name, command_type)) = command_key(desired_payload) else {
            continue;
        };
        match existing_by_key.remove(&(name.clone(), command_type)) {
            None => {
                summary.created += 1;
                summary
                    .mutations
                    .push(DiscordCommandSyncMutation::Create { name });
            }
            Some(existing_payload)
                if normalize_command_payload(desired_payload)
                    == normalize_command_payload(existing_payload) =>
            {
                summary.unchanged += 1;
            }
            Some(existing_payload)
                if command_requires_recreate(desired_payload, existing_payload) =>
            {
                summary.recreated += 1;
                summary
                    .mutations
                    .push(DiscordCommandSyncMutation::Recreate { name });
            }
            Some(existing_payload)
                if command_patchable_view(desired_payload)
                    != command_patchable_view(existing_payload) =>
            {
                summary.updated += 1;
                summary
                    .mutations
                    .push(DiscordCommandSyncMutation::Update { name });
            }
            Some(_) => {
                summary.unchanged += 1;
            }
        }
    }

    for ((name, _), _) in existing_by_key {
        summary.deleted += 1;
        summary
            .mutations
            .push(DiscordCommandSyncMutation::Delete { name });
    }
    summary
}

pub fn discord_command_fingerprint(commands: &[serde_json::Value]) -> String {
    let mut normalized = commands
        .iter()
        .map(normalize_command_payload)
        .collect::<Vec<_>>();
    normalized.sort_by(|a, b| command_key(a).cmp(&command_key(b)));
    serde_json::to_string(&normalized).unwrap_or_default()
}

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
// Discord Gateway opcodes & payload
// ---------------------------------------------------------------------------

/// Discord Gateway payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayPayload {
    pub op: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub d: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub t: Option<String>,
}

/// Discord Gateway opcodes.
pub mod opcodes {
    pub const DISPATCH: u8 = 0;
    pub const HEARTBEAT: u8 = 1;
    pub const IDENTIFY: u8 = 2;
    pub const PRESENCE_UPDATE: u8 = 3;
    pub const VOICE_STATE: u8 = 4;
    pub const RESUME: u8 = 6;
    pub const RECONNECT: u8 = 7;
    pub const REQUEST_GUILD_MEMBERS: u8 = 8;
    pub const INVALID_SESSION: u8 = 9;
    pub const HELLO: u8 = 10;
    pub const HEARTBEAT_ACK: u8 = 11;
}

/// Discord IDENTIFY payload data.
#[derive(Debug, Serialize)]
pub struct IdentifyData {
    pub token: String,
    pub intents: u64,
    pub properties: IdentifyProperties,
}

/// Discord IDENTIFY connection properties.
#[derive(Debug, Serialize)]
pub struct IdentifyProperties {
    pub os: String,
    pub browser: String,
    pub device: String,
}

/// Discord RESUME payload data.
#[derive(Debug, Serialize)]
pub struct ResumeData {
    pub token: String,
    pub session_id: String,
    pub seq: u64,
}

// ---------------------------------------------------------------------------
// Gateway state machine
// ---------------------------------------------------------------------------

/// Actions that the external WebSocket driver should take after processing
/// a gateway event through [`GatewaySession::handle_gateway_event`].
#[derive(Debug, Clone, PartialEq)]
pub enum GatewayAction {
    /// Send an IDENTIFY payload to the gateway.
    SendIdentify,
    /// Send a HEARTBEAT payload with the current sequence number.
    SendHeartbeat,
    /// Send a RESUME payload to continue a disconnected session.
    SendResume,
    /// The gateway requested a reconnect – close and reconnect.
    Reconnect,
    /// The session has been invalidated; if `bool` is true the session
    /// is resumable, otherwise a fresh IDENTIFY is required.
    InvalidSession(bool),
    /// A dispatch event arrived. Contains the event name and its data.
    Dispatch(String, serde_json::Value),
}

/// Manages the client-side state for a single Discord Gateway connection.
///
/// This is a pure state machine: feed it [`GatewayPayload`]s received from
/// the WebSocket and it will return a list of [`GatewayAction`]s that the
/// driver should execute. The struct never performs I/O itself, making it
/// easy to test and compose with any WebSocket library.
#[derive(Debug)]
pub struct GatewaySession {
    /// Last received sequence number.
    pub sequence: Option<u64>,
    /// Session ID from the READY event.
    pub session_id: Option<String>,
    /// Resume gateway URL from the READY event.
    pub resume_gateway_url: Option<String>,
    /// Heartbeat interval in milliseconds, extracted from HELLO.
    pub heartbeat_interval_ms: Option<u64>,
    /// Whether the last heartbeat was acknowledged.
    pub heartbeat_acknowledged: bool,
    /// Tracks whether we have successfully identified.
    pub identified: bool,
}

impl GatewaySession {
    pub fn new() -> Self {
        Self {
            sequence: None,
            session_id: None,
            resume_gateway_url: None,
            heartbeat_interval_ms: None,
            heartbeat_acknowledged: true,
            identified: false,
        }
    }

    /// Returns `true` if the session holds enough data to attempt a RESUME.
    pub fn can_resume(&self) -> bool {
        self.session_id.is_some() && self.sequence.is_some()
    }

    /// Process an incoming gateway payload and return the actions the driver
    /// should perform.
    pub fn handle_gateway_event(&mut self, payload: &GatewayPayload) -> Vec<GatewayAction> {
        if let Some(seq) = payload.s {
            self.sequence = Some(seq);
        }

        match payload.op {
            opcodes::HELLO => self.handle_hello(payload),
            opcodes::HEARTBEAT_ACK => self.handle_heartbeat_ack(),
            opcodes::HEARTBEAT => self.handle_heartbeat_request(),
            opcodes::RECONNECT => vec![GatewayAction::Reconnect],
            opcodes::INVALID_SESSION => self.handle_invalid_session(payload),
            opcodes::DISPATCH => self.handle_dispatch(payload),
            _ => {
                debug!("unhandled gateway opcode {}", payload.op);
                vec![]
            }
        }
    }

    fn handle_hello(&mut self, payload: &GatewayPayload) -> Vec<GatewayAction> {
        let mut actions = Vec::new();

        if let Some(d) = &payload.d {
            if let Some(interval) = d.get("heartbeat_interval").and_then(|v| v.as_u64()) {
                self.heartbeat_interval_ms = Some(interval);
                debug!("gateway HELLO: heartbeat_interval={}ms", interval);
            }
        }

        actions.push(GatewayAction::SendHeartbeat);

        if self.can_resume() {
            actions.push(GatewayAction::SendResume);
        } else {
            actions.push(GatewayAction::SendIdentify);
        }

        actions
    }

    fn handle_heartbeat_ack(&mut self) -> Vec<GatewayAction> {
        self.heartbeat_acknowledged = true;
        debug!("heartbeat ACK received");
        vec![]
    }

    fn handle_heartbeat_request(&self) -> Vec<GatewayAction> {
        vec![GatewayAction::SendHeartbeat]
    }

    fn handle_invalid_session(&mut self, payload: &GatewayPayload) -> Vec<GatewayAction> {
        let resumable = payload
            .d
            .as_ref()
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !resumable {
            self.session_id = None;
            self.sequence = None;
            self.identified = false;
        }

        warn!("INVALID_SESSION received (resumable={})", resumable);
        vec![GatewayAction::InvalidSession(resumable)]
    }

    fn handle_dispatch(&mut self, payload: &GatewayPayload) -> Vec<GatewayAction> {
        let event_name = match &payload.t {
            Some(name) => name.clone(),
            None => return vec![],
        };

        let data = payload.d.clone().unwrap_or(serde_json::Value::Null);

        if event_name == "READY" {
            self.handle_ready(&data);
        }

        vec![GatewayAction::Dispatch(event_name, data)]
    }

    fn handle_ready(&mut self, data: &serde_json::Value) {
        self.identified = true;

        if let Some(sid) = data.get("session_id").and_then(|v| v.as_str()) {
            self.session_id = Some(sid.to_string());
        }
        if let Some(url) = data.get("resume_gateway_url").and_then(|v| v.as_str()) {
            self.resume_gateway_url = Some(url.to_string());
        }

        info!(
            "READY: session_id={:?}, resume_url={:?}",
            self.session_id, self.resume_gateway_url
        );
    }

    /// Mark a heartbeat as sent (used by the driver before sending).
    pub fn heartbeat_sent(&mut self) {
        self.heartbeat_acknowledged = false;
    }

    /// Returns `true` if the last heartbeat was not acknowledged, indicating
    /// the connection is likely zombied and should be reconnected.
    pub fn is_zombie(&self) -> bool {
        !self.heartbeat_acknowledged
    }

    /// Reset the session state for a fresh connection.
    pub fn reset(&mut self) {
        self.sequence = None;
        self.session_id = None;
        self.resume_gateway_url = None;
        self.heartbeat_interval_ms = None;
        self.heartbeat_acknowledged = true;
        self.identified = false;
    }
}

impl Default for GatewaySession {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Discord REST API types
// ---------------------------------------------------------------------------

/// Discord Message object.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscordMessage {
    pub id: String,
    pub channel_id: String,
    #[serde(default)]
    pub content: String,
    #[serde(default)]
    pub author: Option<DiscordUser>,
}

/// Discord User object.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscordUser {
    pub id: String,
    #[serde(default)]
    pub username: Option<String>,
    #[serde(default)]
    pub bot: Option<bool>,
}

/// Incoming message parsed from a Discord MESSAGE_CREATE event.
#[derive(Debug, Clone)]
pub struct IncomingDiscordMessage {
    pub channel_id: String,
    pub message_id: String,
    pub user_id: Option<String>,
    pub username: Option<String>,
    pub content: String,
    pub is_bot: bool,
    pub message_type: u8,
    pub mention_user_ids: Vec<String>,
    pub reply_to_message_id: Option<String>,
    pub reply_to_text: Option<String>,
}

impl IncomingDiscordMessage {
    pub fn mentions_user(&self, user_id: &str) -> bool {
        let needle = user_id.trim();
        !needle.is_empty()
            && self
                .mention_user_ids
                .iter()
                .any(|mentioned| mentioned.trim() == needle)
    }
}

// ---------------------------------------------------------------------------
// Event types: MESSAGE_UPDATE
// ---------------------------------------------------------------------------

/// Parsed data from a `MESSAGE_UPDATE` dispatch event.
///
/// Discord may send partial updates — only `id` and `channel_id` are
/// guaranteed; other fields are optional.
#[derive(Debug, Clone)]
pub struct MessageUpdateEvent {
    pub channel_id: String,
    pub message_id: String,
    pub content: Option<String>,
    pub author_id: Option<String>,
    pub guild_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Event types: INTERACTION_CREATE (slash commands)
// ---------------------------------------------------------------------------

/// Parsed interaction from `INTERACTION_CREATE`.
#[derive(Debug, Clone)]
pub struct InteractionData {
    pub id: String,
    pub application_id: String,
    /// Interaction type (2 = APPLICATION_COMMAND, 3 = MESSAGE_COMPONENT, …).
    pub interaction_type: u8,
    pub token: String,
    pub channel_id: Option<String>,
    pub guild_id: Option<String>,
    pub user_id: Option<String>,
    pub command_name: Option<String>,
    pub command_options: Vec<InteractionOption>,
}

/// A single option supplied to a slash command invocation.
#[derive(Debug, Clone)]
pub struct InteractionOption {
    pub name: String,
    pub value: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Event types: Reactions
// ---------------------------------------------------------------------------

/// Parsed data from `MESSAGE_REACTION_ADD` / `MESSAGE_REACTION_REMOVE`.
#[derive(Debug, Clone)]
pub struct ReactionEvent {
    pub user_id: String,
    pub channel_id: String,
    pub message_id: String,
    pub guild_id: Option<String>,
    pub emoji_name: Option<String>,
    pub emoji_id: Option<String>,
}

// ---------------------------------------------------------------------------
// Event types: Voice state
// ---------------------------------------------------------------------------

/// Parsed `VOICE_STATE_UPDATE` event.
#[derive(Debug, Clone)]
pub struct VoiceState {
    pub guild_id: Option<String>,
    pub channel_id: Option<String>,
    pub user_id: String,
    pub session_id: String,
    pub deaf: bool,
    pub mute: bool,
    pub self_deaf: bool,
    pub self_mute: bool,
    pub suppress: bool,
}

// ---------------------------------------------------------------------------
// Slash command registration types
// ---------------------------------------------------------------------------

/// Definition of a slash command to register with Discord.
#[derive(Debug, Clone, Serialize)]
pub struct SlashCommand {
    pub name: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub options: Option<Vec<SlashCommandOption>>,
    /// Discord permission bitset string. "0" hides the command by default.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default_member_permissions: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dm_permission: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub nsfw: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub contexts: Option<Vec<u8>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub integration_types: Option<Vec<u8>>,
    /// Command type (1 = CHAT_INPUT, 2 = USER, 3 = MESSAGE). Default 1.
    #[serde(rename = "type", default = "default_command_type")]
    pub command_type: u8,
}

/// A single option for a slash command.
#[derive(Debug, Clone, Serialize)]
pub struct SlashCommandOption {
    pub name: String,
    pub description: String,
    /// Option type (3 = STRING, 4 = INTEGER, 5 = BOOLEAN, 6 = USER, …).
    #[serde(rename = "type")]
    pub option_type: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub choices: Option<Vec<SlashCommandChoice>>,
}

/// A predefined choice for a slash command option.
#[derive(Debug, Clone, Serialize)]
pub struct SlashCommandChoice {
    pub name: String,
    pub value: serde_json::Value,
}

pub fn apply_owner_only_slash_visibility(commands: &mut [SlashCommand]) {
    for command in commands {
        command.default_member_permissions = Some("0".to_string());
    }
}

// ---------------------------------------------------------------------------
// Embed types
// ---------------------------------------------------------------------------

/// A Discord rich embed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordEmbed {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub color: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub footer: Option<EmbedFooter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub image: Option<EmbedMedia>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub thumbnail: Option<EmbedMedia>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<EmbedAuthor>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fields: Vec<EmbedField>,
}

impl DiscordEmbed {
    pub fn new() -> Self {
        Self {
            title: None,
            description: None,
            url: None,
            color: None,
            timestamp: None,
            footer: None,
            image: None,
            thumbnail: None,
            author: None,
            fields: Vec::new(),
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_description(mut self, desc: impl Into<String>) -> Self {
        self.description = Some(desc.into());
        self
    }

    pub fn with_color(mut self, color: u32) -> Self {
        self.color = Some(color);
        self
    }

    pub fn with_footer(mut self, text: impl Into<String>) -> Self {
        self.footer = Some(EmbedFooter {
            text: text.into(),
            icon_url: None,
        });
        self
    }

    pub fn with_timestamp(mut self, ts: impl Into<String>) -> Self {
        self.timestamp = Some(ts.into());
        self
    }

    pub fn add_field(
        mut self,
        name: impl Into<String>,
        value: impl Into<String>,
        inline: bool,
    ) -> Self {
        self.fields.push(EmbedField {
            name: name.into(),
            value: value.into(),
            inline: Some(inline),
        });
        self
    }
}

impl Default for DiscordEmbed {
    fn default() -> Self {
        Self::new()
    }
}

/// Embed footer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedFooter {
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
}

/// Embed media (image / thumbnail).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedMedia {
    pub url: String,
}

/// Embed author.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedAuthor {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon_url: Option<String>,
}

/// A single field in an embed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbedField {
    pub name: String,
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline: Option<bool>,
}

// ---------------------------------------------------------------------------
// Thread creation result
// ---------------------------------------------------------------------------

/// Response from creating a thread.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscordThread {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub thread_type: Option<u8>,
    pub guild_id: Option<String>,
    pub parent_id: Option<String>,
}

/// Response from creating a Discord forum post thread.
#[derive(Debug, Clone, Deserialize)]
pub struct DiscordForumThread {
    pub id: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub message: Option<DiscordMessage>,
}

/// Result of a forum post send where follow-up chunk failures are non-fatal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscordForumSendOutcome {
    pub thread_id: String,
    pub message_id: String,
    pub warnings: Vec<String>,
}

// ---------------------------------------------------------------------------
// DiscordAdapter
// ---------------------------------------------------------------------------

/// Discord Bot API platform adapter.
pub struct DiscordAdapter {
    base: BasePlatformAdapter,
    config: DiscordConfig,
    client: Client,
    stop_signal: Arc<Notify>,
    thread_participation: Mutex<DiscordThreadParticipationTracker>,
}

impl DiscordAdapter {
    /// Create a new Discord adapter with the given configuration.
    pub fn new(config: DiscordConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.token).with_proxy(config.proxy.clone());

        base.validate_token()?;

        let client = base.build_client()?;

        Ok(Self {
            base,
            config,
            client,
            stop_signal: Arc::new(Notify::new()),
            thread_participation: Mutex::new(DiscordThreadParticipationTracker::new("discord")),
        })
    }

    /// Get a reference to the configuration.
    pub fn config(&self) -> &DiscordConfig {
        &self.config
    }

    pub fn channel_controls(&self) -> &DiscordChannelControls {
        &self.config.channel_controls
    }

    pub fn should_ignore_channel(&self, context: &DiscordChannelContext) -> bool {
        self.config.channel_controls.is_ignored(context)
    }

    pub fn should_auto_thread(&self, context: &DiscordChannelContext) -> bool {
        self.config.channel_controls.should_auto_thread(context)
    }

    pub fn resolve_channel_skills(
        &self,
        channel_id: &str,
        parent_id: Option<&str>,
    ) -> Option<Vec<String>> {
        resolve_channel_skills_from_bindings(
            &self.config.channel_skill_bindings,
            channel_id,
            parent_id,
        )
    }

    pub fn thread_participation_contains(&self, thread_id: &str) -> bool {
        self.thread_participation
            .lock()
            .map(|tracker| tracker.contains(thread_id))
            .unwrap_or(false)
    }

    pub fn mark_thread_participation(&self, thread_id: &str) -> std::io::Result<bool> {
        self.thread_participation
            .lock()
            .map_err(|_| std::io::Error::other("discord thread tracker lock poisoned"))?
            .mark(thread_id)
    }

    /// Return the authorization header value.
    fn auth_header(&self) -> String {
        format!("Bot {}", self.config.token)
    }

    // -----------------------------------------------------------------------
    // REST API: Sending messages
    // -----------------------------------------------------------------------

    /// Send a message to a Discord channel, splitting if it exceeds 2000 chars.
    pub async fn send_text(
        &self,
        channel_id: &str,
        content: &str,
    ) -> Result<Vec<String>, GatewayError> {
        self.send_text_with_metadata(channel_id, content, None)
            .await
    }

    /// Send a message, honoring Discord thread routing metadata when present.
    pub async fn send_text_with_metadata(
        &self,
        channel_id: &str,
        content: &str,
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<Vec<String>, GatewayError> {
        let target_channel_id = target_channel_id_for_metadata(channel_id, metadata);
        let chunks = split_message(content, MAX_MESSAGE_LENGTH);
        let mut message_ids = Vec::new();
        let reply_to_message_id = reply_to_message_id_for_metadata(metadata);
        let reply_to_mode = DiscordReplyToMode::parse(Some(&self.config.reply_to_mode));
        let mut suppress_reply_references = false;

        for (index, chunk) in chunks.iter().enumerate() {
            let url = format!(
                "{}/channels/{}/messages",
                DISCORD_API_BASE, target_channel_id
            );
            let include_reply_reference = !suppress_reply_references
                && reply_to_message_id.is_some()
                && reply_to_mode.references_chunk(index);
            let body = discord_message_body(
                chunk,
                include_reply_reference.then_some(reply_to_message_id.unwrap_or_default()),
                default_discord_allowed_mentions(),
            );

            let resp = self
                .client
                .post(&url)
                .header("Authorization", self.auth_header())
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| GatewayError::SendFailed(format!("Discord send failed: {}", e)))?;

            if !resp.status().is_success() {
                let text = resp.text().await.unwrap_or_default();
                if include_reply_reference && discord_reply_reference_error_allows_retry(&text) {
                    suppress_reply_references = true;
                    let retry_body =
                        discord_message_body(chunk, None, default_discord_allowed_mentions());
                    let retry_resp = self
                        .client
                        .post(&url)
                        .header("Authorization", self.auth_header())
                        .header("Content-Type", "application/json")
                        .json(&retry_body)
                        .send()
                        .await
                        .map_err(|e| {
                            GatewayError::SendFailed(format!("Discord send failed: {}", e))
                        })?;

                    if !retry_resp.status().is_success() {
                        let retry_text = retry_resp.text().await.unwrap_or_default();
                        return Err(GatewayError::SendFailed(format!(
                            "Discord API error: {}",
                            retry_text
                        )));
                    }

                    let msg: DiscordMessage = retry_resp.json().await.map_err(|e| {
                        GatewayError::SendFailed(format!("Failed to parse Discord response: {}", e))
                    })?;

                    message_ids.push(msg.id);
                    continue;
                }

                return Err(GatewayError::SendFailed(format!(
                    "Discord API error: {}",
                    text
                )));
            }

            let msg: DiscordMessage = resp.json().await.map_err(|e| {
                GatewayError::SendFailed(format!("Failed to parse Discord response: {}", e))
            })?;

            message_ids.push(msg.id);
        }

        Ok(message_ids)
    }

    /// Create a Discord forum post thread from message content.
    ///
    /// Follow-up chunks are sent to the created thread. If the starter post is
    /// created but a follow-up chunk fails, the successful starter message is
    /// returned together with warnings, matching the upstream partial-send
    /// behavior.
    pub async fn send_forum_post(
        &self,
        forum_channel_id: &str,
        content: &str,
        auto_archive_duration: Option<u32>,
    ) -> Result<DiscordForumSendOutcome, GatewayError> {
        let chunks = split_message(content, MAX_MESSAGE_LENGTH);
        let Some(first_chunk) = chunks.first() else {
            return Err(GatewayError::SendFailed(
                "Discord forum post requires content".into(),
            ));
        };
        let url = format!("{}/channels/{}/threads", DISCORD_API_BASE, forum_channel_id);
        let body = forum_thread_payload(first_chunk, None, auto_archive_duration);

        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord forum post failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord forum post API error: {}",
                text
            )));
        }

        let thread: DiscordForumThread = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to parse forum thread response: {}", e))
        })?;
        let message_id = thread
            .message
            .as_ref()
            .map(|message| message.id.clone())
            .unwrap_or_else(|| thread.id.clone());
        let mut warnings = Vec::new();

        for chunk in chunks.iter().skip(1) {
            let metadata = DiscordSendMetadata::with_thread_id(thread.id.clone());
            if let Err(err) = self
                .send_text_with_metadata(forum_channel_id, chunk, Some(&metadata))
                .await
            {
                warnings.push(err.to_string());
            }
        }

        Ok(DiscordForumSendOutcome {
            thread_id: thread.id,
            message_id,
            warnings,
        })
    }

    /// Edit an existing message in a Discord channel.
    pub async fn edit_text(
        &self,
        channel_id: &str,
        message_id: &str,
        content: &str,
    ) -> Result<(), GatewayError> {
        let url = format!(
            "{}/channels/{}/messages/{}",
            DISCORD_API_BASE, channel_id, message_id
        );

        let body = with_default_allowed_mentions(serde_json::json!({
            "content": &content[..content.len().min(MAX_MESSAGE_LENGTH)],
        }));

        let resp = self
            .client
            .patch(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord edit failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord edit API error: {}",
                text
            )));
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // REST API: Embeds
    // -----------------------------------------------------------------------

    /// Send a message with one or more embeds to a Discord channel.
    pub async fn send_embed(
        &self,
        channel_id: &str,
        content: Option<&str>,
        embeds: &[DiscordEmbed],
    ) -> Result<String, GatewayError> {
        self.send_embed_with_metadata(channel_id, content, embeds, None)
            .await
    }

    /// Send embeds, honoring Discord thread routing metadata when present.
    pub async fn send_embed_with_metadata(
        &self,
        channel_id: &str,
        content: Option<&str>,
        embeds: &[DiscordEmbed],
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<String, GatewayError> {
        let target_channel_id = target_channel_id_for_metadata(channel_id, metadata);
        let url = format!(
            "{}/channels/{}/messages",
            DISCORD_API_BASE, target_channel_id
        );

        let mut body = with_default_allowed_mentions(serde_json::json!({ "embeds": embeds }));
        if let Some(text) = content {
            body["content"] = serde_json::Value::String(text.to_string());
        }

        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord embed send failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord embed API error: {}",
                text
            )));
        }

        let msg: DiscordMessage = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to parse Discord response: {}", e))
        })?;

        Ok(msg.id)
    }

    // -----------------------------------------------------------------------
    // REST API: File uploads
    // -----------------------------------------------------------------------

    /// Upload a file to a Discord channel using multipart form data.
    pub async fn upload_file(
        &self,
        channel_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<String, GatewayError> {
        self.upload_file_with_metadata(channel_id, file_path, caption, None)
            .await
    }

    /// Upload a file, honoring Discord thread routing metadata when present.
    pub async fn upload_file_with_metadata(
        &self,
        channel_id: &str,
        file_path: &str,
        caption: Option<&str>,
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<String, GatewayError> {
        let target_channel_id = target_channel_id_for_metadata(channel_id, metadata);
        let url = format!(
            "{}/channels/{}/messages",
            DISCORD_API_BASE, target_channel_id
        );

        let file_bytes = tokio::fs::read(file_path).await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to read file {}: {}", file_path, e))
        })?;

        let file_name = std::path::Path::new(file_path)
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("file")
            .to_string();

        let part = reqwest::multipart::Part::bytes(file_bytes).file_name(file_name);

        let mut form = reqwest::multipart::Form::new().part("files[0]", part);

        let payload = with_default_allowed_mentions(match caption {
            Some(cap) => serde_json::json!({ "content": cap }),
            None => serde_json::json!({}),
        });
        form = form.text("payload_json", payload.to_string());

        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .multipart(form)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord file upload failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord file upload API error: {}",
                text
            )));
        }

        let msg: DiscordMessage = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to parse Discord response: {}", e))
        })?;

        Ok(msg.id)
    }

    /// Send a local image file as a Discord attachment.
    pub async fn send_image_file(
        &self,
        channel_id: &str,
        image_path: &str,
        caption: Option<&str>,
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<String, GatewayError> {
        self.upload_file_with_metadata(channel_id, image_path, caption, metadata)
            .await
    }

    /// Send an image URL as a Discord embed.
    pub async fn send_image(
        &self,
        channel_id: &str,
        image_url: &str,
        caption: Option<&str>,
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<String, GatewayError> {
        self.send_image_url_with_metadata(channel_id, image_url, caption, metadata)
            .await
    }

    /// Send a voice/audio file as a Discord attachment.
    pub async fn send_voice(
        &self,
        channel_id: &str,
        audio_path: &str,
        caption: Option<&str>,
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<String, GatewayError> {
        self.upload_file_with_metadata(channel_id, audio_path, caption, metadata)
            .await
    }

    /// Send an image URL as an embed, honoring thread routing metadata.
    pub async fn send_image_url_with_metadata(
        &self,
        channel_id: &str,
        image_url: &str,
        caption: Option<&str>,
        metadata: Option<&DiscordSendMetadata>,
    ) -> Result<String, GatewayError> {
        let mut embed = DiscordEmbed::new();
        embed.image = Some(EmbedMedia {
            url: image_url.to_string(),
        });
        self.send_embed_with_metadata(channel_id, caption, &[embed], metadata)
            .await
    }

    // -----------------------------------------------------------------------
    // REST API: Reactions
    // -----------------------------------------------------------------------

    /// Add a reaction to a message.
    ///
    /// `emoji` should be a URL-encoded unicode emoji (e.g. `%F0%9F%91%8D`)
    /// or a custom emoji in the form `name:id`.
    pub async fn add_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        let url = format!(
            "{}/channels/{}/messages/{}/reactions/{}/@me",
            DISCORD_API_BASE, channel_id, message_id, emoji
        );

        let resp = self
            .client
            .put(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Length", "0")
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Discord add_reaction failed: {}", e)))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord add_reaction API error: {}",
                text
            )));
        }

        Ok(())
    }

    /// Remove the bot's own reaction from a message.
    pub async fn remove_reaction(
        &self,
        channel_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        let url = format!(
            "{}/channels/{}/messages/{}/reactions/{}/@me",
            DISCORD_API_BASE, channel_id, message_id, emoji
        );

        let resp = self
            .client
            .delete(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("Discord remove_reaction failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord remove_reaction API error: {}",
                text
            )));
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // REST API: Threads
    // -----------------------------------------------------------------------

    /// Create a public thread from an existing message.
    pub async fn create_thread(
        &self,
        channel_id: &str,
        message_id: &str,
        name: &str,
        auto_archive_duration: Option<u32>,
    ) -> Result<DiscordThread, GatewayError> {
        let url = format!(
            "{}/channels/{}/messages/{}/threads",
            DISCORD_API_BASE, channel_id, message_id
        );

        let mut body = serde_json::json!({ "name": name });
        if let Some(dur) = auto_archive_duration {
            body["auto_archive_duration"] = serde_json::Value::Number(dur.into());
        }

        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("Discord create_thread failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord create_thread API error: {}",
                text
            )));
        }

        let thread: DiscordThread = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to parse thread response: {}", e))
        })?;

        Ok(thread)
    }

    // -----------------------------------------------------------------------
    // REST API: Slash command registration
    // -----------------------------------------------------------------------

    /// Register (overwrite) global application commands.
    ///
    /// This uses the bulk-overwrite endpoint which replaces all existing
    /// global commands with the ones provided.
    pub async fn register_slash_commands(
        &self,
        commands: &[SlashCommand],
    ) -> Result<(), GatewayError> {
        let app_id = self.config.application_id.as_deref().ok_or_else(|| {
            GatewayError::Platform("application_id required for slash commands".into())
        })?;

        let url = format!("{}/applications/{}/commands", DISCORD_API_BASE, app_id);

        let resp = self
            .client
            .put(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(commands)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("Discord register_commands failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord register_commands API error: {}",
                text
            )));
        }

        info!("registered {} global slash commands", commands.len());
        Ok(())
    }

    /// Register application commands scoped to a specific guild (faster
    /// propagation, useful during development).
    pub async fn register_guild_slash_commands(
        &self,
        guild_id: &str,
        commands: &[SlashCommand],
    ) -> Result<(), GatewayError> {
        let app_id = self.config.application_id.as_deref().ok_or_else(|| {
            GatewayError::Platform("application_id required for slash commands".into())
        })?;

        let url = format!(
            "{}/applications/{}/guilds/{}/commands",
            DISCORD_API_BASE, app_id, guild_id
        );

        let resp = self
            .client
            .put(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(commands)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("Discord register_guild_commands failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord register_guild_commands API error: {}",
                text
            )));
        }

        info!(
            "registered {} guild slash commands for {}",
            commands.len(),
            guild_id
        );
        Ok(())
    }

    // -----------------------------------------------------------------------
    // REST API: Interaction responses
    // -----------------------------------------------------------------------

    /// Send an initial response to an interaction (slash command, button, etc.).
    pub async fn respond_to_interaction(
        &self,
        interaction_id: &str,
        interaction_token: &str,
        content: &str,
    ) -> Result<(), GatewayError> {
        let url = format!(
            "{}/interactions/{}/{}/callback",
            DISCORD_API_BASE, interaction_id, interaction_token
        );

        let body = serde_json::json!({
            "type": 4, // CHANNEL_MESSAGE_WITH_SOURCE
            "data": { "content": content }
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("Discord interaction response failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord interaction response API error: {}",
                text
            )));
        }

        Ok(())
    }

    /// Send a deferred response (shows "thinking..." indicator).
    pub async fn defer_interaction(
        &self,
        interaction_id: &str,
        interaction_token: &str,
    ) -> Result<(), GatewayError> {
        let url = format!(
            "{}/interactions/{}/{}/callback",
            DISCORD_API_BASE, interaction_id, interaction_token
        );

        let body = serde_json::json!({
            "type": 5, // DEFERRED_CHANNEL_MESSAGE_WITH_SOURCE
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("Discord defer interaction failed: {}", e))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Discord defer interaction API error: {}",
                text
            )));
        }

        Ok(())
    }

    // -----------------------------------------------------------------------
    // Gateway WebSocket helpers
    // -----------------------------------------------------------------------

    /// Build an IDENTIFY payload for the Discord Gateway.
    pub fn build_identify_payload(&self) -> GatewayPayload {
        GatewayPayload {
            op: opcodes::IDENTIFY,
            d: Some(
                serde_json::to_value(IdentifyData {
                    token: self.config.token.clone(),
                    intents: self.config.intents,
                    properties: IdentifyProperties {
                        os: "linux".into(),
                        browser: "hermes-agent".into(),
                        device: "hermes-agent".into(),
                    },
                })
                .unwrap(),
            ),
            s: None,
            t: None,
        }
    }

    /// Build a HEARTBEAT payload.
    pub fn build_heartbeat_payload(sequence: Option<u64>) -> GatewayPayload {
        GatewayPayload {
            op: opcodes::HEARTBEAT,
            d: sequence.map(|s| serde_json::Value::Number(s.into())),
            s: None,
            t: None,
        }
    }

    /// Build a RESUME payload.
    pub fn build_resume_payload(&self, session_id: &str, seq: u64) -> GatewayPayload {
        GatewayPayload {
            op: opcodes::RESUME,
            d: Some(
                serde_json::to_value(ResumeData {
                    token: self.config.token.clone(),
                    session_id: session_id.to_string(),
                    seq,
                })
                .unwrap(),
            ),
            s: None,
            t: None,
        }
    }

    // -----------------------------------------------------------------------
    // Event parsing
    // -----------------------------------------------------------------------

    /// Parse a MESSAGE_CREATE dispatch event into an IncomingDiscordMessage.
    pub fn parse_message_create(data: &serde_json::Value) -> Option<IncomingDiscordMessage> {
        let channel_id = data.get("channel_id")?.as_str()?.to_string();
        let message_id = data.get("id")?.as_str()?.to_string();
        let content = data
            .get("content")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let author = data.get("author");
        let user_id = author
            .and_then(|a| a.get("id"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let username = author
            .and_then(|a| a.get("username"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let is_bot = author
            .and_then(|a| a.get("bot"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let message_type = data.get("type").and_then(|v| v.as_u64()).unwrap_or(0) as u8;
        let mention_user_ids = data
            .get("mentions")
            .and_then(|v| v.as_array())
            .map(|mentions| {
                mentions
                    .iter()
                    .filter_map(|mention| mention.get("id").and_then(|id| id.as_str()))
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let reply_to_message_id = data
            .get("message_reference")
            .and_then(|reference| reference.get("message_id"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let reply_to_text = data
            .get("referenced_message")
            .and_then(|message| message.get("content"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(String::from);

        Some(IncomingDiscordMessage {
            channel_id,
            message_id,
            user_id,
            username,
            content,
            is_bot,
            message_type,
            mention_user_ids,
            reply_to_message_id,
            reply_to_text,
        })
    }

    /// Apply Discord inbound self/system/bot filtering to a parsed message.
    pub fn should_accept_message(
        message: &IncomingDiscordMessage,
        client_user_id: Option<&str>,
        bot_policy: DiscordBotMessagePolicy,
    ) -> bool {
        if let (Some(author_id), Some(client_id)) = (message.user_id.as_deref(), client_user_id) {
            if author_id.trim() == client_id.trim() {
                return false;
            }
        }

        if !discord_message_type_is_user_visible(message.message_type) {
            return false;
        }

        if !message.is_bot {
            return true;
        }

        match bot_policy {
            DiscordBotMessagePolicy::None => false,
            DiscordBotMessagePolicy::All => true,
            DiscordBotMessagePolicy::Mentions => client_user_id
                .map(|id| message.mentions_user(id))
                .unwrap_or(false),
        }
    }

    /// Parse a MESSAGE_UPDATE dispatch event.
    pub fn parse_message_update(data: &serde_json::Value) -> Option<MessageUpdateEvent> {
        let channel_id = data.get("channel_id")?.as_str()?.to_string();
        let message_id = data.get("id")?.as_str()?.to_string();

        let content = data
            .get("content")
            .and_then(|v| v.as_str())
            .map(String::from);
        let author_id = data
            .get("author")
            .and_then(|a| a.get("id"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let guild_id = data
            .get("guild_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        Some(MessageUpdateEvent {
            channel_id,
            message_id,
            content,
            author_id,
            guild_id,
        })
    }

    /// Parse an INTERACTION_CREATE dispatch event.
    pub fn parse_interaction_create(data: &serde_json::Value) -> Option<InteractionData> {
        let id = data.get("id")?.as_str()?.to_string();
        let application_id = data.get("application_id")?.as_str()?.to_string();
        let interaction_type = data.get("type")?.as_u64()? as u8;
        let token = data.get("token")?.as_str()?.to_string();

        let channel_id = data
            .get("channel_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let guild_id = data
            .get("guild_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        // User ID can be in `member.user.id` (guild) or `user.id` (DM).
        let user_id = data
            .get("member")
            .and_then(|m| m.get("user"))
            .and_then(|u| u.get("id"))
            .and_then(|v| v.as_str())
            .or_else(|| {
                data.get("user")
                    .and_then(|u| u.get("id"))
                    .and_then(|v| v.as_str())
            })
            .map(String::from);

        let cmd_data = data.get("data");
        let command_name = cmd_data
            .and_then(|d| d.get("name"))
            .and_then(|v| v.as_str())
            .map(String::from);

        let command_options = cmd_data
            .and_then(|d| d.get("options"))
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|opt| {
                        let name = opt.get("name")?.as_str()?.to_string();
                        let value = opt.get("value").cloned().unwrap_or(serde_json::Value::Null);
                        Some(InteractionOption { name, value })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Some(InteractionData {
            id,
            application_id,
            interaction_type,
            token,
            channel_id,
            guild_id,
            user_id,
            command_name,
            command_options,
        })
    }

    /// Parse a MESSAGE_REACTION_ADD or MESSAGE_REACTION_REMOVE event.
    pub fn parse_reaction_event(data: &serde_json::Value) -> Option<ReactionEvent> {
        let user_id = data.get("user_id")?.as_str()?.to_string();
        let channel_id = data.get("channel_id")?.as_str()?.to_string();
        let message_id = data.get("message_id")?.as_str()?.to_string();

        let guild_id = data
            .get("guild_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        let emoji = data.get("emoji");
        let emoji_name = emoji
            .and_then(|e| e.get("name"))
            .and_then(|v| v.as_str())
            .map(String::from);
        let emoji_id = emoji
            .and_then(|e| e.get("id"))
            .and_then(|v| v.as_str())
            .map(String::from);

        Some(ReactionEvent {
            user_id,
            channel_id,
            message_id,
            guild_id,
            emoji_name,
            emoji_id,
        })
    }

    /// Parse a VOICE_STATE_UPDATE event.
    pub fn parse_voice_state_update(data: &serde_json::Value) -> Option<VoiceState> {
        let user_id = data.get("user_id")?.as_str()?.to_string();
        let session_id = data.get("session_id")?.as_str()?.to_string();

        let guild_id = data
            .get("guild_id")
            .and_then(|v| v.as_str())
            .map(String::from);
        let channel_id = data
            .get("channel_id")
            .and_then(|v| v.as_str())
            .map(String::from);

        let deaf = data.get("deaf").and_then(|v| v.as_bool()).unwrap_or(false);
        let mute = data.get("mute").and_then(|v| v.as_bool()).unwrap_or(false);
        let self_deaf = data
            .get("self_deaf")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let self_mute = data
            .get("self_mute")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let suppress = data
            .get("suppress")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        Some(VoiceState {
            guild_id,
            channel_id,
            user_id,
            session_id,
            deaf,
            mute,
            self_deaf,
            self_mute,
            suppress,
        })
    }

    /// Route a dispatch event by name to the appropriate parser.
    ///
    /// Returns a [`DispatchEvent`] for known event types, or `None`.
    pub fn parse_dispatch(event_name: &str, data: &serde_json::Value) -> Option<DispatchEvent> {
        match event_name {
            "MESSAGE_CREATE" => Self::parse_message_create(data).map(DispatchEvent::MessageCreate),
            "MESSAGE_UPDATE" => Self::parse_message_update(data).map(DispatchEvent::MessageUpdate),
            "INTERACTION_CREATE" => {
                Self::parse_interaction_create(data).map(DispatchEvent::InteractionCreate)
            }
            "MESSAGE_REACTION_ADD" => {
                Self::parse_reaction_event(data).map(DispatchEvent::ReactionAdd)
            }
            "MESSAGE_REACTION_REMOVE" => {
                Self::parse_reaction_event(data).map(DispatchEvent::ReactionRemove)
            }
            "VOICE_STATE_UPDATE" => {
                Self::parse_voice_state_update(data).map(DispatchEvent::VoiceStateUpdate)
            }
            _ => None,
        }
    }
}

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
mod tests {
    use super::*;

    // -- existing tests (preserved) -----------------------------------------

    fn test_config() -> DiscordConfig {
        DiscordConfig {
            token: "test-token".into(),
            application_id: None,
            proxy: AdapterProxyConfig::default(),
            require_mention: false,
            intents: default_intents(),
            reply_to_mode: default_reply_to_mode(),
            channel_controls: DiscordChannelControls::default(),
            channel_skill_bindings: Vec::new(),
        }
    }

    #[test]
    fn send_metadata_targets_thread_id_when_present() {
        let metadata = DiscordSendMetadata::with_thread_id(" 987654321 ");
        assert_eq!(metadata.target_channel_id("123"), "987654321");
        assert_eq!(metadata.reply_to_message_id(), None);

        let blank_metadata = DiscordSendMetadata::with_thread_id("   ");
        assert_eq!(blank_metadata.target_channel_id("123"), "123");
        assert_eq!(target_channel_id_for_metadata("123", None), "123");

        let reply_metadata = DiscordSendMetadata::with_reply_to_message_id(" origin-1 ");
        assert_eq!(reply_metadata.target_channel_id("123"), "123");
        assert_eq!(reply_metadata.reply_to_message_id(), Some("origin-1"));

        let combined = DiscordSendMetadata::with_thread_and_reply("thread-1", "origin-2");
        assert_eq!(combined.target_channel_id("123"), "thread-1");
        assert_eq!(combined.reply_to_message_id(), Some("origin-2"));
    }

    #[test]
    fn allowed_mentions_safe_defaults_block_broad_pings() {
        let mentions = discord_allowed_mentions_from_lookup(|_| None);
        assert_eq!(mentions.parse, vec!["users".to_string()]);
        assert!(mentions.replied_user);

        let body = with_allowed_mentions(serde_json::json!({ "content": "hello" }), mentions);
        assert_eq!(
            body["allowed_mentions"],
            serde_json::json!({ "parse": ["users"], "replied_user": true })
        );
    }

    #[test]
    fn reply_to_mode_defaults_to_first_and_parses_effective_behavior() {
        assert_eq!(default_reply_to_mode(), "first");
        assert_eq!(DiscordReplyToMode::parse(None), DiscordReplyToMode::First);
        assert_eq!(
            DiscordReplyToMode::parse(Some("")),
            DiscordReplyToMode::First
        );
        assert_eq!(
            DiscordReplyToMode::parse(Some("off")),
            DiscordReplyToMode::Off
        );
        assert_eq!(
            DiscordReplyToMode::parse(Some("ALL")),
            DiscordReplyToMode::All
        );
        assert_eq!(
            DiscordReplyToMode::parse(Some("banana")),
            DiscordReplyToMode::First
        );

        assert!(!DiscordReplyToMode::Off.references_chunk(0));
        assert!(DiscordReplyToMode::First.references_chunk(0));
        assert!(!DiscordReplyToMode::First.references_chunk(1));
        assert!(DiscordReplyToMode::All.references_chunk(0));
        assert!(DiscordReplyToMode::All.references_chunk(7));
    }

    #[test]
    fn reply_reference_body_matches_discord_reference_contract() {
        let body = discord_message_body(
            "chunk",
            Some(" origin-1 "),
            DiscordAllowedMentions::from_flags(false, false, true, true),
        );

        assert_eq!(body["content"], "chunk");
        assert_eq!(
            body["allowed_mentions"],
            serde_json::json!({ "parse": ["users"], "replied_user": true })
        );
        assert_eq!(
            body["message_reference"],
            serde_json::json!({
                "message_id": "origin-1",
                "fail_if_not_exists": false
            })
        );

        let no_reference = discord_message_body(
            "chunk",
            None,
            DiscordAllowedMentions::from_flags(false, false, true, true),
        );
        assert!(no_reference.get("message_reference").is_none());
    }

    #[test]
    fn reply_reference_retry_classifier_only_matches_reference_failures() {
        assert!(discord_reply_reference_error_allows_retry(
            "400 Bad Request (error code: 50035): Invalid Form Body\nIn message_reference: Cannot reply to a system message"
        ));
        assert!(discord_reply_reference_error_allows_retry(
            "400 Bad Request (error code: 10008): Unknown Message"
        ));
        assert!(!discord_reply_reference_error_allows_retry(
            "403 Forbidden (error code: 50013): Missing Permissions"
        ));
    }

    #[test]
    fn forum_parent_and_payload_contract_matches_python_send_path() {
        assert!(!discord_channel_type_is_forum_parent(None));
        assert!(!discord_channel_type_is_forum_parent(Some(0)));
        assert!(!discord_channel_type_is_forum_parent(Some(11)));
        assert!(discord_channel_type_is_forum_parent(Some(15)));

        assert_eq!(
            forum_thread_name(Some("  here is a photo\nsecond line"), Some("photo.png")),
            "here is a photo"
        );
        assert_eq!(forum_thread_name(Some(""), Some("voice.ogg")), "voice.ogg");
        assert_eq!(forum_thread_name(None, None), "Hermes");

        let payload = forum_thread_payload("Hello forum!", None, Some(60));
        assert_eq!(payload["name"], "Hello forum!");
        assert_eq!(payload["auto_archive_duration"], 60);
        assert_eq!(payload["message"]["content"], "Hello forum!");
        assert_eq!(
            payload["message"]["allowed_mentions"],
            serde_json::json!({ "parse": ["users"], "replied_user": true })
        );
    }

    #[test]
    fn allowed_mentions_env_style_knobs_parse_like_upstream() {
        let mentions = discord_allowed_mentions_from_lookup(|name| match name {
            DISCORD_ALLOW_MENTION_EVERYONE_ENV => Some(" true ".to_string()),
            DISCORD_ALLOW_MENTION_ROLES_ENV => Some("YES".to_string()),
            DISCORD_ALLOW_MENTION_USERS_ENV => Some("false".to_string()),
            DISCORD_ALLOW_MENTION_REPLIED_USER_ENV => Some("0".to_string()),
            _ => None,
        });

        assert_eq!(
            mentions,
            DiscordAllowedMentions::from_flags(true, true, false, false)
        );
    }

    #[test]
    fn allowed_mentions_boolean_parser_falls_back_for_empty_or_unknown_values() {
        for raw in ["true", "True", "1", "yes", "on", " true "] {
            assert!(parse_allowed_mention_bool(raw, false));
        }
        for raw in ["false", "False", "0", "no", "off"] {
            assert!(!parse_allowed_mention_bool(raw, true));
        }

        assert!(!parse_allowed_mention_bool("", false));
        assert!(parse_allowed_mention_bool("", true));
        assert!(!parse_allowed_mention_bool("garbage", false));
        assert!(parse_allowed_mention_bool("garbage", true));
    }

    #[test]
    fn bot_message_policy_defaults_to_none_and_parses_case_insensitively() {
        assert_eq!(
            DiscordBotMessagePolicy::parse(None),
            DiscordBotMessagePolicy::None
        );
        assert_eq!(
            DiscordBotMessagePolicy::parse(Some(" ALL ")),
            DiscordBotMessagePolicy::All
        );
        assert_eq!(
            DiscordBotMessagePolicy::parse(Some("mentions")),
            DiscordBotMessagePolicy::Mentions
        );
        assert_eq!(
            DiscordBotMessagePolicy::parse(Some("banana")),
            DiscordBotMessagePolicy::None
        );
        assert_eq!(
            DiscordBotMessagePolicy::from_lookup(|name| {
                (name == DISCORD_ALLOW_BOTS_ENV).then(|| "Mentions".to_string())
            }),
            DiscordBotMessagePolicy::Mentions
        );
        assert!(!DiscordBotMessagePolicy::None.bypasses_gateway_allowlist());
        assert!(DiscordBotMessagePolicy::Mentions.bypasses_gateway_allowlist());
        assert!(DiscordBotMessagePolicy::All.bypasses_gateway_allowlist());
    }

    #[test]
    fn bot_message_filter_matches_upstream_contract() {
        let human = IncomingDiscordMessage {
            channel_id: "channel".into(),
            message_id: "message".into(),
            user_id: Some("human".into()),
            username: Some("Jezza".into()),
            content: "hello".into(),
            is_bot: false,
            message_type: 0,
            mention_user_ids: Vec::new(),
            reply_to_message_id: None,
            reply_to_text: None,
        };
        assert!(DiscordAdapter::should_accept_message(
            &human,
            Some("self"),
            DiscordBotMessagePolicy::None
        ));

        let bot = IncomingDiscordMessage {
            is_bot: true,
            user_id: Some("bot".into()),
            username: Some("Worker".into()),
            mention_user_ids: vec!["self".into()],
            ..human.clone()
        };
        assert!(!DiscordAdapter::should_accept_message(
            &bot,
            Some("self"),
            DiscordBotMessagePolicy::None
        ));
        assert!(DiscordAdapter::should_accept_message(
            &bot,
            Some("self"),
            DiscordBotMessagePolicy::All
        ));
        assert!(DiscordAdapter::should_accept_message(
            &bot,
            Some("self"),
            DiscordBotMessagePolicy::Mentions
        ));

        let unmentioned_bot = IncomingDiscordMessage {
            mention_user_ids: vec!["someone-else".into()],
            ..bot.clone()
        };
        assert!(!DiscordAdapter::should_accept_message(
            &unmentioned_bot,
            Some("self"),
            DiscordBotMessagePolicy::Mentions
        ));

        let own_message = IncomingDiscordMessage {
            user_id: Some("self".into()),
            is_bot: true,
            ..bot
        };
        assert!(!DiscordAdapter::should_accept_message(
            &own_message,
            Some("self"),
            DiscordBotMessagePolicy::All
        ));
    }

    #[test]
    fn system_message_filter_only_accepts_default_and_reply() {
        let mut msg = IncomingDiscordMessage {
            channel_id: "channel".into(),
            message_id: "message".into(),
            user_id: Some("human".into()),
            username: Some("Jezza".into()),
            content: "hello".into(),
            is_bot: false,
            message_type: 0,
            mention_user_ids: Vec::new(),
            reply_to_message_id: None,
            reply_to_text: None,
        };
        assert!(DiscordAdapter::should_accept_message(
            &msg,
            Some("self"),
            DiscordBotMessagePolicy::None
        ));
        msg.message_type = 19;
        assert!(DiscordAdapter::should_accept_message(
            &msg,
            Some("self"),
            DiscordBotMessagePolicy::None
        ));
        for system_type in [1, 6, 7, 8] {
            msg.message_type = system_type;
            assert!(!DiscordAdapter::should_accept_message(
                &msg,
                Some("self"),
                DiscordBotMessagePolicy::None
            ));
        }
    }

    #[test]
    fn discord_reactions_default_enabled_and_false_values_disable() {
        assert!(discord_reactions_enabled_from_raw(None));
        assert!(discord_reactions_enabled_from_raw(Some("")));
        assert!(discord_reactions_enabled_from_raw(Some("yes")));
        assert!(!discord_reactions_enabled_from_raw(Some("false")));
        assert!(!discord_reactions_enabled_from_raw(Some("0")));
        assert!(!discord_reactions_enabled_from_raw(Some("off")));
    }

    #[test]
    fn discord_channel_controls_parse_csv_and_yaml_shapes() {
        let mut extra = std::collections::HashMap::new();
        extra.insert(
            "ignored_channels".into(),
            serde_json::json!("500, 600 ,700"),
        );
        extra.insert("no_thread_channels".into(), serde_json::json!(["800", 900]));
        extra.insert("free_response_channels".into(), serde_json::json!(1000));
        extra.insert("auto_thread".into(), serde_json::json!("false"));
        extra.insert("thread_require_mention".into(), serde_json::json!("yes"));

        let controls = DiscordChannelControls::from_extra(&extra);
        assert_eq!(
            controls.ignored_channels,
            ["500", "600", "700"]
                .into_iter()
                .map(String::from)
                .collect()
        );
        assert_eq!(
            controls.no_thread_channels,
            ["800", "900"].into_iter().map(String::from).collect()
        );
        assert_eq!(
            controls.free_response_channels,
            ["1000"].into_iter().map(String::from).collect()
        );
        assert!(!controls.auto_thread);
        assert!(controls.thread_require_mention);
    }

    #[test]
    fn discord_channel_controls_ignore_server_channels_and_thread_parents() {
        let controls = DiscordChannelControls {
            ignored_channels: ["500"].into_iter().map(String::from).collect(),
            ..DiscordChannelControls::default()
        };

        assert!(controls.is_ignored(&DiscordChannelContext::server("500")));
        assert!(controls.is_ignored(&DiscordChannelContext::thread("501", "500")));
        assert!(!controls.is_ignored(&DiscordChannelContext::server("700")));
        assert!(!controls.is_ignored(&DiscordChannelContext::dm("500")));
    }

    #[test]
    fn discord_channel_controls_auto_thread_policy_matches_upstream_cases() {
        let controls = DiscordChannelControls {
            no_thread_channels: ["800"].into_iter().map(String::from).collect(),
            free_response_channels: ["900"].into_iter().map(String::from).collect(),
            ..DiscordChannelControls::default()
        };

        assert!(!controls.should_auto_thread(&DiscordChannelContext::server("800")));
        assert!(!controls.should_auto_thread(&DiscordChannelContext::thread("801", "800")));
        assert!(!controls.should_auto_thread(&DiscordChannelContext::server("900")));
        assert!(!controls.should_auto_thread(&DiscordChannelContext::dm("700")));
        let mut reply = DiscordChannelContext::server("700");
        reply.is_reply = true;
        assert!(!controls.should_auto_thread(&reply));
        assert!(controls.should_auto_thread(&DiscordChannelContext::server("700")));

        let disabled = DiscordChannelControls {
            auto_thread: false,
            no_thread_channels: ["800"].into_iter().map(String::from).collect(),
            ..DiscordChannelControls::default()
        };
        assert!(!disabled.should_auto_thread(&DiscordChannelContext::server("700")));
        assert!(!disabled.should_auto_thread(&DiscordChannelContext::server("800")));
    }

    #[test]
    fn discord_channel_controls_honor_wildcard_lists() {
        let ignored = DiscordChannelControls {
            ignored_channels: ["*"].into_iter().map(String::from).collect(),
            ..DiscordChannelControls::default()
        };
        assert!(ignored.is_ignored(&DiscordChannelContext::server("700")));
        assert!(ignored.is_ignored(&DiscordChannelContext::thread("701", "700")));
        assert!(!ignored.is_ignored(&DiscordChannelContext::dm("700")));

        let free = DiscordChannelControls {
            free_response_channels: ["*"].into_iter().map(String::from).collect(),
            ..DiscordChannelControls::default()
        };
        assert!(free.allows_free_response(&DiscordChannelContext::server("900")));
        assert!(free.allows_free_response(&DiscordChannelContext::thread("901", "900")));
        assert!(!free.should_auto_thread(&DiscordChannelContext::server("900")));
    }

    #[test]
    fn discord_component_auth_user_or_role_matches_and_fails_closed() {
        let policy = DiscordInteractionAuthPolicy {
            allowed_user_ids: ["11111"].into_iter().map(String::from).collect(),
            allowed_role_ids: ["42"].into_iter().map(String::from).collect(),
            ..DiscordInteractionAuthPolicy::default()
        };

        assert!(policy.component_allows(&DiscordInteractionSubject::user("11111")));
        assert!(policy.component_allows(&DiscordInteractionSubject {
            user_id: Some("99999".into()),
            role_ids: ["42"].into_iter().map(String::from).collect(),
            role_guild_id: None,
        }));
        assert!(!policy.component_allows(&DiscordInteractionSubject {
            user_id: Some("99999".into()),
            role_ids: ["7"].into_iter().map(String::from).collect(),
            role_guild_id: None,
        }));
        assert!(!policy.component_allows(&DiscordInteractionSubject::default()));
        assert!(DiscordInteractionAuthPolicy::default()
            .component_allows(&DiscordInteractionSubject::default()));
    }

    #[test]
    fn discord_slash_auth_matches_channel_and_identity_policy() {
        let policy = DiscordInteractionAuthPolicy {
            allowed_user_ids: ["100200300"].into_iter().map(String::from).collect(),
            allowed_channels: ["1111", "2222"].into_iter().map(String::from).collect(),
            ignored_channels: ["9999"].into_iter().map(String::from).collect(),
            ..DiscordInteractionAuthPolicy::default()
        };
        let subject = DiscordInteractionSubject::user("100200300");

        assert_eq!(
            policy.authorize_slash(
                &subject,
                Some(&DiscordChannelContext::server("1111")),
                Some("guild-1"),
                None,
            ),
            DiscordAuthDecision::Allow
        );
        assert_eq!(
            policy.authorize_slash(
                &subject,
                Some(&DiscordChannelContext::server("3333")),
                Some("guild-1"),
                None,
            ),
            DiscordAuthDecision::Deny(DiscordAuthDenyReason::AllowedChannels)
        );
        assert_eq!(
            policy.authorize_slash(
                &subject,
                Some(&DiscordChannelContext::server("9999")),
                Some("guild-1"),
                None,
            ),
            DiscordAuthDecision::Deny(DiscordAuthDenyReason::IgnoredChannels)
        );
        assert_eq!(
            policy.authorize_slash(&subject, None, Some("guild-1"), None),
            DiscordAuthDecision::Deny(DiscordAuthDenyReason::AllowedChannels)
        );
        let identity_only_policy = DiscordInteractionAuthPolicy {
            allowed_user_ids: ["100200300"].into_iter().map(String::from).collect(),
            ..DiscordInteractionAuthPolicy::default()
        };
        assert_eq!(
            identity_only_policy.authorize_slash(
                &DiscordInteractionSubject::user("other"),
                None,
                Some("guild-1"),
                None,
            ),
            DiscordAuthDecision::Deny(DiscordAuthDenyReason::AllowedUsersOrRoles)
        );
        assert_eq!(
            identity_only_policy.authorize_slash(&subject, None, Some("guild-1"), None),
            DiscordAuthDecision::Allow
        );
        assert_eq!(
            policy.authorize_slash(
                &DiscordInteractionSubject::user("other"),
                Some(&DiscordChannelContext::server("1111")),
                Some("guild-1"),
                None,
            ),
            DiscordAuthDecision::Deny(DiscordAuthDenyReason::AllowedUsersOrRoles)
        );
        assert_eq!(
            policy.authorize_slash(
                &DiscordInteractionSubject::default(),
                Some(&DiscordChannelContext::dm("dm-1")),
                None,
                None,
            ),
            DiscordAuthDecision::Deny(DiscordAuthDenyReason::AllowedUsersOrRoles)
        );
    }

    #[test]
    fn discord_slash_auth_scopes_roles_to_origin_guild_or_dm_opt_in() {
        let policy = DiscordInteractionAuthPolicy {
            allowed_role_ids: ["5555"].into_iter().map(String::from).collect(),
            ..DiscordInteractionAuthPolicy::default()
        };
        let foreign_role = DiscordInteractionSubject::member("42", ["5555"], "guild-a");
        let in_scope_role = DiscordInteractionSubject::member("42", ["5555"], "guild-b");

        assert_eq!(
            policy.authorize_slash(
                &foreign_role,
                Some(&DiscordChannelContext::server("9999")),
                Some("guild-b"),
                None,
            ),
            DiscordAuthDecision::Deny(DiscordAuthDenyReason::AllowedUsersOrRoles)
        );
        assert_eq!(
            policy.authorize_slash(
                &in_scope_role,
                Some(&DiscordChannelContext::server("9999")),
                Some("guild-b"),
                None,
            ),
            DiscordAuthDecision::Allow
        );
        assert_eq!(
            policy.authorize_slash(
                &foreign_role,
                Some(&DiscordChannelContext::dm("dm-1")),
                None,
                None,
            ),
            DiscordAuthDecision::Deny(DiscordAuthDenyReason::AllowedUsersOrRoles)
        );
        assert_eq!(
            policy.authorize_slash(
                &foreign_role,
                Some(&DiscordChannelContext::dm("dm-1")),
                None,
                Some("guild-a"),
            ),
            DiscordAuthDecision::Allow
        );
    }

    #[test]
    fn discord_mention_policy_covers_free_response_and_participated_threads() {
        let controls = DiscordChannelControls {
            free_response_channels: ["222"].into_iter().map(String::from).collect(),
            ..DiscordChannelControls::default()
        };
        assert!(!discord_allows_message_without_mention(
            true,
            &controls,
            &DiscordChannelContext::server("111"),
            false,
            false,
        ));
        assert!(discord_allows_message_without_mention(
            true,
            &controls,
            &DiscordChannelContext::server("222"),
            false,
            false,
        ));
        assert!(discord_allows_message_without_mention(
            true,
            &controls,
            &DiscordChannelContext::thread("333", "222"),
            false,
            false,
        ));
        assert!(discord_allows_message_without_mention(
            true,
            &controls,
            &DiscordChannelContext::thread("444", "111"),
            true,
            false,
        ));
        let strict_threads = DiscordChannelControls {
            thread_require_mention: true,
            ..DiscordChannelControls::default()
        };
        assert!(!discord_allows_message_without_mention(
            true,
            &strict_threads,
            &DiscordChannelContext::thread("444", "111"),
            true,
            false,
        ));
        assert!(discord_allows_message_without_mention(
            true,
            &controls,
            &DiscordChannelContext::server("111"),
            false,
            true,
        ));
    }

    #[test]
    fn discord_unauthorized_notify_soft_fail_falls_through() {
        assert!(!discord_notify_result_counts_delivered(Some(false)));
        assert!(discord_notify_result_counts_delivered(Some(true)));
        assert!(discord_notify_result_counts_delivered(None));
    }

    #[test]
    fn discord_skill_slash_auth_gates_autocomplete_and_handler_before_lookup() {
        let policy = DiscordInteractionAuthPolicy {
            allowed_user_ids: ["100200300"].into_iter().map(String::from).collect(),
            ..DiscordInteractionAuthPolicy::default()
        };
        let entries = vec![
            DiscordSkillCommandEntry {
                name: "alpha".into(),
                description: "First skill".into(),
                command_key: "/alpha".into(),
            },
            DiscordSkillCommandEntry {
                name: "beta".into(),
                description: "Search documents".into(),
                command_key: "/beta".into(),
            },
        ];
        let channel = DiscordChannelContext::server("1111");

        let unauthorized = DiscordInteractionSubject::user("999999999");
        assert!(discord_skill_autocomplete_choices(
            &policy,
            &unauthorized,
            Some(&channel),
            Some("guild-1"),
            None,
            &entries,
            ""
        )
        .is_empty());
        assert_eq!(
            discord_skill_command_decision(
                &policy,
                &unauthorized,
                Some(&channel),
                Some("guild-1"),
                None,
                &entries,
                "alpha",
                "extra"
            ),
            DiscordSkillCommandDecision::Unauthorized
        );
        assert_eq!(
            discord_skill_command_decision(
                &policy,
                &unauthorized,
                Some(&channel),
                Some("guild-1"),
                None,
                &entries,
                "definitely-not-a-skill",
                ""
            ),
            DiscordSkillCommandDecision::Unauthorized
        );

        let authorized = DiscordInteractionSubject::user("100200300");
        assert_eq!(
            discord_skill_autocomplete_choices(
                &policy,
                &authorized,
                Some(&channel),
                Some("guild-1"),
                None,
                &entries,
                "doc"
            ),
            vec!["beta".to_string()]
        );
        assert_eq!(
            discord_skill_command_decision(
                &policy,
                &authorized,
                Some(&channel),
                Some("guild-1"),
                None,
                &entries,
                "alpha",
                "extra args"
            ),
            DiscordSkillCommandDecision::Dispatch {
                text: "/alpha extra args".into()
            }
        );
        assert_eq!(
            discord_skill_command_decision(
                &policy,
                &authorized,
                Some(&channel),
                Some("guild-1"),
                None,
                &entries,
                "missing",
                ""
            ),
            DiscordSkillCommandDecision::UnknownSkill {
                requested_name: "missing".into()
            }
        );
    }

    #[test]
    fn discord_connect_policy_matches_members_intent_and_sync_opt_out() {
        assert!(!discord_members_intent_required(["769524422783664158"]));
        assert!(discord_members_intent_required(["abhey-gupta"]));
        assert!(discord_members_intent_required([
            "769524422783664158",
            "abhey-gupta"
        ]));

        assert_eq!(
            discord_client_reentry_action(false),
            DiscordClientReentryAction::ReuseFreshSlot
        );
        assert_eq!(
            discord_client_reentry_action(true),
            DiscordClientReentryAction::ClosePreviousClient
        );

        assert!(!DiscordSlashSyncPolicy::Off.should_register(true));
        assert!(!DiscordSlashSyncPolicy::Bulk.should_register(false));
        assert!(DiscordSlashSyncPolicy::parse(Some("bulk")).should_register(true));
        assert_eq!(
            DiscordSlashSyncPolicy::parse(Some("unknown")),
            DiscordSlashSyncPolicy::Diff
        );
    }

    #[test]
    fn discord_command_sync_plans_diffs_recreates_and_deletes() {
        let desired = vec![
            serde_json::json!({
                "name": "status",
                "description": "Show Hermes status",
                "type": 1,
                "options": [],
                "nsfw": false,
                "dm_permission": true,
                "default_member_permissions": null,
            }),
            serde_json::json!({
                "name": "help",
                "description": "Show available commands",
                "type": 1,
                "options": [],
                "nsfw": false,
                "dm_permission": true,
            }),
            serde_json::json!({
                "name": "metricas",
                "description": "Metrics dashboard",
                "type": 1,
                "options": [],
            }),
            serde_json::json!({
                "name": "admin",
                "description": "Admin-only command",
                "type": 1,
                "options": [],
                "nsfw": true,
                "dm_permission": false,
                "default_member_permissions": "8",
            }),
            serde_json::json!({
                "name": "contexts",
                "description": "Context drift check",
                "type": 1,
                "options": [],
                "contexts": [0, 1, 2],
                "integration_types": [0, 1],
            }),
        ];
        let existing = vec![
            serde_json::json!({
                "id": 11,
                "application_id": 999,
                "name": "status",
                "description": "Show Hermes status",
                "type": 1,
                "options": [],
                "nsfw": false,
                "dm_permission": true,
                "default_member_permissions": null,
                "name_localizations": {},
                "description_localizations": {},
            }),
            serde_json::json!({
                "id": 12,
                "application_id": 999,
                "name": "help",
                "description": "Old help text",
                "type": 1,
                "options": [],
                "nsfw": false,
                "dm_permission": true,
            }),
            serde_json::json!({
                "id": 13,
                "name": "old-command",
                "description": "To be deleted",
                "type": 1,
                "options": [],
            }),
            serde_json::json!({
                "id": 14,
                "name": "admin",
                "description": "Admin-only command",
                "type": 1,
                "options": [],
                "nsfw": true,
                "dm_permission": false,
            }),
            serde_json::json!({
                "id": 15,
                "name": "contexts",
                "description": "Context drift check",
                "type": 1,
                "options": [],
                "contexts": [0],
                "integration_types": [0],
            }),
        ];

        let summary = plan_discord_command_sync(&desired, &existing);

        assert_eq!(summary.total, 5);
        assert_eq!(summary.unchanged, 1);
        assert_eq!(summary.updated, 1);
        assert_eq!(summary.recreated, 2);
        assert_eq!(summary.created, 1);
        assert_eq!(summary.deleted, 1);
        assert!(summary
            .mutations
            .contains(&DiscordCommandSyncMutation::Update {
                name: "help".into()
            }));
        assert!(summary
            .mutations
            .contains(&DiscordCommandSyncMutation::Recreate {
                name: "admin".into()
            }));
        assert!(summary
            .mutations
            .contains(&DiscordCommandSyncMutation::Recreate {
                name: "contexts".into()
            }));
        assert!(summary
            .mutations
            .contains(&DiscordCommandSyncMutation::Create {
                name: "metricas".into()
            }));
        assert!(summary
            .mutations
            .contains(&DiscordCommandSyncMutation::Delete {
                name: "old-command".into()
            }));
    }

    #[test]
    fn discord_command_sync_state_skips_same_fingerprint_and_honors_retry_after() {
        let commands = vec![serde_json::json!({
            "name": "status",
            "description": "Show Hermes status",
            "type": 1,
            "options": [],
        })];
        let fingerprint = discord_command_fingerprint(&commands);
        let mut state = DiscordCommandSyncStateEntry::default();

        assert!(state.should_attempt(&fingerprint, 10));
        state.record_attempt(10);
        state.record_success(fingerprint.clone(), 11);
        assert!(!state.should_attempt(&fingerprint, 12));

        let changed = discord_command_fingerprint(&[serde_json::json!({
            "name": "status",
            "description": "Show current Hermes status",
            "type": 1,
            "options": [],
        })]);
        assert!(state.should_attempt(&changed, 13));
        state.record_rate_limit(123, 20);
        assert!(!state.should_attempt(&changed, 100));
        assert!(state.should_attempt(&changed, 144));
        assert_eq!(state.retry_after, Some(123));
        assert_eq!(state.retry_after_until, Some(143));
    }

    #[test]
    fn discord_channel_prompt_and_model_picker_contracts_match_python_order() {
        let prompts = BTreeMap::from([
            ("200".to_string(), "Parent prompt".to_string()),
            ("999".to_string(), "Thread prompt".to_string()),
            ("blank".to_string(), "   ".to_string()),
        ]);

        assert_eq!(
            discord_resolve_channel_prompt(&prompts, "999", Some("200")),
            Some("Thread prompt")
        );
        assert_eq!(
            discord_resolve_channel_prompt(&prompts, "123", Some("200")),
            Some("Parent prompt")
        );
        assert_eq!(
            discord_resolve_channel_prompt(&prompts, "blank", None),
            None
        );
        assert_eq!(
            discord_compose_ephemeral_system_prompt(
                Some("Context prompt"),
                Some("Channel prompt"),
                Some("Global prompt"),
            )
            .as_deref(),
            Some("Context prompt\n\nChannel prompt\n\nGlobal prompt")
        );

        let (initial, final_edit) = discord_model_picker_switch_edits("gpt-5.4", "Model switched");
        assert_eq!(initial.title, "Switching Model");
        assert_eq!(initial.description, "Switching to `gpt-5.4`...");
        assert!(initial.clears_view);
        assert_eq!(final_edit.title, "Model Switched");
        assert_eq!(final_edit.description, "Model switched");
        assert!(final_edit.clears_view);
    }

    #[test]
    fn discord_auto_thread_names_and_feedback_match_slash_contract() {
        assert_eq!(
            discord_auto_thread_name("<@&1490963422786093149> <@555> please help <#123>"),
            "please help"
        );
        assert_eq!(
            discord_auto_thread_name("<@&1490963422786093149>"),
            "Hermes"
        );
        let long_name = discord_auto_thread_name(&"a".repeat(200));
        assert_eq!(long_name.len(), 80);
        assert!(long_name.ends_with("..."));
        assert!(discord_thread_create_success_message("555").contains("<#555>"));
        assert!(discord_thread_create_failure_message("nope").contains("Failed to create thread"));
    }

    #[test]
    fn discord_attachment_document_opus_and_voice_contracts_match_upstream_cases() {
        let image = discord_attachment_handling("file.png", Some("image/png"), 64);
        assert_eq!(image.kind, DiscordAttachmentKind::Image);
        assert!(image.prefer_bot_session_read);
        assert!(image.fallback_uses_ssrf_gate);
        assert!(!image.inject_text_content);

        let audio = discord_attachment_handling("voice.ogg", Some("audio/ogg"), 64);
        assert_eq!(audio.kind, DiscordAttachmentKind::Audio);
        assert!(audio.prefer_bot_session_read);

        let txt = discord_attachment_handling("notes.txt", Some("text/plain"), 1024);
        assert_eq!(txt.kind, DiscordAttachmentKind::Document);
        assert!(txt.prefer_bot_session_read);
        assert!(txt.fallback_uses_ssrf_gate);
        assert!(txt.inject_text_content);
        assert_eq!(
            discord_inject_document_text("summarize this", "notes.txt", "Hello"),
            "[Content of notes.txt]:\nHello\n\nsummarize this"
        );

        let pdf = discord_attachment_handling("report.pdf", Some("application/pdf"), 1024);
        assert_eq!(pdf.kind, DiscordAttachmentKind::Document);
        assert!(!pdf.inject_text_content);

        assert_eq!(
            discord_opus_library_candidates("linux", Some("libopus.so")),
            vec!["libopus.so".to_string()]
        );
        let mac_fallbacks = discord_opus_library_candidates("darwin", None);
        assert!(mac_fallbacks[0].contains("/opt/homebrew"));
        assert!(discord_should_log_opus_decode_error(Some("decode failed")));

        let mut joins = DiscordVoiceJoinTracker::default();
        assert_eq!(joins.begin_join("42"), DiscordVoiceJoinAction::Connect);
        assert_eq!(joins.begin_join("42"), DiscordVoiceJoinAction::MoveExisting);
        joins.complete_join("42", true);
        assert_eq!(
            joins.begin_join("42"),
            DiscordVoiceJoinAction::AlreadyConnected
        );
    }

    #[test]
    fn discord_auto_registration_skips_conflicts_and_dispatches_args() {
        let gateway = vec![
            DiscordSlashRegistrationSpec::new(
                "debug",
                "Generate a debug report",
                None::<String>,
                "/debug",
            ),
            DiscordSlashRegistrationSpec::new(
                "branch",
                "Show or switch branch",
                Some("[name]"),
                "/branch",
            ),
        ];
        let plugins = vec![
            DiscordSlashRegistrationSpec::new(
                "status",
                "Plugin status",
                None::<String>,
                "/status-plugin",
            ),
            DiscordSlashRegistrationSpec::new(
                "metricas",
                "Metrics dashboard",
                Some("dias:7 formato:json"),
                "/metricas",
            ),
        ];

        let registered = discord_auto_registered_commands(["status", "thread"], gateway, plugins);
        let names = registered
            .iter()
            .map(|spec| spec.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(names, vec!["debug", "branch", "metricas"]);
        assert_eq!(registered[0].dispatch_text(None), "/debug");
        assert_eq!(
            registered[1].dispatch_text(Some("my-branch")),
            "/branch my-branch"
        );
        assert_eq!(
            registered[2].dispatch_text(Some("dias:7 formato:json")),
            "/metricas dias:7 formato:json"
        );

        let slash = registered[1].to_slash_command();
        assert_eq!(slash.name, "branch");
        assert_eq!(slash.options.as_ref().map(Vec::len), Some(1));
    }

    #[test]
    fn discord_channel_skill_bindings_resolve_exact_parent_and_deduped_skills() {
        let bindings = DiscordChannelSkillBinding::list_from_json(Some(&serde_json::json!([
            {"id": "100", "skills": ["a", "b", "a", "c", "b"]},
            {"id": "200", "skill": "forum-skill"},
            {"id": 300, "skills": "solo"},
        ])));

        assert_eq!(
            resolve_channel_skills_from_bindings(&bindings, "100", None),
            Some(vec!["a".into(), "b".into(), "c".into()])
        );
        assert_eq!(
            resolve_channel_skills_from_bindings(&bindings, "999", Some("200")),
            Some(vec!["forum-skill".into()])
        );
        assert_eq!(
            resolve_channel_skills_from_bindings(&bindings, "300", None),
            Some(vec!["solo".into()])
        );
        assert_eq!(
            resolve_channel_skills_from_bindings(&bindings, "999", None),
            None
        );
    }

    #[test]
    fn discord_adapter_resolves_configured_channel_skills() {
        let mut cfg = test_config();
        cfg.channel_skill_bindings = DiscordChannelSkillBinding::list_from_json(Some(
            &serde_json::json!([{"id": "100", "skills": ["skill-a", "skill-b"]}]),
        ));
        let adapter = DiscordAdapter::new(cfg).unwrap();
        assert_eq!(
            adapter.resolve_channel_skills("100", None),
            Some(vec!["skill-a".into(), "skill-b".into()])
        );
        assert_eq!(adapter.resolve_channel_skills("101", None), None);
    }

    #[test]
    fn discord_thread_participation_tracker_persists_and_keeps_newest() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("discord_threads.json");
        let mut tracker = DiscordThreadParticipationTracker::from_path(&path, 5);

        assert!(tracker.is_empty());
        assert!(tracker.mark("0").unwrap());
        assert!(!tracker.mark("0").unwrap());
        for id in ["1", "2", "3", "4", "newest"] {
            assert!(tracker.mark(id).unwrap());
        }

        assert_eq!(tracker.entries(), vec!["1", "2", "3", "4", "newest"]);
        let saved: Vec<String> =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(saved, vec!["1", "2", "3", "4", "newest"]);

        let reloaded = DiscordThreadParticipationTracker::from_path(&path, 5);
        assert!(reloaded.contains("newest"));
        assert!(!reloaded.contains("0"));
    }

    #[test]
    fn discord_thread_participation_tracker_tolerates_corrupt_and_missing_state() {
        let tmp = tempfile::tempdir().unwrap();
        let corrupt_path = tmp.path().join("discord_threads.json");
        std::fs::write(&corrupt_path, "not valid json{{{").unwrap();
        let tracker = DiscordThreadParticipationTracker::from_path(&corrupt_path, 5);
        assert!(tracker.is_empty());

        let missing_parent = tmp
            .path()
            .join("missing")
            .join("deep")
            .join("discord_threads.json");
        let mut tracker = DiscordThreadParticipationTracker::from_path(&missing_parent, 5);
        assert!(tracker.mark("111").unwrap());
        assert!(missing_parent.exists());
    }

    #[test]
    fn media_methods_accept_metadata_contract() {
        let adapter = DiscordAdapter::new(test_config()).unwrap();
        let metadata = DiscordSendMetadata::with_thread_id("thread-1");

        let image_file = adapter.send_image_file(
            "channel-1",
            "/tmp/missing-image.png",
            Some("caption"),
            Some(&metadata),
        );
        drop(image_file);

        let image = adapter.send_image(
            "channel-1",
            "https://example.com/image.png",
            Some("caption"),
            Some(&metadata),
        );
        drop(image);

        let voice = adapter.send_voice(
            "channel-1",
            "/tmp/missing-audio.ogg",
            Some("caption"),
            Some(&metadata),
        );
        drop(voice);
    }

    #[test]
    fn split_message_short() {
        let chunks = split_message("hello", 2000);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn split_message_long() {
        let text = "a".repeat(3000);
        let chunks = split_message(&text, 2000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 2000);
        assert_eq!(chunks[1].len(), 1000);
    }

    #[test]
    fn gateway_payload_identify() {
        let adapter = DiscordAdapter::new(test_config()).unwrap();
        let payload = adapter.build_identify_payload();
        assert_eq!(payload.op, opcodes::IDENTIFY);
        assert!(payload.d.is_some());
    }

    #[test]
    fn gateway_payload_heartbeat() {
        let payload = DiscordAdapter::build_heartbeat_payload(Some(42));
        assert_eq!(payload.op, opcodes::HEARTBEAT);
        assert_eq!(payload.d, Some(serde_json::Value::Number(42.into())));
    }

    #[test]
    fn parse_message_create_event() {
        let data = serde_json::json!({
            "id": "msg123",
            "channel_id": "ch456",
            "content": "hello world",
            "type": 19,
            "mentions": [
                { "id": "bot-self", "username": "Hermes" }
            ],
            "message_reference": { "message_id": "origin-1" },
            "referenced_message": { "content": "original message" },
            "author": {
                "id": "user789",
                "username": "testuser",
                "bot": false
            }
        });

        let msg = DiscordAdapter::parse_message_create(&data).unwrap();
        assert_eq!(msg.channel_id, "ch456");
        assert_eq!(msg.message_id, "msg123");
        assert_eq!(msg.content, "hello world");
        assert_eq!(msg.user_id, Some("user789".into()));
        assert_eq!(msg.username, Some("testuser".into()));
        assert!(!msg.is_bot);
        assert_eq!(msg.message_type, 19);
        assert!(msg.mentions_user("bot-self"));
        assert_eq!(msg.reply_to_message_id.as_deref(), Some("origin-1"));
        assert_eq!(msg.reply_to_text.as_deref(), Some("original message"));
    }

    #[test]
    fn parse_message_create_bot() {
        let data = serde_json::json!({
            "id": "msg1",
            "channel_id": "ch1",
            "content": "bot msg",
            "author": { "id": "bot1", "username": "mybot", "bot": true }
        });

        let msg = DiscordAdapter::parse_message_create(&data).unwrap();
        assert!(msg.is_bot);
    }

    // -- GatewaySession tests -----------------------------------------------

    #[test]
    fn session_handles_hello() {
        let mut session = GatewaySession::new();
        let payload = GatewayPayload {
            op: opcodes::HELLO,
            d: Some(serde_json::json!({ "heartbeat_interval": 41250 })),
            s: None,
            t: None,
        };

        let actions = session.handle_gateway_event(&payload);
        assert_eq!(session.heartbeat_interval_ms, Some(41250));
        assert!(actions.contains(&GatewayAction::SendHeartbeat));
        assert!(actions.contains(&GatewayAction::SendIdentify));
    }

    #[test]
    fn session_handles_hello_with_resume() {
        let mut session = GatewaySession::new();
        session.session_id = Some("sess123".into());
        session.sequence = Some(42);

        let payload = GatewayPayload {
            op: opcodes::HELLO,
            d: Some(serde_json::json!({ "heartbeat_interval": 30000 })),
            s: None,
            t: None,
        };

        let actions = session.handle_gateway_event(&payload);
        assert!(actions.contains(&GatewayAction::SendResume));
        assert!(!actions.contains(&GatewayAction::SendIdentify));
    }

    #[test]
    fn session_handles_heartbeat_ack() {
        let mut session = GatewaySession::new();
        session.heartbeat_acknowledged = false;

        let payload = GatewayPayload {
            op: opcodes::HEARTBEAT_ACK,
            d: None,
            s: None,
            t: None,
        };

        session.handle_gateway_event(&payload);
        assert!(session.heartbeat_acknowledged);
    }

    #[test]
    fn session_handles_reconnect() {
        let mut session = GatewaySession::new();
        let payload = GatewayPayload {
            op: opcodes::RECONNECT,
            d: None,
            s: None,
            t: None,
        };

        let actions = session.handle_gateway_event(&payload);
        assert_eq!(actions, vec![GatewayAction::Reconnect]);
    }

    #[test]
    fn session_handles_invalid_session_resumable() {
        let mut session = GatewaySession::new();
        session.session_id = Some("sess".into());
        session.sequence = Some(10);

        let payload = GatewayPayload {
            op: opcodes::INVALID_SESSION,
            d: Some(serde_json::Value::Bool(true)),
            s: None,
            t: None,
        };

        let actions = session.handle_gateway_event(&payload);
        assert_eq!(actions, vec![GatewayAction::InvalidSession(true)]);
        assert!(session.session_id.is_some());
    }

    #[test]
    fn session_handles_invalid_session_not_resumable() {
        let mut session = GatewaySession::new();
        session.session_id = Some("sess".into());
        session.sequence = Some(10);

        let payload = GatewayPayload {
            op: opcodes::INVALID_SESSION,
            d: Some(serde_json::Value::Bool(false)),
            s: None,
            t: None,
        };

        let actions = session.handle_gateway_event(&payload);
        assert_eq!(actions, vec![GatewayAction::InvalidSession(false)]);
        assert!(session.session_id.is_none());
        assert!(session.sequence.is_none());
    }

    #[test]
    fn session_handles_ready_dispatch() {
        let mut session = GatewaySession::new();
        let payload = GatewayPayload {
            op: opcodes::DISPATCH,
            d: Some(serde_json::json!({
                "session_id": "abc123",
                "resume_gateway_url": "wss://resume.discord.gg",
                "user": { "id": "12345", "username": "testbot" }
            })),
            s: Some(1),
            t: Some("READY".into()),
        };

        let actions = session.handle_gateway_event(&payload);
        assert_eq!(session.session_id, Some("abc123".into()));
        assert_eq!(
            session.resume_gateway_url,
            Some("wss://resume.discord.gg".into())
        );
        assert_eq!(session.sequence, Some(1));
        assert!(session.identified);

        assert_eq!(actions.len(), 1);
        match &actions[0] {
            GatewayAction::Dispatch(name, _) => assert_eq!(name, "READY"),
            other => panic!("expected Dispatch, got {:?}", other),
        }
    }

    #[test]
    fn session_tracks_sequence() {
        let mut session = GatewaySession::new();
        let payload = GatewayPayload {
            op: opcodes::DISPATCH,
            d: Some(serde_json::json!({})),
            s: Some(42),
            t: Some("GUILD_CREATE".into()),
        };

        session.handle_gateway_event(&payload);
        assert_eq!(session.sequence, Some(42));
    }

    #[test]
    fn session_zombie_detection() {
        let mut session = GatewaySession::new();
        assert!(!session.is_zombie());

        session.heartbeat_sent();
        assert!(session.is_zombie());

        session.heartbeat_acknowledged = true;
        assert!(!session.is_zombie());
    }

    #[test]
    fn session_reset() {
        let mut session = GatewaySession::new();
        session.session_id = Some("s".into());
        session.sequence = Some(99);
        session.heartbeat_interval_ms = Some(5000);
        session.identified = true;

        session.reset();
        assert!(session.session_id.is_none());
        assert!(session.sequence.is_none());
        assert!(session.heartbeat_interval_ms.is_none());
        assert!(!session.identified);
    }

    #[test]
    fn session_heartbeat_request() {
        let mut session = GatewaySession::new();
        let payload = GatewayPayload {
            op: opcodes::HEARTBEAT,
            d: None,
            s: None,
            t: None,
        };

        let actions = session.handle_gateway_event(&payload);
        assert_eq!(actions, vec![GatewayAction::SendHeartbeat]);
    }

    // -- Event parsing tests ------------------------------------------------

    #[test]
    fn parse_message_update_full() {
        let data = serde_json::json!({
            "id": "msg100",
            "channel_id": "ch200",
            "content": "edited content",
            "author": { "id": "user300" },
            "guild_id": "guild400"
        });

        let evt = DiscordAdapter::parse_message_update(&data).unwrap();
        assert_eq!(evt.message_id, "msg100");
        assert_eq!(evt.channel_id, "ch200");
        assert_eq!(evt.content, Some("edited content".into()));
        assert_eq!(evt.author_id, Some("user300".into()));
        assert_eq!(evt.guild_id, Some("guild400".into()));
    }

    #[test]
    fn parse_message_update_partial() {
        let data = serde_json::json!({
            "id": "msg100",
            "channel_id": "ch200"
        });

        let evt = DiscordAdapter::parse_message_update(&data).unwrap();
        assert!(evt.content.is_none());
        assert!(evt.author_id.is_none());
    }

    #[test]
    fn parse_interaction_create_slash_command() {
        let data = serde_json::json!({
            "id": "int1",
            "application_id": "app1",
            "type": 2,
            "token": "tok1",
            "channel_id": "ch1",
            "guild_id": "g1",
            "member": {
                "user": { "id": "u1" }
            },
            "data": {
                "name": "hello",
                "options": [
                    { "name": "target", "value": "world" },
                    { "name": "count", "value": 3 }
                ]
            }
        });

        let interaction = DiscordAdapter::parse_interaction_create(&data).unwrap();
        assert_eq!(interaction.id, "int1");
        assert_eq!(interaction.interaction_type, 2);
        assert_eq!(interaction.command_name, Some("hello".into()));
        assert_eq!(interaction.user_id, Some("u1".into()));
        assert_eq!(interaction.command_options.len(), 2);
        assert_eq!(interaction.command_options[0].name, "target");
        assert_eq!(
            interaction.command_options[0].value,
            serde_json::json!("world")
        );
        assert_eq!(interaction.command_options[1].name, "count");
        assert_eq!(interaction.command_options[1].value, serde_json::json!(3));
    }

    #[test]
    fn parse_interaction_create_dm() {
        let data = serde_json::json!({
            "id": "int2",
            "application_id": "app2",
            "type": 2,
            "token": "tok2",
            "user": { "id": "dm_user" },
            "data": { "name": "ping" }
        });

        let interaction = DiscordAdapter::parse_interaction_create(&data).unwrap();
        assert_eq!(interaction.user_id, Some("dm_user".into()));
        assert!(interaction.guild_id.is_none());
        assert!(interaction.command_options.is_empty());
    }

    #[test]
    fn parse_reaction_add_event() {
        let data = serde_json::json!({
            "user_id": "u1",
            "channel_id": "ch1",
            "message_id": "msg1",
            "guild_id": "g1",
            "emoji": {
                "name": "\u{1f44d}",
                "id": null
            }
        });

        let evt = DiscordAdapter::parse_reaction_event(&data).unwrap();
        assert_eq!(evt.user_id, "u1");
        assert_eq!(evt.channel_id, "ch1");
        assert_eq!(evt.message_id, "msg1");
        assert_eq!(evt.guild_id, Some("g1".into()));
        assert_eq!(evt.emoji_name, Some("\u{1f44d}".into()));
        assert!(evt.emoji_id.is_none());
    }

    #[test]
    fn parse_reaction_custom_emoji() {
        let data = serde_json::json!({
            "user_id": "u2",
            "channel_id": "ch2",
            "message_id": "msg2",
            "emoji": {
                "name": "custom_emote",
                "id": "12345678"
            }
        });

        let evt = DiscordAdapter::parse_reaction_event(&data).unwrap();
        assert_eq!(evt.emoji_name, Some("custom_emote".into()));
        assert_eq!(evt.emoji_id, Some("12345678".into()));
    }

    #[test]
    fn parse_voice_state_update_event() {
        let data = serde_json::json!({
            "guild_id": "g1",
            "channel_id": "vc1",
            "user_id": "u1",
            "session_id": "sess1",
            "deaf": false,
            "mute": false,
            "self_deaf": true,
            "self_mute": true,
            "suppress": false
        });

        let vs = DiscordAdapter::parse_voice_state_update(&data).unwrap();
        assert_eq!(vs.guild_id, Some("g1".into()));
        assert_eq!(vs.channel_id, Some("vc1".into()));
        assert_eq!(vs.user_id, "u1");
        assert!(!vs.deaf);
        assert!(!vs.mute);
        assert!(vs.self_deaf);
        assert!(vs.self_mute);
        assert!(!vs.suppress);
    }

    #[test]
    fn parse_voice_state_leave() {
        let data = serde_json::json!({
            "guild_id": "g1",
            "channel_id": null,
            "user_id": "u1",
            "session_id": "sess2",
            "deaf": false,
            "mute": false,
            "self_deaf": false,
            "self_mute": false,
            "suppress": false
        });

        let vs = DiscordAdapter::parse_voice_state_update(&data).unwrap();
        assert!(vs.channel_id.is_none());
    }

    // -- Dispatch routing tests ---------------------------------------------

    #[test]
    fn dispatch_routes_message_create() {
        let data = serde_json::json!({
            "id": "m1",
            "channel_id": "c1",
            "content": "hi",
            "author": { "id": "u1", "username": "a", "bot": false }
        });

        let evt = DiscordAdapter::parse_dispatch("MESSAGE_CREATE", &data);
        assert!(matches!(evt, Some(DispatchEvent::MessageCreate(_))));
    }

    #[test]
    fn dispatch_routes_message_update() {
        let data = serde_json::json!({ "id": "m1", "channel_id": "c1" });
        let evt = DiscordAdapter::parse_dispatch("MESSAGE_UPDATE", &data);
        assert!(matches!(evt, Some(DispatchEvent::MessageUpdate(_))));
    }

    #[test]
    fn dispatch_routes_interaction_create() {
        let data = serde_json::json!({
            "id": "i1",
            "application_id": "a1",
            "type": 2,
            "token": "t1",
            "data": { "name": "test" }
        });
        let evt = DiscordAdapter::parse_dispatch("INTERACTION_CREATE", &data);
        assert!(matches!(evt, Some(DispatchEvent::InteractionCreate(_))));
    }

    #[test]
    fn dispatch_routes_reaction_add() {
        let data = serde_json::json!({
            "user_id": "u1",
            "channel_id": "c1",
            "message_id": "m1",
            "emoji": { "name": "x" }
        });
        let evt = DiscordAdapter::parse_dispatch("MESSAGE_REACTION_ADD", &data);
        assert!(matches!(evt, Some(DispatchEvent::ReactionAdd(_))));
    }

    #[test]
    fn dispatch_routes_reaction_remove() {
        let data = serde_json::json!({
            "user_id": "u1",
            "channel_id": "c1",
            "message_id": "m1",
            "emoji": { "name": "x" }
        });
        let evt = DiscordAdapter::parse_dispatch("MESSAGE_REACTION_REMOVE", &data);
        assert!(matches!(evt, Some(DispatchEvent::ReactionRemove(_))));
    }

    #[test]
    fn dispatch_routes_voice_state() {
        let data = serde_json::json!({
            "user_id": "u1",
            "session_id": "s1",
            "deaf": false,
            "mute": false,
            "self_deaf": false,
            "self_mute": false,
            "suppress": false
        });
        let evt = DiscordAdapter::parse_dispatch("VOICE_STATE_UPDATE", &data);
        assert!(matches!(evt, Some(DispatchEvent::VoiceStateUpdate(_))));
    }

    #[test]
    fn dispatch_unknown_event_returns_none() {
        let data = serde_json::json!({});
        let evt = DiscordAdapter::parse_dispatch("UNKNOWN_EVENT", &data);
        assert!(evt.is_none());
    }

    // -- Embed builder tests ------------------------------------------------

    #[test]
    fn embed_builder() {
        let embed = DiscordEmbed::new()
            .with_title("Test Embed")
            .with_description("A description")
            .with_color(0xFF5733)
            .with_footer("footer text")
            .with_timestamp("2026-01-01T00:00:00Z")
            .add_field("Field 1", "Value 1", true)
            .add_field("Field 2", "Value 2", false);

        assert_eq!(embed.title, Some("Test Embed".into()));
        assert_eq!(embed.description, Some("A description".into()));
        assert_eq!(embed.color, Some(0xFF5733));
        assert_eq!(embed.footer.as_ref().unwrap().text, "footer text");
        assert_eq!(embed.timestamp, Some("2026-01-01T00:00:00Z".into()));
        assert_eq!(embed.fields.len(), 2);
        assert_eq!(embed.fields[0].name, "Field 1");
        assert_eq!(embed.fields[0].inline, Some(true));
        assert_eq!(embed.fields[1].inline, Some(false));
    }

    #[test]
    fn embed_serialization() {
        let embed = DiscordEmbed::new().with_title("Hello").with_color(0x00FF00);

        let json = serde_json::to_value(&embed).unwrap();
        assert_eq!(json["title"], "Hello");
        assert_eq!(json["color"], 0x00FF00);
        assert!(json.get("description").is_none());
        assert!(json.get("footer").is_none());
    }

    // -- Slash command serialization tests ----------------------------------

    #[test]
    fn slash_command_serialization() {
        let cmd = SlashCommand {
            name: "greet".into(),
            description: "Say hello".into(),
            default_member_permissions: None,
            dm_permission: None,
            nsfw: None,
            contexts: None,
            integration_types: None,
            command_type: 1,
            options: Some(vec![
                SlashCommandOption {
                    name: "name".into(),
                    description: "Who to greet".into(),
                    option_type: 3, // STRING
                    required: Some(true),
                    choices: None,
                },
                SlashCommandOption {
                    name: "style".into(),
                    description: "Greeting style".into(),
                    option_type: 3,
                    required: Some(false),
                    choices: Some(vec![
                        SlashCommandChoice {
                            name: "Formal".into(),
                            value: serde_json::json!("formal"),
                        },
                        SlashCommandChoice {
                            name: "Casual".into(),
                            value: serde_json::json!("casual"),
                        },
                    ]),
                },
            ]),
        };

        let json = serde_json::to_value(&cmd).unwrap();
        assert_eq!(json["name"], "greet");
        assert_eq!(json["type"], 1);
        let options = json["options"].as_array().unwrap();
        assert_eq!(options.len(), 2);
        assert_eq!(options[0]["required"], true);
        let choices = options[1]["choices"].as_array().unwrap();
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0]["name"], "Formal");
    }

    #[test]
    fn slash_owner_only_visibility_sets_zero_permissions() {
        let mut commands = vec![SlashCommand {
            name: "restart".into(),
            description: "Restart Hermes".into(),
            options: None,
            default_member_permissions: None,
            dm_permission: None,
            nsfw: None,
            contexts: None,
            integration_types: None,
            command_type: 1,
        }];

        apply_owner_only_slash_visibility(&mut commands);

        assert_eq!(commands[0].default_member_permissions.as_deref(), Some("0"));
        let json = serde_json::to_value(&commands[0]).unwrap();
        assert_eq!(json["default_member_permissions"], "0");
    }

    // -- Emoji encoding tests -----------------------------------------------

    #[test]
    fn encode_emoji_unicode() {
        let encoded = encode_emoji("\u{1f44d}");
        assert_eq!(encoded, "%F0%9F%91%8D");
    }

    #[test]
    fn encode_emoji_custom() {
        let encoded = encode_emoji("custom_emote:12345");
        assert_eq!(encoded, "custom_emote:12345");
    }

    // -- Default trait impls ------------------------------------------------

    #[test]
    fn gateway_session_default() {
        let session = GatewaySession::default();
        assert!(session.sequence.is_none());
        assert!(session.session_id.is_none());
        assert!(!session.identified);
        assert!(session.heartbeat_acknowledged);
    }

    #[test]
    fn embed_default() {
        let embed = DiscordEmbed::default();
        assert!(embed.title.is_none());
        assert!(embed.fields.is_empty());
    }
}
