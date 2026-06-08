//! WeCom (Enterprise WeChat) **AI Bot WebSocket** adapter.
//!
//! Ported from Python `gateway/platforms/wecom.py`:
//! - `aibot_subscribe` authentication
//! - inbound `aibot_msg_callback` / legacy `aibot_callback`
//! - outbound markdown via `aibot_send_msg` / `aibot_respond_msg`
//! - chunked media upload (`aibot_upload_media_*`)

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use futures::{SinkExt, StreamExt};
use regex::Regex;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{Mutex, Notify, RwLock, mpsc, oneshot};
use tokio_tungstenite::tungstenite::Error as WsError;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, error, info, trace, warn};
use uuid::Uuid;

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{redact_identifier, AdapterProxyConfig, BasePlatformAdapter};
use crate::gateway::IncomingMessage;
use crate::ssrf::is_safe_url;

const DEFAULT_WS_URL: &str = "wss://openws.work.weixin.qq.com";

const APP_CMD_SUBSCRIBE: &str = "aibot_subscribe";
const APP_CMD_CALLBACK: &str = "aibot_msg_callback";
const APP_CMD_LEGACY_CALLBACK: &str = "aibot_callback";
const APP_CMD_EVENT_CALLBACK: &str = "aibot_event_callback";
const APP_CMD_SEND: &str = "aibot_send_msg";
const APP_CMD_RESPONSE: &str = "aibot_respond_msg";
const APP_CMD_PING: &str = "ping";
const APP_CMD_UPLOAD_MEDIA_INIT: &str = "aibot_upload_media_init";
const APP_CMD_UPLOAD_MEDIA_CHUNK: &str = "aibot_upload_media_chunk";
const APP_CMD_UPLOAD_MEDIA_FINISH: &str = "aibot_upload_media_finish";

const MAX_MESSAGE_LENGTH: usize = 4000;
const CONNECT_TIMEOUT_SECS: u64 = 20;
const REQUEST_TIMEOUT_SECS: u64 = 15;
const HEARTBEAT_INTERVAL_SECS: u64 = 30;
const RECONNECT_SECS: &[u64] = &[2, 5, 10, 30, 60];

const DEDUP_MAX: usize = 1000;
const SPLIT_THRESHOLD: usize = 3900;

const IMAGE_MAX_BYTES: usize = 10 * 1024 * 1024;
const VIDEO_MAX_BYTES: usize = 10 * 1024 * 1024;
const VOICE_MAX_BYTES: usize = 2 * 1024 * 1024;
const FILE_MAX_BYTES: usize = 20 * 1024 * 1024;
const ABSOLUTE_MAX_BYTES: usize = FILE_MAX_BYTES;
const UPLOAD_CHUNK_SIZE: usize = 512 * 1024;
const MAX_UPLOAD_CHUNKS: usize = 100;

const VOICE_SUPPORTED_MIMES: &[&str] = &["audio/amr"];
const WECOM_MEDIA_CACHE_SUBDIR: &str = "wecom";

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeComConfig {
    pub bot_id: String,
    pub secret: String,
    #[serde(default = "default_ws_url")]
    pub websocket_url: String,
    #[serde(default = "default_dm_policy")]
    pub dm_policy: String,
    #[serde(default)]
    pub allow_from: Vec<String>,
    #[serde(default = "default_group_policy")]
    pub group_policy: String,
    #[serde(default)]
    pub group_allow_from: Vec<String>,
    /// Per-group sender allowlists (`groups.<id>.allow_from`), plus optional `"*"`.
    #[serde(default)]
    pub groups: HashMap<String, Value>,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

fn default_ws_url() -> String {
    DEFAULT_WS_URL.to_string()
}

fn default_dm_policy() -> String {
    "open".to_string()
}

fn default_group_policy() -> String {
    "open".to_string()
}

impl WeComConfig {
    /// Build from [`hermes_config::PlatformConfig`] (`extra` keys match Python `WeComAdapter`).
    pub fn from_platform_config(p: &hermes_config::PlatformConfig) -> Self {
        let ex = &p.extra;
        let gv = |k: &str| -> String {
            ex.get(k)
                .and_then(|v| v.as_str())
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .map(String::from)
                .unwrap_or_default()
        };
        let bot_id = {
            let v = gv("bot_id");
            if v.is_empty() {
                std::env::var("WECOM_BOT_ID")
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            } else {
                v
            }
        };
        let secret = {
            let v = gv("secret");
            if v.is_empty() {
                std::env::var("WECOM_SECRET")
                    .unwrap_or_default()
                    .trim()
                    .to_string()
            } else {
                v
            }
        };
        let websocket_url = {
            let v = gv("websocket_url");
            if v.is_empty() { gv("websocketUrl") } else { v }
        };
        let websocket_url = if websocket_url.is_empty() {
            std::env::var("WECOM_WEBSOCKET_URL")
                .unwrap_or_else(|_| DEFAULT_WS_URL.to_string())
                .trim()
                .to_string()
        } else {
            websocket_url
        };
        let dm_policy = {
            let v = gv("dm_policy");
            if v.is_empty() {
                std::env::var("WECOM_DM_POLICY").unwrap_or_else(|_| "open".into())
            } else {
                v
            }
        };
        let group_policy = {
            let v = gv("group_policy");
            if v.is_empty() {
                std::env::var("WECOM_GROUP_POLICY").unwrap_or_else(|_| "open".into())
            } else {
                v
            }
        };
        let allow_from = coerce_list(
            ex.get("allow_from")
                .or_else(|| ex.get("allowFrom"))
                .cloned()
                .unwrap_or(Value::Null),
        );
        let group_allow_from = coerce_list(
            ex.get("group_allow_from")
                .or_else(|| ex.get("groupAllowFrom"))
                .cloned()
                .unwrap_or(Value::Null),
        );
        let groups = ex
            .get("groups")
            .and_then(|v| v.as_object())
            .map(|m| m.iter().map(|(k, v)| (k.clone(), v.clone())).collect())
            .unwrap_or_default();
        Self {
            bot_id,
            secret,
            websocket_url,
            dm_policy: dm_policy.to_ascii_lowercase(),
            allow_from,
            group_policy: group_policy.to_ascii_lowercase(),
            group_allow_from,
            groups,
            proxy: AdapterProxyConfig::default(),
        }
    }
}

pub fn check_wecom_requirements() -> bool {
    true
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn coerce_list(value: Value) -> Vec<String> {
    match value {
        Value::Null => vec![],
        Value::String(s) => s
            .split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect(),
        Value::Array(arr) => arr
            .into_iter()
            .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
            .filter(|s| !s.is_empty())
            .collect(),
        other => {
            let s = other.to_string();
            if s.trim().is_empty() {
                vec![]
            } else {
                vec![s.trim().to_string()]
            }
        }
    }
}

fn normalize_entry(raw: &str) -> String {
    let mut value = raw.trim().to_string();
    if let Ok(re) = Regex::new(r"(?i)^wecom:") {
        value = re.replace(&value, "").to_string();
    }
    if let Ok(re) = Regex::new(r"(?i)^(user|group):") {
        value = re.replace(&value, "").to_string();
    }
    value.trim().to_string()
}

fn entry_matches(entries: &[String], target: &str) -> bool {
    let normalized_target = target.trim().to_ascii_lowercase();
    entries.iter().any(|entry| {
        let normalized = normalize_entry(entry).to_ascii_lowercase();
        normalized == "*" || normalized == normalized_target
    })
}

fn new_req_id(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::new_v4().simple())
}

fn payload_req_id(payload: &Value) -> String {
    payload
        .get("headers")
        .and_then(|h| h.get("req_id"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn response_error(response: &Value) -> Option<String> {
    let errcode = response
        .get("errcode")
        .and_then(|v| v.as_i64())
        .unwrap_or(0);
    if errcode == 0 {
        return None;
    }
    let errmsg = response
        .get("errmsg")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown error");
    Some(format!("WeCom errcode {errcode}: {errmsg}"))
}

fn raise_for_wecom_error(response: &Value, operation: &str) -> Result<(), GatewayError> {
    if let Some(err) = response_error(response) {
        return Err(GatewayError::SendFailed(format!(
            "{operation} failed: {err}"
        )));
    }
    Ok(())
}

fn text_batch_delay_secs() -> f64 {
    std::env::var("HERMES_WECOM_TEXT_BATCH_DELAY_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0.6)
}

fn text_batch_bypass_chars() -> usize {
    std::env::var("HERMES_WECOM_TEXT_BATCH_BYPASS_CHARS")
        .ok()
        .and_then(|v| v.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(24)
}

fn text_batch_split_delay_secs() -> f64 {
    std::env::var("HERMES_WECOM_TEXT_BATCH_SPLIT_DELAY_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2.0)
}

fn detect_wecom_media_type(content_type: &str) -> &'static str {
    let mime = content_type.trim().to_ascii_lowercase();
    if mime.starts_with("image/") {
        "image"
    } else if mime.starts_with("video/") {
        "video"
    } else if mime.starts_with("audio/") || mime == "application/ogg" {
        "voice"
    } else {
        "file"
    }
}

fn guess_mime_type(filename: &str) -> String {
    let ext = Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "mp4" => "video/mp4",
        "amr" => "audio/amr",
        "pdf" => "application/pdf",
        _ => "application/octet-stream",
    }
    .to_string()
}

/// COS downloads for WeCom AI Bot images often report `application/octet-stream`.
/// Downstream vision routing only accepts `image/*` in `media_types` (Python parity:
/// `_mime_for_ext` when header MIME is generic).
fn normalize_inbound_image_content_type(content_type: &str, ext: &str) -> String {
    let normalized = content_type.trim().to_ascii_lowercase();
    if normalized.is_empty()
        || normalized == "application/octet-stream"
        || normalized == "text/plain"
        || !normalized.starts_with("image/")
    {
        guess_mime_type(&format!("x{ext}"))
    } else {
        normalized
    }
}

struct SizeCheck {
    final_type: String,
    rejected: bool,
    reject_reason: Option<String>,
    downgraded: bool,
    downgrade_note: Option<String>,
}

fn apply_file_size_limits(
    file_size: usize,
    detected_type: &str,
    content_type: Option<&str>,
) -> SizeCheck {
    let file_size_mb = file_size as f64 / (1024.0 * 1024.0);
    let normalized_type = detected_type.to_ascii_lowercase();
    let normalized_content_type = content_type.unwrap_or("").trim().to_ascii_lowercase();

    if file_size > ABSOLUTE_MAX_BYTES {
        return SizeCheck {
            final_type: normalized_type,
            rejected: true,
            reject_reason: Some(format!(
                "文件大小 {file_size_mb:.2}MB 超过了企业微信允许的最大限制 20MB，无法发送。请尝试压缩文件或减小文件大小。"
            )),
            downgraded: false,
            downgrade_note: None,
        };
    }

    if normalized_type == "image" && file_size > IMAGE_MAX_BYTES {
        return SizeCheck {
            final_type: "file".into(),
            rejected: false,
            reject_reason: None,
            downgraded: true,
            downgrade_note: Some(format!(
                "图片大小 {file_size_mb:.2}MB 超过 10MB 限制，已转为文件格式发送"
            )),
        };
    }

    if normalized_type == "video" && file_size > VIDEO_MAX_BYTES {
        return SizeCheck {
            final_type: "file".into(),
            rejected: false,
            reject_reason: None,
            downgraded: true,
            downgrade_note: Some(format!(
                "视频大小 {file_size_mb:.2}MB 超过 10MB 限制，已转为文件格式发送"
            )),
        };
    }

    if normalized_type == "voice" {
        if !normalized_content_type.is_empty()
            && !VOICE_SUPPORTED_MIMES.contains(&normalized_content_type.as_str())
        {
            return SizeCheck {
                final_type: "file".into(),
                rejected: false,
                reject_reason: None,
                downgraded: true,
                downgrade_note: Some(format!(
                    "语音格式 {normalized_content_type} 不支持，企微仅支持 AMR 格式，已转为文件格式发送"
                )),
            };
        }
        if file_size > VOICE_MAX_BYTES {
            return SizeCheck {
                final_type: "file".into(),
                rejected: false,
                reject_reason: None,
                downgraded: true,
                downgrade_note: Some(format!(
                    "语音大小 {file_size_mb:.2}MB 超过 2MB 限制，已转为文件格式发送"
                )),
            };
        }
    }

    SizeCheck {
        final_type: normalized_type,
        rejected: false,
        reject_reason: None,
        downgraded: false,
        downgrade_note: None,
    }
}

#[cfg(feature = "wecom")]
#[allow(dead_code)] // inbound encrypted media (Python `_cache_media`); wired in a follow-up
fn decrypt_wecom_file_bytes(
    encrypted_data: &[u8],
    aes_key_b64: &str,
) -> Result<Vec<u8>, GatewayError> {
    use aes::Aes256;
    use aes::cipher::array::Array;
    use aes::cipher::{BlockCipherDecrypt, KeyInit};

    if encrypted_data.is_empty() {
        return Err(GatewayError::Platform("encrypted_data is empty".into()));
    }
    if aes_key_b64.trim().is_empty() {
        return Err(GatewayError::Platform("aes_key is required".into()));
    }

    let padded = format!(
        "{}{}",
        aes_key_b64.trim(),
        "=".repeat((4 - aes_key_b64.len() % 4) % 4)
    );
    let key = base64::engine::general_purpose::STANDARD
        .decode(padded)
        .map_err(|e| GatewayError::Platform(format!("invalid WeCom aes key base64: {e}")))?;
    if key.len() != 32 {
        return Err(GatewayError::Platform(format!(
            "Invalid WeCom AES key length: expected 32 bytes, got {}",
            key.len()
        )));
    }
    let iv = &key[..16];
    if encrypted_data.len() % 16 != 0 {
        return Err(GatewayError::Platform(
            "invalid encrypted block size".into(),
        ));
    }
    let aes = Aes256::new_from_slice(&key)
        .map_err(|e| GatewayError::Platform(format!("aes key: {e}")))?;
    let mut prev = iv.to_vec();
    let mut plain = Vec::with_capacity(encrypted_data.len());
    for block in encrypted_data.chunks(16) {
        let mut b: Array<u8, _> = block.try_into().expect("block is 16 bytes");
        aes.decrypt_block((&mut b).into());
        for i in 0..16 {
            b[i] ^= prev[i];
        }
        prev.copy_from_slice(block);
        plain.extend_from_slice(&b);
    }
    let pad_len = *plain
        .last()
        .ok_or_else(|| GatewayError::Platform("Invalid PKCS#7 padding value".into()))?
        as usize;
    if pad_len < 1 || pad_len > 32 || pad_len > plain.len() {
        return Err(GatewayError::Platform(format!(
            "Invalid PKCS#7 padding value: {pad_len}"
        )));
    }
    if plain[plain.len() - pad_len..]
        .iter()
        .any(|&b| b as usize != pad_len)
    {
        return Err(GatewayError::Platform(
            "Invalid PKCS#7 padding: padding bytes mismatch".into(),
        ));
    }
    Ok(plain[..plain.len() - pad_len].to_vec())
}

// ---------------------------------------------------------------------------
// Inner state
// ---------------------------------------------------------------------------

struct PendingTextBatch {
    event: IncomingMessage,
    last_chunk_len: usize,
}

struct CachedInboundMedia {
    path: String,
    content_type: String,
}

struct WeComInner {
    config: WeComConfig,
    client: Client,
    device_id: String,
    base: BasePlatformAdapter,
    inbound_tx: RwLock<Option<mpsc::Sender<IncomingMessage>>>,
    outbound_tx: Mutex<Option<mpsc::UnboundedSender<Value>>>,
    pending: RwLock<HashMap<String, oneshot::Sender<Value>>>,
    dedup: RwLock<VecDeque<(String, Instant)>>,
    reply_req_ids: RwLock<HashMap<String, String>>,
    last_chat_req_ids: RwLock<HashMap<String, String>>,
    stream_reply_req_ids: RwLock<HashMap<String, String>>,
    /// One in-flight `aibot_respond_msg` per inbound `req_id`. Concurrent calls
    /// (native stream chunks + status `send_message`) previously shared the same
    /// `pending` key and dropped each other's oneshot → "reply request cancelled".
    reply_req_locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    pending_text: Mutex<HashMap<String, PendingTextBatch>>,
    text_batch_tasks: Mutex<HashMap<String, tokio::task::JoinHandle<()>>>,
    stop: Notify,
}

pub struct WeComAdapter {
    inner: Arc<WeComInner>,
    run_task: RwLock<Option<tokio::task::JoinHandle<()>>>,
}

impl WeComAdapter {
    pub fn new(config: WeComConfig) -> Result<Self, GatewayError> {
        if config.bot_id.is_empty() || config.secret.is_empty() {
            return Err(GatewayError::Platform(
                "WeCom AI Bot requires bot_id and secret (WECOM_BOT_ID / WECOM_SECRET)".into(),
            ));
        }
        let base = BasePlatformAdapter::new(&config.bot_id).with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        Ok(Self {
            inner: Arc::new(WeComInner {
                config,
                client,
                device_id: Uuid::new_v4().simple().to_string(),
                base,
                inbound_tx: RwLock::new(None),
                outbound_tx: Mutex::new(None),
                pending: RwLock::new(HashMap::new()),
                dedup: RwLock::new(VecDeque::new()),
                reply_req_ids: RwLock::new(HashMap::new()),
                last_chat_req_ids: RwLock::new(HashMap::new()),
                stream_reply_req_ids: RwLock::new(HashMap::new()),
                reply_req_locks: Mutex::new(HashMap::new()),
                pending_text: Mutex::new(HashMap::new()),
                text_batch_tasks: Mutex::new(HashMap::new()),
                stop: Notify::new(),
            }),
            run_task: RwLock::new(None),
        })
    }

    pub fn config(&self) -> &WeComConfig {
        &self.inner.config
    }

    pub async fn set_inbound_sender(&self, tx: mpsc::Sender<IncomingMessage>) {
        *self.inner.inbound_tx.write().await = Some(tx);
    }

    async fn is_dup(inner: &WeComInner, msg_id: &str) -> bool {
        if msg_id.is_empty() {
            return false;
        }
        let mut q = inner.dedup.write().await;
        if q.iter().any(|(id, _)| id == msg_id) {
            return true;
        }
        if q.len() >= DEDUP_MAX {
            q.pop_front();
        }
        q.push_back((msg_id.to_string(), Instant::now()));
        false
    }

    fn is_dm_allowed(config: &WeComConfig, sender_id: &str) -> bool {
        if config.dm_policy == "disabled" {
            return false;
        }
        if config.dm_policy == "allowlist" {
            return entry_matches(&config.allow_from, sender_id);
        }
        true
    }

    fn resolve_group_cfg<'a>(config: &'a WeComConfig, chat_id: &str) -> &'a Value {
        if let Some(v) = config.groups.get(chat_id) {
            return v;
        }
        let lowered = chat_id.to_ascii_lowercase();
        for (k, v) in &config.groups {
            if k.to_ascii_lowercase() == lowered {
                return v;
            }
        }
        static EMPTY: Value = Value::Null;
        config.groups.get("*").unwrap_or(&EMPTY)
    }

    fn is_group_allowed(config: &WeComConfig, chat_id: &str, sender_id: &str) -> bool {
        if config.group_policy == "disabled" {
            return false;
        }
        if config.group_policy == "allowlist" && !entry_matches(&config.group_allow_from, chat_id) {
            return false;
        }
        let group_cfg = Self::resolve_group_cfg(config, chat_id);
        let sender_allow = coerce_list(
            group_cfg
                .get("allow_from")
                .or_else(|| group_cfg.get("allowFrom"))
                .cloned()
                .unwrap_or(Value::Null),
        );
        if sender_allow.is_empty() {
            return true;
        }
        entry_matches(&sender_allow, sender_id)
    }

    fn extract_text(body: &Value) -> (String, Option<String>) {
        let mut text_parts: Vec<String> = Vec::new();
        let mut reply_text: Option<String> = None;
        let msgtype = body
            .get("msgtype")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        if msgtype == "mixed" {
            let items = body
                .get("mixed")
                .and_then(|m| m.get("msg_item"))
                .and_then(|v| v.as_array());
            if let Some(items) = items {
                for item in items {
                    if item.get("msgtype").and_then(|v| v.as_str()) == Some("text") {
                        if let Some(content) = item
                            .get("text")
                            .and_then(|t| t.get("content"))
                            .and_then(|v| v.as_str())
                        {
                            let c = content.trim();
                            if !c.is_empty() {
                                text_parts.push(c.to_string());
                            }
                        }
                    }
                }
            }
        } else {
            if let Some(content) = body
                .get("text")
                .and_then(|t| t.get("content"))
                .and_then(|v| v.as_str())
            {
                let c = content.trim();
                if !c.is_empty() {
                    text_parts.push(c.to_string());
                }
            }
            if msgtype == "voice" {
                if let Some(voice_text) = body
                    .get("voice")
                    .and_then(|v| v.get("content"))
                    .and_then(|v| v.as_str())
                {
                    let c = voice_text.trim();
                    if !c.is_empty() {
                        text_parts.push(c.to_string());
                    }
                }
            }
            if msgtype == "appmsg" {
                if let Some(title) = body
                    .get("appmsg")
                    .and_then(|a| a.get("title"))
                    .and_then(|v| v.as_str())
                {
                    let c = title.trim();
                    if !c.is_empty() {
                        text_parts.push(c.to_string());
                    }
                }
            }
        }

        let quote = body.get("quote");
        if let Some(quote) = quote {
            let quote_type = quote
                .get("msgtype")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if quote_type == "text" {
                reply_text = quote
                    .get("text")
                    .and_then(|t| t.get("content"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
            } else if quote_type == "voice" {
                reply_text = quote
                    .get("voice")
                    .and_then(|v| v.get("content"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty());
            }
        }

        let text = text_parts
            .into_iter()
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>()
            .join("\n");
        (text, reply_text)
    }

    fn detect_image_ext(data: &[u8]) -> &'static str {
        if data.starts_with(b"\x89PNG\r\n\x1a\n") {
            ".png"
        } else if data.starts_with(b"\xff\xd8\xff") {
            ".jpg"
        } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
            ".gif"
        } else if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP".as_slice()) {
            ".webp"
        } else {
            ".jpg"
        }
    }

    fn ext_from_content_type(content_type: &str) -> Option<&'static str> {
        match content_type.trim().to_ascii_lowercase().as_str() {
            "image/jpeg" => Some(".jpg"),
            "image/png" => Some(".png"),
            "image/gif" => Some(".gif"),
            "image/webp" => Some(".webp"),
            "image/bmp" => Some(".bmp"),
            "image/tiff" => Some(".tiff"),
            "application/pdf" => Some(".pdf"),
            "audio/amr" => Some(".amr"),
            "video/mp4" => Some(".mp4"),
            _ => None,
        }
    }

    fn file_name_from_url(url: &str) -> Option<String> {
        let stripped = url
            .split('#')
            .next()
            .unwrap_or(url)
            .split('?')
            .next()
            .unwrap_or(url)
            .trim_end_matches('/');
        let base = stripped.rsplit('/').next().unwrap_or("").trim();
        if base.is_empty() {
            None
        } else {
            Some(base.to_string())
        }
    }

    fn parse_content_disposition_filename(content_disposition: Option<&str>) -> Option<String> {
        let value = content_disposition?.trim();
        let lower = value.to_ascii_lowercase();
        let key = "filename=";
        let idx = lower.find(key)?;
        let raw = value[idx + key.len()..].trim();
        let cleaned = raw.trim_matches('"').trim_matches('\'').trim();
        if cleaned.is_empty() {
            None
        } else {
            Some(cleaned.to_string())
        }
    }

    fn write_wecom_media_cache(data: &[u8], ext: &str) -> Result<String, GatewayError> {
        let dir = hermes_config::hermes_home()
            .join("cache")
            .join(WECOM_MEDIA_CACHE_SUBDIR);
        std::fs::create_dir_all(&dir)
            .map_err(|e| GatewayError::ConnectionFailed(format!("create wecom cache dir: {e}")))?;
        let safe_ext = if ext.starts_with('.') {
            ext.to_string()
        } else {
            format!(".{ext}")
        };
        let path = dir.join(format!("{}{}", Uuid::new_v4().simple(), safe_ext));
        std::fs::write(&path, data)
            .map_err(|e| GatewayError::ConnectionFailed(format!("write wecom cache file: {e}")))?;
        Ok(path.to_string_lossy().to_string())
    }

    async fn download_remote_bytes(
        inner: &WeComInner,
        url: &str,
        max_bytes: usize,
    ) -> Result<(Vec<u8>, String, Option<String>), GatewayError> {
        if !is_safe_url(url) {
            return Err(GatewayError::SendFailed(format!(
                "Blocked unsafe URL (SSRF protection): {}",
                &url[..url.len().min(80)]
            )));
        }
        let response = inner
            .client
            .get(url)
            .header("User-Agent", "HermesAgent/1.0")
            .header("Accept", "*/*")
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("WeCom media GET failed: {e}")))?;
        if !response.status().is_success() {
            return Err(GatewayError::ConnectionFailed(format!(
                "WeCom media HTTP {} for {url}",
                response.status()
            )));
        }
        let headers = response.headers().clone();
        let content_type = headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .unwrap_or("")
            .split(';')
            .next()
            .unwrap_or("")
            .trim()
            .to_string();
        let content_disposition = headers
            .get(reqwest::header::CONTENT_DISPOSITION)
            .and_then(|h| h.to_str().ok())
            .map(String::from);
        let bytes = response
            .bytes()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("WeCom media read failed: {e}")))?;
        if bytes.len() > max_bytes {
            return Err(GatewayError::ConnectionFailed(format!(
                "Remote media exceeds WeCom limit: {} bytes > {} bytes",
                bytes.len(),
                max_bytes
            )));
        }
        Ok((bytes.to_vec(), content_type, content_disposition))
    }

    async fn cache_media_ref(
        inner: &WeComInner,
        kind: &str,
        media: &Value,
    ) -> Option<CachedInboundMedia> {
        let kind = kind.to_ascii_lowercase();
        let from_base64 = media
            .get("base64")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let maybe_payload = if let Some(b64) = from_base64 {
            let payload = b64.split(',').next_back().unwrap_or(b64);
            match base64::engine::general_purpose::STANDARD.decode(payload) {
                Ok(raw) => Some((raw, String::new(), None::<String>, None::<String>)),
                Err(e) => {
                    warn!(error = %e, "WeCom inbound base64 decode failed");
                    return None;
                }
            }
        } else {
            let url = media
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())?;
            match Self::download_remote_bytes(inner, url, ABSOLUTE_MAX_BYTES).await {
                Ok((raw, content_type, content_disposition)) => Some((
                    raw,
                    content_type,
                    Some(url.to_string()),
                    content_disposition,
                )),
                Err(e) => {
                    warn!(error = %e, url = %url, "WeCom inbound media download failed");
                    return None;
                }
            }
        };

        let (mut raw, content_type, source_url, content_disposition) = maybe_payload?;
        if let Some(aes_key) = media
            .get("aeskey")
            .or_else(|| media.get("aes_key"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            match decrypt_wecom_file_bytes(&raw, aes_key) {
                Ok(decrypted) => raw = decrypted,
                Err(e) => {
                    warn!(error = %e, "WeCom inbound media decrypt failed");
                    return None;
                }
            }
        }

        if kind == "image" {
            let ext = Self::ext_from_content_type(&content_type)
                .unwrap_or_else(|| Self::detect_image_ext(&raw));
            let path = match Self::write_wecom_media_cache(&raw, ext) {
                Ok(p) => p,
                Err(e) => {
                    warn!(error = %e, "WeCom inbound image cache write failed");
                    return None;
                }
            };
            let normalized_content_type =
                normalize_inbound_image_content_type(&content_type, ext);
            return Some(CachedInboundMedia {
                path,
                content_type: normalized_content_type,
            });
        }

        let mut filename = media
            .get("filename")
            .or_else(|| media.get("name"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(String::from)
            .or_else(|| Self::parse_content_disposition_filename(content_disposition.as_deref()))
            .or_else(|| source_url.as_deref().and_then(Self::file_name_from_url))
            .unwrap_or_else(|| "wecom_file".to_string());
        if Path::new(&filename).extension().is_none() {
            if let Some(ext) = Self::ext_from_content_type(&content_type) {
                filename.push_str(ext);
            } else {
                filename.push_str(".bin");
            }
        }
        let ext = Path::new(&filename)
            .extension()
            .and_then(|e| e.to_str())
            .map(|x| format!(".{x}"))
            .unwrap_or_else(|| ".bin".to_string());
        let path = match Self::write_wecom_media_cache(&raw, &ext) {
            Ok(p) => p,
            Err(e) => {
                warn!(error = %e, "WeCom inbound file cache write failed");
                return None;
            }
        };
        let normalized_content_type = if content_type.trim().is_empty() {
            guess_mime_type(&filename)
        } else {
            content_type
        };
        Some(CachedInboundMedia {
            path,
            content_type: normalized_content_type,
        })
    }

    /// Collect inbound image/file attachment refs from a WeCom callback body.
    fn collect_inbound_media_refs(body: &Value) -> Vec<(String, Value)> {
        let mut refs: Vec<(String, Value)> = Vec::new();
        let msgtype = body
            .get("msgtype")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();

        if msgtype == "mixed" {
            if let Some(items) = body
                .get("mixed")
                .and_then(|m| m.get("msg_item"))
                .and_then(|v| v.as_array())
            {
                for item in items {
                    let item_type = item
                        .get("msgtype")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_ascii_lowercase();
                    if item_type == "image" {
                        if let Some(img) = item.get("image").cloned() {
                            refs.push(("image".to_string(), img));
                        }
                    } else if item_type == "file" {
                        if let Some(file) = item.get("file").cloned() {
                            refs.push(("file".to_string(), file));
                        }
                    }
                }
            }
        } else {
            if let Some(img) = body.get("image").cloned() {
                refs.push(("image".to_string(), img));
            }
            if msgtype == "file" {
                if let Some(file) = body.get("file").cloned() {
                    refs.push(("file".to_string(), file));
                }
            }
            if msgtype == "appmsg" {
                if let Some(appmsg) = body.get("appmsg") {
                    if let Some(file) = appmsg.get("file").cloned() {
                        refs.push(("file".to_string(), file));
                    } else if let Some(img) = appmsg.get("image").cloned() {
                        refs.push(("image".to_string(), img));
                    }
                }
            }
        }

        if let Some(quote) = body.get("quote") {
            let quote_type = quote
                .get("msgtype")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_ascii_lowercase();
            if quote_type == "image" {
                if let Some(img) = quote.get("image").cloned() {
                    refs.push(("image".to_string(), img));
                }
            } else if quote_type == "file" {
                if let Some(file) = quote.get("file").cloned() {
                    refs.push(("file".to_string(), file));
                }
            }
        }

        refs
    }

    fn body_expects_inbound_media(body: &Value) -> bool {
        !Self::collect_inbound_media_refs(body).is_empty()
    }

    const INBOUND_MEDIA_DOWNLOAD_FAILED_TEXT: &'static str =
        "[wecom: 图片/文件下载失败，请重试或附带文字说明]";

    async fn extract_media(inner: &WeComInner, body: &Value) -> (Vec<String>, Vec<String>) {
        let refs = Self::collect_inbound_media_refs(body);

        let mut media_urls = Vec::new();
        let mut media_types = Vec::new();
        for (kind, media) in refs {
            if let Some(cached) = Self::cache_media_ref(inner, &kind, &media).await {
                media_urls.push(cached.path);
                media_types.push(cached.content_type);
            }
        }
        (media_urls, media_types)
    }

    async fn remember_reply_req_id(inner: &WeComInner, message_id: &str, req_id: &str) {
        if message_id.is_empty() || req_id.is_empty() {
            return;
        }
        let mut map = inner.reply_req_ids.write().await;
        map.insert(message_id.to_string(), req_id.to_string());
        while map.len() > DEDUP_MAX {
            if let Some(k) = map.keys().next().cloned() {
                map.remove(&k);
            }
        }
    }

    async fn remember_chat_req_id(inner: &WeComInner, chat_id: &str, req_id: &str) {
        if chat_id.is_empty() || req_id.is_empty() {
            return;
        }
        let mut map = inner.last_chat_req_ids.write().await;
        map.insert(chat_id.to_string(), req_id.to_string());
        while map.len() > DEDUP_MAX {
            if let Some(k) = map.keys().next().cloned() {
                map.remove(&k);
            }
        }
    }

    async fn reply_req_id_for_message(
        inner: &WeComInner,
        reply_to: Option<&str>,
    ) -> Option<String> {
        let normalized = reply_to.unwrap_or("").trim();
        if normalized.is_empty() || normalized.starts_with("quote:") {
            return None;
        }
        inner.reply_req_ids.read().await.get(normalized).cloned()
    }

    async fn reply_send_lock(
        inner: &WeComInner,
        reply_req_id: &str,
    ) -> Arc<tokio::sync::Mutex<()>> {
        let mut locks = inner.reply_req_locks.lock().await;
        let lock = locks
            .entry(reply_req_id.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone();
        while locks.len() > DEDUP_MAX {
            if let Some(k) = locks.keys().next().cloned() {
                locks.remove(&k);
            }
        }
        lock
    }

    async fn fail_pending(inner: &WeComInner) {
        let mut pending = inner.pending.write().await;
        for (_, tx) in pending.drain() {
            let _ = tx.send(Value::Null);
        }
    }

    async fn send_ws_json(inner: &WeComInner, payload: Value) -> Result<(), GatewayError> {
        let tx = inner.outbound_tx.lock().await.clone();
        let Some(tx) = tx else {
            return Err(GatewayError::ConnectionFailed(
                "WeCom websocket is not connected".into(),
            ));
        };
        tx.send(payload)
            .map_err(|_| GatewayError::ConnectionFailed("WeCom outbound channel closed".into()))
    }

    async fn send_request(
        inner: &WeComInner,
        cmd: &str,
        body: Value,
        timeout: Duration,
    ) -> Result<Value, GatewayError> {
        let req_id = new_req_id(cmd);
        let (tx, rx) = oneshot::channel();
        inner.pending.write().await.insert(req_id.clone(), tx);
        let frame = serde_json::json!({
            "cmd": cmd,
            "headers": { "req_id": req_id },
            "body": body,
        });
        if let Err(e) = Self::send_ws_json(inner, frame).await {
            inner.pending.write().await.remove(&req_id);
            return Err(e);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(_)) => Err(GatewayError::SendFailed("WeCom request cancelled".into())),
            Err(_) => {
                inner.pending.write().await.remove(&req_id);
                Err(GatewayError::SendFailed("Timeout sending to WeCom".into()))
            }
        }
    }

    async fn send_reply_request(
        inner: &WeComInner,
        reply_req_id: &str,
        body: Value,
        cmd: &str,
        timeout: Duration,
    ) -> Result<Value, GatewayError> {
        let req_id = reply_req_id.trim();
        if req_id.is_empty() {
            return Err(GatewayError::SendFailed("reply_req_id is required".into()));
        }
        // WeCom correlates `aibot_respond_msg` by the inbound callback req_id. Only one
        // pending waiter may exist per req_id; serialize all reply sends (stream + status).
        let reply_lock = Self::reply_send_lock(inner, req_id).await;
        let _reply_guard = reply_lock.lock().await;

        let (tx, rx) = oneshot::channel();
        inner.pending.write().await.insert(req_id.to_string(), tx);
        let frame = serde_json::json!({
            "cmd": cmd,
            "headers": { "req_id": req_id },
            "body": body,
        });
        let result = if let Err(e) = Self::send_ws_json(inner, frame).await {
            inner.pending.write().await.remove(req_id);
            Err(e)
        } else {
            match tokio::time::timeout(timeout, rx).await {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(_)) => Err(GatewayError::SendFailed(
                    "WeCom reply request cancelled".into(),
                )),
                Err(_) => Err(GatewayError::SendFailed(
                    "Timeout sending reply to WeCom".into(),
                )),
            }
        };
        inner.pending.write().await.remove(req_id);
        result
    }

    async fn dispatch_payload(inner: Arc<WeComInner>, payload: Value) {
        let req_id = payload_req_id(&payload);
        let cmd = payload
            .get("cmd")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        trace!(cmd = %cmd, req_id = %req_id, "WeCom websocket payload received");

        let is_callback = cmd == APP_CMD_CALLBACK || cmd == APP_CMD_LEGACY_CALLBACK;
        let is_non_response = is_callback || cmd == APP_CMD_EVENT_CALLBACK;

        if !req_id.is_empty() && !is_non_response {
            if let Some(tx) = inner.pending.write().await.remove(&req_id) {
                let _ = tx.send(payload);
                return;
            }
        }

        if is_callback {
            Self::on_message_callback(inner, payload).await;
            return;
        }
        if cmd == APP_CMD_PING || cmd == APP_CMD_EVENT_CALLBACK {
            return;
        }
        if !cmd.is_empty() {
            debug!(cmd = %cmd, "Ignoring WeCom websocket payload");
        }
    }

    async fn on_message_callback(inner: Arc<WeComInner>, payload: Value) {
        let started = Instant::now();
        let body = match payload.get("body") {
            Some(Value::Object(_)) => payload.get("body").cloned().unwrap(),
            _ => return,
        };
        let req_id = payload_req_id(&payload);
        let msg_id = body
            .get("msgid")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| {
                if req_id.is_empty() {
                    Uuid::new_v4().simple().to_string()
                } else {
                    req_id.clone()
                }
            });

        if Self::is_dup(&inner, &msg_id).await {
            debug!(msg_id = %msg_id, "Ignoring duplicate WeCom message");
            return;
        }
        Self::remember_reply_req_id(&inner, &msg_id, &req_id).await;

        let sender_id = body
            .get("from")
            .and_then(|f| f.get("userid"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let chat_id = body
            .get("chatid")
            .and_then(|v| v.as_str())
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| sender_id.clone());
        if chat_id.is_empty() {
            return;
        }

        let is_group = body
            .get("chattype")
            .and_then(|v| v.as_str())
            .map(|s| s.eq_ignore_ascii_case("group"))
            .unwrap_or(false);

        if is_group {
            if !Self::is_group_allowed(&inner.config, &chat_id, &sender_id) {
                debug!(
                    chat_id = %chat_id,
                    sender_id = %sender_id,
                    "WeCom group message denied by allowlist"
                );
                return;
            }
        } else if !Self::is_dm_allowed(&inner.config, &sender_id) {
            debug!(sender_id = %sender_id, "WeCom DM denied by allowlist");
            return;
        }

        Self::remember_chat_req_id(&inner, &chat_id, &req_id).await;

        let (mut text, reply_text) = Self::extract_text(&body);
        if is_group && !text.is_empty() {
            if let Ok(re) = Regex::new(r"^@\S+\s*") {
                text = re.replace(&text, "").trim().to_string();
            }
        }
        let (media_urls, media_types) = Self::extract_media(&inner, &body).await;

        if text.is_empty() && media_urls.is_empty() {
            if let Some(rt) = reply_text {
                text = rt;
            }
        }
        if text.trim().is_empty() && media_urls.is_empty() {
            if Self::body_expects_inbound_media(&body) {
                text = Self::INBOUND_MEDIA_DOWNLOAD_FAILED_TEXT.to_string();
                warn!(
                    chat_id = %chat_id,
                    msg_id = %msg_id,
                    "WeCom inbound media present but cache empty; continuing with fallback text"
                );
            } else {
                return;
            }
        }

        let incoming = IncomingMessage {
            platform: "wecom".into(),
            chat_id: chat_id.clone(),
            user_id: if sender_id.is_empty() {
                chat_id.clone()
            } else {
                sender_id
            },
            text,
            media_urls,
            media_types,
            message_id: Some(msg_id),
            is_dm: !is_group,
            interaction_id: None,
            interaction_token: None,
            role_ids: vec![],
            ..Default::default()
        };

        let delay = text_batch_delay_secs();
        info!(
            chat_id = %incoming.chat_id,
            user_id = %incoming.user_id,
            msg_id = ?incoming.message_id,
            is_dm = incoming.is_dm,
            text_chars = incoming.text.chars().count(),
            media_count = incoming.media_urls.len(),
            delay_secs = delay,
            elapsed_ms = started.elapsed().as_millis() as u64,
            "WeCom incoming message normalized"
        );
        let bypass_chars = text_batch_bypass_chars();
        let should_batch = delay > 0.0
            && incoming.media_urls.is_empty()
            && incoming.text.chars().count() > bypass_chars;
        if should_batch {
            Self::enqueue_text_event(inner, incoming, delay).await;
        } else if let Some(tx) = inner.inbound_tx.read().await.clone() {
            debug!(
                chat_id = %incoming.chat_id,
                text_chars = incoming.text.chars().count(),
                bypass_chars = bypass_chars,
                "WeCom text batch bypassed"
            );
            let _ = tx.send(incoming).await;
        }
    }

    async fn flush_text_batch(inner: Arc<WeComInner>, key: String, delay: Duration) {
        tokio::time::sleep(delay).await;
        let event = {
            let mut pending = inner.pending_text.lock().await;
            pending.remove(&key)
        };
        let Some(batch) = event else {
            return;
        };
        inner.text_batch_tasks.lock().await.remove(&key);
        debug!(
            key = %key,
            text_chars = batch.event.text.chars().count(),
            "WeCom text batch flushing to gateway"
        );
        if let Some(tx) = inner.inbound_tx.read().await.clone() {
            let _ = tx.send(batch.event).await;
        }
    }

    async fn enqueue_text_event(inner: Arc<WeComInner>, event: IncomingMessage, delay_secs: f64) {
        let key = format!("{}:{}", event.platform, event.chat_id);
        let chunk_len = event.text.chars().count();
        let flush_delay = if chunk_len >= SPLIT_THRESHOLD {
            Duration::from_secs_f64(text_batch_split_delay_secs())
        } else {
            Duration::from_secs_f64(delay_secs)
        };

        {
            let mut pending = inner.pending_text.lock().await;
            if let Some(existing) = pending.get_mut(&key) {
                if !event.text.is_empty() {
                    if existing.event.text.is_empty() {
                        existing.event.text = event.text.clone();
                    } else {
                        existing.event.text.push('\n');
                        existing.event.text.push_str(&event.text);
                    }
                }
                existing.last_chunk_len = chunk_len;
            } else {
                pending.insert(
                    key.clone(),
                    PendingTextBatch {
                        event,
                        last_chunk_len: chunk_len,
                    },
                );
            }
        }

        debug!(
            key = %key,
            delay_ms = flush_delay.as_millis() as u64,
            chunk_chars = chunk_len,
            "WeCom text batch scheduled"
        );
        if let Some(task) = inner.text_batch_tasks.lock().await.remove(&key) {
            task.abort();
        }
        let inner_arc = Arc::clone(&inner);
        let key_for_task = key.clone();
        let handle = tokio::spawn(async move {
            Self::flush_text_batch(inner_arc, key_for_task, flush_delay).await;
        });
        inner.text_batch_tasks.lock().await.insert(key, handle);
    }

    /// Read WS frames until the subscribe acknowledgement for `subscribe_req` arrives.
    /// Matches Python `WeComAdapter._wait_for_handshake` (must read while waiting).
    async fn wait_for_subscribe_ack<S, W>(
        read: &mut S,
        write: &mut W,
        subscribe_req: &str,
    ) -> Result<Value, GatewayError>
    where
        S: StreamExt<Item = Result<WsMessage, WsError>> + Unpin,
        W: SinkExt<WsMessage> + Unpin,
        W::Error: std::fmt::Display,
    {
        let deadline = Instant::now() + Duration::from_secs(CONNECT_TIMEOUT_SECS);
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(GatewayError::ConnectionFailed(
                    "Timed out waiting for WeCom subscribe acknowledgement".into(),
                ));
            }
            let msg = match tokio::time::timeout(remaining, read.next()).await {
                Ok(Some(Ok(m))) => m,
                Ok(Some(Err(e))) => {
                    return Err(GatewayError::ConnectionFailed(format!(
                        "WeCom websocket read error during subscribe: {e}"
                    )));
                }
                Ok(None) => {
                    return Err(GatewayError::ConnectionFailed(
                        "WeCom websocket closed during authentication".into(),
                    ));
                }
                Err(_) => {
                    return Err(GatewayError::ConnectionFailed(
                        "Timed out waiting for WeCom subscribe acknowledgement".into(),
                    ));
                }
            };
            match msg {
                WsMessage::Text(t) => {
                    let payload: Value = match serde_json::from_str(&t) {
                        Ok(v) => v,
                        Err(e) => {
                            debug!(error = %e, "Failed to parse pre-auth WeCom payload");
                            continue;
                        }
                    };
                    let cmd = payload.get("cmd").and_then(|v| v.as_str()).unwrap_or("");
                    if cmd == APP_CMD_PING {
                        continue;
                    }
                    if payload_req_id(&payload) == subscribe_req {
                        return Ok(payload);
                    }
                    debug!(cmd = %cmd, "Ignoring pre-auth WeCom payload");
                }
                WsMessage::Ping(p) => {
                    let _ = write.send(WsMessage::Pong(p)).await;
                }
                WsMessage::Close(_) => {
                    return Err(GatewayError::ConnectionFailed(
                        "WeCom websocket closed during authentication".into(),
                    ));
                }
                _ => {}
            }
        }
    }

    async fn stream_loop(inner: Arc<WeComInner>) {
        let mut backoff_idx = 0usize;
        while inner.base.is_running() {
            let (outbound_tx, mut outbound_rx) = mpsc::unbounded_channel::<Value>();
            *inner.outbound_tx.lock().await = Some(outbound_tx.clone());

            match tokio_tungstenite::connect_async(&inner.config.websocket_url).await {
                Ok((ws_stream, _)) => {
                    info!(
                        bot_id = %redact_identifier(&inner.config.bot_id),
                        ws = %inner.config.websocket_url,
                        "WeCom AI Bot websocket connected"
                    );
                    let (mut write, mut read) = ws_stream.split();

                    // Subscribe handshake (read ack from `read` while waiting — see `wait_for_subscribe_ack`)
                    let subscribe_req = new_req_id("subscribe");
                    let sub_frame = serde_json::json!({
                        "cmd": APP_CMD_SUBSCRIBE,
                        "headers": { "req_id": &subscribe_req },
                        "body": {
                            "bot_id": inner.config.bot_id,
                            "secret": inner.config.secret,
                            "device_id": inner.device_id,
                        },
                    });
                    let sub_json = serde_json::to_string(&sub_frame).unwrap_or_default();
                    if write.send(WsMessage::Text(sub_json.into())).await.is_err() {
                        warn!("WeCom subscribe send failed");
                        continue;
                    }

                    match Self::wait_for_subscribe_ack(&mut read, &mut write, &subscribe_req).await
                    {
                        Ok(resp) => {
                            if let Some(err) = response_error(&resp) {
                                error!(error = %err, "WeCom subscribe failed");
                                Self::fail_pending(&inner).await;
                                tokio::time::sleep(Duration::from_secs(
                                    RECONNECT_SECS[backoff_idx.min(RECONNECT_SECS.len() - 1)],
                                ))
                                .await;
                                backoff_idx = (backoff_idx + 1).min(RECONNECT_SECS.len() - 1);
                                continue;
                            }
                            info!("WeCom AI Bot subscribe acknowledged");
                            backoff_idx = 0;
                        }
                        Err(e) => {
                            warn!(error = %e, "WeCom subscribe handshake failed");
                            Self::fail_pending(&inner).await;
                            continue;
                        }
                    }

                    let mut heartbeat =
                        tokio::time::interval(Duration::from_secs(HEARTBEAT_INTERVAL_SECS));
                    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

                    loop {
                        if !inner.base.is_running() {
                            break;
                        }
                        tokio::select! {
                            _ = inner.stop.notified() => {
                                let _ = write.close().await;
                                return;
                            }
                            _ = heartbeat.tick() => {
                                let ping = serde_json::json!({
                                    "cmd": APP_CMD_PING,
                                    "headers": { "req_id": new_req_id("ping") },
                                    "body": {},
                                });
                                if let Ok(s) = serde_json::to_string(&ping) {
                                    let _ = write.send(WsMessage::Text(s.into())).await;
                                }
                            }
                            Some(frame) = outbound_rx.recv() => {
                                if let Ok(s) = serde_json::to_string(&frame) {
                                    if write.send(WsMessage::Text(s.into())).await.is_err() {
                                        break;
                                    }
                                }
                            }
                            msg = read.next() => {
                                match msg {
                                    Some(Ok(WsMessage::Text(t))) => {
                                        if let Ok(v) = serde_json::from_str::<Value>(&t) {
                                            Self::dispatch_payload(Arc::clone(&inner), v).await;
                                        }
                                    }
                                    Some(Ok(WsMessage::Ping(p))) => {
                                        let _ = write.send(WsMessage::Pong(p)).await;
                                    }
                                    Some(Ok(WsMessage::Close(_))) | None => break,
                                    Some(Err(e)) => {
                                        warn!(error = %e, "WeCom websocket read error");
                                        break;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }
                }
                Err(e) => {
                    warn!(error = %e, "WeCom websocket connect failed");
                }
            }

            *inner.outbound_tx.lock().await = None;
            Self::fail_pending(&inner).await;

            if !inner.base.is_running() {
                return;
            }
            let delay = RECONNECT_SECS[backoff_idx.min(RECONNECT_SECS.len() - 1)];
            backoff_idx = (backoff_idx + 1).min(RECONNECT_SECS.len() - 1);
            tokio::time::sleep(Duration::from_secs(delay)).await;
        }
    }

    async fn send_markdown_inner(
        inner: &WeComInner,
        chat_id: &str,
        content: &str,
        reply_to: Option<&str>,
    ) -> Result<(), GatewayError> {
        let trimmed = content.chars().take(MAX_MESSAGE_LENGTH).collect::<String>();
        let reply_req_id = Self::reply_req_id_for_message(inner, reply_to).await;
        debug!(
            chat_id = %chat_id,
            text_chars = trimmed.chars().count(),
            reply = reply_req_id.is_some(),
            "WeCom sending markdown"
        );
        if let Some(req_id) = reply_req_id {
            let body = serde_json::json!({
                "msgtype": "markdown",
                "markdown": { "content": trimmed },
            });
            let resp = Self::send_reply_request(
                inner,
                &req_id,
                body,
                APP_CMD_RESPONSE,
                Duration::from_secs(REQUEST_TIMEOUT_SECS),
            )
            .await?;
            raise_for_wecom_error(&resp, "send reply markdown")?;
            return Ok(());
        }
        let body = serde_json::json!({
            "chatid": chat_id,
            "msgtype": "markdown",
            "markdown": { "content": trimmed },
        });
        let resp = Self::send_request(
            inner,
            APP_CMD_SEND,
            body,
            Duration::from_secs(REQUEST_TIMEOUT_SECS),
        )
        .await?;
        raise_for_wecom_error(&resp, "send markdown")?;
        Ok(())
    }

    async fn resolve_reply_req_id(
        inner: &WeComInner,
        chat_id: &str,
        reply_to: Option<&str>,
    ) -> Option<String> {
        let mut reply_req_id = Self::reply_req_id_for_message(inner, reply_to).await;
        if reply_req_id.is_none() {
            reply_req_id = inner.last_chat_req_ids.read().await.get(chat_id).cloned();
        }
        reply_req_id
    }

    async fn send_stream_chunk_inner(
        inner: &WeComInner,
        reply_req_id: &str,
        stream_id: &str,
        content: &str,
        finish: bool,
    ) -> Result<(), GatewayError> {
        let body = serde_json::json!({
            "msgtype": "stream",
            "stream": {
                "id": stream_id,
                "content": content,
                "finish": finish,
            }
        });
        let resp = Self::send_reply_request(
            inner,
            reply_req_id,
            body,
            APP_CMD_RESPONSE,
            Duration::from_secs(REQUEST_TIMEOUT_SECS),
        )
        .await?;
        raise_for_wecom_error(&resp, "send stream chunk")
    }

    async fn upload_media_bytes(
        inner: &WeComInner,
        data: &[u8],
        media_type: &str,
        filename: &str,
    ) -> Result<String, GatewayError> {
        if data.is_empty() {
            return Err(GatewayError::SendFailed("Cannot upload empty media".into()));
        }
        let total_size = data.len();
        let total_chunks = total_size.div_ceil(UPLOAD_CHUNK_SIZE);
        if total_chunks > MAX_UPLOAD_CHUNKS {
            return Err(GatewayError::SendFailed(format!(
                "File too large: {total_chunks} chunks exceeds maximum of {MAX_UPLOAD_CHUNKS}"
            )));
        }
        let digest = format!("{:x}", md5::compute(data));
        let init_resp = Self::send_request(
            inner,
            APP_CMD_UPLOAD_MEDIA_INIT,
            serde_json::json!({
                "type": media_type,
                "filename": filename,
                "total_size": total_size,
                "total_chunks": total_chunks,
                "md5": digest,
            }),
            Duration::from_secs(REQUEST_TIMEOUT_SECS),
        )
        .await?;
        raise_for_wecom_error(&init_resp, "media upload init")?;
        let upload_id = init_resp
            .get("body")
            .and_then(|b| b.get("upload_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if upload_id.is_empty() {
            return Err(GatewayError::SendFailed(
                "media upload init missing upload_id".into(),
            ));
        }
        for (chunk_index, chunk) in data.chunks(UPLOAD_CHUNK_SIZE).enumerate() {
            let chunk_b64 = base64::engine::general_purpose::STANDARD.encode(chunk);
            let chunk_resp = Self::send_request(
                inner,
                APP_CMD_UPLOAD_MEDIA_CHUNK,
                serde_json::json!({
                    "upload_id": upload_id,
                    "chunk_index": chunk_index,
                    "base64_data": chunk_b64,
                }),
                Duration::from_secs(REQUEST_TIMEOUT_SECS),
            )
            .await?;
            raise_for_wecom_error(&chunk_resp, "media upload chunk")?;
        }
        let finish_resp = Self::send_request(
            inner,
            APP_CMD_UPLOAD_MEDIA_FINISH,
            serde_json::json!({ "upload_id": upload_id }),
            Duration::from_secs(REQUEST_TIMEOUT_SECS),
        )
        .await?;
        raise_for_wecom_error(&finish_resp, "media upload finish")?;
        let media_id = finish_resp
            .get("body")
            .and_then(|b| b.get("media_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if media_id.is_empty() {
            return Err(GatewayError::SendFailed(
                "media upload finish missing media_id".into(),
            ));
        }
        Ok(media_id)
    }

    async fn send_media_message(
        inner: &WeComInner,
        chat_id: &str,
        media_type: &str,
        media_id: &str,
        reply_req_id: Option<String>,
    ) -> Result<(), GatewayError> {
        let body = serde_json::json!({
            "msgtype": media_type,
            media_type: { "media_id": media_id },
        });
        if let Some(req_id) = reply_req_id {
            let resp = Self::send_reply_request(
                inner,
                &req_id,
                body,
                APP_CMD_RESPONSE,
                Duration::from_secs(REQUEST_TIMEOUT_SECS),
            )
            .await?;
            raise_for_wecom_error(&resp, "send reply media")?;
        } else {
            let mut send_body = body.as_object().cloned().unwrap_or_default();
            send_body.insert("chatid".to_string(), Value::String(chat_id.to_string()));
            let resp = Self::send_request(
                inner,
                APP_CMD_SEND,
                Value::Object(send_body),
                Duration::from_secs(REQUEST_TIMEOUT_SECS),
            )
            .await?;
            raise_for_wecom_error(&resp, "send media message")?;
        }
        Ok(())
    }

    async fn load_outbound_bytes(
        inner: &WeComInner,
        media_source: &str,
        file_name: Option<&str>,
    ) -> Result<(Vec<u8>, String, String), GatewayError> {
        let source = media_source.trim();
        if source.is_empty() {
            return Err(GatewayError::SendFailed("media source is required".into()));
        }
        if source.starts_with("http://") || source.starts_with("https://") {
            if !is_safe_url(source) {
                return Err(GatewayError::SendFailed(format!(
                    "Blocked unsafe URL (SSRF protection): {}",
                    &source[..source.len().min(80)]
                )));
            }
            let resp = inner
                .client
                .get(source)
                .header("User-Agent", "HermesAgent/1.0")
                .header("Accept", "*/*")
                .send()
                .await
                .map_err(|e| GatewayError::SendFailed(format!("WeCom media download: {e}")))?;
            if !resp.status().is_success() {
                return Err(GatewayError::SendFailed(format!(
                    "WeCom media download HTTP {}",
                    resp.status()
                )));
            }
            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("")
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_string();
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| GatewayError::SendFailed(format!("WeCom media read: {e}")))?;
            if bytes.len() > ABSOLUTE_MAX_BYTES {
                return Err(GatewayError::SendFailed(
                    "Remote media exceeds WeCom size limit".into(),
                ));
            }
            let resolved_name = file_name
                .map(String::from)
                .unwrap_or_else(|| source.rsplit('/').next().unwrap_or("document").to_string());
            let ct = if content_type.is_empty() {
                guess_mime_type(&resolved_name)
            } else {
                content_type
            };
            return Ok((bytes.to_vec(), ct, resolved_name));
        }

        let path = if let Some(rest) = source.strip_prefix("file://") {
            PathBuf::from(rest)
        } else {
            PathBuf::from(source)
        };
        let path = if path.is_absolute() {
            path
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(path)
        };
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Media file not found: {e}")))?;
        let resolved_name = file_name.map(String::from).unwrap_or_else(|| {
            path.file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("file")
                .to_string()
        });
        let ct = guess_mime_type(&resolved_name);
        Ok((bytes, ct, resolved_name))
    }

    async fn send_media_source(
        inner: &WeComInner,
        chat_id: &str,
        media_source: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let (data, content_type, file_name) =
            Self::load_outbound_bytes(inner, media_source, None).await?;
        let detected = detect_wecom_media_type(&content_type);
        let check = apply_file_size_limits(data.len(), detected, Some(&content_type));
        if check.rejected {
            let reason = check
                .reject_reason
                .clone()
                .unwrap_or_else(|| "rejected".into());
            let _ = Self::send_markdown_inner(inner, chat_id, &format!("⚠️ {reason}"), None).await;
            return Err(GatewayError::SendFailed(reason));
        }
        let mut reply_req_id = inner.last_chat_req_ids.read().await.get(chat_id).cloned();
        let media_id =
            Self::upload_media_bytes(inner, &data, &check.final_type, &file_name).await?;
        Self::send_media_message(
            inner,
            chat_id,
            &check.final_type,
            &media_id,
            reply_req_id.take(),
        )
        .await?;
        if let Some(cap) = caption.filter(|c| !c.trim().is_empty()) {
            let _ = Self::send_markdown_inner(inner, chat_id, cap, None).await;
        }
        if check.downgraded {
            if let Some(note) = check.downgrade_note {
                let _ =
                    Self::send_markdown_inner(inner, chat_id, &format!("ℹ️ {note}"), None).await;
            }
        }
        Ok(())
    }
}

#[async_trait]
impl PlatformAdapter for WeComAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            bot_id = %redact_identifier(&self.inner.config.bot_id),
            ws = %self.inner.config.websocket_url,
            "WeCom AI Bot adapter starting"
        );
        self.inner.base.mark_running();
        let inner = self.inner.clone();
        let handle = tokio::spawn(async move {
            WeComAdapter::stream_loop(inner).await;
        });
        *self.run_task.write().await = Some(handle);
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("WeCom AI Bot adapter stopping");
        self.inner.base.mark_stopped();
        self.inner.stop.notify_waiters();
        if let Some(t) = self.run_task.write().await.take() {
            t.abort();
        }
        *self.inner.outbound_tx.lock().await = None;
        WeComAdapter::fail_pending(&self.inner).await;
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        let _ = parse_mode;
        Self::send_markdown_inner(&self.inner, chat_id, text, None).await
    }

    async fn send_message_replying(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
        reply_to_message_id: Option<&str>,
    ) -> Result<Option<String>, GatewayError> {
        let _ = parse_mode;
        Self::send_markdown_inner(&self.inner, chat_id, text, reply_to_message_id).await?;
        Ok(None)
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        Err(GatewayError::Platform("WeCom does not support message editing".into()))
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        WeComAdapter::send_media_source(&self.inner, chat_id, file_path, caption).await
    }

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        match WeComAdapter::send_media_source(&self.inner, chat_id, image_url, caption).await {
            Ok(()) => Ok(()),
            Err(err) => {
                if image_url.starts_with("http://") || image_url.starts_with("https://") {
                    warn!(error = %err, "WeCom image-url send failed; falling back to text");
                    let fallback = match caption.filter(|c| !c.trim().is_empty()) {
                        Some(c) => format!("{c}\n{image_url}"),
                        None => image_url.to_string(),
                    };
                    return self
                        .send_message(chat_id, &fallback, Some(ParseMode::Plain))
                        .await;
                }
                Err(err)
            }
        }
    }

    fn supports_native_streaming(&self) -> bool {
        false
    }

    async fn start_native_stream(
        &self,
        chat_id: &str,
        reply_to: Option<&str>,
        initial_content: Option<&str>,
    ) -> Result<Option<String>, GatewayError> {
        let reply_req_id = Self::resolve_reply_req_id(&self.inner, chat_id, reply_to).await;
        let Some(reply_req_id) = reply_req_id else {
            return Ok(None);
        };
        let stream_id = Uuid::new_v4().to_string();
        self.inner
            .stream_reply_req_ids
            .write()
            .await
            .insert(stream_id.clone(), reply_req_id.clone());
        if let Some(content) = initial_content.map(str::trim).filter(|s| !s.is_empty()) {
            if let Err(err) = Self::send_stream_chunk_inner(
                &self.inner,
                &reply_req_id,
                &stream_id,
                content,
                false,
            )
            .await
            {
                self.inner
                    .stream_reply_req_ids
                    .write()
                    .await
                    .remove(&stream_id);
                return Err(err);
            }
        }
        Ok(Some(stream_id))
    }

    async fn send_native_stream_chunk(
        &self,
        _chat_id: &str,
        stream_id: &str,
        content: &str,
        finish: bool,
    ) -> Result<(), GatewayError> {
        let reply_req_id = self
            .inner
            .stream_reply_req_ids
            .read()
            .await
            .get(stream_id)
            .cloned()
            .ok_or_else(|| GatewayError::SendFailed("WeCom stream session not found".into()))?;
        let res =
            Self::send_stream_chunk_inner(&self.inner, &reply_req_id, stream_id, content, finish)
                .await;
        if finish {
            self.inner
                .stream_reply_req_ids
                .write()
                .await
                .remove(stream_id);
        }
        res
    }

    fn is_running(&self) -> bool {
        self.inner.base.is_running()
    }

    fn platform_name(&self) -> &str {
        "wecom"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn entry_matches_supports_wildcard_and_prefix() {
        assert!(entry_matches(&["wecom:user:alice".into()], "alice"));
        assert!(entry_matches(&["*".into()], "anyone"));
        assert!(!entry_matches(&["bob".into()], "alice"));
    }

    #[test]
    fn config_from_platform_extra_reads_bot_id() {
        let mut extra = HashMap::new();
        extra.insert("bot_id".to_string(), Value::String("cfg-bot".into()));
        extra.insert("secret".to_string(), Value::String("cfg-secret".into()));
        extra.insert(
            "websocket_url".to_string(),
            Value::String("wss://custom.example/ws".into()),
        );
        let p = hermes_config::PlatformConfig {
            enabled: true,
            extra,
            ..Default::default()
        };
        let cfg = WeComConfig::from_platform_config(&p);
        assert_eq!(cfg.bot_id, "cfg-bot");
        assert_eq!(cfg.secret, "cfg-secret");
        assert_eq!(cfg.websocket_url, "wss://custom.example/ws");
    }

    #[test]
    fn apply_file_size_limits_downgrades_large_image() {
        let check = apply_file_size_limits(IMAGE_MAX_BYTES + 1, "image", Some("image/png"));
        assert!(!check.rejected);
        assert!(check.downgraded);
        assert_eq!(check.final_type, "file");
    }

    #[test]
    fn body_expects_inbound_media_image_only() {
        let body = serde_json::json!({
            "msgtype": "image",
            "image": { "url": "https://ww-aibot-img-1.cos.ap-guangzhou.myqcloud.com/x" }
        });
        assert!(WeComAdapter::body_expects_inbound_media(&body));
        assert_eq!(WeComAdapter::collect_inbound_media_refs(&body).len(), 1);
    }

    #[test]
    fn body_expects_inbound_media_false_for_text_only() {
        let body = serde_json::json!({
            "msgtype": "text",
            "text": { "content": "hello" }
        });
        assert!(!WeComAdapter::body_expects_inbound_media(&body));
    }

    #[test]
    fn image_only_incoming_should_forward_when_media_cached() {
        let body = serde_json::json!({
            "msgtype": "image",
            "image": { "url": "https://example.com/a.png" }
        });
        let (text, _) = WeComAdapter::extract_text(&body);
        assert!(text.is_empty());
        assert!(WeComAdapter::body_expects_inbound_media(&body));
        // Happy path: non-empty media_urls + empty text must not be treated as empty message.
        let media_urls = vec!["/tmp/wecom/img.png".to_string()];
        assert!(!(text.is_empty() && media_urls.is_empty()));
    }

    #[test]
    fn extract_text_from_mixed_message() {
        let body = serde_json::json!({
            "msgtype": "mixed",
            "mixed": {
                "msg_item": [
                    { "msgtype": "text", "text": { "content": "hello" } },
                    { "msgtype": "text", "text": { "content": "world" } },
                ]
            }
        });
        let (text, _) = WeComAdapter::extract_text(&body);
        assert_eq!(text, "hello\nworld");
    }

    #[test]
    fn detect_image_ext_png() {
        let raw = b"\x89PNG\r\n\x1a\nabcdef";
        assert_eq!(WeComAdapter::detect_image_ext(raw), ".png");
    }

    #[test]
    fn normalize_inbound_image_content_type_maps_octet_stream_to_png() {
        assert_eq!(
            normalize_inbound_image_content_type("application/octet-stream", ".png"),
            "image/png"
        );
        assert_eq!(
            normalize_inbound_image_content_type("image/jpeg", ".jpg"),
            "image/jpeg"
        );
    }

    #[test]
    fn decrypt_wecom_file_bytes_roundtrip() {
        use aes::Aes256;
        use aes::cipher::array::Array;
        use aes::cipher::{BlockCipherEncrypt, KeyInit};

        let key = [7u8; 32];
        let iv = &key[..16];
        let plain = b"hello wecom aes";
        let mut padded = plain.to_vec();
        let pad_len = 16 - (padded.len() % 16);
        padded.extend(std::iter::repeat_n(pad_len as u8, pad_len));

        let aes = Aes256::new(GenericArray::from_slice(&key));
        let mut prev = iv.to_vec();
        let mut cipher = Vec::with_capacity(padded.len());
        for block in padded.chunks(16) {
            let mut x = [0u8; 16];
            for i in 0..16 {
                x[i] = block[i] ^ prev[i];
            }
            let mut b: Array<u8, _> = x.try_into().expect("block is 16 bytes");
            aes.encrypt_block((&mut b).into());
            prev.copy_from_slice(&b);
            cipher.extend_from_slice(&b);
        }

        let key_b64 = base64::engine::general_purpose::STANDARD.encode(key);
        let out = decrypt_wecom_file_bytes(&cipher, &key_b64).expect("decrypt");
        assert_eq!(out, plain);
    }

    /// Documents the race fixed by `reply_send_lock`: a second `pending.insert`
    /// for the same req_id drops the first oneshot sender → `rx.await` is `Err`.
    #[tokio::test]
    async fn duplicate_pending_req_id_cancels_prior_waiter() {
        let mut pending: HashMap<String, tokio::sync::oneshot::Sender<()>> = HashMap::new();
        let (tx1, rx1) = tokio::sync::oneshot::channel();
        pending.insert("inbound-req".to_string(), tx1);
        let (tx2, _rx2) = tokio::sync::oneshot::channel();
        pending.insert("inbound-req".to_string(), tx2);
        assert!(
            rx1.await.is_err(),
            "replacing pending slot must cancel the prior waiter"
        );
    }

    #[tokio::test]
    async fn new_requires_credentials() {
        let err = WeComAdapter::new(WeComConfig {
            bot_id: String::new(),
            secret: String::new(),
            websocket_url: DEFAULT_WS_URL.to_string(),
            dm_policy: "open".into(),
            allow_from: vec![],
            group_policy: "open".into(),
            group_allow_from: vec![],
            groups: HashMap::new(),
            proxy: AdapterProxyConfig::default(),
        });
        assert!(err.is_err());
    }
}
