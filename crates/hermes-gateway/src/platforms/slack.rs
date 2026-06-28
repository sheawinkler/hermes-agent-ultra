//! Slack Bot API adapter.
//!
//! Implements the `PlatformAdapter` trait for Slack using the Web API
//! for message operations (`chat.postMessage`, `chat.update`, `files.upload`)
//! and Socket Mode via WebSocket for receiving events.
//! Supports Block Kit formatting and thread replies via `thread_ts`.
//!
//! Additional capabilities: Socket Mode session management, Block Kit builder,
//! App Home tab publishing, interactive component handling, modals, user info,
//! reactions, topic setting, and permalinks.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use regex::{Regex, RegexBuilder};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
use tracing::{debug, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{describe_secret, AdapterProxyConfig, BasePlatformAdapter};
use crate::channel_directory::{ChannelDirectoryProvider, ChannelEntry};

/// Slack Web API base URL.
const SLACK_API_BASE: &str = "https://slack.com/api";

/// Maximum message length for Slack (4000 characters for text blocks).
const MAX_MESSAGE_LENGTH: usize = 4000;

const SLACK_AUDIO_MIME_TO_EXT: &[(&str, &str)] = &[
    ("audio/ogg", ".ogg"),
    ("audio/opus", ".ogg"),
    ("audio/mpeg", ".mp3"),
    ("audio/mp3", ".mp3"),
    ("audio/wav", ".wav"),
    ("audio/x-wav", ".wav"),
    ("audio/webm", ".webm"),
    ("audio/mp4", ".m4a"),
    ("audio/x-m4a", ".m4a"),
    ("audio/m4a", ".m4a"),
    ("audio/aac", ".m4a"),
    ("audio/flac", ".flac"),
    ("audio/x-flac", ".flac"),
];

const SLACK_STT_SUPPORTED_EXTS: &[&str] = &[
    ".mp3", ".mp4", ".mpeg", ".mpga", ".m4a", ".wav", ".webm", ".ogg", ".aac", ".flac",
];

const SLACK_EXT_TO_AUDIO_MIME: &[(&str, &str)] = &[
    (".mp4", "audio/mp4"),
    (".m4a", "audio/mp4"),
    (".mp3", "audio/mpeg"),
    (".mpeg", "audio/mpeg"),
    (".mpga", "audio/mpeg"),
    (".wav", "audio/wav"),
    (".webm", "audio/webm"),
    (".ogg", "audio/ogg"),
    (".aac", "audio/aac"),
    (".flac", "audio/flac"),
];

include!("slack/config_media.rs");
include!("slack/socket_blocks.rs");
include!("slack/adapter_impl.rs");
include!("slack/trait_impls.rs");
include!("slack/helpers.rs");
#[cfg(test)]
mod tests;
