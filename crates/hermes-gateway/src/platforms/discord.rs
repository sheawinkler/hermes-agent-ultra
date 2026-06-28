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

include!("discord/core_config.rs");
include!("discord/history_context.rs");
include!("discord/channel_auth.rs");
include!("discord/command_media.rs");
include!("discord/trackers.rs");
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
