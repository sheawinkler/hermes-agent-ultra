use std::cmp::Ordering;
use std::collections::{BinaryHeap, VecDeque};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio::task::JoinHandle;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, connect_async};
use tracing::{info, warn};

use crate::config::{AipcTalkConfig, AipcTalkTransport};
use crate::error::{DemoError, Result};

type WsStream = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Work item dispatched to the in-process Hermes agent worker.
#[derive(Debug)]
pub struct HermesWorkItem {
    pub request_id: String,
    pub text: String,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub respond: oneshot::Sender<Result<String>>,
}

/// Push unsolicited Hermes messages (e.g. cron completions) into the voice session.
#[derive(Clone)]
pub struct TalkPushBridge {
    tx: mpsc::Sender<HermesMessage>,
}

impl TalkPushBridge {
    pub fn new(tx: mpsc::Sender<HermesMessage>) -> Self {
        Self { tx }
    }

    pub async fn push(&self, msg: HermesMessage) {
        let _ = self.tx.send(msg).await;
    }
}

fn normalize_delivery_status(status: &str) -> String {
    match status {
        "ok" | "final" => "final".to_string(),
        "error" => "error".to_string(),
        other => other.to_string(),
    }
}

#[async_trait]
trait HermesTransport: Send {
    async fn submit(&mut self, req: &HermesRequest, timeout_secs: Option<u64>) -> Result<String>;
    async fn poll_push(&mut self) -> Option<HermesMessage>;
}

struct WsTransport {
    config: AipcTalkConfig,
    conn: Option<HermesConnection>,
    msg_tx: mpsc::Sender<HermesMessage>,
}

struct HermesConnection {
    ws: WsStream,
}

impl HermesConnection {
    async fn connect(config: &AipcTalkConfig) -> Result<Self> {
        if !config.url.starts_with("ws://") && !config.url.starts_with("wss://") {
            return Err(DemoError::Tool(format!(
                "invalid hermes url '{}': must start with ws:// or wss://",
                config.url
            )));
        }
        let (ws, _response) =
            tokio::time::timeout(Duration::from_secs(10), connect_async(config.url.as_str()))
                .await
                .map_err(|_| DemoError::Tool("hermes connection timeout (>10s)".to_string()))?
                .map_err(|e| DemoError::Tool(format!("hermes WS connect failed: {e}")))?;
        Ok(Self { ws })
    }

    async fn request(
        &mut self,
        request_id: &str,
        text: &str,
        timeout_secs: Option<u64>,
        msg_tx: &mpsc::Sender<HermesMessage>,
    ) -> Result<String> {
        let req_json = serde_json::json!({
            "request_id": request_id,
            "text": text,
        })
        .to_string();

        self.ws
            .send(WsMessage::Text(req_json.into()))
            .await
            .map_err(|e| DemoError::Tool(format!("hermes send failed: {e}")))?;

        eprintln!("\n══════════ 发送给 hermes ══════════\n{text}\n══════════════════════════");

        loop {
            let response_msg = match timeout_secs {
                Some(secs) => tokio::time::timeout(Duration::from_secs(secs), self.ws.next())
                    .await
                    .map_err(|_| DemoError::Tool(format!("hermes response timeout (>{secs}s)")))?,
                None => self.ws.next().await,
            }
            .ok_or_else(|| DemoError::Tool("hermes WS stream ended".to_string()))?
            .map_err(|e| DemoError::Tool(format!("hermes WS read error: {e}")))?;

            let response_text = match response_msg {
                WsMessage::Text(t) => t.to_string(),
                WsMessage::Close(frame) => {
                    return Err(DemoError::Tool(format!(
                        "hermes closed connection: {:?}",
                        frame.map(|f| f.reason.to_string())
                    )));
                }
                other => {
                    return Err(DemoError::Tool(format!(
                        "hermes unexpected message type: {other:?}"
                    )));
                }
            };

            let response: serde_json::Value =
                serde_json::from_str(&response_text).map_err(|e| {
                    DemoError::Tool(format!(
                        "hermes invalid JSON response: {e}, raw: {response_text}"
                    ))
                })?;

            let resp_id = response["request_id"].as_str().unwrap_or("");
            if resp_id == request_id {
                let status = response["status"].as_str().unwrap_or("");
                if status != "ok" && status != "final" {
                    warn!(%status, %response_text, "hermes: non-ok status");
                }

                let text = response["text"].as_str().unwrap_or("").to_string();
                if text.contains("did not respond in time")
                    || text.contains("timeout")
                    || text.contains("timed out")
                {
                    return Err(DemoError::Tool(format!(
                        "hermes agent timeout, will retry: {text}"
                    )));
                }

                return Ok(text);
            }

            let text = response["text"].as_str().unwrap_or("").to_string();
            let status = normalize_delivery_status(response["status"].as_str().unwrap_or("ok"));
            info!(%resp_id, %text, "hermes: forwarding unsolicited message");
            let _ = msg_tx
                .send(HermesMessage {
                    request_id: resp_id.to_string(),
                    text,
                    status,
                })
                .await;
        }
    }
}

#[async_trait]
impl HermesTransport for WsTransport {
    async fn submit(&mut self, req: &HermesRequest, timeout_secs: Option<u64>) -> Result<String> {
        if self.conn.is_none() {
            self.conn = Some(HermesConnection::connect(&self.config).await?);
        }
        let conn = self.conn.as_mut().expect("connection just established");
        match conn
            .request(&req.id, &req.text, timeout_secs, &self.msg_tx)
            .await
        {
            Ok(text) => Ok(text),
            Err(e) => {
                self.conn = None;
                Err(e)
            }
        }
    }

    async fn poll_push(&mut self) -> Option<HermesMessage> {
        let conn = self.conn.as_mut()?;
        match conn.ws.next().await {
            Some(Ok(WsMessage::Text(t))) => {
                if let Ok(v) = serde_json::from_str::<serde_json::Value>(&t) {
                    let id = v["request_id"].as_str().unwrap_or("").to_string();
                    let txt = v["text"].as_str().unwrap_or("").to_string();
                    let st = normalize_delivery_status(v["status"].as_str().unwrap_or("ok"));
                    info!(%id, %txt, "hermes: unsolicited message received");
                    return Some(HermesMessage {
                        request_id: id,
                        text: txt,
                        status: st,
                    });
                }
                None
            }
            Some(Ok(WsMessage::Close(_))) | None => {
                self.conn = None;
                None
            }
            Some(Err(e)) => {
                warn!(%e, "hermes: WS error in idle read");
                self.conn = None;
                None
            }
            _ => None,
        }
    }
}

struct ChannelTransport {
    work_tx: mpsc::Sender<HermesWorkItem>,
}

#[async_trait]
impl HermesTransport for ChannelTransport {
    async fn submit(&mut self, req: &HermesRequest, timeout_secs: Option<u64>) -> Result<String> {
        let (tx, rx) = oneshot::channel();
        self.work_tx
            .send(HermesWorkItem {
                request_id: req.id.clone(),
                text: req.text.clone(),
                model: req.model.clone(),
                provider: req.provider.clone(),
                respond: tx,
            })
            .await
            .map_err(|_| DemoError::Tool("hermes bridge worker closed".to_string()))?;

        eprintln!(
            "\n══════════ 发送给 hermes ══════════\n{}\n══════════════════════════",
            req.text
        );

        match timeout_secs {
            Some(secs) => tokio::time::timeout(Duration::from_secs(secs), rx)
                .await
                .map_err(|_| DemoError::Tool(format!("hermes response timeout (>{secs}s)")))?
                .map_err(|_| DemoError::Tool("hermes bridge response lost".to_string()))?,
            None => rx
                .await
                .map_err(|_| DemoError::Tool("hermes bridge response lost".to_string()))?,
        }
    }

    async fn poll_push(&mut self) -> Option<HermesMessage> {
        None
    }
}

enum TransportBackend {
    WebSocket(Box<WsTransport>),
    Channel(ChannelTransport),
}

impl TransportBackend {
    fn from_config(
        config: &AipcTalkConfig,
        work_tx: Option<mpsc::Sender<HermesWorkItem>>,
        msg_tx: mpsc::Sender<HermesMessage>,
    ) -> Result<Self> {
        match config.transport {
            AipcTalkTransport::Ws => Ok(Self::WebSocket(Box::new(WsTransport {
                config: config.clone(),
                conn: None,
                msg_tx,
            }))),
            AipcTalkTransport::Channel => {
                let work_tx = work_tx.ok_or_else(|| {
                    DemoError::Tool(
                        "call_hermes channel transport requires embedded Hermes runtime"
                            .to_string(),
                    )
                })?;
                Ok(Self::Channel(ChannelTransport { work_tx }))
            }
        }
    }

    async fn submit(&mut self, req: &HermesRequest, timeout_secs: Option<u64>) -> Result<String> {
        match self {
            Self::WebSocket(t) => t.submit(req, timeout_secs).await,
            Self::Channel(t) => t.submit(req, timeout_secs).await,
        }
    }

    async fn poll_push(&mut self) -> Option<HermesMessage> {
        match self {
            Self::WebSocket(t) => t.poll_push().await,
            Self::Channel(t) => t.poll_push().await,
        }
    }

    async fn ensure_connected(&mut self) -> Result<()> {
        match self {
            Self::WebSocket(t) => {
                if t.conn.is_none() {
                    t.conn = Some(HermesConnection::connect(&t.config).await?);
                    info!(url = %t.config.url, "hermes: connected");
                }
                Ok(())
            }
            Self::Channel(_) => Ok(()),
        }
    }

    fn clear_connection(&mut self) {
        if let Self::WebSocket(t) = self {
            t.conn = None;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum HermesPriority {
    Low = 0,
    Normal = 1,
    High = 2,
}

impl HermesPriority {
    pub fn from_str(s: &str) -> Self {
        match s {
            "high" => Self::High,
            "low" => Self::Low,
            _ => Self::Normal,
        }
    }
}

#[derive(Debug, Clone)]
pub struct HermesRequest {
    pub id: String,
    pub text: String,
    pub priority: HermesPriority,
    pub created_at: Instant,
    pub model: Option<String>,
    pub provider: Option<String>,
}

impl PartialEq for HermesRequest {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for HermesRequest {}

impl PartialOrd for HermesRequest {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for HermesRequest {
    fn cmp(&self, other: &Self) -> Ordering {
        self.priority
            .cmp(&other.priority)
            .then_with(|| other.created_at.cmp(&self.created_at))
    }
}

const MAX_QUEUE_SIZE: usize = 100;

#[derive(Debug, Clone)]
pub struct HermesMessage {
    pub request_id: String,
    pub text: String,
    pub status: String,
}

#[derive(Debug, Clone)]
pub struct TaskSummary {
    pub request_id: String,
    pub text: String,
    pub priority: String,
    pub created_at_secs: u64,
}

#[derive(Debug, Clone)]
pub struct CompletedTask {
    pub request_id: String,
    pub status: String,
    pub text: String,
    pub completed_at_secs: u64,
}

#[derive(Debug, Clone)]
pub struct ListResult {
    pub pending: Vec<TaskSummary>,
    pub history: Vec<CompletedTask>,
}

const MAX_HISTORY_SIZE: usize = 1000;

enum QueueCommand {
    Enqueue {
        req: HermesRequest,
        respond: oneshot::Sender<Result<String>>,
    },
    Cancel {
        request_id: String,
        respond: oneshot::Sender<Result<bool>>,
    },
    List {
        request_id: Option<String>,
        respond: oneshot::Sender<ListResult>,
    },
}

#[derive(Clone)]
pub struct HermesQueueSender {
    cmd_tx: mpsc::Sender<QueueCommand>,
}

impl HermesQueueSender {
    pub async fn add_request(
        &self,
        text: String,
        priority: HermesPriority,
        model: Option<String>,
        provider: Option<String>,
    ) -> Result<String> {
        let request_id = uuid::Uuid::new_v4().to_string();
        let req = HermesRequest {
            id: request_id.clone(),
            text,
            priority,
            created_at: Instant::now(),
            model,
            provider,
        };
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(QueueCommand::Enqueue { req, respond: tx })
            .await
            .map_err(|_| DemoError::Tool("hermes queue closed".to_string()))?;
        rx.await
            .map_err(|_| DemoError::Tool("hermes queue response lost".to_string()))?
    }

    pub async fn cancel_request(&self, request_id: &str) -> Result<bool> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(QueueCommand::Cancel {
                request_id: request_id.to_string(),
                respond: tx,
            })
            .await
            .map_err(|_| DemoError::Tool("hermes queue closed".to_string()))?;
        rx.await
            .map_err(|_| DemoError::Tool("hermes queue response lost".to_string()))?
    }

    pub async fn list_tasks(&self, request_id: Option<String>) -> Result<ListResult> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(QueueCommand::List {
                request_id,
                respond: tx,
            })
            .await
            .map_err(|_| DemoError::Tool("hermes queue closed".to_string()))?;
        Ok(rx
            .await
            .map_err(|_| DemoError::Tool("hermes queue response lost".to_string()))?)
    }
}

pub struct HermesQueue {
    pub sender: HermesQueueSender,
}

impl HermesQueue {
    pub fn new(
        config: AipcTalkConfig,
    ) -> (
        Self,
        mpsc::Receiver<HermesMessage>,
        JoinHandle<()>,
        TalkPushBridge,
    ) {
        Self::start(config, None, None, None)
    }

    pub fn new_channel(
        config: AipcTalkConfig,
        work_tx: mpsc::Sender<HermesWorkItem>,
    ) -> (
        Self,
        mpsc::Receiver<HermesMessage>,
        JoinHandle<()>,
        TalkPushBridge,
    ) {
        Self::start(config, Some(work_tx), None, None)
    }

    pub fn new_channel_shared(
        config: AipcTalkConfig,
        work_tx: mpsc::Sender<HermesWorkItem>,
        msg_tx: mpsc::Sender<HermesMessage>,
        msg_rx: mpsc::Receiver<HermesMessage>,
    ) -> (Self, mpsc::Receiver<HermesMessage>, JoinHandle<()>) {
        let (queue, rx, handle, _) = Self::start(config, Some(work_tx), Some(msg_tx), Some(msg_rx));
        (queue, rx, handle)
    }

    pub fn new_shared(
        config: AipcTalkConfig,
        msg_tx: mpsc::Sender<HermesMessage>,
        msg_rx: mpsc::Receiver<HermesMessage>,
        work_tx: Option<mpsc::Sender<HermesWorkItem>>,
    ) -> (Self, mpsc::Receiver<HermesMessage>, JoinHandle<()>) {
        let (queue, rx, handle, _) = Self::start(config, work_tx, Some(msg_tx), Some(msg_rx));
        (queue, rx, handle)
    }

    fn start(
        config: AipcTalkConfig,
        work_tx: Option<mpsc::Sender<HermesWorkItem>>,
        external_msg_tx: Option<mpsc::Sender<HermesMessage>>,
        external_msg_rx: Option<mpsc::Receiver<HermesMessage>>,
    ) -> (
        Self,
        mpsc::Receiver<HermesMessage>,
        JoinHandle<()>,
        TalkPushBridge,
    ) {
        let (cmd_tx, cmd_rx) = mpsc::channel(128);
        let (msg_tx, msg_rx) = match (external_msg_tx, external_msg_rx) {
            (Some(tx), Some(rx)) => (tx, rx),
            _ => {
                let (tx, rx) = mpsc::channel(128);
                (tx, rx)
            }
        };
        let push_bridge = TalkPushBridge::new(msg_tx.clone());
        let handle = tokio::spawn(hermes_worker(config, work_tx, cmd_rx, msg_tx));
        (
            Self {
                sender: HermesQueueSender { cmd_tx },
            },
            msg_rx,
            handle,
            push_bridge,
        )
    }
}

async fn hermes_worker(
    config: AipcTalkConfig,
    work_tx: Option<mpsc::Sender<HermesWorkItem>>,
    mut cmd_rx: mpsc::Receiver<QueueCommand>,
    msg_tx: mpsc::Sender<HermesMessage>,
) {
    let mut heap: BinaryHeap<HermesRequest> = BinaryHeap::new();
    let mut history: VecDeque<CompletedTask> = VecDeque::new();

    let mut transport = match TransportBackend::from_config(&config, work_tx, msg_tx.clone()) {
        Ok(t) => t,
        Err(e) => {
            warn!(error = %e, "hermes_queue: transport init failed");
            return;
        }
    };

    if config.transport == AipcTalkTransport::Ws {
        if let Err(e) = transport.ensure_connected().await {
            warn!(%e, url = %config.url, "hermes: initial connect failed, will retry on first request");
        }
    }

    loop {
        let cmd = if heap.is_empty() {
            tokio::select! {
                cmd = cmd_rx.recv() => {
                    match cmd {
                        Some(cmd) => cmd,
                        None => break,
                    }
                }
                push = transport.poll_push() => {
                    if let Some(msg) = push {
                        let _ = msg_tx.try_send(msg);
                    }
                    continue;
                }
                _ = tokio::time::sleep(Duration::from_secs(5)), if config.transport == AipcTalkTransport::Ws => {
                    let _ = transport.ensure_connected().await;
                    continue;
                }
            }
        } else {
            tokio::select! {
                Some(c) = cmd_rx.recv() => c,
                else => break,
            }
        };

        match cmd {
            QueueCommand::Enqueue { req, respond } => {
                if heap.len() >= MAX_QUEUE_SIZE {
                    let _ = respond.send(Err(DemoError::Tool(format!(
                        "hermes queue full (max {MAX_QUEUE_SIZE})"
                    ))));
                    continue;
                }
                let request_id = req.id.clone();
                info!(%request_id, text = %req.text, priority = ?req.priority, "hermes_queue: enqueue");
                heap.push(req);
                let _ = respond.send(Ok(request_id));
            }
            QueueCommand::Cancel {
                request_id,
                respond,
            } => {
                let len_before = heap.len();
                heap = heap.into_iter().filter(|r| r.id != request_id).collect();
                let found = len_before != heap.len();
                info!(%request_id, found, "hermes_queue: cancel");
                let _ = respond.send(Ok(found));
            }
            QueueCommand::List {
                request_id: filter_id,
                respond,
            } => {
                let pending: Vec<TaskSummary> = heap
                    .iter()
                    .filter(|r| filter_id.as_ref().map_or(true, |id| r.id == *id))
                    .map(|r| TaskSummary {
                        request_id: r.id.clone(),
                        text: r.text.clone(),
                        priority: format!("{:?}", r.priority).to_lowercase(),
                        created_at_secs: r.created_at.elapsed().as_secs(),
                    })
                    .collect();
                let completed: Vec<CompletedTask> = history
                    .iter()
                    .filter(|c| filter_id.as_ref().map_or(true, |id| c.request_id == *id))
                    .cloned()
                    .collect();
                info!(
                    filter = ?filter_id,
                    pending = pending.len(),
                    history = completed.len(),
                    "hermes_queue: list"
                );
                let _ = respond.send(ListResult {
                    pending,
                    history: completed,
                });
            }
        }

        if heap.is_empty() || msg_tx.is_closed() {
            continue;
        }

        if let Err(e) = transport.ensure_connected().await {
            warn!(%e, "hermes: reconnect failed, requeuing");
            continue;
        }

        let req = heap.pop().unwrap();
        info!(
            id = %req.id,
            text = %req.text,
            priority = ?req.priority,
            "hermes_queue: processing"
        );

        let mut result = transport.submit(&req, config.timeout_secs).await;
        if result.is_err() && config.transport == AipcTalkTransport::Ws {
            transport.clear_connection();
            if transport.ensure_connected().await.is_ok() {
                result = transport.submit(&req, config.timeout_secs).await;
            }
        }

        match result {
            Ok(text) => {
                info!(id = %req.id, len = text.len(), "hermes_queue: got reply");
                let summary = text.chars().take(200).collect::<String>();
                let summary = if text.chars().count() > 200 {
                    format!("{summary}...")
                } else {
                    summary
                };
                if history.len() >= MAX_HISTORY_SIZE {
                    history.pop_front();
                }
                history.push_back(CompletedTask {
                    request_id: req.id.clone(),
                    status: "final".to_string(),
                    text: summary,
                    completed_at_secs: req.created_at.elapsed().as_secs(),
                });
                let _ = msg_tx
                    .send(HermesMessage {
                        request_id: req.id,
                        text,
                        status: "final".to_string(),
                    })
                    .await;
            }
            Err(e) => {
                warn!(id = %req.id, error = %e, "hermes_queue: request failed, giving up");
                if history.len() >= MAX_HISTORY_SIZE {
                    history.pop_front();
                }
                history.push_back(CompletedTask {
                    request_id: req.id.clone(),
                    status: "error".to_string(),
                    text: format!("{e}"),
                    completed_at_secs: req.created_at.elapsed().as_secs(),
                });
                let _ = msg_tx
                    .send(HermesMessage {
                        request_id: req.id,
                        text: format!("hermes request failed: {e}"),
                        status: "error".to_string(),
                    })
                    .await;
            }
        }
    }

    info!("hermes_queue: worker exiting");
}
