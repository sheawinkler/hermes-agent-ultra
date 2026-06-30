//! Matrix Client-Server API adapter.
//!
//! Full-featured adapter supporting messaging, media, sync loop with exponential
//! backoff, room management, read receipts, typing indicators, reactions,
//! redactions, formatted messages, and E2EE metadata hooks.
//!
//! All HTTP calls target the Matrix Client-Server API v3 endpoints.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::HashSet;
use std::io::Write;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use matrix_sdk_crypto::{
    types::{
        events::room::encrypted::EncryptedEvent,
        requests::{AnyOutgoingRequest, KeysQueryRequest, ToDeviceRequest},
    },
    DecryptionSettings, EncryptionSyncChanges, OlmMachine, TrustRequirement,
};
use regex::Regex;
use reqwest::Client;
use ruma::{
    api::{
        auth_scheme::SendAccessToken,
        client::{
            keys::{claim_keys, get_keys, upload_keys, upload_signatures},
            message::send_message_event,
            sync::sync_events::DeviceLists,
            to_device::send_event_to_device,
        },
        IncomingResponse, MatrixVersion, OutgoingRequest, SupportedVersions,
    },
    events::AnyToDeviceEvent,
    exports::http,
    serde::Raw,
    OneTimeKeyAlgorithm, UInt,
};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex as AsyncMutex, Notify};
use tracing::{debug, error, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};
use crate::platforms::helpers::{download_media_url, media_category, mime_from_extension};

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const SYNC_TIMEOUT_MS: u64 = 30_000;
const SYNC_TIMELINE_LIMIT: u64 = 50;
const BACKOFF_STEPS: &[u64] = &[2, 5, 10, 30, 60];
const DEFAULT_DECRYPT_FFI_TIMEOUT_MS: u64 = 1_500;
const DECRYPT_FFI_COMMAND_ENV: &str = "HERMES_MATRIX_DECRYPT_FFI_COMMAND";
const DECRYPT_FFI_ARGS_ENV: &str = "HERMES_MATRIX_DECRYPT_FFI_ARGS";
const DECRYPT_FFI_TIMEOUT_ENV: &str = "HERMES_MATRIX_DECRYPT_FFI_TIMEOUT_MS";
const NATIVE_DECRYPT_ENABLED_ENV: &str = "HERMES_MATRIX_NATIVE_DECRYPT";
const NATIVE_DEVICE_ID_ENV: &str = "HERMES_MATRIX_DEVICE_ID";

#[derive(Debug, Clone)]
struct MatrixDecryptFfiConfig {
    command: String,
    args: Vec<String>,
    timeout: Duration,
}

impl MatrixDecryptFfiConfig {
    fn from_env() -> Option<Self> {
        let command = std::env::var(DECRYPT_FFI_COMMAND_ENV)
            .ok()?
            .trim()
            .to_string();
        if command.is_empty() {
            return None;
        }

        let args = std::env::var(DECRYPT_FFI_ARGS_ENV)
            .ok()
            .map(|raw| Self::parse_args(&raw))
            .unwrap_or_default();

        let timeout_ms = match std::env::var(DECRYPT_FFI_TIMEOUT_ENV) {
            Ok(raw) => match raw.parse::<u64>() {
                Ok(v) if v > 0 => v,
                _ => {
                    warn!(
                        env_var = DECRYPT_FFI_TIMEOUT_ENV,
                        value = %raw,
                        default_ms = DEFAULT_DECRYPT_FFI_TIMEOUT_MS,
                        "Invalid Matrix decrypt FFI timeout; using default"
                    );
                    DEFAULT_DECRYPT_FFI_TIMEOUT_MS
                }
            },
            Err(_) => DEFAULT_DECRYPT_FFI_TIMEOUT_MS,
        };

        Some(Self {
            command,
            args,
            timeout: Duration::from_millis(timeout_ms),
        })
    }

    fn parse_args(raw: &str) -> Vec<String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Vec::new();
        }

        if trimmed.starts_with('[') {
            if let Ok(parsed) = serde_json::from_str::<Vec<String>>(trimmed) {
                return parsed
                    .into_iter()
                    .filter(|arg| !arg.trim().is_empty())
                    .collect();
            }
            warn!(
                env_var = DECRYPT_FFI_ARGS_ENV,
                "Failed to parse Matrix decrypt FFI args JSON; falling back to whitespace split"
            );
        }

        trimmed
            .split_whitespace()
            .map(|arg| arg.to_string())
            .collect()
    }
}

#[derive(Debug, Clone)]
struct MatrixNativeDecryptConfig {
    device_id_override: Option<String>,
}

impl MatrixNativeDecryptConfig {
    fn from_env() -> Option<Self> {
        if !Self::env_truthy(NATIVE_DECRYPT_ENABLED_ENV) {
            return None;
        }

        let device_id_override = std::env::var(NATIVE_DEVICE_ID_ENV)
            .ok()
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());

        Some(Self { device_id_override })
    }

    fn env_truthy(key: &str) -> bool {
        std::env::var(key)
            .ok()
            .map(|v| {
                let normalized = v.trim().to_ascii_lowercase();
                matches!(normalized.as_str(), "1" | "true" | "yes" | "on")
            })
            .unwrap_or(false)
    }
}

#[derive(Debug)]
struct MatrixNativeDecryptRuntime {
    machine: OlmMachine,
    decryption_settings: DecryptionSettings,
    outgoing_lock: AsyncMutex<()>,
}

impl MatrixNativeDecryptRuntime {
    fn new(machine: OlmMachine) -> Self {
        Self {
            machine,
            decryption_settings: DecryptionSettings {
                sender_device_trust_requirement: TrustRequirement::Untrusted,
            },
            outgoing_lock: AsyncMutex::new(()),
        }
    }
}

// ---------------------------------------------------------------------------
// Incoming message types
// ---------------------------------------------------------------------------

/// Incoming Matrix message extracted from /sync timeline events.
#[derive(Debug, Clone)]
pub struct IncomingMatrixMessage {
    pub room_id: String,
    pub event_id: String,
    pub sender: String,
    pub body: String,
    pub event_type: String,
    pub is_edit: bool,
    pub relates_to: Option<RelatesTo>,
}

/// Relation metadata attached to Matrix events.
#[derive(Debug, Clone)]
pub struct RelatesTo {
    pub rel_type: String,
    pub event_id: String,
    pub key: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MatrixInviteJoinRequest {
    room_id: String,
    is_direct: bool,
    inviter: Option<String>,
}

#[derive(Debug)]
struct MatrixDecryptFfiOutput {
    body: String,
    event_type: String,
    is_edit: bool,
    relates_to: Option<RelatesTo>,
}

/// Tracks the `next_batch` token for incremental `/sync` polling.
pub struct MatrixSyncState {
    pub next_batch: Option<String>,
}

/// A member entry returned from room membership queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RoomMember {
    pub user_id: String,
    pub display_name: Option<String>,
    pub membership: String,
}

/// Matrix room identity metadata used for DM-vs-room routing decisions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MatrixRoomIdentity {
    pub room_id: String,
    pub room_name: Option<String>,
    pub canonical_alias: Option<String>,
    pub server_name: Option<String>,
    pub joined_member_count: Option<usize>,
    pub is_direct_account_data: bool,
    pub direct_conflict: bool,
    pub chat_type: String,
    pub display_name: String,
}

// ---------------------------------------------------------------------------
// E2EE support
// ---------------------------------------------------------------------------

/// API-backed E2EE metadata + key lifecycle helper.
///
/// This handles encryption state checks, device-key verification, and one-time
/// key claim attempts through Matrix Client-Server APIs. Message decryption
/// still requires Olm/Megolm cryptographic session support.
pub struct MatrixE2ee {
    client: Client,
    homeserver_url: String,
    access_token: String,
    user_id: String,
    encrypted_rooms: Mutex<HashSet<String>>,
}

impl MatrixE2ee {
    pub fn new(
        client: Client,
        homeserver_url: String,
        access_token: String,
        user_id: String,
    ) -> Self {
        Self {
            client,
            homeserver_url,
            access_token,
            user_id,
            encrypted_rooms: Mutex::new(HashSet::new()),
        }
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.access_token)
    }

    pub fn remember_encrypted_room(&self, room_id: &str) {
        if let Ok(mut rooms) = self.encrypted_rooms.lock() {
            rooms.insert(room_id.to_string());
        }
    }

    pub fn is_room_marked_encrypted(&self, room_id: &str) -> bool {
        self.encrypted_rooms
            .lock()
            .map(|rooms| rooms.contains(room_id))
            .unwrap_or(false)
    }

    /// Check whether a room is encrypted using `m.room.encryption` state.
    pub async fn is_encrypted_room(&self, room_id: &str) -> Result<bool, GatewayError> {
        if self.is_room_marked_encrypted(room_id) {
            return Ok(true);
        }

        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/state/m.room.encryption/",
            self.homeserver_url, room_id
        );

        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!("Matrix encryption state failed: {e}"))
            })?;

        if resp.status().as_u16() == 404 {
            return Ok(false);
        }
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Matrix encryption state error: {text}"
            )));
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Matrix encryption state parse failed: {e}"))
        })?;
        let encrypted = body
            .get("algorithm")
            .and_then(|v| v.as_str())
            .map(|v| !v.is_empty())
            .unwrap_or(false);
        if encrypted {
            self.remember_encrypted_room(room_id);
        }
        Ok(encrypted)
    }

    /// Verify device keys for a user via `/keys/query`.
    pub async fn verify_device_keys(&self, user_id: &str) -> Result<usize, GatewayError> {
        let url = format!("{}/_matrix/client/v3/keys/query", self.homeserver_url);
        let payload = serde_json::json!({
            "device_keys": {
                user_id: []
            }
        });

        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .json(&payload)
            .send()
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!("Matrix keys/query failed: {e}"))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Matrix keys/query error: {text}"
            )));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Matrix keys/query parse: {e}")))?;

        let count = body
            .get("device_keys")
            .and_then(|v| v.get(user_id))
            .and_then(|v| v.as_object())
            .map(|m| m.len())
            .unwrap_or(0);

        if count == 0 {
            return Err(GatewayError::Platform(format!(
                "No device keys published for user {user_id}"
            )));
        }

        Ok(count)
    }

    /// Attempt one-time key claims for joined users in an encrypted room.
    pub async fn share_room_keys(&self, room_id: &str) -> Result<usize, GatewayError> {
        let members_url = format!(
            "{}/_matrix/client/v3/rooms/{}/members",
            self.homeserver_url, room_id
        );
        let members_resp = self
            .client
            .get(&members_url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!("Matrix room members failed: {e}"))
            })?;
        if !members_resp.status().is_success() {
            let text = members_resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Matrix room members error: {text}"
            )));
        }
        let members_body: serde_json::Value = members_resp.json().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Matrix room members parse failed: {e}"))
        })?;

        let joined_users: Vec<String> = members_body
            .get("chunk")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|event| {
                        let membership = event
                            .get("content")
                            .and_then(|c| c.get("membership"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("leave");
                        if membership != "join" {
                            return None;
                        }
                        event
                            .get("state_key")
                            .and_then(|v| v.as_str())
                            .map(String::from)
                    })
                    .filter(|uid| uid != &self.user_id)
                    .collect()
            })
            .unwrap_or_default();

        if joined_users.is_empty() {
            return Ok(0);
        }

        let mut device_keys_req = serde_json::Map::new();
        for user_id in &joined_users {
            device_keys_req.insert(user_id.clone(), serde_json::Value::Array(vec![]));
        }

        let query_url = format!("{}/_matrix/client/v3/keys/query", self.homeserver_url);
        let query_payload = serde_json::json!({ "device_keys": device_keys_req });
        let query_resp = self
            .client
            .post(&query_url)
            .header("Authorization", self.auth_header())
            .json(&query_payload)
            .send()
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!("Matrix keys/query failed: {e}"))
            })?;
        if !query_resp.status().is_success() {
            let text = query_resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Matrix keys/query error: {text}"
            )));
        }
        let query_body: serde_json::Value = query_resp
            .json()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Matrix keys/query parse: {e}")))?;

        let mut one_time_keys = serde_json::Map::new();
        if let Some(device_keys_map) = query_body.get("device_keys").and_then(|v| v.as_object()) {
            for user_id in &joined_users {
                if let Some(devices) = device_keys_map.get(user_id).and_then(|v| v.as_object()) {
                    let mut claim_map = serde_json::Map::new();
                    for (device_id, _) in devices {
                        claim_map.insert(
                            device_id.clone(),
                            serde_json::Value::String("signed_curve25519".to_string()),
                        );
                    }
                    if !claim_map.is_empty() {
                        one_time_keys.insert(user_id.clone(), serde_json::Value::Object(claim_map));
                    }
                }
            }
        }

        if one_time_keys.is_empty() {
            warn!(room_id, "No peer device keys available for room-key claim");
            return Ok(0);
        }

        let claim_url = format!("{}/_matrix/client/v3/keys/claim", self.homeserver_url);
        let claim_payload = serde_json::json!({
            "timeout": 10_000,
            "one_time_keys": one_time_keys
        });
        let claim_resp = self
            .client
            .post(&claim_url)
            .header("Authorization", self.auth_header())
            .json(&claim_payload)
            .send()
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!("Matrix keys/claim failed: {e}"))
            })?;
        if !claim_resp.status().is_success() {
            let text = claim_resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Matrix keys/claim error: {text}"
            )));
        }
        let claim_body: serde_json::Value = claim_resp
            .json()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Matrix keys/claim parse: {e}")))?;

        let claimed = claim_body
            .get("one_time_keys")
            .and_then(|v| v.as_object())
            .map(|users| {
                users
                    .values()
                    .filter_map(|devices| devices.as_object())
                    .map(|devices| devices.len())
                    .sum()
            })
            .unwrap_or(0usize);

        self.remember_encrypted_room(room_id);
        Ok(claimed)
    }
}

// ---------------------------------------------------------------------------
// Markdown → HTML helper
// ---------------------------------------------------------------------------

/// Convert basic Markdown to Matrix `org.matrix.custom.html` format.
///
/// Handles: **bold**, *italic*, `inline code`, ```code blocks```,
/// and `[text](url)` links. This is intentionally simple; a full
/// CommonMark parser (e.g. `pulldown-cmark`) can replace it later.
pub fn markdown_to_html(md: &str) -> String {
    let mut html = escape_html_text(md);

    // Code blocks (triple backtick) — must come before inline code
    let code_block_re = Regex::new(r"```(\w*)\n([\s\S]*?)```").expect("valid regex");
    html = code_block_re
        .replace_all(&html, |caps: &regex::Captures| {
            let lang = &caps[1];
            let code = &caps[2];
            if lang.is_empty() {
                format!("<pre><code>{}</code></pre>", code)
            } else {
                format!(
                    "<pre><code class=\"language-{}\">{}</code></pre>",
                    lang, code
                )
            }
        })
        .into_owned();

    // Inline code
    let inline_code_re = Regex::new(r"`([^`]+)`").expect("valid regex");
    html = inline_code_re
        .replace_all(&html, "<code>$1</code>")
        .into_owned();

    // Bold **text**
    let bold_re = Regex::new(r"\*\*(.+?)\*\*").expect("valid regex");
    html = bold_re
        .replace_all(&html, "<strong>$1</strong>")
        .into_owned();

    // Italic *text* (bold markers were already consumed above)
    let italic_re = Regex::new(r"\*([^*]+)\*").expect("valid regex");
    html = italic_re.replace_all(&html, "<em>$1</em>").into_owned();

    // Links [text](url)
    let link_re = Regex::new(r"\[([^\]]+)\]\(([^)]+)\)").expect("valid regex");
    html = link_re
        .replace_all(&html, |caps: &regex::Captures| {
            let label = &caps[1];
            let href = caps[2].trim();
            if markdown_href_is_safe(href) {
                format!(
                    r#"<a href="{}">{}</a>"#,
                    escape_preescaped_attr(href),
                    label
                )
            } else {
                label.to_string()
            }
        })
        .into_owned();

    // Line breaks
    html = html.replace('\n', "<br/>");

    html
}

fn markdown_href_is_safe(href: &str) -> bool {
    let lower = href.trim_start().to_ascii_lowercase();
    lower.starts_with("https://") || lower.starts_with("http://") || lower.starts_with("mailto:")
}

fn escape_html_text(raw: &str) -> String {
    let mut escaped = String::with_capacity(raw.len());
    for ch in raw.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&#39;"),
            _ => escaped.push(ch),
        }
    }
    escaped
}

fn escape_preescaped_attr(raw: &str) -> String {
    raw.replace('`', "&#96;")
}

// ---------------------------------------------------------------------------
// MXC URI helper
// ---------------------------------------------------------------------------

/// Convert an `mxc://` content URI to an HTTP(S) download URL.
///
/// `mxc://server_name/media_id` → `{homeserver}/_matrix/media/v3/download/{server_name}/{media_id}`
pub fn mxc_to_http(homeserver_url: &str, mxc_uri: &str) -> Option<String> {
    let stripped = mxc_uri.strip_prefix("mxc://")?;
    let (server, media_id) = stripped.split_once('/')?;
    Some(format!(
        "{}/_matrix/media/v3/download/{}/{}",
        homeserver_url.trim_end_matches('/'),
        server,
        media_id
    ))
}

fn normalized_image_content_type(content_type: Option<&str>) -> Option<String> {
    let normalized = content_type?
        .split(';')
        .next()
        .map(str::trim)
        .filter(|s| !s.is_empty())?
        .to_ascii_lowercase();
    if normalized.starts_with("image/") {
        Some(normalized)
    } else {
        None
    }
}

fn image_extension_from_content_type(content_type: Option<&str>) -> Option<&'static str> {
    let normalized = normalized_image_content_type(content_type)?;
    match normalized.as_str() {
        "image/jpeg" => Some("jpg"),
        "image/png" => Some("png"),
        "image/gif" => Some("gif"),
        "image/webp" => Some("webp"),
        "image/bmp" => Some("bmp"),
        "image/tiff" => Some("tiff"),
        "image/svg+xml" => Some("svg"),
        "image/heic" => Some("heic"),
        "image/heif" => Some("heif"),
        "image/avif" => Some("avif"),
        _ => None,
    }
}

fn remote_image_file_name(image_url: &str, content_type: Option<&str>) -> String {
    let stripped = image_url
        .split('#')
        .next()
        .unwrap_or(image_url)
        .split('?')
        .next()
        .unwrap_or(image_url)
        .trim_end_matches('/');
    let base = stripped.rsplit('/').next().unwrap_or("").trim();
    let mut file_name = if base.is_empty() {
        "image".to_string()
    } else {
        base.to_string()
    };

    let has_extension = std::path::Path::new(&file_name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some();

    if !has_extension {
        let ext = image_extension_from_content_type(content_type).unwrap_or("png");
        file_name.push('.');
        file_name.push_str(ext);
    }

    file_name
}

fn image_fallback_text(image_url: &str, caption: Option<&str>) -> String {
    match caption.map(str::trim).filter(|s| !s.is_empty()) {
        Some(c) => format!("{c}\n{image_url}"),
        None => image_url.to_string(),
    }
}

// ---------------------------------------------------------------------------
// MatrixConfig
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MatrixConfig {
    pub homeserver_url: String,
    pub user_id: String,
    pub access_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub room_id: Option<String>,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

// ---------------------------------------------------------------------------
// MatrixAdapter
// ---------------------------------------------------------------------------

pub struct MatrixAdapter {
    base: BasePlatformAdapter,
    config: MatrixConfig,
    client: Client,
    txn_counter: AtomicU64,
    stop_signal: Arc<Notify>,
    sync_running: AtomicBool,
    pending_invite_joins: Arc<Mutex<HashSet<String>>>,
    pub e2ee: MatrixE2ee,
    decrypt_ffi: Option<MatrixDecryptFfiConfig>,
    native_decrypt: Option<MatrixNativeDecryptConfig>,
    native_runtime: AsyncMutex<Option<Arc<MatrixNativeDecryptRuntime>>>,
}

include!("matrix/adapter_impl.rs");

#[cfg(test)]
mod tests;
