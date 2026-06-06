//! ntfy push-notification platform adapter.
//!
//! The adapter subscribes to `{server}/{topic}/json` and publishes replies with
//! `X-Tags: hermes-agent`. Incoming messages carrying that tag are skipped so a
//! shared subscribe/publish topic cannot make the agent answer its own replies.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use base64::{engine::general_purpose, Engine as _};
use futures::StreamExt;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, Notify, RwLock};
use tracing::{debug, info, warn};

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter, SendMessageOptions};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};
use crate::gateway::IncomingMessage;

pub const DEFAULT_SERVER: &str = "https://ntfy.sh";
pub const ECHO_TAG: &str = "hermes-agent";
pub const MAX_MESSAGE_LENGTH: usize = 4096;

const DEDUP_WINDOW: Duration = Duration::from_secs(300);
const DEDUP_MAX: usize = 1000;
const STREAM_TIMEOUT_SECONDS: u64 = 90;
const RECONNECT_SECS: &[u64] = &[2, 5, 10, 30, 60];

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NtfyConfig {
    #[serde(default = "default_server")]
    pub server: String,
    pub topic: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publish_topic: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(default)]
    pub markdown: bool,
    #[serde(default = "default_stream_timeout_secs")]
    pub stream_timeout_secs: u64,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

impl NtfyConfig {
    pub fn from_platform_config(platform: &hermes_config::PlatformConfig) -> Self {
        let extra = &platform.extra;
        let server = extra_string(extra, "server")
            .or_else(|| std::env::var("NTFY_SERVER_URL").ok())
            .map(|v| v.trim().trim_end_matches('/').to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_else(default_server);
        let topic = extra_string(extra, "topic")
            .or_else(|| std::env::var("NTFY_TOPIC").ok())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty())
            .unwrap_or_default();
        let publish_topic = extra_string(extra, "publish_topic")
            .or_else(|| std::env::var("NTFY_PUBLISH_TOPIC").ok())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let token = platform
            .token
            .clone()
            .or_else(|| extra_string(extra, "token"))
            .or_else(|| std::env::var("NTFY_TOKEN").ok())
            .map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty());
        let markdown = extra_bool(extra, "markdown").unwrap_or_else(|| {
            std::env::var("NTFY_MARKDOWN")
                .ok()
                .map(|v| truthy(&v))
                .unwrap_or(false)
        });
        let stream_timeout_secs = extra
            .get("stream_timeout_secs")
            .and_then(|v| v.as_u64())
            .unwrap_or(STREAM_TIMEOUT_SECONDS);

        Self {
            server,
            topic,
            publish_topic,
            token,
            markdown,
            stream_timeout_secs,
            proxy: AdapterProxyConfig::default(),
        }
    }
}

fn default_server() -> String {
    DEFAULT_SERVER.to_string()
}

fn default_stream_timeout_secs() -> u64 {
    STREAM_TIMEOUT_SECONDS
}

fn extra_string(extra: &HashMap<String, serde_json::Value>, key: &str) -> Option<String> {
    extra
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|v| !v.is_empty())
        .map(String::from)
}

fn extra_bool(extra: &HashMap<String, serde_json::Value>, key: &str) -> Option<bool> {
    match extra.get(key)? {
        serde_json::Value::Bool(v) => Some(*v),
        serde_json::Value::String(v) => Some(truthy(v)),
        serde_json::Value::Number(n) => Some(n.as_i64().unwrap_or_default() != 0),
        _ => None,
    }
}

fn truthy(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

#[derive(Debug, Clone, Deserialize)]
pub struct NtfyEvent {
    #[serde(default)]
    pub event: String,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub topic: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub time: Option<i64>,
}

struct NtfyInner {
    config: NtfyConfig,
    client: Client,
    base: BasePlatformAdapter,
    inbound_tx: RwLock<Option<mpsc::Sender<IncomingMessage>>>,
    seen: RwLock<HashMap<String, Instant>>,
    stop: Notify,
}

pub struct NtfyAdapter {
    inner: Arc<NtfyInner>,
    run_task: RwLock<Option<tokio::task::JoinHandle<()>>>,
}

impl NtfyAdapter {
    pub fn new(config: NtfyConfig) -> Result<Self, GatewayError> {
        if config.topic.trim().is_empty() {
            return Err(GatewayError::Platform(
                "ntfy requires topic (platforms.ntfy.topic or NTFY_TOPIC)".into(),
            ));
        }
        let base = BasePlatformAdapter::new(config.token.clone().unwrap_or_else(|| {
            if config.topic.is_empty() {
                "ntfy".to_string()
            } else {
                config.topic.clone()
            }
        }))
        .with_proxy(config.proxy.clone());
        let client = base.build_client()?;
        let inner = Arc::new(NtfyInner {
            config,
            client,
            base,
            inbound_tx: RwLock::new(None),
            seen: RwLock::new(HashMap::new()),
            stop: Notify::new(),
        });
        Ok(Self {
            inner,
            run_task: RwLock::new(None),
        })
    }

    pub fn config(&self) -> &NtfyConfig {
        &self.inner.config
    }

    pub async fn set_inbound_sender(&self, tx: mpsc::Sender<IncomingMessage>) {
        *self.inner.inbound_tx.write().await = Some(tx);
    }

    pub async fn handle_event(&self, event: NtfyEvent) -> Result<bool, GatewayError> {
        Self::process_event(self.inner.clone(), event).await
    }

    fn build_headers(
        &self,
        include_echo_tag: bool,
        markdown: bool,
    ) -> Result<HeaderMap, GatewayError> {
        build_headers(
            self.inner.config.token.as_deref(),
            include_echo_tag,
            markdown,
        )
    }

    fn publish_topic_for(&self, chat_id: &str, explicit_chat_id: bool) -> String {
        if explicit_chat_id {
            return chat_id.trim().to_string();
        }
        self.inner
            .config
            .publish_topic
            .as_deref()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or(chat_id)
            .trim()
            .to_string()
    }

    async fn is_duplicate(inner: &NtfyInner, message_id: &str) -> bool {
        let now = Instant::now();
        let mut seen = inner.seen.write().await;
        if seen.len() > DEDUP_MAX {
            seen.retain(|_, at| now.duration_since(*at) <= DEDUP_WINDOW);
        }
        if seen.contains_key(message_id) {
            return true;
        }
        seen.insert(message_id.to_string(), now);
        false
    }

    async fn process_event(inner: Arc<NtfyInner>, event: NtfyEvent) -> Result<bool, GatewayError> {
        if event.event != "message" {
            return Ok(false);
        }
        let message_id = event.id.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        if Self::is_duplicate(&inner, &message_id).await {
            debug!("ntfy duplicate message {}, skipping", message_id);
            return Ok(false);
        }
        if event.tags.iter().any(|tag| tag == ECHO_TAG) {
            debug!("ntfy skipping own echoed message tagged {}", ECHO_TAG);
            return Ok(false);
        }
        let text = event.message.unwrap_or_default().trim().to_string();
        if text.is_empty() {
            debug!("ntfy empty message body, skipping");
            return Ok(false);
        }
        let topic = event
            .topic
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .unwrap_or(inner.config.topic.as_str())
            .to_string();
        let incoming = IncomingMessage {
            platform: "ntfy".to_string(),
            chat_id: topic.clone(),
            user_id: topic,
            text,
            message_id: Some(message_id),
            is_dm: true,
        };
        if let Some(tx) = inner.inbound_tx.read().await.as_ref() {
            tx.send(incoming)
                .await
                .map_err(|e| GatewayError::Platform(format!("ntfy inbound channel closed: {e}")))?;
            Ok(true)
        } else {
            debug!("ntfy inbound sender unset; dropping message");
            Ok(false)
        }
    }

    async fn run_stream(inner: Arc<NtfyInner>) {
        let mut backoff_idx = 0;
        while inner.base.is_running() {
            match Self::consume_stream(inner.clone()).await {
                Ok(()) => backoff_idx = 0,
                Err(e) => {
                    warn!("ntfy stream error: {}", e);
                    let delay = RECONNECT_SECS
                        .get(backoff_idx)
                        .copied()
                        .unwrap_or(*RECONNECT_SECS.last().unwrap_or(&60));
                    backoff_idx += 1;
                    tokio::select! {
                        _ = tokio::time::sleep(Duration::from_secs(delay)) => {}
                        _ = inner.stop.notified() => break,
                    }
                }
            }
        }
    }

    async fn consume_stream(inner: Arc<NtfyInner>) -> Result<(), GatewayError> {
        let url = format!(
            "{}/{}/json",
            inner.config.server.trim_end_matches('/'),
            inner.config.topic
        );
        let headers = build_headers(inner.config.token.as_deref(), false, false)?;
        let response = inner
            .client
            .get(&url)
            .headers(headers)
            .query(&[("poll", "false")])
            .timeout(Duration::from_secs(inner.config.stream_timeout_secs))
            .send()
            .await
            .map_err(|e| GatewayError::ConnectionFailed(format!("ntfy stream connect: {e}")))?;
        let status = response.status();
        if status == reqwest::StatusCode::UNAUTHORIZED {
            return Err(GatewayError::Auth(
                "ntfy server rejected auth (401). Check NTFY_TOKEN.".into(),
            ));
        }
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(GatewayError::ConnectionFailed(format!(
                "ntfy topic '{}' returned 404",
                inner.config.topic
            )));
        }
        if !status.is_success() {
            return Err(GatewayError::ConnectionFailed(format!(
                "ntfy stream HTTP {status}"
            )));
        }

        let mut stream = response.bytes_stream();
        let mut buffer = String::new();
        while let Some(chunk) = stream.next().await {
            if !inner.base.is_running() {
                break;
            }
            let chunk =
                chunk.map_err(|e| GatewayError::ConnectionFailed(format!("ntfy stream: {e}")))?;
            buffer.push_str(&String::from_utf8_lossy(&chunk));
            while let Some(pos) = buffer.find('\n') {
                let line = buffer[..pos].trim().to_string();
                buffer = buffer[pos + 1..].to_string();
                if line.is_empty() {
                    continue;
                }
                match serde_json::from_str::<NtfyEvent>(&line) {
                    Ok(event) => {
                        if let Err(e) = Self::process_event(inner.clone(), event).await {
                            warn!("ntfy event dispatch failed: {}", e);
                        }
                    }
                    Err(e) => debug!("ntfy ignored malformed event line: {}", e),
                }
            }
        }
        Ok(())
    }
}

fn build_headers(
    token: Option<&str>,
    include_echo_tag: bool,
    markdown: bool,
) -> Result<HeaderMap, GatewayError> {
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    if include_echo_tag {
        headers.insert("X-Tags", HeaderValue::from_static(ECHO_TAG));
    }
    if markdown {
        headers.insert("X-Markdown", HeaderValue::from_static("true"));
    }
    if let Some(token) = token.map(str::trim).filter(|v| !v.is_empty()) {
        let value = if token.contains(':') {
            let encoded = general_purpose::STANDARD.encode(token.as_bytes());
            format!("Basic {encoded}")
        } else {
            format!("Bearer {token}")
        };
        let header = HeaderValue::from_str(&value)
            .map_err(|e| GatewayError::Auth(format!("invalid ntfy auth header: {e}")))?;
        headers.insert(AUTHORIZATION, header);
    }
    Ok(headers)
}

fn truncate_message(text: &str) -> String {
    text.chars().take(MAX_MESSAGE_LENGTH).collect()
}

#[async_trait]
impl PlatformAdapter for NtfyAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "ntfy adapter starting (server: {}, topic: {})",
            self.inner.config.server, self.inner.config.topic
        );
        self.inner.base.mark_running();
        let inner = self.inner.clone();
        let task = tokio::spawn(async move {
            Self::run_stream(inner).await;
        });
        *self.run_task.write().await = Some(task);
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("ntfy adapter stopping");
        self.inner.base.mark_stopped();
        self.inner.stop.notify_waiters();
        if let Some(task) = self.run_task.write().await.take() {
            task.abort();
        }
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        self.send_message_with_options(chat_id, text, parse_mode, SendMessageOptions::default())
            .await
    }

    async fn send_message_with_options(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
        options: SendMessageOptions,
    ) -> Result<(), GatewayError> {
        let publish_topic = self.publish_topic_for(chat_id, options.explicit_chat_id);
        if publish_topic.is_empty() {
            return Err(GatewayError::SendFailed(
                "ntfy publish topic is empty".into(),
            ));
        }
        let markdown =
            self.inner.config.markdown || matches!(parse_mode, Some(ParseMode::Markdown));
        let headers = self.build_headers(true, markdown)?;
        let url = format!(
            "{}/{}",
            self.inner.config.server.trim_end_matches('/'),
            publish_topic
        );
        let body = truncate_message(text);
        let response = self
            .inner
            .client
            .post(&url)
            .headers(headers)
            .body(body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("ntfy publish failed: {e}")))?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "ntfy HTTP {status}: {}",
                body.chars().take(200).collect::<String>()
            )));
        }
        Ok(())
    }

    async fn edit_message(
        &self,
        chat_id: &str,
        _message_id: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        self.send_message(chat_id, text, None).await
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        self.send_file_with_options(chat_id, file_path, caption, SendMessageOptions::default())
            .await
    }

    async fn send_file_with_options(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
        options: SendMessageOptions,
    ) -> Result<(), GatewayError> {
        let text = match caption.map(str::trim).filter(|v| !v.is_empty()) {
            Some(caption) => format!("{caption}\n[Attachment: {file_path}]"),
            None => format!("[Attachment: {file_path}]"),
        };
        self.send_message_with_options(chat_id, &text, None, options)
            .await
    }

    fn is_running(&self) -> bool {
        self.inner.base.is_running()
    }

    fn platform_name(&self) -> &str {
        "ntfy"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;
    use wiremock::matchers::{body_string, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn test_config(server: String) -> NtfyConfig {
        NtfyConfig {
            server,
            topic: "hermes-in".to_string(),
            publish_topic: None,
            token: None,
            markdown: false,
            stream_timeout_secs: STREAM_TIMEOUT_SECONDS,
            proxy: AdapterProxyConfig::default(),
        }
    }

    #[tokio::test]
    async fn echo_tagged_event_is_skipped() {
        let adapter = NtfyAdapter::new(test_config(DEFAULT_SERVER.to_string())).unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        adapter.set_inbound_sender(tx).await;

        let dispatched = adapter
            .handle_event(NtfyEvent {
                event: "message".into(),
                id: Some("echo-1".into()),
                topic: Some("hermes-in".into()),
                message: Some("own reply".into()),
                tags: vec![ECHO_TAG.into()],
                time: None,
            })
            .await
            .unwrap();

        assert!(!dispatched);
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn event_with_other_tags_is_dispatched() {
        let adapter = NtfyAdapter::new(test_config(DEFAULT_SERVER.to_string())).unwrap();
        let (tx, mut rx) = mpsc::channel(1);
        adapter.set_inbound_sender(tx).await;

        let dispatched = adapter
            .handle_event(NtfyEvent {
                event: "message".into(),
                id: Some("user-1".into()),
                topic: Some("hermes-in".into()),
                message: Some("hello".into()),
                tags: vec!["warning".into()],
                time: None,
            })
            .await
            .unwrap();

        assert!(dispatched);
        let incoming = rx.recv().await.unwrap();
        assert_eq!(incoming.platform, "ntfy");
        assert_eq!(incoming.chat_id, "hermes-in");
        assert_eq!(incoming.user_id, "hermes-in");
        assert_eq!(incoming.text, "hello");
        assert_eq!(incoming.message_id.as_deref(), Some("user-1"));
    }

    #[tokio::test]
    async fn send_message_emits_echo_tag_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hermes-in"))
            .and(header("X-Tags", ECHO_TAG))
            .and(body_string("Hello!"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "id": "id-1"
            })))
            .expect(1)
            .mount(&server)
            .await;

        let adapter = NtfyAdapter::new(test_config(server.uri())).unwrap();
        adapter
            .send_message("hermes-in", "Hello!", None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn send_message_uses_configured_publish_topic_for_non_explicit_reply() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/configured-topic"))
            .and(header("X-Tags", ECHO_TAG))
            .and(body_string("done"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let mut config = test_config(server.uri());
        config.publish_topic = Some("configured-topic".into());
        let adapter = NtfyAdapter::new(config).unwrap();
        adapter
            .send_message("hermes-in", "done", None)
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn explicit_send_overrides_configured_publish_topic() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/alerts-channel"))
            .and(header("X-Tags", ECHO_TAG))
            .and(body_string("done"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let mut config = test_config(server.uri());
        config.publish_topic = Some("configured-topic".into());
        let adapter = NtfyAdapter::new(config).unwrap();
        adapter
            .send_message_with_options(
                "alerts-channel",
                "done",
                None,
                SendMessageOptions {
                    explicit_chat_id: true,
                    ..SendMessageOptions::default()
                },
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn explicit_file_send_overrides_configured_publish_topic() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/alerts-channel"))
            .and(header("X-Tags", ECHO_TAG))
            .and(body_string("[Attachment: /tmp/report.pdf]"))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let mut config = test_config(server.uri());
        config.publish_topic = Some("configured-topic".into());
        let adapter = NtfyAdapter::new(config).unwrap();
        adapter
            .send_file_with_options(
                "alerts-channel",
                "/tmp/report.pdf",
                None,
                SendMessageOptions {
                    explicit_chat_id: true,
                    ..SendMessageOptions::default()
                },
            )
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn send_message_emits_auth_and_markdown_headers() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/hermes-in"))
            .and(header("Authorization", "Bearer mytoken"))
            .and(header("X-Markdown", "true"))
            .and(header("X-Tags", ECHO_TAG))
            .respond_with(ResponseTemplate::new(200))
            .expect(1)
            .mount(&server)
            .await;

        let mut config = test_config(server.uri());
        config.token = Some("mytoken".into());
        config.markdown = true;
        let adapter = NtfyAdapter::new(config).unwrap();
        adapter
            .send_message("hermes-in", "**bold**", None)
            .await
            .unwrap();
    }
}
