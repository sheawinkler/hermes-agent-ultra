//! DingTalk chatbot adapter — **Stream mode** (Open DingTalk).
//!
//! Matches Python `gateway/platforms/dingtalk.py` + `dingtalk-stream-sdk-python`:
//! - `POST {DINGTALK_OPENAPI_ENDPOINT}/v1.0/gateway/connections/open`
//! - WebSocket to `endpoint?ticket=...`
//! - Callback topic `/v1.0/im/bot/messages/get`
//! - Outbound replies via **`sessionWebhook`** (markdown), keyed by `conversation_id`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::{SinkExt, StreamExt};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, Notify, RwLock};
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tracing::{debug, error, info, warn};
use url::Url;

use hermes_core::errors::GatewayError;
use hermes_core::traits::{ParseMode, PlatformAdapter};

use crate::adapter::{AdapterProxyConfig, BasePlatformAdapter};
use crate::gateway::IncomingMessage;

const CHATBOT_TOPIC: &str = "/v1.0/im/bot/messages/get";
const SESSION_WEBHOOK_RE: &str = r"^https://api\.dingtalk\.com/";
const MAX_MARKDOWN: usize = 20_000;
const DEDUP_WINDOW: Duration = Duration::from_secs(300);
const DEDUP_MAX: usize = 1000;
const RECONNECT_SECS: &[u64] = &[2, 5, 10, 30, 60];
const SESSION_WEBHOOKS_MAX: usize = 500;

fn default_openapi() -> String {
    std::env::var("DINGTALK_OPENAPI_ENDPOINT")
        .unwrap_or_else(|_| "https://api.dingtalk.com".to_string())
}

fn guess_local_ip() -> String {
    std::net::UdpSocket::bind("0.0.0.0:0")
        .ok()
        .and_then(|s| {
            s.connect("8.8.8.8:80").ok()?;
            s.local_addr().ok().map(|a| a.ip().to_string())
        })
        .unwrap_or_else(|| "127.0.0.1".into())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DingTalkConfig {
    /// App key / client id (`DINGTALK_CLIENT_ID` in Python).
    pub client_id: String,
    /// App secret (`DINGTALK_CLIENT_SECRET` in Python).
    pub client_secret: String,
    #[serde(default = "default_openapi")]
    pub openapi_endpoint: String,
    #[serde(default)]
    pub proxy: AdapterProxyConfig,
}

impl DingTalkConfig {
    /// 从 [`hermes_config::PlatformConfig`] 构建（`extra` 键名与 Python `DingTalkAdapter` 一致）。
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
        let openapi = {
            let v = gv("openapi_endpoint");
            if v.is_empty() {
                default_openapi()
            } else {
                v
            }
        };
        Self {
            client_id: gv("client_id"),
            client_secret: gv("client_secret"),
            openapi_endpoint: openapi,
            proxy: AdapterProxyConfig::default(),
        }
    }
}

/// Parsed inbound chatbot payload (subset of `ChatbotMessage`).
#[derive(Debug, Clone)]
pub struct IncomingDingTalkMessage {
    pub conversation_id: String,
    pub sender_id: String,
    pub text: String,
    pub is_group: bool,
    pub message_id: String,
    pub session_webhook: Option<String>,
}

struct DingTalkInner {
    config: DingTalkConfig,
    client: Client,
    session_webhooks: RwLock<HashMap<String, String>>,
    seen: RwLock<HashMap<String, Instant>>,
    inbound_tx: RwLock<Option<mpsc::Sender<IncomingMessage>>>,
    stop: Notify,
    base: BasePlatformAdapter,
}

pub struct DingTalkAdapter {
    inner: Arc<DingTalkInner>,
    stop_signal: Arc<Notify>,
    run_task: RwLock<Option<tokio::task::JoinHandle<()>>>,
}

impl DingTalkAdapter {
    pub fn new(config: DingTalkConfig) -> Result<Self, GatewayError> {
        if config.client_id.is_empty() || config.client_secret.is_empty() {
            return Err(GatewayError::Platform(
                "DingTalk Stream requires client_id and client_secret".into(),
            ));
        }
        let base = BasePlatformAdapter::new(&config.client_id).with_proxy(config.proxy.clone());
        base.validate_token()?;
        let client = base.build_client()?;
        let inner = Arc::new(DingTalkInner {
            config,
            client,
            session_webhooks: RwLock::new(HashMap::new()),
            seen: RwLock::new(HashMap::new()),
            inbound_tx: RwLock::new(None),
            stop: Notify::new(),
            base,
        });
        Ok(Self {
            inner,
            stop_signal: Arc::new(Notify::new()),
            run_task: RwLock::new(None),
        })
    }

    pub fn config(&self) -> &DingTalkConfig {
        &self.inner.config
    }

    pub async fn set_inbound_sender(&self, tx: mpsc::Sender<IncomingMessage>) {
        *self.inner.inbound_tx.write().await = Some(tx);
    }

    #[cfg(test)]
    fn test_inner(&self) -> Arc<DingTalkInner> {
        self.inner.clone()
    }

    async fn open_connection(inner: &DingTalkInner) -> Result<Value, GatewayError> {
        let url = format!(
            "{}/v1.0/gateway/connections/open",
            inner.config.openapi_endpoint.trim_end_matches('/')
        );
        let ua = format!(
            "DingTalkStream/1.0 SDK/rust-hermes (+https://github.com/sheawinkler/hermes-agent-ultra)"
        );
        let body = serde_json::json!({
            "clientId": inner.config.client_id,
            "clientSecret": inner.config.client_secret,
            "subscriptions": [{"type": "CALLBACK", "topic": CHATBOT_TOPIC}],
            "ua": ua,
            "localIp": guess_local_ip(),
        });
        let resp = inner
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| {
                GatewayError::ConnectionFailed(format!("DingTalk open_connection: {e}"))
            })?;
        let status = resp.status();
        if !status.is_success() {
            let t = resp.text().await.unwrap_or_default();
            return Err(GatewayError::ConnectionFailed(format!(
                "DingTalk open_connection HTTP {status}: {t}"
            )));
        }
        resp.json().await.map_err(|e| {
            GatewayError::ConnectionFailed(format!("DingTalk open_connection json: {e}"))
        })
    }

    fn ws_uri(endpoint: &str, ticket: &str) -> Result<String, GatewayError> {
        let mut u = Url::parse(endpoint)
            .map_err(|e| GatewayError::ConnectionFailed(format!("Bad WS endpoint: {e}")))?;
        u.query_pairs_mut().append_pair("ticket", ticket);
        Ok(u.into())
    }

    fn parse_callback_data(data: &Value) -> Option<IncomingDingTalkMessage> {
        let conversation_id = data
            .get("conversationId")
            .and_then(|v| v.as_str())?
            .to_string();
        let sender_id = data
            .get("senderId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let conv_type = data
            .get("conversationType")
            .and_then(|v| v.as_str())
            .unwrap_or("1");
        let is_group = conv_type == "2";
        let message_id = data
            .get("msgId")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let session_webhook = data
            .get("sessionWebhook")
            .and_then(|v| v.as_str())
            .map(String::from);
        let text = if data.get("msgtype").and_then(|v| v.as_str()) == Some("text") {
            data.get("text")
                .and_then(|t| t.get("content"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_string()
        } else if data.get("msgtype").and_then(|v| v.as_str()) == Some("richText") {
            let mut parts = Vec::new();
            if let Some(arr) = data
                .get("content")
                .and_then(|c| c.get("richText"))
                .and_then(|r| r.as_array())
            {
                for item in arr {
                    if let Some(t) = item.get("text").and_then(|v| v.as_str()) {
                        parts.push(t);
                    }
                }
            }
            parts.join(" ").trim().to_string()
        } else {
            String::new()
        };
        Some(IncomingDingTalkMessage {
            conversation_id,
            sender_id,
            text,
            is_group,
            message_id,
            session_webhook,
        })
    }

    fn build_ack(message_id: Option<&str>) -> Value {
        serde_json::json!({
            "code": 200,
            "headers": {
                "messageId": message_id.unwrap_or(""),
                "contentType": "application/json",
            },
            "message": "OK",
            "data": serde_json::json!({"response": "OK"}).to_string(),
        })
    }

    async fn is_dup(inner: &DingTalkInner, msg_id: &str) -> bool {
        if msg_id.is_empty() {
            return false;
        }
        let now = Instant::now();
        let mut map = inner.seen.write().await;
        if map.len() > DEDUP_MAX {
            let cutoff = now - DEDUP_WINDOW;
            map.retain(|_, t| *t > cutoff);
        }
        if map.contains_key(msg_id) {
            return true;
        }
        map.insert(msg_id.to_string(), now);
        false
    }

    /// 处理一条 WS 文本帧。对聊天机器人 CALLBACK 返回待发送的 ACK JSON 字符串（与 Python Stream SDK 一致，应尽快回 ACK）。
    async fn handle_ws_message(
        inner: &DingTalkInner,
        txt: &str,
    ) -> Result<Option<String>, GatewayError> {
        let v: Value = serde_json::from_str(txt)
            .map_err(|e| GatewayError::Platform(format!("DingTalk WS JSON: {e}")))?;
        let msg_type = v.get("type").and_then(|t| t.as_str()).unwrap_or("");
        let headers = v.get("headers").cloned().unwrap_or(Value::Null);
        let topic = headers.get("topic").and_then(|t| t.as_str()).unwrap_or("");
        if msg_type == "SYSTEM" {
            if topic == "disconnect" {
                return Err(GatewayError::ConnectionFailed(
                    "DingTalk stream disconnect".into(),
                ));
            }
            return Ok(None);
        }

        if msg_type != "CALLBACK" || topic != CHATBOT_TOPIC {
            return Ok(None);
        }

        let mid = headers
            .get("messageId")
            .and_then(|x| x.as_str())
            .map(str::to_owned);
        let ack_line = || -> String { Self::build_ack(mid.as_deref()).to_string() };

        let data_raw = v.get("data").cloned().unwrap_or(Value::Null);
        let data: Value = if let Some(s) = data_raw.as_str() {
            serde_json::from_str(s).unwrap_or(Value::Null)
        } else {
            data_raw
        };

        let Some(parsed) = Self::parse_callback_data(&data) else {
            return Ok(Some(ack_line()));
        };
        if parsed.text.is_empty() {
            return Ok(Some(ack_line()));
        }
        if Self::is_dup(inner, &parsed.message_id).await {
            return Ok(Some(ack_line()));
        }

        if let Some(ref wh) = parsed.session_webhook {
            let re = regex::Regex::new(SESSION_WEBHOOK_RE).expect("valid regex");
            if re.is_match(wh) && !parsed.conversation_id.is_empty() {
                let mut m = inner.session_webhooks.write().await;
                if m.len() >= SESSION_WEBHOOKS_MAX {
                    if let Some(k) = m.keys().next().cloned() {
                        m.remove(&k);
                    }
                }
                m.insert(parsed.conversation_id.clone(), wh.clone());
            }
        }

        let chat_id = if parsed.conversation_id.is_empty() {
            parsed.sender_id.clone()
        } else {
            parsed.conversation_id.clone()
        };
        let incoming = IncomingMessage {
            platform: "dingtalk".into(),
            chat_id: chat_id.clone(),
            user_id: parsed.sender_id.clone(),
            text: parsed.text.clone(),
            message_id: if parsed.message_id.is_empty() {
                None
            } else {
                Some(parsed.message_id.clone())
            },
            is_dm: !parsed.is_group,
        };
        if let Some(tx) = inner.inbound_tx.read().await.clone() {
            tokio::spawn(async move {
                let _ = tx.send(incoming).await;
            });
        }

        Ok(Some(ack_line()))
    }

    async fn stream_loop(inner: Arc<DingTalkInner>) {
        let mut backoff = 0usize;
        while inner.base.is_running() {
            match Self::open_connection(&inner).await {
                Ok(conn) => {
                    let endpoint = conn
                        .get("endpoint")
                        .and_then(|e| e.as_str())
                        .unwrap_or("")
                        .to_string();
                    let ticket = conn.get("ticket").and_then(|t| t.as_str()).unwrap_or("");
                    if endpoint.is_empty() || ticket.is_empty() {
                        error!("DingTalk open_connection missing endpoint/ticket: {conn}");
                    } else {
                        match Self::ws_uri(&endpoint, ticket) {
                            Ok(uri) => {
                                info!("DingTalk stream connecting…");
                                match tokio_tungstenite::connect_async(&uri).await {
                                    Ok((mut ws, _)) => {
                                        backoff = 0;
                                        while inner.base.is_running() {
                                            tokio::select! {
                                                _ = inner.stop.notified() => {
                                                    let _ = ws.close(None).await;
                                                    return;
                                                }
                                                msg = ws.next() => {
                                                    match msg {
                                                        Some(Ok(WsMessage::Text(t))) => {
                                                            let (disconnect, ack) =
                                                                match Self::handle_ws_message(&inner, &t).await {
                                                                    Ok(ack) => (false, ack),
                                                                    Err(GatewayError::ConnectionFailed(ref s))
                                                                        if s.contains("disconnect") =>
                                                                    {
                                                                        (true, None)
                                                                    }
                                                                    Err(e) => {
                                                                        debug!(error = %e, "DingTalk callback handling");
                                                                        (false, None)
                                                                    }
                                                                };
                                                            if disconnect {
                                                                let _ = ws.close(None).await;
                                                                break;
                                                            }
                                                            if let Some(line) = ack {
                                                                let _ = ws.send(WsMessage::Text(line)).await;
                                                            }
                                                        }
                                                        Some(Ok(WsMessage::Ping(p))) => {
                                                            let _ = ws.send(WsMessage::Pong(p)).await;
                                                        }
                                                        Some(Ok(WsMessage::Close(_))) | None => break,
                                                        Some(Err(e)) => {
                                                            warn!("DingTalk WS error: {e}");
                                                            break;
                                                        }
                                                        _ => {}
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    Err(e) => warn!("DingTalk WS connect failed: {e}"),
                                }
                            }
                            Err(e) => warn!("DingTalk WS URI: {e}"),
                        }
                    }
                }
                Err(e) => warn!("DingTalk open_connection: {e}"),
            }
            let delay = RECONNECT_SECS[backoff.min(RECONNECT_SECS.len() - 1)];
            backoff = (backoff + 1).min(RECONNECT_SECS.len() - 1);
            tokio::time::sleep(Duration::from_secs(delay)).await;
        }
    }

    /// POST markdown to the session webhook for `chat_id` (conversation id).
    pub async fn send_markdown_session(
        &self,
        chat_id: &str,
        title: &str,
        text: &str,
    ) -> Result<(), GatewayError> {
        let wh = {
            let m = self.inner.session_webhooks.read().await;
            m.get(chat_id).cloned()
        };
        let Some(url) = wh else {
            return Err(GatewayError::SendFailed(
                "No session_webhook for chat_id; inbound message required first".into(),
            ));
        };
        let body = serde_json::json!({
            "msgtype": "markdown",
            "markdown": {
                "title": title,
                "text": text.chars().take(MAX_MARKDOWN).collect::<String>(),
            }
        });
        let resp = self
            .inner
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| GatewayError::SendFailed(format!("DingTalk session webhook: {e}")))?;
        let status = resp.status();
        if !status.is_success() {
            let t = resp.text().await.unwrap_or_default();
            return Err(GatewayError::SendFailed(format!(
                "DingTalk session webhook HTTP {status}: {t}"
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl PlatformAdapter for DingTalkAdapter {
    async fn start(&self) -> Result<(), GatewayError> {
        info!(
            "DingTalk Stream adapter starting (client_id={}, endpoint={})",
            self.inner.config.client_id, self.inner.config.openapi_endpoint
        );
        self.inner.base.mark_running();
        let inner = self.inner.clone();
        let h = tokio::spawn(async move {
            DingTalkAdapter::stream_loop(inner).await;
        });
        *self.run_task.write().await = Some(h);
        Ok(())
    }

    async fn stop(&self) -> Result<(), GatewayError> {
        info!("DingTalk Stream adapter stopping");
        self.inner.base.mark_stopped();
        self.inner.stop.notify_waiters();
        self.stop_signal.notify_one();
        if let Some(t) = self.run_task.write().await.take() {
            t.abort();
        }
        Ok(())
    }

    async fn send_message(
        &self,
        chat_id: &str,
        text: &str,
        parse_mode: Option<ParseMode>,
    ) -> Result<(), GatewayError> {
        let title = match parse_mode {
            Some(ParseMode::Markdown) => "Hermes",
            _ => "Hermes",
        };
        self.send_markdown_session(chat_id, title, text).await
    }

    async fn edit_message(
        &self,
        _chat_id: &str,
        _message_id: &str,
        _text: &str,
    ) -> Result<(), GatewayError> {
        debug!("DingTalk Stream does not support message editing");
        Ok(())
    }

    async fn send_file(
        &self,
        chat_id: &str,
        file_path: &str,
        caption: Option<&str>,
    ) -> Result<(), GatewayError> {
        let cap = caption.unwrap_or("");
        let msg = if file_path.starts_with("http://") || file_path.starts_with("https://") {
            format!("{cap}\n\n[file]({file_path})")
        } else {
            format!("{cap}\n\n[local file: {file_path}]")
        };
        self.send_markdown_session(chat_id, "Attachment", &msg)
            .await
    }

    fn is_running(&self) -> bool {
        self.inner.base.is_running()
    }

    fn platform_name(&self) -> &str {
        "dingtalk"
    }
}

#[cfg(test)]
mod dingtalk_ack_tests {
    use super::*;

    fn sample_cfg() -> DingTalkConfig {
        DingTalkConfig {
            client_id: "test_client".into(),
            client_secret: "test_secret".into(),
            openapi_endpoint: "https://api.dingtalk.com".into(),
            proxy: AdapterProxyConfig::default(),
        }
    }

    fn callback_frame_json(message_id: &str, msg_id: &str, text: &str) -> String {
        let data = serde_json::json!({
            "conversationId": "conv-1",
            "senderId": "user-1",
            "conversationType": "1",
            "msgtype": "text",
            "text": { "content": text },
            "msgId": msg_id,
            "sessionWebhook": "https://api.dingtalk.com/v1.0/robot/oapi/inbound/xxx",
        });
        serde_json::json!({
            "type": "CALLBACK",
            "headers": {
                "topic": CHATBOT_TOPIC,
                "messageId": message_id,
            },
            "data": data.to_string(),
        })
        .to_string()
    }

    #[test]
    fn build_ack_shape_matches_stream_contract() {
        let v = DingTalkAdapter::build_ack(Some("abc-123"));
        assert_eq!(v.get("code"), Some(&serde_json::json!(200)));
        assert_eq!(v.get("message"), Some(&serde_json::json!("OK")));
        let headers = v.get("headers").unwrap();
        assert_eq!(
            headers.get("messageId"),
            Some(&serde_json::json!("abc-123"))
        );
        assert_eq!(
            headers.get("contentType"),
            Some(&serde_json::json!("application/json"))
        );
        let data = v.get("data").and_then(|d| d.as_str()).unwrap();
        let parsed: Value = serde_json::from_str(data).unwrap();
        assert_eq!(parsed.get("response"), Some(&serde_json::json!("OK")));

        let v2 = DingTalkAdapter::build_ack(None);
        assert_eq!(
            v2.pointer("/headers/messageId").and_then(|x| x.as_str()),
            Some("")
        );
    }

    #[tokio::test]
    async fn handle_ws_callback_returns_ack_with_message_id() {
        let adapter = DingTalkAdapter::new(sample_cfg()).unwrap();
        let inner = adapter.test_inner();
        let frame = callback_frame_json("ws-mid-9", "ding-1", "hello");
        let ack_line = DingTalkAdapter::handle_ws_message(&inner, &frame)
            .await
            .unwrap()
            .expect("ack expected");
        let ack: Value = serde_json::from_str(&ack_line).unwrap();
        assert_eq!(
            ack.pointer("/headers/messageId").and_then(|x| x.as_str()),
            Some("ws-mid-9")
        );
    }

    #[tokio::test]
    async fn handle_ws_callback_empty_text_still_acks() {
        let adapter = DingTalkAdapter::new(sample_cfg()).unwrap();
        let inner = adapter.test_inner();
        let frame = callback_frame_json("mid-empty", "m2", "   ");
        let ack = DingTalkAdapter::handle_ws_message(&inner, &frame)
            .await
            .unwrap()
            .expect("ack");
        let v: Value = serde_json::from_str(&ack).unwrap();
        assert_eq!(
            v.pointer("/headers/messageId").and_then(|x| x.as_str()),
            Some("mid-empty")
        );
    }

    #[tokio::test]
    async fn handle_ws_callback_dup_still_acks() {
        let adapter = DingTalkAdapter::new(sample_cfg()).unwrap();
        let inner = adapter.test_inner();
        let frame = callback_frame_json("mid-dup", "same-msg", "x");
        let a1 = DingTalkAdapter::handle_ws_message(&inner, &frame)
            .await
            .unwrap();
        let a2 = DingTalkAdapter::handle_ws_message(&inner, &frame)
            .await
            .unwrap();
        assert!(a1.is_some());
        assert!(a2.is_some());
    }

    #[tokio::test]
    async fn handle_ws_wrong_topic_no_ack() {
        let adapter = DingTalkAdapter::new(sample_cfg()).unwrap();
        let inner = adapter.test_inner();
        let v = serde_json::json!({
            "type": "CALLBACK",
            "headers": { "topic": "/other/topic", "messageId": "x" },
            "data": "{}",
        });
        let out = DingTalkAdapter::handle_ws_message(&inner, &v.to_string())
            .await
            .unwrap();
        assert!(out.is_none());
    }

    #[tokio::test]
    async fn handle_ws_system_disconnect_errors() {
        let adapter = DingTalkAdapter::new(sample_cfg()).unwrap();
        let inner = adapter.test_inner();
        let v = serde_json::json!({
            "type": "SYSTEM",
            "headers": { "topic": "disconnect" },
        });
        let err = DingTalkAdapter::handle_ws_message(&inner, &v.to_string())
            .await
            .unwrap_err();
        match err {
            GatewayError::ConnectionFailed(s) => assert!(s.contains("disconnect")),
            e => panic!("unexpected {e:?}"),
        }
    }
}
