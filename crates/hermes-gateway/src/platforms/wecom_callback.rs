//! WeCom callback-mode adapter.
//! Inbound: signature verify + AES-CBC decrypt callback XML.
//! Outbound: proactive `message/send`.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use aes::cipher::generic_array::GenericArray;
use aes::cipher::{BlockDecrypt, KeyInit};
use aes::Aes256;
use async_trait::async_trait;
use base64::Engine;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use sha1::{Digest, Sha1};
use tokio::sync::{mpsc, Notify, RwLock};
use tracing::{debug, error, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};
use crate::gateway::IncomingMessage;

const WECOM_API_BASE: &str = "https://qyapi.weixin.qq.com/cgi-bin";
const DEDUP_TTL_SECS: u64 = 300;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeComCallbackApp {
    pub name: String,
    pub corp_id: String,
    pub corp_secret: String,
    pub agent_id: String,
    pub token: String,
    pub encoding_aes_key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WeComCallbackConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default = "default_path")]
    pub path: String,
    #[serde(default)]
    pub apps: Vec<WeComCallbackApp>,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

fn default_host() -> String {
    "0.0.0.0".to_string()
}
fn default_port() -> u16 {
    8645
}
fn default_path() -> String {
    "/wecom/callback".to_string()
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

fn strip_group_mention_prefix(text: &str) -> String {
    let trimmed_start = text.trim_start();
    if !trimmed_start.starts_with('@') {
        return text.to_string();
    }
    let rest = trimmed_start
        .char_indices()
        .skip(1)
        .find_map(|(idx, ch)| ch.is_whitespace().then_some(&trimmed_start[idx..]))
        .unwrap_or("");
    rest.trim().to_string()
}

pub struct WeComCallbackAdapter {
    base: BasePlatformAdapter,
    config: WeComCallbackConfig,
    client: Client,
    stop_signal: Arc<Notify>,
    shutdown_tx: RwLock<Option<tokio::sync::oneshot::Sender<()>>>,
    access_tokens: RwLock<HashMap<String, (String, Instant)>>,
    seen: Arc<RwLock<HashMap<String, Instant>>>,
    inbound_tx: Arc<RwLock<Option<mpsc::Sender<IncomingMessage>>>>,
}

impl WeComCallbackAdapter {
    pub fn new(config: WeComCallbackConfig) -> Result<Self, GatewayError> {
        if config.apps.is_empty() {
            return Err(GatewayError::Platform(
                "WeCom callback requires at least one app".into(),
            ));
        }
        for app in &config.apps {
            if app.corp_id.trim().is_empty()
                || app.corp_secret.trim().is_empty()
                || app.agent_id.trim().is_empty()
                || app.token.trim().is_empty()
                || app.encoding_aes_key.trim().len() != 43
            {
                return Err(GatewayError::Platform(
                    "invalid wecom_callback app config".into(),
                ));
            }
        }
        let base =
            BasePlatformAdapter::new(&config.apps[0].corp_id).with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        Ok(Self {
            base,
            config,
            client,
            stop_signal: Arc::new(Notify::new()),
            shutdown_tx: RwLock::new(None),
            access_tokens: RwLock::new(HashMap::new()),
            seen: Arc::new(RwLock::new(HashMap::new())),
            inbound_tx: Arc::new(RwLock::new(None)),
        })
    }

    pub async fn set_inbound_sender(&self, tx: mpsc::Sender<IncomingMessage>) {
        *self.inbound_tx.write().await = Some(tx);
    }

    fn xml_tag(xml: &str, tag: &str) -> Option<String> {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        let start = xml.find(&open)? + open.len();
        let end = xml[start..].find(&close)? + start;
        Some(xml[start..end].to_string())
    }

    fn sha1_signature(token: &str, timestamp: &str, nonce: &str, encrypt: &str) -> String {
        let mut parts = [
            token.to_string(),
            timestamp.to_string(),
            nonce.to_string(),
            encrypt.to_string(),
        ];
        parts.sort();
        let mut hasher = Sha1::new();
        hasher.update(parts.join("").as_bytes());
        format!("{:x}", hasher.finalize())
    }

    fn pkcs7_unpad(data: &[u8]) -> Result<Vec<u8>, GatewayError> {
        let Some(last) = data.last() else {
            return Err(GatewayError::Platform("empty decrypted payload".into()));
        };
        let pad = *last as usize;
        if pad == 0 || pad > 32 || pad > data.len() {
            return Err(GatewayError::Platform("invalid pkcs7 padding".into()));
        }
        if !data[data.len() - pad..].iter().all(|b| *b as usize == pad) {
            return Err(GatewayError::Platform("malformed pkcs7 padding".into()));
        }
        Ok(data[..data.len() - pad].to_vec())
    }

    fn decrypt_xml(
        app: &WeComCallbackApp,
        msg_signature: &str,
        timestamp: &str,
        nonce: &str,
        encrypt_b64: &str,
    ) -> Result<String, GatewayError> {
        let expected = Self::sha1_signature(&app.token, timestamp, nonce, encrypt_b64);
        if expected != msg_signature {
            return Err(GatewayError::Platform("wecom signature mismatch".into()));
        }
        let key = base64::engine::general_purpose::STANDARD
            .decode(format!("{}=", app.encoding_aes_key))
            .map_err(|e| GatewayError::Platform(format!("invalid aes key: {e}")))?;
        if key.len() != 32 {
            return Err(GatewayError::Platform("invalid aes key length".into()));
        }
        let iv = &key[..16];
        let cipher_bytes = base64::engine::general_purpose::STANDARD
            .decode(encrypt_b64)
            .map_err(|e| GatewayError::Platform(format!("invalid encrypted payload: {e}")))?;
        if cipher_bytes.is_empty() || cipher_bytes.len() % 16 != 0 {
            return Err(GatewayError::Platform(
                "invalid encrypted block size".into(),
            ));
        }
        let aes = Aes256::new(GenericArray::from_slice(&key));
        let mut prev = iv.to_vec();
        let mut plain = Vec::with_capacity(cipher_bytes.len());
        for block in cipher_bytes.chunks(16) {
            let mut b = GenericArray::clone_from_slice(block);
            aes.decrypt_block(&mut b);
            for i in 0..16 {
                b[i] ^= prev[i];
            }
            prev.copy_from_slice(block);
            plain.extend_from_slice(&b);
        }
        let plain = Self::pkcs7_unpad(&plain)?;
        if plain.len() < 20 {
            return Err(GatewayError::Platform("decrypted payload too short".into()));
        }
        let content = &plain[16..];
        let xml_len = u32::from_be_bytes([content[0], content[1], content[2], content[3]]) as usize;
        if content.len() < 4 + xml_len {
            return Err(GatewayError::Platform(
                "invalid xml length in payload".into(),
            ));
        }
        let xml = String::from_utf8(content[4..4 + xml_len].to_vec())
            .map_err(|e| GatewayError::Platform(format!("xml utf8 decode failed: {e}")))?;
        let receive_id = String::from_utf8(content[4 + xml_len..].to_vec())
            .map_err(|e| GatewayError::Platform(format!("receive_id decode failed: {e}")))?;
        if receive_id != app.corp_id {
            return Err(GatewayError::Platform("wecom receive_id mismatch".into()));
        }
        Ok(xml)
    }

    fn resolve_app_for_chat<'a>(&'a self, chat_id: &str) -> &'a WeComCallbackApp {
        self.config
            .apps
            .iter()
            .find(|a| chat_id.starts_with(&format!("{}:", a.corp_id)))
            .unwrap_or(&self.config.apps[0])
    }

    async fn get_access_token_for_app(
        &self,
        app: &WeComCallbackApp,
    ) -> Result<String, GatewayError> {
        if let Some((token, exp)) = self.access_tokens.read().await.get(&app.name).cloned() {
            if Instant::now() < exp {
                return Ok(token);
            }
        }
        let url = format!(
            "{WECOM_API_BASE}/gettoken?corpid={}&corpsecret={}",
            app.corp_id, app.corp_secret
        );
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| GatewayError::Auth(format!("WeCom callback gettoken failed: {e}")))?;
        let v: serde_json::Value = resp.json().await.map_err(|e| {
            GatewayError::Auth(format!("WeCom callback gettoken parse failed: {e}"))
        })?;
        let token = v
            .get("access_token")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .to_string();
        if token.trim().is_empty() {
            return Err(GatewayError::Auth("WeCom callback token missing".into()));
        }
        let ttl = v.get("expires_in").and_then(|x| x.as_u64()).unwrap_or(7200);
        self.access_tokens.write().await.insert(
            app.name.clone(),
            (
                token.clone(),
                Instant::now() + Duration::from_secs(ttl.saturating_sub(60)),
            ),
        );
        Ok(token)
    }
}

#[async_trait]
impl PlatformAdapter for WeComCallbackAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "WeCom callback adapter starting ({} app(s), endpoint {}:{}{})",
            self.config.apps.len(),
            self.config.host,
            self.config.port,
            self.config.path
        );
        let addr: SocketAddr = format!("{}:{}", self.config.host, self.config.port)
            .parse()
            .map_err(|e| GatewayError::ConnectionFailed(format!("invalid address: {e}")))?;
        let apps = self.config.apps.clone();
        let path = self.config.path.clone();
        let inbound_tx = self.inbound_tx.clone();
        let seen = self.seen.clone();
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        *self.shutdown_tx.write().await = Some(shutdown_tx);

        tokio::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(addr).await {
                Ok(l) => l,
                Err(e) => {
                    error!("WeCom callback bind failed: {e}");
                    return;
                }
            };
            loop {
                tokio::select! {
                    accept = listener.accept() => {
                        match accept {
                            Ok((stream, peer)) => {
                                let apps = apps.clone();
                                let path = path.clone();
                                let inbound_tx = inbound_tx.clone();
                                let seen = seen.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = handle_callback_request(stream, peer, &apps, &path, inbound_tx, seen).await {
                                        debug!("WeCom callback request error from {peer}: {e}");
                                    }
                                });
                            }
                            Err(e) => warn!("WeCom callback accept error: {e}"),
                        }
                    }
                    _ = &mut shutdown_rx => break,
                }
            }
        });

        self.base.mark_running();
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("WeCom callback adapter stopping");
        if let Some(tx) = self.shutdown_tx.write().await.take() {
            let _ = tx.send(());
        }
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
        let app = self.resolve_app_for_chat(chat_id);
        let token = self.get_access_token_for_app(app).await?;
        let touser = chat_id.split_once(':').map(|(_, u)| u).unwrap_or(chat_id);
        let url = format!("{WECOM_API_BASE}/message/send?access_token={token}");
        let body = serde_json::json!({
            "touser": touser,
            "msgtype": "text",
            "agentid": app.agent_id.parse::<i64>().unwrap_or(0),
            "text": { "content": text },
            "safe": 0
        });
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("WeCom callback send failed: {e}")))?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "WeCom callback send non-success: {text}"
            )));
        }
        Ok(())
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        Ok(())
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        use crate::platforms::helpers::{media_category, mime_from_extension};

        let app = self.resolve_app_for_chat(chat_id);
        let token = self.get_access_token_for_app(app).await?;
        let touser = chat_id.split_once(':').map(|(_, u)| u).unwrap_or(chat_id);

        let path = std::path::Path::new(file_path);
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        let mime = mime_from_extension(ext);
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("file");
        let file_bytes = tokio::fs::read(file_path)
            .await
            .map_err(|e| GatewayError::SendFailed(format!("Failed to read file: {e}")))?;

        let media_type = match media_category(ext) {
            "image" => "image",
            "video" => "video",
            "audio" => "voice",
            _ => "file",
        };

        let upload_url =
            format!("{WECOM_API_BASE}/media/upload?access_token={token}&type={media_type}");
        let part = reqwest::multipart::Part::bytes(file_bytes)
            .file_name(file_name.to_string())
            .mime_str(mime)
            .map_err(|e| GatewayError::SendFailed(format!("MIME error: {e}")))?;
        let form = reqwest::multipart::Form::new().part("media", part);
        let resp = self
            .client
            .post(&upload_url)
            .multipart(form)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("WeCom callback media upload failed: {e}"))
            })?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "WeCom callback upload error: {text}"
            )));
        }
        let result: serde_json::Value = resp.json().await.map_err(|e| {
            GatewayError::SendFailed(format!("WeCom callback upload parse failed: {e}"))
        })?;
        let media_id = result
            .get("media_id")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                GatewayError::SendFailed("No media_id in WeCom callback response".into())
            })?;

        let send_url = format!("{WECOM_API_BASE}/message/send?access_token={token}");
        let agent_id = app.agent_id.parse::<i64>().unwrap_or(0);
        let body = match media_type {
            "image" => serde_json::json!({
                "touser": touser,
                "msgtype": "image",
                "agentid": agent_id,
                "image": { "media_id": media_id }
            }),
            "voice" => serde_json::json!({
                "touser": touser,
                "msgtype": "voice",
                "agentid": agent_id,
                "voice": { "media_id": media_id }
            }),
            "video" => serde_json::json!({
                "touser": touser,
                "msgtype": "video",
                "agentid": agent_id,
                "video": { "media_id": media_id, "title": caption.unwrap_or(file_name) }
            }),
            _ => serde_json::json!({
                "touser": touser,
                "msgtype": "file",
                "agentid": agent_id,
                "file": { "media_id": media_id }
            }),
        };
        let resp = self
            .client
            .post(&send_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                GatewayError::SendFailed(format!("WeCom callback media send failed: {e}"))
            })?;
        if !resp.status().is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "WeCom callback media send error: {text}"
            )));
        }
        Ok(())
    }

    async fn send_image_url(
        &self,
        chat_id: &str,
        image_url: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        if let Some(path) = image_url.strip_prefix("file://") {
            let decoded_path = urlencoding::decode(path)
                .map(|s| s.into_owned())
                .unwrap_or_else(|_| path.to_string());
            return self.send_file(chat_id, &decoded_path, caption).await;
        }

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
            let bytes = resp
                .bytes()
                .await
                .map_err(|e| format!("read body failed: {e}"))?
                .to_vec();
            if bytes.is_empty() {
                return Err("empty body".to_string());
            }
            Ok((bytes, content_type))
        }
        .await;

        let (bytes, content_type) = match downloaded {
            Ok(result) => result,
            Err(err) => {
                warn!(
                    image_url = %image_url,
                    error = %err,
                    "WeCom callback image-url download failed; falling back to text"
                );
                let fallback = image_fallback_text(image_url, caption);
                return self
                    .send_message(chat_id, &fallback, Some(ParseMode::Plain))
                    .await;
            }
        };

        let file_name = remote_image_file_name(image_url, content_type.as_deref());
        let suffix = std::path::Path::new(&file_name)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{e}"))
            .unwrap_or_else(|| ".png".to_string());
        let temp_path = std::env::temp_dir().join(format!(
            "hermes_wecom_callback_img_{}{}",
            uuid::Uuid::new_v4(),
            suffix
        ));
        tokio::fs::write(&temp_path, &bytes).await.map_err(|e| {
            GatewayError::SendFailed(format!("Failed to write temp image file: {e}"))
        })?;

        let temp_path_str = temp_path.to_string_lossy().to_string();
        let send_result = self.send_file(chat_id, &temp_path_str, caption).await;
        if let Err(err) = tokio::fs::remove_file(&temp_path).await {
            warn!(
                path = %temp_path.display(),
                error = %err,
                "Failed to remove temporary WeCom callback image file"
            );
        }

        match send_result {
            Ok(()) => Ok(()),
            Err(err) => {
                warn!(
                    image_url = %image_url,
                    error = %err,
                    "WeCom callback image upload failed; falling back to text"
                );
                let fallback = image_fallback_text(image_url, caption);
                self.send_message(chat_id, &fallback, Some(ParseMode::Plain))
                    .await
            }
        }
    }

    fn is_running(&self) -> bool {
        self.base.is_running()
    }

    fn platform_name(&self) -> &str {
        "wecom_callback"
    }
}

async fn handle_callback_request(
    stream: tokio::net::TcpStream,
    _peer: SocketAddr,
    apps: &[WeComCallbackApp],
    expected_path: &str,
    inbound_tx: Arc<RwLock<Option<mpsc::Sender<IncomingMessage>>>>,
    seen: Arc<RwLock<HashMap<String, Instant>>>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut buf = vec![0u8; 65536];
    let (mut reader, mut writer) = stream.into_split();
    let n = reader.read(&mut buf).await?;
    if n == 0 {
        return Ok(());
    }
    let req = String::from_utf8_lossy(&buf[..n]);
    let line = req.lines().next().unwrap_or("");
    let parts: Vec<&str> = line.split_whitespace().collect();
    let method = parts.first().copied().unwrap_or("GET");
    let full_path = parts.get(1).copied().unwrap_or("/");
    let (path, query) = if let Some((p, q)) = full_path.split_once('?') {
        (p, q)
    } else {
        (full_path, "")
    };
    if path != expected_path {
        writer
            .write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n")
            .await?;
        return Ok(());
    }
    let params: HashMap<String, String> = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect();
    let msg_signature = params.get("msg_signature").cloned().unwrap_or_default();
    let timestamp = params.get("timestamp").cloned().unwrap_or_default();
    let nonce = params.get("nonce").cloned().unwrap_or_default();

    if method == "GET" {
        let echostr = params.get("echostr").cloned().unwrap_or_default();
        for app in apps {
            if let Ok(xml) =
                WeComCallbackAdapter::decrypt_xml(app, &msg_signature, &timestamp, &nonce, &echostr)
            {
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{}",
                    xml.len(),
                    xml
                );
                writer.write_all(resp.as_bytes()).await?;
                return Ok(());
            }
        }
        writer
            .write_all(b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\n\r\n")
            .await?;
        return Ok(());
    }

    if method != "POST" {
        writer
            .write_all(b"HTTP/1.1 405 Method Not Allowed\r\nContent-Length: 0\r\n\r\n")
            .await?;
        return Ok(());
    }

    let body_start = req.find("\r\n\r\n").map(|i| i + 4).unwrap_or(n);
    let body = String::from_utf8_lossy(&buf[body_start..n]).to_string();
    let encrypt = WeComCallbackAdapter::xml_tag(&body, "Encrypt").unwrap_or_default();
    for app in apps {
        let Ok(xml) =
            WeComCallbackAdapter::decrypt_xml(app, &msg_signature, &timestamp, &nonce, &encrypt)
        else {
            continue;
        };
        let from_user = WeComCallbackAdapter::xml_tag(&xml, "FromUserName").unwrap_or_default();
        if from_user.trim().is_empty() {
            break;
        }
        let msg_id = WeComCallbackAdapter::xml_tag(&xml, "MsgId")
            .or_else(|| {
                WeComCallbackAdapter::xml_tag(&xml, "CreateTime")
                    .map(|t| format!("{from_user}:{t}"))
            })
            .unwrap_or_else(|| format!("{from_user}:0"));
        {
            let mut s = seen.write().await;
            let now = Instant::now();
            s.retain(|_, at| now.duration_since(*at) < Duration::from_secs(DEDUP_TTL_SECS));
            if s.contains_key(&msg_id) {
                writer.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 7\r\n\r\nsuccess").await?;
                return Ok(());
            }
            s.insert(msg_id.clone(), now);
        }
        let chat_id = WeComCallbackAdapter::xml_tag(&xml, "ChatId").unwrap_or_default();
        let is_group = !chat_id.trim().is_empty();
        let msg_type = WeComCallbackAdapter::xml_tag(&xml, "MsgType")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let event = WeComCallbackAdapter::xml_tag(&xml, "Event")
            .unwrap_or_default()
            .to_ascii_lowercase();
        let content = WeComCallbackAdapter::xml_tag(&xml, "Content").unwrap_or_default();
        let mut text = if msg_type == "text" {
            content
        } else if msg_type == "event" && (event == "subscribe" || event == "enter_agent") {
            "/start".to_string()
        } else {
            "".to_string()
        };
        if is_group && !text.is_empty() {
            text = strip_group_mention_prefix(&text);
        }
        if !text.is_empty() {
            if let Some(tx) = inbound_tx.read().await.clone() {
                let _ = tx
                    .send(IncomingMessage {
                        platform: "wecom_callback".to_string(),
                        chat_id: format!(
                            "{}:{}",
                            app.corp_id,
                            if is_group {
                                chat_id.as_str()
                            } else {
                                from_user.as_str()
                            }
                        ),
                        user_id: from_user,
                        text,
                        message_id: Some(msg_id),
                        is_dm: !is_group,
                    })
                    .await;
            }
        }
        writer
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 7\r\n\r\nsuccess",
            )
            .await?;
        return Ok(());
    }

    writer
        .write_all(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn app() -> WeComCallbackApp {
        WeComCallbackApp {
            name: "default".to_string(),
            corp_id: "ww_test".to_string(),
            corp_secret: "secret".to_string(),
            agent_id: "100001".to_string(),
            token: "token-abc".to_string(),
            encoding_aes_key: "abcdefghijklmnopqrstuvwxyz0123456789ABCDEFG".to_string(),
        }
    }

    #[test]
    fn signature_is_stable() {
        let sig = WeComCallbackAdapter::sha1_signature("t", "1", "2", "enc");
        assert_eq!(sig, "97df0b59cbd11ad14d4dfbdafb8dbd7b6dafcfec");
    }

    #[test]
    fn decrypt_rejects_bad_signature() {
        let app = app();
        let err = WeComCallbackAdapter::decrypt_xml(&app, "bad", "1", "2", "Zm9vYmFy")
            .expect_err("signature mismatch expected");
        assert!(format!("{err}").contains("signature mismatch"));
    }

    #[test]
    fn normalized_image_content_type_filters_non_images() {
        assert_eq!(
            normalized_image_content_type(Some("image/png; charset=utf-8")),
            Some("image/png".to_string())
        );
        assert_eq!(normalized_image_content_type(Some("text/plain")), None);
        assert_eq!(normalized_image_content_type(None), None);
    }

    #[test]
    fn remote_image_file_name_adds_extension_from_content_type() {
        let file_name =
            remote_image_file_name("https://cdn.example.com/photos/latest", Some("image/jpeg"));
        assert_eq!(file_name, "latest.jpg");
    }

    #[test]
    fn remote_image_file_name_uses_default_when_url_is_empty() {
        let file_name = remote_image_file_name("", None);
        assert_eq!(file_name, "image.png");
    }

    #[test]
    fn remote_image_file_name_keeps_existing_extension() {
        let file_name = remote_image_file_name(
            "https://cdn.example.com/photos/snapshot.webp?x=1",
            Some("image/png"),
        );
        assert_eq!(file_name, "snapshot.webp");
        assert_eq!(
            Path::new(&file_name).extension().and_then(|e| e.to_str()),
            Some("webp")
        );
    }

    #[test]
    fn image_fallback_text_includes_caption_when_present() {
        assert_eq!(
            image_fallback_text("https://example.com/plot.png", Some("Daily chart")),
            "Daily chart\nhttps://example.com/plot.png"
        );
        assert_eq!(
            image_fallback_text("https://example.com/plot.png", Some("   ")),
            "https://example.com/plot.png"
        );
    }

    #[test]
    fn strip_group_mention_prefix_removes_leading_mention() {
        assert_eq!(
            strip_group_mention_prefix("@HermesBot /approve run-123"),
            "/approve run-123"
        );
        assert_eq!(
            strip_group_mention_prefix("   @HermesBot    /approve run-123"),
            "/approve run-123"
        );
    }

    #[test]
    fn strip_group_mention_prefix_handles_mention_only() {
        assert_eq!(strip_group_mention_prefix("@HermesBot"), "");
    }

    #[test]
    fn strip_group_mention_prefix_keeps_non_mentions() {
        assert_eq!(
            strip_group_mention_prefix("/approve run-123"),
            "/approve run-123"
        );
        assert_eq!(strip_group_mention_prefix("hello team"), "hello team");
    }
}
