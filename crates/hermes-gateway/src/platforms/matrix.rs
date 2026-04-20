//! Matrix Client-Server API adapter.
//!
//! Full-featured adapter supporting messaging, media, sync loop with exponential
//! backoff, room management, read receipts, typing indicators, reactions,
//! redactions, formatted messages, and E2EE metadata hooks.
//!
//! All HTTP calls target the Matrix Client-Server API v3 endpoints.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::Notify;
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
}

impl MatrixAdapter {
    pub fn new(config: MatrixConfig) -> Result<Self, GatewayError> {
        let base = BasePlatformAdapter::new(&config.access_token).with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
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

        let mut messages = self.parse_sync_events(&body);

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
    fn parse_sync_events(&self, sync_response: &serde_json::Value) -> Vec<IncomingMatrixMessage> {
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
                        self.e2ee.remember_encrypted_room(room_id);
                        let body = Self::render_encrypted_event_body(event);
                        warn!(
                            event_id,
                            room_id, "Received encrypted event — forwarding encrypted metadata"
                        );
                        messages.push(IncomingMatrixMessage {
                            room_id: room_id.clone(),
                            event_id,
                            sender,
                            body,
                            event_type: "m.room.encrypted".to_string(),
                            is_edit: false,
                            relates_to: None,
                        });
                    }
                    _ => {}
                }
            }
        }

        messages
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
    fn test_parse_sync_events_messages() {
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

        let msgs = adapter.parse_sync_events(&sync);
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[0].body, "hello");
        assert_eq!(msgs[0].event_type, "m.room.message");
        assert_eq!(msgs[1].body, "👍");
        assert_eq!(msgs[1].event_type, "m.reaction");
    }

    #[test]
    fn test_parse_sync_events_edit() {
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

        let msgs = adapter.parse_sync_events(&sync);
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

    #[test]
    fn test_parse_sync_encrypted_event_metadata() {
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

        let msgs = adapter.parse_sync_events(&sync);
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0].event_type, "m.room.encrypted");
        assert!(msgs[0].body.contains("m.megolm.v1.aes-sha2"));
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
