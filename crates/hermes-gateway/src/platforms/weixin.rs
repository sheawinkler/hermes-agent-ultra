//! WeChat **iLink Bot** adapter (`ilinkai.weixin.qq.com`).
//!
//! Aligns with Python `gateway/platforms/weixin.py`: long-poll `getupdates`,
//! `context_token` echo on send, AES-128-ECB CDN download, DM/group policies.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::Engine;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{mpsc, Mutex, Notify, RwLock};
use tracing::{debug, info, warn};
use url::Url;
use rand::Rng;
use uuid::Uuid;

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, BlockEncrypt, KeyInit};
use aes::Aes128;

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};
use crate::gateway::IncomingMessage;
use crate::platforms::helpers::split_long_message;

const ILINK_BASE_URL: &str = "https://ilinkai.weixin.qq.com";
const WEIXIN_CDN_BASE: &str = "https://novac2c.cdn.weixin.qq.com/c2c";
const ILINK_APP_ID: &str = "bot";
const CHANNEL_VERSION: &str = "2.2.0";
const ILINK_APP_CLIENT_VERSION: i32 = (2 << 16) | (2 << 8) | 0;

const EP_GET_UPDATES: &str = "ilink/bot/getupdates";
const EP_SEND_MESSAGE: &str = "ilink/bot/sendmessage";

const LONG_POLL_TIMEOUT_MS: u64 = 35_000;
const API_TIMEOUT_MS: u64 = 15_000;
const SESSION_EXPIRED: i64 = -14;
const MSG_TYPE_BOT: i32 = 2;
const MSG_STATE_FINISH: i32 = 2;
const ITEM_TEXT: i32 = 1;
const DEDUP_TTL: Duration = Duration::from_secs(300);
const MAX_TEXT: usize = 4000;

const EP_GET_UPLOAD_URL: &str = "ilink/bot/getuploadurl";
const MEDIA_IMAGE: i32 = 1;
const MEDIA_VIDEO: i32 = 2;
const MEDIA_FILE: i32 = 3;
const ITEM_IMAGE: i32 = 2;
const ITEM_FILE: i32 = 4;
const ITEM_VIDEO: i32 = 5;
const ITEM_VOICE: i32 = 3;

fn default_base_url() -> String {
    ILINK_BASE_URL.to_string()
}

fn default_cdn_base_url() -> String {
    WEIXIN_CDN_BASE.to_string()
}

fn default_dm_policy() -> String {
    "open".into()
}

fn default_group_policy() -> String {
    "disabled".into()
}

fn random_wechat_uin() -> String {
    let u = Uuid::new_v4();
    let b = u.as_bytes();
    let n = u32::from_be_bytes([b[0], b[1], b[2], b[3]]);
    base64::engine::general_purpose::STANDARD.encode(n.to_string())
}

fn pkcs7_pad(data: &[u8], block_size: usize) -> Vec<u8> {
    let mut pad_len = block_size - (data.len() % block_size);
    if pad_len == 0 {
        pad_len = block_size;
    }
    let mut out = data.to_vec();
    out.extend(std::iter::repeat_n(pad_len as u8, pad_len));
    out
}

fn aes_padded_size(size: usize) -> usize {
    ((size + 1 + 15) / 16) * 16
}

fn pkcs7_unpad(padded: &[u8]) -> Result<Vec<u8>, GatewayError> {
    if padded.is_empty() {
        return Ok(Vec::new());
    }
    let pad_len = padded[padded.len() - 1] as usize;
    if (1..=16).contains(&pad_len)
        && padded.len() >= pad_len
        && padded[padded.len() - pad_len..]
            .iter()
            .all(|&b| b == pad_len as u8)
    {
        Ok(padded[..padded.len() - pad_len].to_vec())
    } else {
        Ok(padded.to_vec())
    }
}

fn aes128_ecb_encrypt(plaintext: &[u8], key_bytes: &[u8; 16]) -> Vec<u8> {
    let key = GenericArray::from_slice(key_bytes);
    let cipher = Aes128::new(key);
    let padded = pkcs7_pad(plaintext, 16);
    let mut out = Vec::with_capacity(padded.len());
    for chunk in padded.chunks(16) {
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.encrypt_block(&mut block);
        out.extend_from_slice(&block);
    }
    out
}

fn aes128_ecb_decrypt(ciphertext: &[u8], key_bytes: &[u8; 16]) -> Result<Vec<u8>, GatewayError> {
    if ciphertext.is_empty() || ciphertext.len() % 16 != 0 {
        return Err(GatewayError::Platform(
            "weixin: invalid AES ciphertext length".into(),
        ));
    }
    let key = GenericArray::from_slice(key_bytes);
    let cipher = Aes128::new(key);
    let mut padded = Vec::with_capacity(ciphertext.len());
    for chunk in ciphertext.chunks(16) {
        let mut block = GenericArray::clone_from_slice(chunk);
        cipher.decrypt_block(&mut block);
        padded.extend_from_slice(&block);
    }
    pkcs7_unpad(&padded)
}

fn parse_aes_key(aes_key_b64: &str) -> Result<[u8; 16], GatewayError> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(aes_key_b64.trim())
        .map_err(|e| GatewayError::Platform(format!("weixin aes_key base64: {e}")))?;
    if decoded.len() == 16 {
        let mut k = [0u8; 16];
        k.copy_from_slice(&decoded);
        return Ok(k);
    }
    if decoded.len() == 32 {
        let text = String::from_utf8_lossy(&decoded);
        if text
            .chars()
            .all(|c| c.is_ascii_hexdigit())
            && text.len() == 32
        {
            let mut k = [0u8; 16];
            for i in 0..16 {
                let pair = &text[i * 2..i * 2 + 2];
                k[i] = u8::from_str_radix(pair, 16).map_err(|_| {
                    GatewayError::Platform("weixin: invalid hex in aes key".into())
                })?;
            }
            return Ok(k);
        }
    }
    Err(GatewayError::Platform(format!(
        "weixin: unexpected aes_key format ({} decoded bytes)",
        decoded.len()
    )))
}

fn cdn_download_url(cdn_base: &str, encrypted_query_param: &str) -> Result<String, GatewayError> {
    let b = cdn_base.trim_end_matches('/');
    let u = format!("{b}/download");
    let mut url = Url::parse(&u).map_err(|e| {
        GatewayError::ConnectionFailed(format!("weixin cdn download url: {e}"))
    })?;
    url.query_pairs_mut()
        .append_pair("encrypted_query_param", encrypted_query_param);
    Ok(url.into())
}

fn cdn_upload_url(cdn_base: &str, upload_param: &str, filekey: &str) -> Result<String, GatewayError> {
    let b = cdn_base.trim_end_matches('/');
    let u = format!("{b}/upload");
    let mut url = Url::parse(&u).map_err(|e| {
        GatewayError::ConnectionFailed(format!("weixin cdn upload url: {e}"))
    })?;
    url.query_pairs_mut()
        .append_pair("encrypted_query_param", upload_param)
        .append_pair("filekey", filekey);
    Ok(url.into())
}

fn mime_from_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .as_deref()
    {
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("png") => "image/png",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        Some("mp4") | Some("m4v") => "video/mp4",
        Some("mov") => "video/quicktime",
        _ => "application/octet-stream",
    }
}

/// iLink WeChat configuration (mirrors Python `extra` + env names in docs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeixinConfig {
    pub account_id: String,
    pub token: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_cdn_base_url")]
    pub cdn_base_url: String,
    #[serde(default = "default_dm_policy")]
    pub dm_policy: String,
    #[serde(default = "default_group_policy")]
    pub group_policy: String,
    #[serde(default)]
    pub allow_from: Vec<String>,
    #[serde(default)]
    pub group_allow_from: Vec<String>,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

impl WeixinConfig {
    /// 从 [`hermes_config::PlatformConfig`] 构建（`token` + `extra` 键名与 Python `WeixinAdapter` 一致）。
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
        let list = |k: &str| -> Vec<String> {
            match ex.get(k) {
                Some(Value::String(s)) => s
                    .split(',')
                    .map(|x| x.trim().to_string())
                    .filter(|x| !x.is_empty())
                    .collect(),
                Some(Value::Array(a)) => a
                    .iter()
                    .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                    .filter(|x| !x.is_empty())
                    .collect(),
                _ => vec![],
            }
        };
        let account_id = gv("account_id");
        let mut token = p.token.clone().unwrap_or_default();
        if token.is_empty() {
            token = gv("token");
        }
        let base_url = {
            let s = gv("base_url");
            if s.is_empty() {
                default_base_url()
            } else {
                s
            }
        };
        let cdn_base_url = {
            let s = gv("cdn_base_url");
            if s.is_empty() {
                default_cdn_base_url()
            } else {
                s
            }
        };
        let dm_policy = {
            let s = gv("dm_policy");
            if s.is_empty() {
                default_dm_policy()
            } else {
                s
            }
        };
        let group_policy = {
            let s = gv("group_policy");
            if s.is_empty() {
                default_group_policy()
            } else {
                s
            }
        };
        Self {
            account_id,
            token,
            base_url,
            cdn_base_url,
            dm_policy,
            group_policy,
            allow_from: list("allow_from"),
            group_allow_from: list("group_allow_from"),
            proxy: AdapterProxyConfig::default(),
        }
    }
}

struct WeixinInner {
    config: WeixinConfig,
    client: Client,
    base: BasePlatformAdapter,
    context_tokens: Mutex<HashMap<String, String>>,
    seen: Mutex<HashMap<String, Instant>>,
    inbound_tx: RwLock<Option<mpsc::Sender<IncomingMessage>>>,
    stop: Notify,
}

pub struct WeChatAdapter {
    inner: Arc<WeixinInner>,
    stop_signal: Arc<Notify>,
    poll_task: RwLock<Option<tokio::task::JoinHandle<()>>>,
}

impl WeChatAdapter {
    pub fn new(mut config: WeixinConfig) -> Result<Self, GatewayError> {
        if config.account_id.is_empty() {
            return Err(GatewayError::Platform(
                "Weixin iLink requires account_id (WEIXIN_ACCOUNT_ID)".into(),
            ));
        }
        if config.token.is_empty() {
            if let Some(tok) = Self::load_persisted_token(&config.account_id) {
                config.token = tok;
            }
        }
        if config.token.is_empty() {
            return Err(GatewayError::Platform(
                "Weixin iLink requires token (WEIXIN_TOKEN or saved account file)".into(),
            ));
        }
        let base = BasePlatformAdapter::new(&config.token).with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        let inner = Arc::new(WeixinInner {
            config,
            client,
            base,
            context_tokens: Mutex::new(HashMap::new()),
            seen: Mutex::new(HashMap::new()),
            inbound_tx: RwLock::new(None),
            stop: Notify::new(),
        });
        Ok(Self {
            inner,
            stop_signal: Arc::new(Notify::new()),
            poll_task: RwLock::new(None),
        })
    }

    pub fn config(&self) -> &WeixinConfig {
        &self.inner.config
    }

    pub async fn set_inbound_sender(&self, tx: mpsc::Sender<IncomingMessage>) {
        *self.inner.inbound_tx.write().await = Some(tx);
    }

    fn accounts_dir() -> PathBuf {
        hermes_config::hermes_home().join("weixin").join("accounts")
    }

    fn account_json_path(account_id: &str) -> PathBuf {
        Self::accounts_dir().join(format!("{account_id}.json"))
    }

    fn sync_buf_path(account_id: &str) -> PathBuf {
        Self::accounts_dir().join(format!("{account_id}.sync.json"))
    }

    fn context_path(account_id: &str) -> PathBuf {
        Self::accounts_dir().join(format!("{account_id}.context-tokens.json"))
    }

    fn load_persisted_token(account_id: &str) -> Option<String> {
        let p = Self::account_json_path(account_id);
        let s = std::fs::read_to_string(p).ok()?;
        let v: Value = serde_json::from_str(&s).ok()?;
        v.get("token").and_then(|t| t.as_str()).map(str::trim).filter(|x| !x.is_empty()).map(String::from)
    }

    fn load_sync_buf(account_id: &str) -> String {
        let p = Self::sync_buf_path(account_id);
        let Ok(s) = std::fs::read_to_string(p) else {
            return String::new();
        };
        let Ok(v) = serde_json::from_str::<Value>(&s) else {
            return String::new();
        };
        v.get("get_updates_buf")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string()
    }

    fn save_sync_buf(account_id: &str, buf: &str) {
        let _ = std::fs::create_dir_all(Self::accounts_dir());
        let p = Self::sync_buf_path(account_id);
        let _ = std::fs::write(p, json!({ "get_updates_buf": buf }).to_string());
    }

    async fn download_bytes_http(
        inner: &WeixinInner,
        url: &str,
        timeout: Duration,
    ) -> Result<Vec<u8>, GatewayError> {
        let resp = inner
            .client
            .get(url)
            .timeout(timeout)
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("weixin GET: {e}")))?;
        let status = resp.status();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("weixin read body: {e}")))?;
        if !status.is_success() {
            let head = String::from_utf8_lossy(&bytes[..bytes.len().min(200)]);
            return Err(GatewayError::ConnectionFailed(format!(
                "weixin CDN HTTP {status}: {head}"
            )));
        }
        Ok(bytes.to_vec())
    }

    async fn download_and_decrypt_media(
        inner: &WeixinInner,
        encrypted_query_param: Option<&str>,
        aes_key_b64: Option<&str>,
        full_url: Option<&str>,
        timeout: Duration,
    ) -> Result<Vec<u8>, GatewayError> {
        let raw = if let Some(eq) = encrypted_query_param.filter(|s| !s.is_empty()) {
            let u = cdn_download_url(&inner.config.cdn_base_url, eq)?;
            Self::download_bytes_http(inner, &u, timeout).await?
        } else if let Some(u) = full_url.filter(|s| !s.is_empty()) {
            Self::download_bytes_http(inner, u, timeout).await?
        } else {
            return Err(GatewayError::Platform(
                "weixin media: neither encrypt_query_param nor full_url".into(),
            ));
        };
        if let Some(k) = aes_key_b64.filter(|s| !s.is_empty()) {
            let key = parse_aes_key(k)?;
            aes128_ecb_decrypt(&raw, &key)
        } else {
            Ok(raw)
        }
    }

    fn media_map<'a>(item: &'a Value, item_key: &str) -> Option<&'a Value> {
        item.get(item_key)?.get("media")
    }

    fn image_aes_key_b64(item: &Value) -> Option<String> {
        if let Some(hexs) = item
            .get("image_item")
            .and_then(|ii| ii.get("aeskey"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            if hexs.len() == 32 && hexs.chars().all(|c| c.is_ascii_hexdigit()) {
                let mut raw = [0u8; 16];
                let mut ok = true;
                for i in 0..16 {
                    if let Ok(b) = u8::from_str_radix(&hexs[i * 2..i * 2 + 2], 16) {
                        raw[i] = b;
                    } else {
                        ok = false;
                        break;
                    }
                }
                if ok {
                    return Some(base64::engine::general_purpose::STANDARD.encode(raw));
                }
            }
        }
        Self::media_map(item, "image_item")?
            .get("aes_key")
            .and_then(|v| v.as_str())
            .map(String::from)
    }

    fn write_media_cache(ext: &str, data: &[u8]) -> Result<String, std::io::Error> {
        let dir = hermes_config::hermes_home().join("cache").join("weixin");
        std::fs::create_dir_all(&dir)?;
        let name = format!("{}{}", Uuid::new_v4().simple(), ext);
        let p = dir.join(name);
        std::fs::write(&p, data)?;
        Ok(p.to_string_lossy().to_string())
    }

    async fn media_line_for_item(inner: &WeixinInner, item: &Value) -> Option<String> {
        let typ = item.get("type").and_then(|v| v.as_i64())? as i32;
        let res = match typ {
            ITEM_IMAGE => {
                let media = Self::media_map(item, "image_item")?;
                let enc = media.get("encrypt_query_param").and_then(|v| v.as_str());
                let full = media.get("full_url").and_then(|v| v.as_str());
                let key_b64 = Self::image_aes_key_b64(item)
                    .or_else(|| media.get("aes_key").and_then(|v| v.as_str()).map(String::from));
                Self::download_and_decrypt_media(
                    inner,
                    enc,
                    key_b64.as_deref(),
                    full,
                    Duration::from_secs(30),
                )
                .await
            }
            ITEM_VIDEO => {
                let media = Self::media_map(item, "video_item")?;
                let enc = media.get("encrypt_query_param").and_then(|v| v.as_str());
                let full = media.get("full_url").and_then(|v| v.as_str());
                let key_b64 = media.get("aes_key").and_then(|v| v.as_str());
                Self::download_and_decrypt_media(
                    inner,
                    enc,
                    key_b64,
                    full,
                    Duration::from_secs(120),
                )
                .await
            }
            ITEM_FILE => {
                let file_item = item.get("file_item")?;
                let media = file_item.get("media")?;
                let enc = media.get("encrypt_query_param").and_then(|v| v.as_str());
                let full = media.get("full_url").and_then(|v| v.as_str());
                let key_b64 = media.get("aes_key").and_then(|v| v.as_str());
                Self::download_and_decrypt_media(
                    inner,
                    enc,
                    key_b64,
                    full,
                    Duration::from_secs(60),
                )
                .await
            }
            ITEM_VOICE => {
                let voice = item.get("voice_item")?;
                if !voice
                    .get("text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .is_empty()
                {
                    return None;
                }
                let media = voice.get("media")?;
                let enc = media.get("encrypt_query_param").and_then(|v| v.as_str());
                let full = media.get("full_url").and_then(|v| v.as_str());
                let key_b64 = media.get("aes_key").and_then(|v| v.as_str());
                Self::download_and_decrypt_media(
                    inner,
                    enc,
                    key_b64,
                    full,
                    Duration::from_secs(60),
                )
                .await
            }
            _ => return None,
        };
        match res {
            Ok(data) => {
                let line = match typ {
                    ITEM_IMAGE => Self::write_media_cache(".jpg", &data)
                        .map(|p| format!("[图片: {p}]")),
                    ITEM_VIDEO => Self::write_media_cache(".mp4", &data)
                        .map(|p| format!("[视频: {p}]")),
                    ITEM_FILE => {
                        let name = item
                            .get("file_item")
                            .and_then(|f| f.get("file_name"))
                            .and_then(|v| v.as_str())
                            .unwrap_or("file.bin");
                        let ext = Path::new(name)
                            .extension()
                            .and_then(|e| e.to_str())
                            .map(|e| format!(".{e}"))
                            .unwrap_or_else(|| ".bin".into());
                        Self::write_media_cache(&ext, &data).map(|p| format!("[文件 {name}: {p}]"))
                    }
                    ITEM_VOICE => Self::write_media_cache(".silk", &data)
                        .map(|p| format!("[语音: {p}]")),
                    _ => return None,
                };
                match line {
                    Ok(s) => Some(s),
                    Err(e) => {
                        warn!(error = %e, "weixin media cache write");
                        None
                    }
                }
            }
            Err(e) => {
                warn!(error = %e, "weixin inbound media");
                None
            }
        }
    }

    async fn collect_media_lines(inner: &WeixinInner, item_list: &[Value]) -> Vec<String> {
        let mut out = Vec::new();
        for item in item_list {
            if let Some(line) = Self::media_line_for_item(inner, item).await {
                out.push(line);
            }
            if let Some(ref_item) = item.get("ref_msg").and_then(|r| r.get("message_item")) {
                if let Some(line) = Self::media_line_for_item(inner, ref_item).await {
                    out.push(line);
                }
            }
        }
        out
    }

    async fn upload_ciphertext_to_cdn(
        inner: &WeixinInner,
        ciphertext: &[u8],
        upload_param: &str,
        filekey: &str,
    ) -> Result<String, GatewayError> {
        let url = cdn_upload_url(&inner.config.cdn_base_url, upload_param, filekey)?;
        let resp = inner
            .client
            .post(url)
            .timeout(Duration::from_secs(120))
            .header("Content-Type", "application/octet-stream")
            .body(ciphertext.to_vec())
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("weixin CDN upload: {e}")))?;
        let status = resp.status();
        let enc = resp
            .headers()
            .get("x-encrypted-param")
            .and_then(|h| h.to_str().ok())
            .map(String::from);
        let _ = resp.bytes().await;
        enc.ok_or_else(|| {
            GatewayError::ConnectionFailed(format!(
                "weixin CDN upload missing x-encrypted-param (HTTP {status})"
            ))
        })
    }

    async fn weixin_get_upload_url(
        inner: &WeixinInner,
        to_user_id: &str,
        media_type: i32,
        filekey: &str,
        rawsize: usize,
        rawfilemd5: &str,
        filesize: usize,
        aeskey_hex: &str,
    ) -> Result<Value, GatewayError> {
        Self::ilink_post(
            inner,
            EP_GET_UPLOAD_URL,
            json!({
                "filekey": filekey,
                "media_type": media_type,
                "to_user_id": to_user_id,
                "rawsize": rawsize,
                "rawfilemd5": rawfilemd5,
                "filesize": filesize,
                "no_need_thumb": true,
                "aeskey": aeskey_hex,
            }),
            API_TIMEOUT_MS,
        )
        .await
    }

    fn outbound_media_item(
        media_kind: i32,
        encrypt_query_param: &str,
        aes_key_b64: &str,
        ciphertext_len: usize,
        plaintext_size: usize,
        filename: &str,
    ) -> Value {
        match media_kind {
            MEDIA_IMAGE => json!({
                "type": ITEM_IMAGE,
                "image_item": {
                    "media": {
                        "encrypt_query_param": encrypt_query_param,
                        "aes_key": aes_key_b64,
                        "encrypt_type": 1
                    },
                    "mid_size": ciphertext_len
                }
            }),
            MEDIA_VIDEO => json!({
                "type": ITEM_VIDEO,
                "video_item": {
                    "media": {
                        "encrypt_query_param": encrypt_query_param,
                        "aes_key": aes_key_b64,
                        "encrypt_type": 1
                    },
                    "video_size": ciphertext_len
                }
            }),
            _ => json!({
                "type": ITEM_FILE,
                "file_item": {
                    "media": {
                        "encrypt_query_param": encrypt_query_param,
                        "aes_key": aes_key_b64,
                        "encrypt_type": 1
                    },
                    "file_name": filename,
                    "len": plaintext_size.to_string(),
                }
            }),
        }
    }

    async fn restore_context(inner: &WeixinInner) {
        let p = Self::context_path(&inner.config.account_id);
        let Ok(s) = std::fs::read_to_string(p) else {
            return;
        };
        let Ok(map) = serde_json::from_str::<HashMap<String, String>>(&s) else {
            return;
        };
        let mut g = inner.context_tokens.lock().await;
        for (uid, tok) in map {
            if !tok.is_empty() {
                g.insert(
                    format!("{}:{}", inner.config.account_id, uid),
                    tok,
                );
            }
        }
    }

    async fn persist_context(inner: &WeixinInner) -> Result<(), std::io::Error> {
        let g = inner.context_tokens.lock().await;
        let prefix = format!("{}:", inner.config.account_id);
        let mut out: HashMap<String, String> = HashMap::new();
        for (k, v) in g.iter() {
            if let Some(uid) = k.strip_prefix(&prefix) {
                out.insert(uid.to_string(), v.clone());
            }
        }
        let _ = std::fs::create_dir_all(Self::accounts_dir());
        let p = Self::context_path(&inner.config.account_id);
        std::fs::write(p, serde_json::to_string(&out)?)
    }

    fn ilink_headers(token: Option<&str>, body: &str) -> reqwest::header::HeaderMap {
        use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
        let mut h = HeaderMap::new();
        h.insert(
            HeaderName::from_static("content-type"),
            HeaderValue::from_static("application/json"),
        );
        h.insert(
            HeaderName::from_static("authorizationtype"),
            HeaderValue::from_static("ilink_bot_token"),
        );
        let clen = body.as_bytes().len();
        h.insert(
            HeaderName::from_static("content-length"),
            HeaderValue::from_str(&clen.to_string()).unwrap(),
        );
        h.insert(
            HeaderName::from_static("x-wechat-uin"),
            HeaderValue::from_str(&random_wechat_uin()).unwrap(),
        );
        h.insert(
            HeaderName::from_static("ilink-app-id"),
            HeaderValue::from_static(ILINK_APP_ID),
        );
        h.insert(
            HeaderName::from_static("ilink-app-clientversion"),
            HeaderValue::from_str(&ILINK_APP_CLIENT_VERSION.to_string()).unwrap(),
        );
        if let Some(t) = token {
            if let Ok(v) = HeaderValue::from_str(&format!("Bearer {t}")) {
                h.insert(HeaderName::from_static("authorization"), v);
            }
        }
        h
    }

    async fn ilink_post(
        inner: &WeixinInner,
        endpoint: &str,
        payload: Value,
        timeout_ms: u64,
    ) -> Result<Value, GatewayError> {
        let mut obj = payload
            .as_object()
            .cloned()
            .unwrap_or_default();
        obj.insert(
            "base_info".into(),
            json!({ "channel_version": CHANNEL_VERSION }),
        );
        let body = serde_json::to_string(&Value::Object(obj))
            .map_err(|e| GatewayError::ConnectionFailed(format!("weixin json: {e}")))?;

        let url = format!(
            "{}/{}",
            inner.config.base_url.trim_end_matches('/'),
            endpoint.trim_start_matches('/')
        );
        let resp = inner
            .client
            .post(&url)
            .headers(Self::ilink_headers(
                Some(inner.config.token.as_str()),
                &body,
            ))
            .body(body)
            .timeout(Duration::from_millis(timeout_ms.max(1000)))
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("weixin POST {endpoint}: {e}")))?;
        let txt = resp
            .text()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("weixin read: {e}")))?;
        serde_json::from_str(&txt).map_err(|e| {
            GatewayError::ConnectionFailed(format!("weixin JSON {endpoint}: {e} [{txt}]"))
        })
    }

    fn guess_chat(message: &Value, account_id: &str) -> (&'static str, String) {
        let room_id = message
            .get("room_id")
            .or_else(|| message.get("chat_room_id"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let to_user = message
            .get("to_user_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let from_user = message
            .get("from_user_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim();
        let msg_type = message.get("msg_type").and_then(|v| v.as_i64()).unwrap_or(0);
        let is_group = !room_id.is_empty()
            || (!to_user.is_empty()
                && to_user != account_id
                && msg_type == 1);
        if is_group {
            (
                "group",
                if !room_id.is_empty() {
                    room_id.to_string()
                } else if !to_user.is_empty() {
                    to_user.to_string()
                } else {
                    from_user.to_string()
                },
            )
        } else {
            ("dm", from_user.to_string())
        }
    }

    fn extract_text(item_list: &[Value]) -> String {
        for item in item_list {
            if item.get("type").and_then(|v| v.as_i64()) == Some(ITEM_TEXT as i64) {
                if let Some(t) = item.get("text_item").and_then(|x| x.get("text")).and_then(|v| v.as_str()) {
                    return t.to_string();
                }
            }
        }
        String::new()
    }

    async fn is_dup(inner: &WeixinInner, msg_id: &str) -> bool {
        if msg_id.is_empty() {
            return false;
        }
        let now = Instant::now();
        let mut m = inner.seen.lock().await;
        m.retain(|_, t| now.duration_since(*t) < DEDUP_TTL);
        if m.contains_key(msg_id) {
            return true;
        }
        m.insert(msg_id.to_string(), now);
        false
    }

    async fn process_inbound(inner: Arc<WeixinInner>, message: Value) {
        let sender = message
            .get("from_user_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if sender.is_empty() || sender == inner.config.account_id {
            return;
        }
        let msg_id = message
            .get("message_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        if Self::is_dup(&inner, &msg_id).await {
            return;
        }
        let (chat_type, effective_id) = Self::guess_chat(&message, &inner.config.account_id);
        if chat_type == "group" {
            if inner.config.group_policy == "disabled" {
                return;
            }
            if inner.config.group_policy == "allowlist"
                && !inner
                    .config
                    .group_allow_from
                    .iter()
                    .any(|x| x == &effective_id)
            {
                return;
            }
        } else if inner.config.dm_policy == "disabled" {
            return;
        } else if inner.config.dm_policy == "allowlist"
            && !inner.config.allow_from.iter().any(|x| x == &sender)
        {
            return;
        }

        if let Some(ct) = message
            .get("context_token")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            let key = format!("{}:{}", inner.config.account_id, sender);
            inner.context_tokens.lock().await.insert(key, ct.to_string());
            let _ = Self::persist_context(&inner).await;
        }

        let item_list: Vec<Value> = message
            .get("item_list")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let mut text = Self::extract_text(&item_list);
        let media_lines = Self::collect_media_lines(&inner, &item_list).await;
        if !media_lines.is_empty() {
            if !text.is_empty() {
                text.push('\n');
            }
            text.push_str(&media_lines.join("\n"));
        }
        if text.is_empty() {
            return;
        }

        let incoming = IncomingMessage {
            platform: "weixin".into(),
            chat_id: effective_id.clone(),
            user_id: sender.clone(),
            text,
            message_id: if msg_id.is_empty() {
                None
            } else {
                Some(msg_id)
            },
            is_dm: chat_type == "dm",
        };
        if let Some(tx) = inner.inbound_tx.read().await.clone() {
            tokio::spawn(async move {
                let _ = tx.send(incoming).await;
            });
        }
    }

    async fn poll_loop(inner: Arc<WeixinInner>) {
        let mut sync_buf = Self::load_sync_buf(&inner.config.account_id);
        let mut timeout_ms = LONG_POLL_TIMEOUT_MS;
        let mut failures = 0u32;
        while inner.base.is_running() {
            tokio::select! {
                _ = inner.stop.notified() => break,
                res = Self::get_updates(&inner, &sync_buf, timeout_ms) => {
                    match res {
                        Ok(resp) => {
                            failures = 0;
                            if let Some(ms) = resp.get("longpolling_timeout_ms").and_then(|v| v.as_u64()) {
                                if ms > 0 { timeout_ms = ms; }
                            }
                            let ret = resp.get("ret").and_then(|v| v.as_i64()).unwrap_or(0);
                            let errcode = resp.get("errcode").and_then(|v| v.as_i64()).unwrap_or(0);
                            if ret == SESSION_EXPIRED || errcode == SESSION_EXPIRED {
                                warn!("Weixin iLink session expired; sleeping 10m");
                                tokio::time::sleep(Duration::from_secs(600)).await;
                                continue;
                            }
                            if ret != 0 || errcode != 0 {
                                failures += 1;
                                warn!(ret, errcode, errmsg = ?resp.get("errmsg"), "weixin getupdates error");
                                let delay = if failures >= 3 { 30 } else { 2 };
                                tokio::time::sleep(Duration::from_secs(delay)).await;
                                continue;
                            }
                            if let Some(nb) = resp.get("get_updates_buf").and_then(|v| v.as_str()) {
                                sync_buf = nb.to_string();
                                Self::save_sync_buf(&inner.config.account_id, &sync_buf);
                            }
                            if let Some(msgs) = resp.get("msgs").and_then(|v| v.as_array()) {
                                for m in msgs {
                                    let inner2 = inner.clone();
                                    let mv = m.clone();
                                    tokio::spawn(async move {
                                        Self::process_inbound(inner2, mv).await;
                                    });
                                }
                            }
                        }
                        Err(e) => {
                            failures += 1;
                            warn!(error = %e, "weixin getupdates");
                            let delay = if failures >= 3 { 30 } else { 2 };
                            tokio::time::sleep(Duration::from_secs(delay)).await;
                        }
                    }
                }
            }
        }
    }

    async fn get_updates(
        inner: &WeixinInner,
        sync_buf: &str,
        timeout_ms: u64,
    ) -> Result<Value, GatewayError> {
        Self::ilink_post(
            inner,
            EP_GET_UPDATES,
            json!({ "get_updates_buf": sync_buf }),
            timeout_ms.max(1000),
        )
        .await
    }

    /// Send plain text over iLink (with `context_token` when known).
    pub async fn send_ilink_text(&self, to_user_id: &str, text: &str) -> Result<(), GatewayError> {
        let key = format!("{}:{}", self.inner.config.account_id, to_user_id);
        let ctx = self.inner.context_tokens.lock().await.get(&key).cloned();
        for chunk in split_long_message(text, MAX_TEXT) {
            let client_id = format!("hermes-weixin-{}", Uuid::new_v4().simple());
            let mut msg = json!({
                "from_user_id": "",
                "to_user_id": to_user_id,
                "client_id": client_id,
                "message_type": MSG_TYPE_BOT,
                "message_state": MSG_STATE_FINISH,
                "item_list": [{"type": ITEM_TEXT, "text_item": {"text": chunk}}],
            });
            if let Some(ref t) = ctx {
                msg.as_object_mut().unwrap().insert("context_token".into(), json!(t));
            }
            Self::ilink_post(
                &self.inner,
                EP_SEND_MESSAGE,
                json!({ "msg": msg }),
                API_TIMEOUT_MS,
            )
            .await?;
        }
        Ok(())
    }

    fn md5_hex(data: &[u8]) -> String {
        let d = md5::compute(data);
        d.0.iter().map(|b| format!("{b:02x}")).collect()
    }

    /// 发送本地文件（AES-128-ECB + CDN + `sendmessage`），对齐 Python `_send_file`。
    pub async fn send_ilink_file(
        &self,
        to_user_id: &str,
        path: &Path,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let plaintext = tokio::fs::read(path)
            .await
            .map_err(|e| GatewayError::SendFailed(format!("weixin read file: {e}")))?;
        let (filekey, aes_key, aeskey_hex) = {
            let mut rng = rand::thread_rng();
            let filekey: String = (0..16).map(|_| format!("{:02x}", rng.gen::<u8>())).collect();
            let aes_key: [u8; 16] = rng.gen();
            let aeskey_hex: String = aes_key.iter().map(|b| format!("{b:02x}")).collect();
            (filekey, aes_key, aeskey_hex)
        };

        let media_kind = {
            let m = mime_from_path(path);
            if m.starts_with("image/") {
                MEDIA_IMAGE
            } else if m.starts_with("video/") {
                MEDIA_VIDEO
            } else {
                MEDIA_FILE
            }
        };
        let filename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("file.bin");

        let ciphertext = aes128_ecb_encrypt(&plaintext, &aes_key);
        let rawfilemd5 = Self::md5_hex(&plaintext);
        let upload_resp = Self::weixin_get_upload_url(
            &self.inner,
            to_user_id,
            media_kind,
            &filekey,
            plaintext.len(),
            &rawfilemd5,
            aes_padded_size(plaintext.len()),
            &aeskey_hex,
        )
        .await?;

        let upload_param = upload_resp
            .get("upload_param")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let upload_full_url = upload_resp
            .get("upload_full_url")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        let encrypted_query_param = if !upload_param.is_empty() {
            Self::upload_ciphertext_to_cdn(&self.inner, &ciphertext, upload_param, &filekey).await?
        } else if !upload_full_url.is_empty() {
            let resp = self
                .inner
                .client
                .put(upload_full_url)
                .timeout(Duration::from_secs(120))
                .header("Content-Type", "application/octet-stream")
                .body(ciphertext.clone())
                .send()
                .await
                .map_err(|e| {
                    GatewayError::ConnectionFailed(format!("weixin CDN PUT upload_full_url: {e}"))
                })?;
            let _status = resp.status();
            let ep = resp
                .headers()
                .get("x-encrypted-param")
                .and_then(|h| h.to_str().ok())
                .map(String::from);
            let _ = resp.bytes().await;
            ep.unwrap_or_else(|| filekey.clone())
        } else {
            return Err(GatewayError::SendFailed(format!(
                "weixin getuploadurl missing upload_param and upload_full_url: {upload_resp}"
            )));
        };

        if let Some(cap) = caption.map(str::trim).filter(|s| !s.is_empty()) {
            self.send_ilink_text(to_user_id, cap).await?;
        }

        let ctx_key = format!("{}:{}", self.inner.config.account_id, to_user_id);
        let ctx = self
            .inner
            .context_tokens
            .lock()
            .await
            .get(&ctx_key)
            .cloned();
        let aes_key_b64 = base64::engine::general_purpose::STANDARD.encode(aes_key);
        let media_item = Self::outbound_media_item(
            media_kind,
            &encrypted_query_param,
            &aes_key_b64,
            ciphertext.len(),
            plaintext.len(),
            filename,
        );
        let client_id = format!("hermes-weixin-{}", Uuid::new_v4().simple());
        let mut msg = json!({
            "from_user_id": "",
            "to_user_id": to_user_id,
            "client_id": client_id,
            "message_type": MSG_TYPE_BOT,
            "message_state": MSG_STATE_FINISH,
            "item_list": [media_item],
        });
        if let Some(ref t) = ctx {
            msg.as_object_mut()
                .unwrap()
                .insert("context_token".into(), json!(t));
        }
        Self::ilink_post(
            &self.inner,
            EP_SEND_MESSAGE,
            json!({ "msg": msg }),
            API_TIMEOUT_MS,
        )
        .await?;
        Ok(())
    }
}

#[async_trait]
impl PlatformAdapter for WeChatAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "Weixin iLink adapter starting (account_id={})",
            self.inner.config.account_id
        );
        Self::restore_context(&self.inner).await;
        self.inner.base.mark_running();
        let inner = self.inner.clone();
        let h = tokio::spawn(async move {
            WeChatAdapter::poll_loop(inner).await;
        });
        *self.poll_task.write().await = Some(h);
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("Weixin iLink adapter stopping");
        self.inner.base.mark_stopped();
        self.inner.stop.notify_waiters();
        self.stop_signal.notify_one();
        if let Some(t) = self.poll_task.write().await.take() {
            t.abort();
        }
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        _parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.send_ilink_text(chat_id, text).await
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        debug!("Weixin iLink does not support message editing");
        Ok(())
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.send_ilink_file(chat_id, Path::new(file_path), caption)
            .await
    }

    fn is_running(&self) -> bool {
        self.inner.base.is_running()
    }

    fn platform_name(&self) -> &str {
        "weixin"
    }
}

#[cfg(test)]
mod weixin_crypto_tests {
    use super::*;

    #[test]
    fn parse_aes_key_raw_16_bytes() {
        let key: [u8; 16] = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let b64 = base64::engine::general_purpose::STANDARD.encode(key);
        let out = parse_aes_key(&b64).unwrap();
        assert_eq!(out, key);
    }

    #[test]
    fn parse_aes_key_hex_payload_after_b64_decode() {
        let hex = "0123456789abcdef0123456789abcdef";
        let b64 = base64::engine::general_purpose::STANDARD.encode(hex.as_bytes());
        let out = parse_aes_key(&b64).unwrap();
        let expected: [u8; 16] = [
            0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab,
            0xcd, 0xef,
        ];
        assert_eq!(out, expected);
    }

    #[test]
    fn parse_aes_key_rejects_bad_input() {
        assert!(parse_aes_key("not-valid-base64!!!").is_err());
        assert!(parse_aes_key("Zg==").is_err());
    }

    #[test]
    fn aes128_ecb_roundtrip_short_plaintext() {
        let key: [u8; 16] = rand::random();
        let plain = b"hello-weixin-ilink";
        let ct = aes128_ecb_encrypt(plain, &key);
        assert_eq!(ct.len() % 16, 0);
        let back = aes128_ecb_decrypt(&ct, &key).unwrap();
        assert_eq!(back, plain);
    }

    #[test]
    fn aes128_ecb_roundtrip_block_aligned_plaintext() {
        let key: [u8; 16] = [7u8; 16];
        let plain = [0xabu8; 32];
        let ct = aes128_ecb_encrypt(&plain, &key);
        let back = aes128_ecb_decrypt(&ct, &key).unwrap();
        assert_eq!(back, plain.as_slice());
    }

    #[test]
    fn aes128_ecb_decrypt_rejects_non_block_length() {
        let key = [0u8; 16];
        assert!(aes128_ecb_decrypt(&[1u8; 15], &key).is_err());
    }
}


#[cfg(test)]
mod weixin_send_file_tests {
    use super::*;
    use std::io::Write;

    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn sample_cfg(base: &str) -> WeixinConfig {
        WeixinConfig {
            account_id: "acc_test".into(),
            token: "tok_test".into(),
            base_url: base.into(),
            cdn_base_url: base.into(),
            dm_policy: "open".into(),
            group_policy: "disabled".into(),
            allow_from: vec![],
            group_allow_from: vec![],
            proxy: AdapterProxyConfig::default(),
        }
    }

    #[tokio::test]
    async fn send_ilink_file_upload_param_path_end_to_end() {
        let server = MockServer::start().await;

        Mock::given(method("POST"))
            .and(path("/ilink/bot/getuploadurl"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "ret": 0,
                "upload_param": "up_param_1"
            })))
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/upload"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("x-encrypted-param", "enc_param_2")
                    .set_body_string("ok"),
            )
            .mount(&server)
            .await;

        Mock::given(method("POST"))
            .and(path("/ilink/bot/sendmessage"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"ret":0})))
            .mount(&server)
            .await;

        let mut tf = tempfile::Builder::new()
            .suffix(".txt")
            .tempfile()
            .expect("temp file");
        let plain = b"hello weixin send_file mock flow";
        tf.write_all(plain).expect("write plain");
        tf.flush().expect("flush");

        let adapter = WeChatAdapter::new(sample_cfg(&server.uri())).expect("adapter");
        adapter
            .send_ilink_file("wxid_target", tf.path(), None)
            .await
            .expect("send file");

        let requests = server.received_requests().await.expect("requests");

        let up_req = requests
            .iter()
            .find(|r| r.url.path() == "/ilink/bot/getuploadurl")
            .expect("getuploadurl request");
        let up_json: Value = serde_json::from_slice(&up_req.body).expect("upload json");
        assert_eq!(
            up_json.pointer("/to_user_id").and_then(|v| v.as_str()),
            Some("wxid_target")
        );
        assert_eq!(up_json.pointer("/media_type").and_then(|v| v.as_i64()), Some(MEDIA_FILE as i64));
        assert_eq!(up_json.pointer("/rawsize").and_then(|v| v.as_u64()), Some(plain.len() as u64));
        assert_eq!(
            up_json.pointer("/filesize").and_then(|v| v.as_u64()),
            Some(aes_padded_size(plain.len()) as u64)
        );
        let expected_md5 = WeChatAdapter::md5_hex(plain);
        assert_eq!(
            up_json.pointer("/rawfilemd5").and_then(|v| v.as_str()),
            Some(expected_md5.as_str())
        );
        let aes_hex = up_json
            .pointer("/aeskey")
            .and_then(|v| v.as_str())
            .expect("aeskey");
        assert_eq!(aes_hex.len(), 32);

        let cdn_req = requests
            .iter()
            .find(|r| r.url.path() == "/upload")
            .expect("cdn upload request");
        assert_eq!(
            cdn_req
                .url
                .query_pairs()
                .find(|(k, _)| k == "encrypted_query_param")
                .map(|(_, v)| v.to_string())
                .as_deref(),
            Some("up_param_1")
        );
        assert!(
            cdn_req
                .url
                .query_pairs()
                .any(|(k, v)| k == "filekey" && !v.is_empty())
        );
        assert_eq!(cdn_req.body.len() % 16, 0);
        assert_ne!(cdn_req.body, plain);

        let send_req = requests
            .iter()
            .find(|r| r.url.path() == "/ilink/bot/sendmessage")
            .expect("sendmessage request");
        let send_json: Value = serde_json::from_slice(&send_req.body).expect("send json");
        assert_eq!(
            send_json.pointer("/msg/to_user_id").and_then(|v| v.as_str()),
            Some("wxid_target")
        );
        assert_eq!(
            send_json.pointer("/msg/item_list/0/type").and_then(|v| v.as_i64()),
            Some(ITEM_FILE as i64)
        );
        assert_eq!(
            send_json
                .pointer("/msg/item_list/0/file_item/media/encrypt_query_param")
                .and_then(|v| v.as_str()),
            Some("enc_param_2")
        );
        let aes_b64 = send_json
            .pointer("/msg/item_list/0/file_item/media/aes_key")
            .and_then(|v| v.as_str())
            .expect("aes b64");
        let aes_raw = base64::engine::general_purpose::STANDARD
            .decode(aes_b64)
            .expect("decode aes key");
        assert_eq!(aes_raw.len(), 16);
    }
}
