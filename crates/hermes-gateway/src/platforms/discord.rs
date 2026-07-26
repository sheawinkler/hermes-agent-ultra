//! Discord Bot API adapter.
//!
//! Implements the `PlatformAdapter` trait for Discord using the REST API
//! for message operations and the Gateway WebSocket for receiving events.
//! Supports message splitting at 2000 characters, file uploads via multipart
//! form data, embeds, threads, reactions, and Gateway event handling
//! (IDENTIFY, HEARTBEAT, RESUME, READY, and MESSAGE_CREATE).

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use async_trait::async_trait;
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::{debug, error, info, warn};

use hermes_core::errors::{GatewayError, MediaPreflightFailure, PartialDelivery};
use hermes_core::traits::{
    NativeMediaFailureClass, ParseMode, PlatformAdapter, SendMessageOptions,
};

use crate::adapter::{describe_secret, AdapterProxyConfig, BasePlatformAdapter};
use crate::pairing::{PairingManager, PairingState};

/// Maximum message length for Discord (2000 characters).
const MAX_MESSAGE_LENGTH: usize = 2000;

/// Discord API base URL.
const DISCORD_API_BASE: &str = "https://discord.com/api/v10";
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
include!("discord/gateway_driver.rs");

// ---------------------------------------------------------------------------
// Typed dispatch events
// ---------------------------------------------------------------------------

/// A strongly-typed dispatch event produced by [`DiscordAdapter::parse_dispatch`].
#[derive(Debug, Clone)]
pub enum DispatchEvent {
    MessageCreate(IncomingDiscordMessage),
}

// ---------------------------------------------------------------------------
// PlatformAdapter trait impl
// ---------------------------------------------------------------------------

#[async_trait]
impl PlatformAdapter for DiscordAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        let _lifecycle = self.lifecycle.lock().await;
        if !self.gateway_driver.is_reconnectable() {
            self.base.mark_stopped();
            return Err(GatewayError::Platform(
                "Discord Gateway is terminally rejected; recreate the adapter after correcting credentials, intents, or shard configuration"
                    .to_string(),
            ));
        }
        info!(
            "Discord adapter starting (token: {})",
            describe_secret(&self.config.token)
        );
        self.reopen_accepted_state_admission()?;
        self.base.mark_running();
        self.gateway_driver.reset_outbound_stop();
        let mut rollback = DiscordStartupRollback::arm(self);
        self.configure_gateway_identify_session_start_limit().await;
        if let Err(error) = self
            .gateway_driver
            .start_while_adapter_running(&self.base)
            .await
        {
            if let Err(cleanup_error) = self.gateway_driver.stop().await {
                warn!(
                    error = %cleanup_error,
                    "Discord startup rollback could not cleanly drain ingress"
                );
            }
            return Err(error);
        }
        self.start_liveness_probe();
        rollback.disarm();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        let _lifecycle = self.lifecycle.lock().await;
        info!("Discord adapter stopping");
        // Close admission before signalling cancellation: a request that has
        // not yet begun cannot create a remote effect after stop begins.
        self.close_accepted_state_admission();
        self.base.mark_stopped();
        let gateway_result = self.gateway_driver.stop().await;
        self.stop_liveness_probe();
        // Existing admitted operations may now finish the durable tracker
        // update that accompanies their already-accepted REST effect.
        self.wait_for_accepted_state_commits().await;
        let repair_result = match self.flush_tracker_repairs().await {
            Ok(()) => self.shutdown_tracker_writers().await,
            Err(error) => Err(error),
        };
        match repair_result {
            Ok(()) => gateway_result,
            Err(repair_error) => {
                if let Err(gateway_error) = gateway_result {
                    warn!(
                        error = %gateway_error,
                        "Discord gateway stop also failed while reporting undurable accepted state"
                    );
                }
                Err(repair_error)
            }
        }
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

    fn classify_native_media_failure(&self, error: &GatewayError) -> NativeMediaFailureClass {
        match error {
            GatewayError::MediaPreflightFailed { .. }
            | GatewayError::MediaRemoteRejected { .. } => NativeMediaFailureClass::Rejected,
            _ => NativeMediaFailureClass::Uncertain,
        }
    }

    async fn resume_partial_delivery(&self, partial: &PartialDelivery) -> Result<(), GatewayError> {
        self.resume_partial_text(partial).await
    }

    async fn send_or_update_status(
        &self,
        chat_id: &str,
        _status_key: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.send_text(chat_id, text).await?;
        Ok(())
    }

    async fn send_or_update_status_with_options(
        &self,
        chat_id: &str,
        _status_key: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
        options: SendMessageOptions,
    ) -> Result<(), GatewayError> {
        let metadata = discord_metadata_from_send_options(&options);
        self.send_text_with_metadata(chat_id, text, metadata.as_ref())
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

    async fn rename_thread(&self, thread_id: &str, title: &str) -> Result<bool, GatewayError> {
        self.rename_thread_channel(thread_id, title).await?;
        Ok(true)
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

    async fn send_image_url_with_options(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
        options: SendMessageOptions,
    ) -> Result<(), GatewayError> {
        let metadata = discord_metadata_from_send_options(&options);
        self.send_image_url_with_metadata(chat_id, image_url, caption, metadata.as_ref())
            .await?;
        Ok(())
    }

    fn reaction_target_chat_id<'a>(&self, chat_id: &'a str, thread_id: Option<&'a str>) -> &'a str {
        thread_id.unwrap_or(chat_id)
    }

    async fn add_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        let emoji = encode_emoji(emoji);
        DiscordAdapter::add_reaction(self, chat_id, message_id, &emoji).await
    }

    async fn remove_reaction(
        &self,
        chat_id: &str,
        message_id: &str,
        emoji: &str,
    ) -> Result<(), GatewayError> {
        let emoji = encode_emoji(emoji);
        DiscordAdapter::remove_reaction(self, chat_id, message_id, &emoji).await
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
            && self.gateway_driver.is_healthy()
            && !self.liveness_failed.load(Ordering::SeqCst)
    }

    fn is_reconnectable(&self) -> bool {
        self.gateway_driver.is_reconnectable()
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

/// Split a message into chunks that fit within Discord's UTF-16 code-unit
/// limit, preferring newline boundaries.
fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.chars().map(char::len_utf16).sum::<usize>() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut current = String::new();
    let mut current_len = 0usize;

    for segment in text.split_inclusive('\n') {
        let segment_len = segment.chars().map(char::len_utf16).sum::<usize>();
        if current_len + segment_len <= max_len {
            current.push_str(segment);
            current_len += segment_len;
            continue;
        }

        if !current.is_empty() {
            chunks.push(std::mem::take(&mut current));
            current_len = 0;
        }

        if segment_len <= max_len {
            current.push_str(segment);
            current_len = segment_len;
            continue;
        }

        for ch in segment.chars() {
            let char_len = ch.len_utf16();
            if !current.is_empty() && current_len + char_len > max_len {
                chunks.push(std::mem::take(&mut current));
                current_len = 0;
            }
            current.push(ch);
            current_len += char_len;
        }
    }

    if !current.is_empty() {
        chunks.push(current);
    }
    chunks
}

/// Build the single-message text fallback used when Discord conclusively
/// rejects a native image. A rejected embed must never turn a large caption
/// into several fallback posts, because that both duplicates user-visible
/// output and obscures the original rejection boundary.
pub(crate) fn discord_image_fallback_text(image_url: &str, caption: Option<&str>) -> String {
    let url = image_url.trim();
    let candidate = match caption.map(str::trim).filter(|caption| !caption.is_empty()) {
        Some(caption) => format!("{caption}\n{url}"),
        None => url.to_string(),
    };
    if split_message(&candidate, MAX_MESSAGE_LENGTH).len() == 1 {
        return candidate;
    }
    if split_message(url, MAX_MESSAGE_LENGTH).len() == 1 {
        return url.to_string();
    }
    "[image unavailable: URL exceeds Discord's message limit]".to_string()
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
