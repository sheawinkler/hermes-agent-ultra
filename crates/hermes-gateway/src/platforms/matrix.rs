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
use crate::platforms::helpers::{media_category, mime_from_extension};

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
    let mut html = md.to_string();

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
        .replace_all(&html, r#"<a href="$2">$1</a>"#)
        .into_owned();

    // Line breaks
    html = html.replace('\n', "<br/>");

    html
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
    pub e2ee: MatrixE2ee,
    decrypt_ffi: Option<MatrixDecryptFfiConfig>,
    native_decrypt: Option<MatrixNativeDecryptConfig>,
    native_runtime: AsyncMutex<Option<Arc<MatrixNativeDecryptRuntime>>>,
}

impl MatrixAdapter {
    pub fn new(config: MatrixConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.access_token).with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        let decrypt_ffi = MatrixDecryptFfiConfig::from_env();
        let native_decrypt = MatrixNativeDecryptConfig::from_env();
        if let Some(cfg) = &decrypt_ffi {
            info!(
                command = %cfg.command,
                args_len = cfg.args.len(),
                timeout_ms = cfg.timeout.as_millis() as u64,
                "Matrix decrypt FFI bridge enabled"
            );
        }
        if let Some(cfg) = &native_decrypt {
            info!(
                device_id_override = ?cfg.device_id_override,
                "Matrix native decrypt path enabled"
            );
        }
        Ok(Self {
            base,
            e2ee: MatrixE2ee::new(
                client.clone(),
                config.homeserver_url.clone(),
                config.access_token.clone(),
                config.user_id.clone(),
            ),
            config,
            client,
            txn_counter: AtomicU64::new(0),
            stop_signal: Arc::new(Notify::new()),
            sync_running: AtomicBool::new(false),
            decrypt_ffi,
            native_decrypt,
            native_runtime: AsyncMutex::new(None),
        })
    }

    pub fn config(&self) -> &MatrixConfig {
        &self.config
    }

    fn next_txn_id(&self) -> String {
        let n = self.txn_counter.fetch_add(1, Ordering::SeqCst);
        format!("hermes-{}-{}", chrono::Utc::now().timestamp_millis(), n)
    }

    fn auth_header(&self) -> String {
        format!("Bearer {}", self.config.access_token)
    }

    fn supported_versions() -> Cow<'static, SupportedVersions> {
        Cow::Owned(SupportedVersions {
            versions: [MatrixVersion::V1_1].into(),
            features: Default::default(),
        })
    }

    fn build_ruma_request<R>(
        &self,
        request: R,
        operation: &str,
    ) -> Result<http::Request<Vec<u8>>, GatewayError>
    where
        R: OutgoingRequest + Clone,
        for<'a> R::Authentication:
            ruma::api::auth_scheme::AuthScheme<Input<'a> = SendAccessToken<'a>>,
        for<'a> R::PathBuilder:
            ruma::api::path_builder::PathBuilder<Input<'a> = Cow<'a, SupportedVersions>>,
    {
        request
            .try_into_http_request::<Vec<u8>>(
                &self.config.homeserver_url,
                SendAccessToken::Always(&self.config.access_token),
                Self::supported_versions(),
            )
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt {operation} build request failed: {e}"
                ))
            })
    }

    async fn execute_ruma_request(
        &self,
        request: http::Request<Vec<u8>>,
        operation: &str,
    ) -> Result<http::Response<Vec<u8>>, GatewayError> {
        let (parts, body) = request.into_parts();
        let method =
            reqwest::Method::from_bytes(parts.method.as_str().as_bytes()).map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt {operation} invalid HTTP method: {e}"
                ))
            })?;
        let uri = parts.uri.to_string();
        let url = reqwest::Url::parse(&uri).map_err(|e| {
            GatewayError::ConnectionFailed(format!(
                "Matrix native decrypt {operation} invalid request URI: {e}"
            ))
        })?;

        let mut req = self.client.request(method, url);
        for (name, value) in &parts.headers {
            req = req.header(name, value);
        }
        if !body.is_empty() {
            req = req.body(body);
        }

        let response = req.send().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!(
                "Matrix native decrypt {operation} request failed: {e}"
            ))
        })?;

        let status = response.status();
        let headers = response.headers().clone();
        let bytes = response.bytes().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!(
                "Matrix native decrypt {operation} read response failed: {e}"
            ))
        })?;

        let mut response_builder = http::Response::builder().status(status.as_u16());
        for (name, value) in &headers {
            response_builder = response_builder.header(name, value);
        }
        response_builder.body(bytes.to_vec()).map_err(|e| {
            GatewayError::ConnectionFailed(format!(
                "Matrix native decrypt {operation} build response failed: {e}"
            ))
        })
    }

    fn parse_ruma_response<R>(
        response: http::Response<Vec<u8>>,
        operation: &str,
    ) -> Result<R, GatewayError>
    where
        R: IncomingResponse,
        R::EndpointError: std::fmt::Debug,
    {
        R::try_from_http_response(response).map_err(|e| {
            GatewayError::ConnectionFailed(format!(
                "Matrix native decrypt {operation} response parse failed: {e:?}"
            ))
        })
    }

    async fn ensure_native_runtime(
        &self,
    ) -> Result<Option<Arc<MatrixNativeDecryptRuntime>>, GatewayError> {
        let Some(cfg) = self.native_decrypt.as_ref() else {
            return Ok(None);
        };

        {
            let guard = self.native_runtime.lock().await;
            if let Some(runtime) = guard.as_ref() {
                return Ok(Some(runtime.clone()));
            }
        }

        let user_id = self
            .config
            .user_id
            .parse::<ruma::OwnedUserId>()
            .map_err(|e| GatewayError::ConnectionFailed(format!("Invalid Matrix user_id: {e}")))?;
        let device_id = match cfg.device_id_override.as_ref() {
            Some(v) => v.clone(),
            None => self.fetch_whoami_device_id().await?,
        };
        let machine = OlmMachine::new(user_id.as_ref(), device_id.as_str().into()).await;
        let runtime = Arc::new(MatrixNativeDecryptRuntime::new(machine));

        let mut guard = self.native_runtime.lock().await;
        if let Some(existing) = guard.as_ref() {
            Ok(Some(existing.clone()))
        } else {
            *guard = Some(runtime.clone());
            Ok(Some(runtime))
        }
    }

    async fn fetch_whoami_device_id(&self) -> Result<String, GatewayError> {
        let url = format!(
            "{}/_matrix/client/v3/account/whoami",
            self.config.homeserver_url
        );
        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Matrix whoami failed: {e}")))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Matrix whoami error ({status}): {body}"
            )));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Matrix whoami parse: {e}")))?;

        if let Some(whoami_user) = body.get("user_id").and_then(|v| v.as_str()) {
            if whoami_user != self.config.user_id {
                warn!(
                    config_user = %self.config.user_id,
                    whoami_user,
                    "Matrix user_id differs from whoami response"
                );
            }
        }

        body.get("device_id")
            .and_then(|v| v.as_str())
            .filter(|v| !v.trim().is_empty())
            .map(String::from)
            .ok_or_else(|| {
                GatewayError::ConnectionFailed(
                    "Matrix whoami did not return device_id; set HERMES_MATRIX_DEVICE_ID"
                        .to_string(),
                )
            })
    }

    async fn process_native_outgoing_requests(
        &self,
        runtime: &Arc<MatrixNativeDecryptRuntime>,
    ) -> Result<(), GatewayError> {
        let _request_guard = runtime.outgoing_lock.lock().await;
        loop {
            let requests = runtime.machine.outgoing_requests().await.map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt outgoing requests failed: {e}"
                ))
            })?;
            if requests.is_empty() {
                break;
            }

            for request in requests {
                self.send_native_outgoing_request(runtime, request).await?;
            }
        }
        Ok(())
    }

    async fn send_native_outgoing_request(
        &self,
        runtime: &Arc<MatrixNativeDecryptRuntime>,
        request: matrix_sdk_crypto::types::requests::OutgoingRequest,
    ) -> Result<(), GatewayError> {
        let request_id = request.request_id();
        match request.request() {
            AnyOutgoingRequest::KeysUpload(req) => {
                let request = self.build_ruma_request(req.clone(), "keys/upload")?;
                let response = self.execute_ruma_request(request, "keys/upload").await?;
                let body: upload_keys::v3::Response =
                    Self::parse_ruma_response(response, "keys/upload")?;
                runtime
                    .machine
                    .mark_request_as_sent(request_id, &body)
                    .await
                    .map_err(|e| {
                        GatewayError::ConnectionFailed(format!(
                            "Matrix native decrypt mark keys/upload sent failed: {e}"
                        ))
                    })?;
            }
            AnyOutgoingRequest::KeysQuery(req) => {
                self.send_native_keys_query(runtime, request_id, req)
                    .await?;
            }
            AnyOutgoingRequest::KeysClaim(req) => {
                let request = self.build_ruma_request(req.clone(), "keys/claim")?;
                let response = self.execute_ruma_request(request, "keys/claim").await?;
                let body: claim_keys::v3::Response =
                    Self::parse_ruma_response(response, "keys/claim")?;
                runtime
                    .machine
                    .mark_request_as_sent(request_id, &body)
                    .await
                    .map_err(|e| {
                        GatewayError::ConnectionFailed(format!(
                            "Matrix native decrypt mark keys/claim sent failed: {e}"
                        ))
                    })?;
            }
            AnyOutgoingRequest::ToDeviceRequest(req) => {
                self.send_native_to_device(runtime, request_id, req).await?;
            }
            AnyOutgoingRequest::SignatureUpload(req) => {
                let request = self.build_ruma_request(req.clone(), "keys/signatures/upload")?;
                let response = self
                    .execute_ruma_request(request, "keys/signatures/upload")
                    .await?;
                let body: upload_signatures::v3::Response =
                    Self::parse_ruma_response(response, "keys/signatures/upload")?;
                runtime
                    .machine
                    .mark_request_as_sent(request_id, &body)
                    .await
                    .map_err(|e| {
                        GatewayError::ConnectionFailed(format!(
                            "Matrix native decrypt mark signatures/upload sent failed: {e}"
                        ))
                    })?;
            }
            AnyOutgoingRequest::RoomMessage(req) => {
                let request = send_message_event::v3::Request::new(
                    req.room_id.clone(),
                    req.txn_id.clone(),
                    req.content.as_ref(),
                )
                .map_err(|e| {
                    GatewayError::ConnectionFailed(format!(
                        "Matrix native decrypt room message request build failed: {e}"
                    ))
                })?;
                let request = self.build_ruma_request(request, "rooms/send")?;
                let response = self.execute_ruma_request(request, "rooms/send").await?;
                let body: send_message_event::v3::Response =
                    Self::parse_ruma_response(response, "rooms/send")?;
                runtime
                    .machine
                    .mark_request_as_sent(request_id, &body)
                    .await
                    .map_err(|e| {
                        GatewayError::ConnectionFailed(format!(
                            "Matrix native decrypt mark room message sent failed: {e}"
                        ))
                    })?;
            }
        }

        Ok(())
    }

    async fn send_native_keys_query(
        &self,
        runtime: &Arc<MatrixNativeDecryptRuntime>,
        request_id: &ruma::TransactionId,
        req: &KeysQueryRequest,
    ) -> Result<(), GatewayError> {
        let mut request = get_keys::v3::Request::new();
        request.timeout = req.timeout;
        request.device_keys = req.device_keys.clone();

        let request = self.build_ruma_request(request, "keys/query")?;
        let response = self.execute_ruma_request(request, "keys/query").await?;
        let body: get_keys::v3::Response = Self::parse_ruma_response(response, "keys/query")?;
        runtime
            .machine
            .mark_request_as_sent(request_id, &body)
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt mark keys/query sent failed: {e}"
                ))
            })?;
        Ok(())
    }

    async fn send_native_to_device(
        &self,
        runtime: &Arc<MatrixNativeDecryptRuntime>,
        request_id: &ruma::TransactionId,
        req: &ToDeviceRequest,
    ) -> Result<(), GatewayError> {
        let request = send_event_to_device::v3::Request::new_raw(
            req.event_type.clone(),
            req.txn_id.clone(),
            req.messages.clone(),
        );
        let request = self.build_ruma_request(request, "sendToDevice")?;
        let response = self.execute_ruma_request(request, "sendToDevice").await?;
        let body: send_event_to_device::v3::Response =
            Self::parse_ruma_response(response, "sendToDevice")?;
        runtime
            .machine
            .mark_request_as_sent(request_id, &body)
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt mark sendToDevice sent failed: {e}"
                ))
            })?;
        Ok(())
    }

    async fn process_native_sync_changes(
        &self,
        runtime: &Arc<MatrixNativeDecryptRuntime>,
        body: &serde_json::Value,
    ) -> Result<(), GatewayError> {
        let to_device_events: Vec<Raw<AnyToDeviceEvent>> = body
            .get("to_device")
            .and_then(|v| v.get("events"))
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt to-device parse failed: {e}"
                ))
            })?
            .unwrap_or_default();

        let device_lists: DeviceLists = body
            .get("device_lists")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt device_lists parse failed: {e}"
                ))
            })?
            .unwrap_or_default();

        let one_time_keys_counts: BTreeMap<OneTimeKeyAlgorithm, UInt> = body
            .get("device_one_time_keys_count")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt one-time key count parse failed: {e}"
                ))
            })?
            .unwrap_or_default();

        let unused_fallback_keys: Option<Vec<OneTimeKeyAlgorithm>> = body
            .get("device_unused_fallback_key_types")
            .cloned()
            .map(serde_json::from_value)
            .transpose()
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt fallback key parse failed: {e}"
                ))
            })?;

        let next_batch_token = body
            .get("next_batch")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);

        let sync_changes = EncryptionSyncChanges {
            to_device_events,
            changed_devices: &device_lists,
            one_time_keys_counts: &one_time_keys_counts,
            unused_fallback_keys: unused_fallback_keys.as_deref(),
            next_batch_token,
        };

        let (to_device, room_key_updates) = runtime
            .machine
            .receive_sync_changes(sync_changes, &runtime.decryption_settings)
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt receive_sync_changes failed: {e}"
                ))
            })?;
        debug!(
            to_device_events = to_device.len(),
            room_key_updates = room_key_updates.len(),
            "Processed Matrix native crypto sync changes"
        );
        Ok(())
    }

    async fn try_native_decrypt_event(
        &self,
        room_id: &str,
        event: &serde_json::Value,
    ) -> Result<Option<MatrixDecryptFfiOutput>, GatewayError> {
        let Some(runtime) = self.ensure_native_runtime().await? else {
            return Ok(None);
        };

        let room_id = room_id.parse::<ruma::OwnedRoomId>().map_err(|e| {
            GatewayError::ConnectionFailed(format!("Invalid Matrix room_id for decrypt: {e}"))
        })?;
        let raw_event: Raw<EncryptedEvent> =
            serde_json::from_value(event.clone()).map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt failed to parse encrypted event: {e}"
                ))
            })?;

        let decrypted = runtime
            .machine
            .try_decrypt_room_event(&raw_event, room_id.as_ref(), &runtime.decryption_settings)
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt room event failed: {e}"
                ))
            })?;

        let matrix_sdk_crypto::RoomEventDecryptionResult::Decrypted(decrypted) = decrypted else {
            return Ok(None);
        };

        let decrypted_json: serde_json::Value = serde_json::from_str(decrypted.event.json().get())
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!(
                    "Matrix native decrypt returned invalid JSON payload: {e}"
                ))
            })?;

        let out = Self::parse_decrypt_ffi_output(&decrypted_json.to_string()).map_err(|e| {
            GatewayError::ConnectionFailed(format!("Matrix native decrypt parse: {e}"))
        })?;
        Ok(Some(out))
    }

    // -----------------------------------------------------------------------
    // Messaging
    // -----------------------------------------------------------------------

    /// Send a plain-text or HTML message to a Matrix room.
    pub async fn send_text(
        &self,
        room_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<String, GatewayError> {
        let txn_id = self.next_txn_id();
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            self.config.homeserver_url, room_id, txn_id
        );

        let body = match parse_mode {
            Some(ParseMode::Html) => serde_json::json!({
                "msgtype": "m.text",
                "body": text,
                "format": "org.matrix.custom.html",
                "formatted_body": text
            }),
            Some(ParseMode::Markdown) => {
                let html = markdown_to_html(text);
                serde_json::json!({
                    "msgtype": "m.text",
                    "body": text,
                    "format": "org.matrix.custom.html",
                    "formatted_body": html
                })
            }
            _ => serde_json::json!({ "msgtype": "m.text", "body": text }),
        };

        let resp = self
            .client
            .put(&url)
            .header("Authorization", self.auth_header())
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Matrix send failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Matrix API error: {text}"
            )));
        }

        let result: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Matrix parse failed: {e}")))?;

        Ok(result
            .get("event_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    /// Edit a message in a Matrix room using `m.replace` relation.
    pub async fn edit_text(
        &self,
        room_id: &str,
        event_id: &str,
        new_text: &str,
    ) -> Result<(), GatewayError> {
        let txn_id = self.next_txn_id();
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            self.config.homeserver_url, room_id, txn_id
        );

        let body = serde_json::json!({
            "msgtype": "m.text",
            "body": format!("* {}", new_text),
            "m.new_content": { "msgtype": "m.text", "body": new_text },
            "m.relates_to": { "rel_type": "m.replace", "event_id": event_id }
        });

        let resp = self
            .client
            .put(&url)
            .header("Authorization", self.auth_header())
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Matrix edit failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Matrix edit API error: {text}"
            )));
        }
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Reactions
    // -----------------------------------------------------------------------

    /// Send a reaction (emoji annotation) to an event.
    pub async fn send_reaction(
        &self,
        room_id: &str,
        event_id: &str,
        key: &str,
    ) -> Result<String, GatewayError> {
        let txn_id = self.next_txn_id();
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.reaction/{}",
            self.config.homeserver_url, room_id, txn_id
        );

        let body = serde_json::json!({
            "m.relates_to": {
                "rel_type": "m.annotation",
                "event_id": event_id,
                "key": key
            }
        });

        let resp = self
            .client
            .put(&url)
            .header("Authorization", self.auth_header())
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Matrix reaction failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Matrix reaction error: {text}"
            )));
        }

        let result: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Matrix reaction parse: {e}")))?;

        Ok(result
            .get("event_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    // -----------------------------------------------------------------------
    // Redaction
    // -----------------------------------------------------------------------

    /// Redact (delete) an event from a room.
    pub async fn redact_event(
        &self,
        room_id: &str,
        event_id: &str,
        reason: Option<&str>,
    ) -> Result<String, GatewayError> {
        let txn_id = self.next_txn_id();
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/redact/{}/{}",
            self.config.homeserver_url, room_id, event_id, txn_id
        );

        let body = match reason {
            Some(r) => serde_json::json!({ "reason": r }),
            None => serde_json::json!({}),
        };

        let resp = self
            .client
            .put(&url)
            .header("Authorization", self.auth_header())
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Matrix redact failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Matrix redact error: {text}"
            )));
        }

        let result: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Matrix redact parse: {e}")))?;

        Ok(result
            .get("event_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    // -----------------------------------------------------------------------
    // Read receipts & typing indicators
    // -----------------------------------------------------------------------

    /// Send a read receipt for an event.
    pub async fn send_read_receipt(
        &self,
        room_id: &str,
        event_id: &str,
    ) -> Result<(), GatewayError> {
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/receipt/m.read/{}",
            self.config.homeserver_url, room_id, event_id
        );

        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Matrix read receipt failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Matrix read receipt error: {text}"
            )));
        }

        debug!(room_id, event_id, "Read receipt sent");
        Ok(())
    }

    /// Send or cancel a typing indicator.
    pub async fn send_typing(
        &self,
        room_id: &str,
        typing: bool,
        timeout_ms: Option<u64>,
    ) -> Result<(), GatewayError> {
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/typing/{}",
            self.config.homeserver_url, room_id, self.config.user_id
        );

        let body = if typing {
            serde_json::json!({
                "typing": true,
                "timeout": timeout_ms.unwrap_or(30_000)
            })
        } else {
            serde_json::json!({ "typing": false })
        };

        let resp = self
            .client
            .put(&url)
            .header("Authorization", self.auth_header())
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("Matrix typing indicator failed: {e}"))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Matrix typing error: {text}"
            )));
        }

        debug!(room_id, typing, "Typing indicator sent");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Room management
    // -----------------------------------------------------------------------

    /// Join a room by room ID or alias.
    pub async fn join_room(&self, room_id: &str) -> Result<String, GatewayError> {
        let url = format!(
            "{}/_matrix/client/v3/join/{}",
            self.config.homeserver_url, room_id
        );

        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Matrix join room failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Matrix join room error: {text}"
            )));
        }

        let result: serde_json::Value = resp.json().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Matrix join parse failed: {e}"))
        })?;

        let joined_room = result
            .get("room_id")
            .and_then(|v| v.as_str())
            .unwrap_or(room_id)
            .to_string();

        info!(room_id = %joined_room, "Joined room");
        Ok(joined_room)
    }

    /// Leave a room.
    pub async fn leave_room(&self, room_id: &str) -> Result<(), GatewayError> {
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/leave",
            self.config.homeserver_url, room_id
        );

        let resp = self
            .client
            .post(&url)
            .header("Authorization", self.auth_header())
            .json(&serde_json::json!({}))
            .send()
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!("Matrix leave room failed: {e}"))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Matrix leave room error: {text}"
            )));
        }

        info!(room_id, "Left room");
        Ok(())
    }

    /// Get the list of members in a room.
    pub async fn get_room_members(&self, room_id: &str) -> Result<Vec<RoomMember>, GatewayError> {
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/members",
            self.config.homeserver_url, room_id
        );

        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!("Matrix get members failed: {e}"))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Matrix get members error: {text}"
            )));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Matrix members parse: {e}")))?;

        let mut members = Vec::new();
        if let Some(chunks) = body.get("chunk").and_then(|v| v.as_array()) {
            for event in chunks {
                let user_id = event
                    .get("state_key")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let content = event.get("content");
                let membership = content
                    .and_then(|c| c.get("membership"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("leave")
                    .to_string();
                let display_name = content
                    .and_then(|c| c.get("displayname"))
                    .and_then(|v| v.as_str())
                    .map(String::from);

                members.push(RoomMember {
                    user_id,
                    display_name,
                    membership,
                });
            }
        }

        debug!(room_id, count = members.len(), "Fetched room members");
        Ok(members)
    }

    /// Get the power levels for a room.
    pub async fn get_room_power_levels(
        &self,
        room_id: &str,
    ) -> Result<serde_json::Value, GatewayError> {
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/state/m.room.power_levels/",
            self.config.homeserver_url, room_id
        );

        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!("Matrix power levels failed: {e}"))
            })?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Matrix power levels error: {text}"
            )));
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Matrix power levels parse: {e}"))
        })?;

        Ok(body)
    }

    /// Get the display name of a room.
    pub async fn get_room_name(&self, room_id: &str) -> Result<Option<String>, GatewayError> {
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/state/m.room.name/",
            self.config.homeserver_url, room_id
        );

        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Matrix room name failed: {e}")))?;

        if resp.status().as_u16() == 404 {
            return Ok(None);
        }
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Matrix room name error: {text}"
            )));
        }

        let body: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Matrix room name parse: {e}")))?;

        Ok(body.get("name").and_then(|v| v.as_str()).map(String::from))
    }

    // -----------------------------------------------------------------------
    // Media upload
    // -----------------------------------------------------------------------

    /// Upload a file to the Matrix media store and return its `mxc://` URI.
    pub async fn upload_media(
        &self,
        file_bytes: Vec<u8>,
        file_name: &str,
        content_type: &str,
    ) -> Result<String, GatewayError> {
        let upload_url = format!(
            "{}/_matrix/media/v3/upload?filename={}",
            self.config.homeserver_url,
            urlencoding::encode(file_name)
        );

        let resp = self
            .client
            .post(&upload_url)
            .header("Authorization", self.auth_header())
            .header("Content-Type", content_type)
            .body(file_bytes)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Matrix upload failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Matrix upload error: {text}"
            )));
        }

        let result: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Matrix upload parse: {e}")))?;

        result
            .get("content_uri")
            .and_then(|v| v.as_str())
            .map(String::from)
            .ok_or_else(|| GatewayError::SendFailed("No content_uri in upload response".into()))
    }

    /// Send a media message (image/audio/video/file) to a room.
    async fn send_media_message(
        &self,
        room_id: &str,
        mxc_uri: &str,
        file_name: &str,
        mime: &str,
        size: usize,
        caption: Option<&str>,
    ) -> Result<String, GatewayError> {
        let txn_id = self.next_txn_id();
        let url = format!(
            "{}/_matrix/client/v3/rooms/{}/send/m.room.message/{}",
            self.config.homeserver_url, room_id, txn_id
        );

        let ext = std::path::Path::new(file_name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let category = media_category(ext);
        let msgtype = match category {
            "image" => "m.image",
            "video" => "m.video",
            "audio" => "m.audio",
            _ => "m.file",
        };

        let body_text = caption.unwrap_or(file_name);
        let payload = serde_json::json!({
            "msgtype": msgtype,
            "body": body_text,
            "url": mxc_uri,
            "info": {
                "mimetype": mime,
                "size": size,
            }
        });

        let resp = self
            .client
            .put(&url)
            .header("Authorization", self.auth_header())
            .json(&payload)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Matrix media send failed: {e}")))?;

        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "Matrix media send error: {text}"
            )));
        }

        let result: serde_json::Value = resp
            .json()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Matrix media parse: {e}")))?;

        Ok(result
            .get("event_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string())
    }

    // -----------------------------------------------------------------------
    // Sync
    // -----------------------------------------------------------------------

    /// Perform a single `/sync` call and return new messages plus the next batch token.
    pub async fn sync_once(
        &self,
        since: Option<&str>,
    ) -> Result<(Vec<IncomingMatrixMessage>, Option<String>), GatewayError> {
        if let Some(runtime) = self.ensure_native_runtime().await? {
            if let Err(err) = self.process_native_outgoing_requests(&runtime).await {
                warn!(error = %err, "Matrix native decrypt outgoing pre-sync request handling failed");
            }
        }

        let mut url = format!(
            "{}/_matrix/client/v3/sync?timeout={}&filter={{\"room\":{{\"timeline\":{{\"limit\":{}}}}}}}",
            self.config.homeserver_url, SYNC_TIMEOUT_MS, SYNC_TIMELINE_LIMIT
        );
        if let Some(token) = since {
            url.push_str(&format!("&since={}", token));
        }

        let resp = self
            .client
            .get(&url)
            .header("Authorization", self.auth_header())
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("Matrix sync failed: {e}")))?;

        let status = resp.status();
        if status.as_u16() == 401 || status.as_u16() == 403 {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::Auth(format!(
                "Matrix auth error ({status}): {text}"
            )));
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "Matrix sync error ({status}): {text}"
            )));
        }

        let body: serde_json::Value = resp.json().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("Matrix sync parse failed: {e}"))
        })?;

        let next_batch = body
            .get("next_batch")
            .and_then(|v| v.as_str())
            .map(String::from);

        if let Some(runtime) = self.ensure_native_runtime().await? {
            if let Err(err) = self.process_native_sync_changes(&runtime, &body).await {
                warn!(error = %err, "Matrix native decrypt sync-change ingestion failed");
            }
            if let Err(err) = self.process_native_outgoing_requests(&runtime).await {
                warn!(error = %err, "Matrix native decrypt outgoing post-sync request handling failed");
            }
        }

        let messages = self.parse_sync_events(&body).await;

        // Auto-join on invite
        let invites = self.parse_invites(&body);
        for invite_room in invites {
            info!(room_id = %invite_room, "Auto-joining invited room");
            if let Err(e) = self.join_room(&invite_room).await {
                warn!(room_id = %invite_room, error = %e, "Failed to auto-join room");
            }
        }

        Ok((messages, next_batch))
    }

    /// Long-running sync loop with exponential backoff on errors.
    ///
    /// Calls `sync_once` repeatedly, passing the `since` token from each
    /// response. On transient errors the loop sleeps with exponential backoff
    /// (2s → 5s → 10s → 30s → 60s). Auth errors (401/403) cause an
    /// immediate stop.
    ///
    /// The `callback` receives each batch of messages. The loop runs until
    /// `stop()` is called.
    pub async fn sync_loop<F>(&self, mut callback: F) -> Result<(), GatewayError>
    where
        F: FnMut(Vec<IncomingMatrixMessage>) + Send,
    {
        self.sync_running.store(true, Ordering::SeqCst);
        let mut since: Option<String> = None;
        let mut backoff_idx: usize = 0;

        info!("Matrix sync loop starting");

        loop {
            if !self.base.is_running() {
                info!("Matrix sync loop: adapter stopped, exiting");
                break;
            }

            match self.sync_once(since.as_deref()).await {
                Ok((messages, next_batch)) => {
                    backoff_idx = 0;
                    since = next_batch;
                    if !messages.is_empty() {
                        debug!(count = messages.len(), "Sync delivered messages");
                        callback(messages);
                    }
                }
                Err(GatewayError::Auth(ref msg)) => {
                    error!(error = %msg, "Auth error in sync loop — stopping");
                    self.base.mark_stopped();
                    self.sync_running.store(false, Ordering::SeqCst);
                    return Err(GatewayError::Auth(msg.clone()));
                }
                Err(e) => {
                    let delay_secs = BACKOFF_STEPS[backoff_idx.min(BACKOFF_STEPS.len() - 1)];
                    warn!(
                        error = %e,
                        retry_in_secs = delay_secs,
                        "Sync error, backing off"
                    );
                    backoff_idx = (backoff_idx + 1).min(BACKOFF_STEPS.len() - 1);

                    tokio::select! {
                        _ = tokio::time::sleep(std::time::Duration::from_secs(delay_secs)) => {}
                        _ = self.stop_signal.notified() => {
                            info!("Matrix sync loop: stop signal received during backoff");
                            break;
                        }
                    }
                }
            }
        }

        self.sync_running.store(false, Ordering::SeqCst);
        info!("Matrix sync loop exited");
        Ok(())
    }

    // -----------------------------------------------------------------------
    // Sync event parsing
    // -----------------------------------------------------------------------

    /// Extract messages from joined room timelines in a `/sync` response.
    ///
    /// Handles `m.room.message`, `m.reaction`, and `m.room.encrypted` events.
    async fn parse_sync_events(
        &self,
        sync_response: &serde_json::Value,
    ) -> Vec<IncomingMatrixMessage> {
        let mut messages = Vec::new();

        let rooms = match sync_response.get("rooms").and_then(|r| r.get("join")) {
            Some(join) => join,
            None => return messages,
        };

        let rooms_map = match rooms.as_object() {
            Some(m) => m,
            None => return messages,
        };

        for (room_id, room_data) in rooms_map {
            let events = match room_data
                .get("timeline")
                .and_then(|t| t.get("events"))
                .and_then(|e| e.as_array())
            {
                Some(arr) => arr,
                None => continue,
            };

            for event in events {
                let event_type = event.get("type").and_then(|v| v.as_str()).unwrap_or("");
                let event_id = event
                    .get("event_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let sender = event
                    .get("sender")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                match event_type {
                    "m.room.message" => {
                        if let Some(msg) =
                            self.parse_room_message(room_id, &event_id, &sender, event)
                        {
                            messages.push(msg);
                        }
                    }
                    "m.reaction" => {
                        if let Some(msg) = self.parse_reaction(room_id, &event_id, &sender, event) {
                            messages.push(msg);
                        }
                    }
                    "m.room.encryption" => {
                        self.e2ee.remember_encrypted_room(room_id);
                    }
                    "m.room.encrypted" => {
                        messages.push(
                            self.parse_encrypted_event(room_id, event_id, sender, event)
                                .await,
                        );
                    }
                    _ => {}
                }
            }
        }

        messages
    }

    async fn parse_encrypted_event(
        &self,
        room_id: &str,
        event_id: String,
        sender: String,
        event: &serde_json::Value,
    ) -> IncomingMatrixMessage {
        self.e2ee.remember_encrypted_room(room_id);

        match self.try_native_decrypt_event(room_id, event).await {
            Ok(Some(decrypted)) => {
                debug!(
                    event_id = %event_id,
                    room_id,
                    event_type = %decrypted.event_type,
                    "Decrypted Matrix encrypted event via native runtime"
                );
                return IncomingMatrixMessage {
                    room_id: room_id.to_string(),
                    event_id,
                    sender,
                    body: decrypted.body,
                    event_type: decrypted.event_type,
                    is_edit: decrypted.is_edit,
                    relates_to: decrypted.relates_to,
                };
            }
            Ok(None) => {}
            Err(err) => {
                warn!(
                    event_id = %event_id,
                    room_id,
                    error = %err,
                    "Matrix native decrypt failed; trying fallback paths"
                );
            }
        }

        if let Some(cfg) = &self.decrypt_ffi {
            match self.run_decrypt_ffi(room_id, &event_id, &sender, event, cfg) {
                Ok(decrypted) => {
                    debug!(
                        event_id = %event_id,
                        room_id,
                        event_type = %decrypted.event_type,
                        "Decrypted Matrix encrypted event via FFI"
                    );
                    return IncomingMatrixMessage {
                        room_id: room_id.to_string(),
                        event_id,
                        sender,
                        body: decrypted.body,
                        event_type: decrypted.event_type,
                        is_edit: decrypted.is_edit,
                        relates_to: decrypted.relates_to,
                    };
                }
                Err(err) => {
                    warn!(
                        event_id = %event_id,
                        room_id,
                        error = %err,
                        "Matrix decrypt FFI failed; forwarding encrypted metadata fallback"
                    );
                }
            }
        }

        let body = Self::render_encrypted_event_body(event);
        warn!(
            event_id = %event_id,
            room_id,
            "Received encrypted event — forwarding encrypted metadata"
        );
        IncomingMatrixMessage {
            room_id: room_id.to_string(),
            event_id,
            sender,
            body,
            event_type: "m.room.encrypted".to_string(),
            is_edit: false,
            relates_to: None,
        }
    }

    fn run_decrypt_ffi(
        &self,
        room_id: &str,
        event_id: &str,
        sender: &str,
        event: &serde_json::Value,
        cfg: &MatrixDecryptFfiConfig,
    ) -> Result<MatrixDecryptFfiOutput, String> {
        let payload = serde_json::json!({
            "room_id": room_id,
            "event_id": event_id,
            "sender": sender,
            "event": event,
        });
        let payload_bytes =
            serde_json::to_vec(&payload).map_err(|e| format!("serialize payload failed: {e}"))?;

        let mut child = Command::new(&cfg.command)
            .args(&cfg.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("spawn failed for '{}': {e}", cfg.command))?;

        if let Some(stdin) = child.stdin.as_mut() {
            stdin
                .write_all(&payload_bytes)
                .map_err(|e| format!("write stdin failed: {e}"))?;
            stdin
                .flush()
                .map_err(|e| format!("flush stdin failed: {e}"))?;
        }
        drop(child.stdin.take());

        let started = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_status)) => {
                    let output = child
                        .wait_with_output()
                        .map_err(|e| format!("wait failed: {e}"))?;
                    if !output.status.success() {
                        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                        return Err(if stderr.is_empty() {
                            format!("process exited with {}", output.status)
                        } else {
                            format!("process exited with {}: {stderr}", output.status)
                        });
                    }
                    let stdout = String::from_utf8(output.stdout)
                        .map_err(|e| format!("stdout is not valid UTF-8: {e}"))?;
                    return Self::parse_decrypt_ffi_output(stdout.trim());
                }
                Ok(None) => {
                    if started.elapsed() >= cfg.timeout {
                        let _ = child.kill();
                        let output = child.wait_with_output().ok();
                        let stderr = output
                            .as_ref()
                            .map(|out| String::from_utf8_lossy(&out.stderr).trim().to_string())
                            .unwrap_or_default();
                        return Err(if stderr.is_empty() {
                            format!("process timed out after {}ms", cfg.timeout.as_millis())
                        } else {
                            format!(
                                "process timed out after {}ms: {stderr}",
                                cfg.timeout.as_millis()
                            )
                        });
                    }
                    thread::sleep(Duration::from_millis(10));
                }
                Err(e) => return Err(format!("try_wait failed: {e}")),
            }
        }
    }

    fn parse_decrypt_ffi_output(stdout: &str) -> Result<MatrixDecryptFfiOutput, String> {
        if stdout.is_empty() {
            return Err("empty stdout from decrypt FFI".to_string());
        }

        if let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout) {
            if let Some(err) = value.get("error").and_then(|v| v.as_str()) {
                return Err(format!("decrypt FFI error: {err}"));
            }

            let relates_to = value
                .get("relates_to")
                .or_else(|| value.get("m.relates_to"))
                .or_else(|| value.get("content").and_then(|c| c.get("m.relates_to")))
                .and_then(Self::parse_relates_to_json);
            let is_edit = value
                .get("is_edit")
                .and_then(|v| v.as_bool())
                .unwrap_or_else(|| {
                    relates_to
                        .as_ref()
                        .map(|r| r.rel_type == "m.replace")
                        .unwrap_or(false)
                });
            let body = if is_edit {
                value
                    .get("m.new_content")
                    .and_then(|nc| nc.get("body"))
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        value
                            .get("content")
                            .and_then(|c| c.get("m.new_content"))
                            .and_then(|nc| nc.get("body"))
                            .and_then(|v| v.as_str())
                    })
                    .or_else(|| value.get("body").and_then(|v| v.as_str()))
                    .or_else(|| {
                        value
                            .get("content")
                            .and_then(|c| c.get("body"))
                            .and_then(|v| v.as_str())
                    })
            } else {
                value
                    .get("body")
                    .and_then(|v| v.as_str())
                    .or_else(|| {
                        value
                            .get("content")
                            .and_then(|c| c.get("body"))
                            .and_then(|v| v.as_str())
                    })
                    .or_else(|| {
                        value
                            .get("content")
                            .and_then(|c| c.get("m.new_content"))
                            .and_then(|nc| nc.get("body"))
                            .and_then(|v| v.as_str())
                    })
            }
            .map(|s| s.trim().to_string())
            .unwrap_or_default();

            if body.is_empty() {
                return Err("decrypt FFI JSON missing non-empty body".to_string());
            }

            let event_type = value
                .get("event_type")
                .or_else(|| value.get("type"))
                .and_then(|v| v.as_str())
                .unwrap_or("m.room.message")
                .to_string();

            return Ok(MatrixDecryptFfiOutput {
                body,
                event_type,
                is_edit,
                relates_to,
            });
        }

        Ok(MatrixDecryptFfiOutput {
            body: stdout.to_string(),
            event_type: "m.room.message".to_string(),
            is_edit: false,
            relates_to: None,
        })
    }

    fn parse_relates_to_json(value: &serde_json::Value) -> Option<RelatesTo> {
        let rel_type = value
            .get("rel_type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let event_id = value
            .get("event_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if rel_type.is_empty() || event_id.is_empty() {
            return None;
        }

        let key = value.get("key").and_then(|v| v.as_str()).map(String::from);
        Some(RelatesTo {
            rel_type,
            event_id,
            key,
        })
    }

    fn render_encrypted_event_body(event: &serde_json::Value) -> String {
        let content = event.get("content").cloned().unwrap_or_default();
        if let Some(body) = content.get("body").and_then(|v| v.as_str()) {
            if !body.trim().is_empty() {
                return body.to_string();
            }
        }

        let mut meta = Vec::new();
        if let Some(algorithm) = content.get("algorithm").and_then(|v| v.as_str()) {
            meta.push(format!("algorithm={algorithm}"));
        }
        if let Some(sender_key) = content.get("sender_key").and_then(|v| v.as_str()) {
            meta.push(format!("sender_key={sender_key}"));
        }
        if let Some(device_id) = content.get("device_id").and_then(|v| v.as_str()) {
            meta.push(format!("device_id={device_id}"));
        }
        if let Some(session_id) = content.get("session_id").and_then(|v| v.as_str()) {
            meta.push(format!("session_id={session_id}"));
        }

        if meta.is_empty() {
            "[encrypted event]".to_string()
        } else {
            format!("[encrypted event: {}]", meta.join(", "))
        }
    }

    fn parse_room_message(
        &self,
        room_id: &str,
        event_id: &str,
        sender: &str,
        event: &serde_json::Value,
    ) -> Option<IncomingMatrixMessage> {
        let content = event.get("content")?;

        let relates_to_val = content.get("m.relates_to");
        let rel_type = relates_to_val
            .and_then(|r| r.get("rel_type"))
            .and_then(|v| v.as_str());
        let is_edit = rel_type == Some("m.replace");

        let relates_to = relates_to_val.map(|r| RelatesTo {
            rel_type: r
                .get("rel_type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            event_id: r
                .get("event_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            key: r.get("key").and_then(|v| v.as_str()).map(String::from),
        });

        let body = if is_edit {
            content
                .get("m.new_content")
                .and_then(|nc| nc.get("body"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        } else {
            content
                .get("body")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };

        Some(IncomingMatrixMessage {
            room_id: room_id.to_string(),
            event_id: event_id.to_string(),
            sender: sender.to_string(),
            body,
            event_type: "m.room.message".to_string(),
            is_edit,
            relates_to,
        })
    }

    fn parse_reaction(
        &self,
        room_id: &str,
        event_id: &str,
        sender: &str,
        event: &serde_json::Value,
    ) -> Option<IncomingMatrixMessage> {
        let content = event.get("content")?;
        let relates_to_val = content.get("m.relates_to")?;

        let target_event = relates_to_val
            .get("event_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let key = relates_to_val
            .get("key")
            .and_then(|v| v.as_str())
            .map(String::from);

        Some(IncomingMatrixMessage {
            room_id: room_id.to_string(),
            event_id: event_id.to_string(),
            sender: sender.to_string(),
            body: key.clone().unwrap_or_default(),
            event_type: "m.reaction".to_string(),
            is_edit: false,
            relates_to: Some(RelatesTo {
                rel_type: "m.annotation".to_string(),
                event_id: target_event,
                key,
            }),
        })
    }

    /// Extract room IDs from the `invite` section of a sync response.
    fn parse_invites(&self, sync_response: &serde_json::Value) -> Vec<String> {
        sync_response
            .get("rooms")
            .and_then(|r| r.get("invite"))
            .and_then(|inv| inv.as_object())
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }

    // -----------------------------------------------------------------------
    // Helpers
    // -----------------------------------------------------------------------

    /// Returns `true` if the background sync loop is active.
    pub fn is_sync_running(&self) -> bool {
        self.sync_running.load(Ordering::SeqCst)
    }
}

// ---------------------------------------------------------------------------
// PlatformAdapter trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl PlatformAdapter for MatrixAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!("Matrix adapter starting (user: {})", self.config.user_id);
        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Matrix adapter stopping");
        self.base.mark_stopped();
        self.stop_signal.notify_one();
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.send_text(chat_id, text, parse_mode).await?;
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
        let path = std::path::Path::new(file_path);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let mime = mime_from_extension(ext);
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        let file_bytes = tokio::fs::read(file_path)
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to read file: {e}")))?;

        let size = file_bytes.len();
        let mxc_uri = self.upload_media(file_bytes, file_name, mime).await?;
        self.send_media_message(chat_id, &mxc_uri, file_name, mime, size, caption)
            .await?;
        Ok(())
    }

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let downloaded = async {
            let resp = self
                .client
                .get(image_url)
                .send()
                .await
                .map_err(|e| format!("request failed: {e}"))?;
            if !resp.status().is_success() {
                return Err(format!("status {}", resp.status()));
            }

            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .map(|s| s.to_string());
            let file_bytes = resp
                .bytes()
                .await
                .map_err(|e| format!("read body failed: {e}"))?
                .to_vec();
            if file_bytes.is_empty() {
                return Err("empty body".to_string());
            }
            Ok((file_bytes, content_type))
        }
        .await;

        let (file_bytes, content_type) = match downloaded {
            Ok(result) => result,
            Err(err) => {
                warn!(
                    image_url = %image_url,
                    error = %err,
                    "Matrix image-url download failed; falling back to text"
                );
                let fallback = image_fallback_text(image_url, caption);
                return self
                    .send_message(chat_id, &fallback, Some(ParseMode::Plain))
                    .await;
            }
        };

        let file_name = remote_image_file_name(image_url, content_type.as_deref());
        let ext = std::path::Path::new(&file_name)
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("png");
        let mime = normalized_image_content_type(content_type.as_deref())
            .unwrap_or_else(|| mime_from_extension(ext).to_string());
        let size = file_bytes.len();
        let mxc_uri = self.upload_media(file_bytes, &file_name, &mime).await?;
        self.send_media_message(chat_id, &mxc_uri, &file_name, &mime, size, caption)
            .await?;
        Ok(())
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }

    fn platform_name(&self) -> &str {
        "matrix"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn test_markdown_to_html_bold() {
        let html = markdown_to_html("hello **world**");
        assert!(html.contains("<strong>world</strong>"));
    }

    #[test]
    fn test_markdown_to_html_italic() {
        let html = markdown_to_html("hello *world*");
        assert!(html.contains("<em>world</em>"));
    }

    #[test]
    fn test_markdown_to_html_inline_code() {
        let html = markdown_to_html("use `foo()` here");
        assert!(html.contains("<code>foo()</code>"));
    }

    #[test]
    fn test_markdown_to_html_link() {
        let html = markdown_to_html("[click](https://example.com)");
        assert!(html.contains(r#"<a href="https://example.com">click</a>"#));
    }

    #[test]
    fn test_mxc_to_http() {
        let url = mxc_to_http("https://matrix.org", "mxc://matrix.org/abc123");
        assert_eq!(
            url,
            Some("https://matrix.org/_matrix/media/v3/download/matrix.org/abc123".to_string())
        );
    }

    #[test]
    fn test_mxc_to_http_invalid() {
        assert_eq!(mxc_to_http("https://matrix.org", "not-mxc"), None);
    }

    #[test]
    fn test_mxc_to_http_trailing_slash() {
        let url = mxc_to_http("https://matrix.org/", "mxc://matrix.org/xyz");
        assert_eq!(
            url,
            Some("https://matrix.org/_matrix/media/v3/download/matrix.org/xyz".to_string())
        );
    }

    #[test]
    fn remote_image_file_name_keeps_existing_extension() {
        let file_name = remote_image_file_name(
            "https://cdn.example.com/path/diagram.jpeg?token=abc",
            Some("image/jpeg"),
        );
        assert_eq!(file_name, "diagram.jpeg");
    }

    #[test]
    fn remote_image_file_name_uses_content_type_extension_hint() {
        let file_name =
            remote_image_file_name("https://cdn.example.com/path/diagram", Some("image/webp"));
        assert_eq!(file_name, "diagram.webp");
    }

    #[test]
    fn normalized_image_content_type_strips_params() {
        assert_eq!(
            normalized_image_content_type(Some("image/png; charset=binary")).as_deref(),
            Some("image/png")
        );
        assert_eq!(normalized_image_content_type(Some("text/plain")), None);
    }

    #[tokio::test]
    async fn test_parse_sync_events_messages() {
        let config = MatrixConfig {
            homeserver_url: "https://matrix.test".into(),
            user_id: "@bot:test".into(),
            access_token: "tok".into(),
            room_id: None,
            proxy: AdapterProxyConfig::default(),
        };
        let adapter = MatrixAdapter::new(config).unwrap();

        let sync = serde_json::json!({
            "rooms": {
                "join": {
                    "!room:test": {
                        "timeline": {
                            "events": [
                                {
                                    "type": "m.room.message",
                                    "event_id": "$evt1",
                                    "sender": "@user:test",
                                    "content": {
                                        "msgtype": "m.text",
                                        "body": "hello"
                                    }
                                },
                                {
                                    "type": "m.reaction",
                                    "event_id": "$evt2",
                                    "sender": "@user:test",
                                    "content": {
                                        "m.relates_to": {
                                            "rel_type": "m.annotation",
                                            "event_id": "$evt1",
                                            "key": "👍"
                                        }
                                    }
                                }
                            ]
                        }
                    }
                }
            }
        });

        let msgs = adapter.parse_sync_events(&sync).await;
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].body, "hello");
        assert_eq!(msgs[0].event_type, "m.room.message");
        assert_eq!(msgs[1].body, "👍");
        assert_eq!(msgs[1].event_type, "m.reaction");
    }

    #[tokio::test]
    async fn test_parse_sync_events_edit() {
        let config = MatrixConfig {
            homeserver_url: "https://matrix.test".into(),
            user_id: "@bot:test".into(),
            access_token: "tok".into(),
            room_id: None,
            proxy: AdapterProxyConfig::default(),
        };
        let adapter = MatrixAdapter::new(config).unwrap();

        let sync = serde_json::json!({
            "rooms": {
                "join": {
                    "!room:test": {
                        "timeline": {
                            "events": [{
                                "type": "m.room.message",
                                "event_id": "$edit1",
                                "sender": "@user:test",
                                "content": {
                                    "msgtype": "m.text",
                                    "body": "* edited",
                                    "m.new_content": {
                                        "msgtype": "m.text",
                                        "body": "edited"
                                    },
                                    "m.relates_to": {
                                        "rel_type": "m.replace",
                                        "event_id": "$orig1"
                                    }
                                }
                            }]
                        }
                    }
                }
            }
        });

        let msgs = adapter.parse_sync_events(&sync).await;
        assert_eq!(msgs.len(), 1);
        assert!(msgs[0].is_edit);
        assert_eq!(msgs[0].body, "edited");
    }

    #[test]
    fn test_parse_invites() {
        let config = MatrixConfig {
            homeserver_url: "https://matrix.test".into(),
            user_id: "@bot:test".into(),
            access_token: "tok".into(),
            room_id: None,
            proxy: AdapterProxyConfig::default(),
        };
        let adapter = MatrixAdapter::new(config).unwrap();

        let sync = serde_json::json!({
            "rooms": {
                "invite": {
                    "!room_a:test": {},
                    "!room_b:test": {}
                }
            }
        });

        let invites = adapter.parse_invites(&sync);
        assert_eq!(invites.len(), 2);
    }

    #[tokio::test]
    async fn test_parse_sync_encrypted_event_metadata() {
        let config = MatrixConfig {
            homeserver_url: "https://matrix.test".into(),
            user_id: "@bot:test".into(),
            access_token: "tok".into(),
            room_id: None,
            proxy: AdapterProxyConfig::default(),
        };
        let adapter = MatrixAdapter::new(config).unwrap();

        let sync = serde_json::json!({
            "rooms": {
                "join": {
                    "!room:test": {
                        "timeline": {
                            "events": [{
                                "type": "m.room.encrypted",
                                "event_id": "$enc1",
                                "sender": "@user:test",
                                "content": {
                                    "algorithm": "m.megolm.v1.aes-sha2"
                                }
                            }]
                        }
                    }
                }
            }
        });

        let msgs = adapter.parse_sync_events(&sync).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].event_type, "m.room.encrypted");
        assert!(msgs[0].body.contains("m.megolm.v1.aes-sha2"));
        assert!(adapter.e2ee.is_room_marked_encrypted("!room:test"));
    }

    #[test]
    fn test_parse_decrypt_ffi_output_accepts_matrix_message_shape() {
        let out = MatrixAdapter::parse_decrypt_ffi_output(
            r#"{"type":"m.room.message","content":{"body":"hello from decrypt"}}"#,
        )
        .unwrap();
        assert_eq!(out.body, "hello from decrypt");
        assert_eq!(out.event_type, "m.room.message");
        assert!(!out.is_edit);
        assert!(out.relates_to.is_none());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn test_parse_sync_encrypted_event_uses_ffi_bridge() {
        let config = MatrixConfig {
            homeserver_url: "https://matrix.test".into(),
            user_id: "@bot:test".into(),
            access_token: "tok".into(),
            room_id: None,
            proxy: AdapterProxyConfig::default(),
        };
        let mut adapter = MatrixAdapter::new(config).unwrap();
        adapter.decrypt_ffi = Some(MatrixDecryptFfiConfig {
            command: "sh".to_string(),
            args: vec![
                "-lc".to_string(),
                "cat >/dev/null; printf '%s' '{\"body\":\"decrypted hello\",\"event_type\":\"m.room.message\"}'"
                    .to_string(),
            ],
            timeout: Duration::from_millis(500),
        });

        let sync = serde_json::json!({
            "rooms": {
                "join": {
                    "!room:test": {
                        "timeline": {
                            "events": [{
                                "type": "m.room.encrypted",
                                "event_id": "$enc2",
                                "sender": "@user:test",
                                "content": {
                                    "algorithm": "m.megolm.v1.aes-sha2",
                                    "ciphertext": "xyz"
                                }
                            }]
                        }
                    }
                }
            }
        });

        let msgs = adapter.parse_sync_events(&sync).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].event_type, "m.room.message");
        assert_eq!(msgs[0].body, "decrypted hello");
        assert!(adapter.e2ee.is_room_marked_encrypted("!room:test"));
    }

    #[tokio::test]
    async fn test_parse_sync_encrypted_event_fallback_when_ffi_fails() {
        let config = MatrixConfig {
            homeserver_url: "https://matrix.test".into(),
            user_id: "@bot:test".into(),
            access_token: "tok".into(),
            room_id: None,
            proxy: AdapterProxyConfig::default(),
        };
        let mut adapter = MatrixAdapter::new(config).unwrap();
        adapter.decrypt_ffi = Some(MatrixDecryptFfiConfig {
            command: "/definitely-not-a-real-binary-hermes".to_string(),
            args: Vec::new(),
            timeout: Duration::from_millis(100),
        });

        let sync = serde_json::json!({
            "rooms": {
                "join": {
                    "!room:test": {
                        "timeline": {
                            "events": [{
                                "type": "m.room.encrypted",
                                "event_id": "$enc3",
                                "sender": "@user:test",
                                "content": {
                                    "algorithm": "m.megolm.v1.aes-sha2",
                                    "session_id": "abc123"
                                }
                            }]
                        }
                    }
                }
            }
        });

        let msgs = adapter.parse_sync_events(&sync).await;
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].event_type, "m.room.encrypted");
        assert!(msgs[0].body.contains("session_id=abc123"));
        assert!(adapter.e2ee.is_room_marked_encrypted("!room:test"));
    }

    #[tokio::test]
    async fn test_e2ee_is_encrypted_room() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path(
                "/_matrix/client/v3/rooms/room123/state/m.room.encryption/",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "algorithm": "m.megolm.v1.aes-sha2"
            })))
            .mount(&server)
            .await;

        let adapter = MatrixAdapter::new(MatrixConfig {
            homeserver_url: server.uri(),
            user_id: "@bot:test".into(),
            access_token: "tok".into(),
            room_id: None,
            proxy: AdapterProxyConfig::default(),
        })
        .unwrap();

        let encrypted = adapter.e2ee.is_encrypted_room("room123").await.unwrap();
        assert!(encrypted);
        assert!(adapter.e2ee.is_room_marked_encrypted("room123"));
    }

    #[tokio::test]
    async fn test_e2ee_verify_device_keys() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/_matrix/client/v3/keys/query"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_keys": {
                    "@alice:test": {
                        "ALDEVICE1": {
                            "keys": {"curve25519:ALDEVICE1": "abc"},
                            "algorithms": ["m.olm.v1.curve25519-aes-sha2"]
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        let adapter = MatrixAdapter::new(MatrixConfig {
            homeserver_url: server.uri(),
            user_id: "@bot:test".into(),
            access_token: "tok".into(),
            room_id: None,
            proxy: AdapterProxyConfig::default(),
        })
        .unwrap();

        let device_count = adapter
            .e2ee
            .verify_device_keys("@alice:test")
            .await
            .unwrap();
        assert_eq!(device_count, 1);
    }

    #[tokio::test]
    async fn test_e2ee_share_room_keys_claims_one_time_keys() {
        let server = MockServer::start().await;

        Mock::given(method("GET"))
            .and(path("/_matrix/client/v3/rooms/room123/members"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "chunk": [
                    {
                        "state_key": "@bot:test",
                        "content": {"membership": "join"}
                    },
                    {
                        "state_key": "@alice:test",
                        "content": {"membership": "join"}
                    }
                ]
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/_matrix/client/v3/keys/query"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "device_keys": {
                    "@alice:test": {
                        "ALDEVICE1": {
                            "keys": {"curve25519:ALDEVICE1": "abc"}
                        }
                    }
                }
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/_matrix/client/v3/keys/claim"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "one_time_keys": {
                    "@alice:test": {
                        "ALDEVICE1": {"key": "otk"}
                    }
                }
            })))
            .mount(&server)
            .await;

        let adapter = MatrixAdapter::new(MatrixConfig {
            homeserver_url: server.uri(),
            user_id: "@bot:test".into(),
            access_token: "tok".into(),
            room_id: None,
            proxy: AdapterProxyConfig::default(),
        })
        .unwrap();

        let claimed = adapter.e2ee.share_room_keys("room123").await.unwrap();
        assert_eq!(claimed, 1);
        assert!(adapter.e2ee.is_room_marked_encrypted("room123"));
    }
}
