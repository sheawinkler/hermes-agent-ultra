use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http;
use tracing::{error, info, warn};

use crate::config::{DashscopeConfig, TtsConfig};
use crate::dashscope::{self, continue_task, event_name, finish_task, run_task_tts};
use crate::error::{DemoError, Result};

use super::TtsEngine;

pub struct TtsAudio {
    pub pcm: Vec<u8>,
}

enum TtsCommand {
    AppendText {
        text: String,
        done: oneshot::Sender<Result<()>>,
    },
    FinishTurn(oneshot::Sender<Result<()>>),
    WarmupStart(oneshot::Sender<Result<()>>),
    InterruptTurn(oneshot::Sender<Result<()>>),
}

#[derive(Clone)]
pub struct BailianTts {
    cmd_tx: mpsc::Sender<TtsCommand>,
}

impl BailianTts {
    pub async fn connect(
        dashscope: &DashscopeConfig,
        tts: &TtsConfig,
    ) -> Result<(Self, mpsc::Receiver<TtsAudio>)> {
        let (audio_tx, audio_rx) = mpsc::channel(128);
        let (cmd_tx, cmd_rx) = mpsc::channel(32);

        let url = dashscope.ws_url.clone();
        let api_key = dashscope.api_key.clone();
        let model = tts.model.clone();
        let voice = tts.voice.clone();
        let sample_rate = tts.sample_rate;
        let format = tts.format.clone();
        let language_hints = tts.language_hints.clone();

        tokio::spawn(async move {
            if let Err(e) = run_tts_driver(
                &url,
                &api_key,
                &model,
                &voice,
                sample_rate,
                &format,
                language_hints.as_deref(),
                cmd_rx,
                audio_tx,
            )
            .await
            {
                error!(error = %e, "tts driver exited");
            }
        });

        Ok((Self { cmd_tx }, audio_rx))
    }
}

#[async_trait]
impl TtsEngine for BailianTts {
    async fn warmup(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(TtsCommand::WarmupStart(tx))
            .await
            .map_err(|e| DemoError::Tts(e.to_string()))?;
        rx.await.map_err(|e| DemoError::Tts(e.to_string()))?
    }

    async fn append_text(&self, text: &str) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(TtsCommand::AppendText {
                text: text.to_string(),
                done: tx,
            })
            .await
            .map_err(|e| DemoError::Tts(e.to_string()))?;
        rx.await.map_err(|e| DemoError::Tts(e.to_string()))?
    }

    async fn finish_turn(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(TtsCommand::FinishTurn(tx))
            .await
            .map_err(|e| DemoError::Tts(e.to_string()))?;
        match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => Err(DemoError::Tts(e.to_string())),
            Err(_) => Err(DemoError::Tts("finish-task timeout".into())),
        }
    }

    async fn interrupt_turn(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(TtsCommand::InterruptTurn(tx))
            .await
            .map_err(|e| DemoError::Tts(e.to_string()))?;
        match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => Err(DemoError::Tts(e.to_string())),
            Err(_) => Err(DemoError::Tts("interrupt-turn timeout".into())),
        }
    }
}

async fn run_tts_driver(
    url: &str,
    api_key: &str,
    model: &str,
    voice: &str,
    sample_rate: u32,
    format: &str,
    language_hints: Option<&[String]>,
    mut cmd_rx: mpsc::Receiver<TtsCommand>,
    audio_tx: mpsc::Sender<TtsAudio>,
) -> Result<()> {
    loop {
        match run_tts_connection(
            url,
            api_key,
            model,
            voice,
            sample_rate,
            format,
            language_hints,
            &mut cmd_rx,
            &audio_tx,
        )
        .await
        {
            Ok(()) => {
                if cmd_rx.is_closed() {
                    return Ok(());
                }
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            }
            Err(e) => {
                warn!(error = %e, "tts connection lost, reconnecting");
                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            }
        }
    }
}

async fn run_tts_connection(
    url: &str,
    api_key: &str,
    model: &str,
    voice: &str,
    sample_rate: u32,
    format: &str,
    language_hints: Option<&[String]>,
    cmd_rx: &mut mpsc::Receiver<TtsCommand>,
    audio_tx: &mpsc::Sender<TtsAudio>,
) -> Result<()> {
    let mut req = url
        .into_client_request()
        .map_err(|e| DemoError::WebSocket(e.to_string()))?;
    let auth = format!("bearer {api_key}");
    req.headers_mut().insert(
        "Authorization",
        auth.parse()
            .map_err(|e: http::header::InvalidHeaderValue| DemoError::WebSocket(e.to_string()))?,
    );

    let (ws, _) = connect_async(req)
        .await
        .map_err(|e| DemoError::WebSocket(e.to_string()))?;
    let (mut write, mut read) = ws.split();

    info!("tts connected");

    let mut task_id = String::new();
    let mut ready = false;
    let mut pending_finish: Option<oneshot::Sender<Result<()>>> = None;
    let mut pending_interrupt: Option<oneshot::Sender<Result<()>>> = None;

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { return Ok(()) };
                match cmd {
                    TtsCommand::WarmupStart(done) => {
                        pending_finish = None;
                        pending_interrupt = None;
                        let r = start_new_task(&mut write, &mut read, model, voice, sample_rate, format, language_hints, &mut task_id, &mut ready, &audio_tx).await;
                        let _ = done.send(r);
                    }
                    TtsCommand::InterruptTurn(done) => {
                        ready = false;
                        pending_finish = None;
                        pending_interrupt = None;
                        if !task_id.is_empty() {
                            let fin = finish_task(&task_id);
                            let _ = write.send(Message::Text(fin.to_string().into())).await;
                            task_id.clear();
                        }
                        let _ = done.send(Ok(()));
                    }
                    TtsCommand::AppendText { text, done } => {
                        let r = async {
                            if !ready {
                                start_new_task(&mut write, &mut read, model, voice, sample_rate, format, language_hints, &mut task_id, &mut ready, &audio_tx).await?;
                            }
                            let msg = continue_task(&task_id, &text);
                            write.send(Message::Text(msg.to_string().into())).await
                                .map_err(|e| DemoError::Tts(e.to_string()))?;
                            Ok(())
                        }.await;
                        let _ = done.send(r);
                    }
                    TtsCommand::FinishTurn(done) => {
                        if ready {
                            let fin = finish_task(&task_id);
                            let _ = write.send(Message::Text(fin.to_string().into())).await;
                            pending_finish = Some(done);
                        } else {
                            let _ = done.send(Ok(()));
                        }
                    }
                }
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Binary(b))) => {
                        let _ = audio_tx.send(TtsAudio { pcm: b.to_vec() }).await;
                    }
                    Some(Ok(Message::Text(t))) => {
                        if let Ok(v) = serde_json::from_str::<Value>(&t) {
                            match event_name(&v).as_deref() {
                                Some("task-started") => ready = true,
                                Some("task-finished") => {
                                    ready = false;
                                    if let Some(done) = pending_finish.take() {
                                        let _ = done.send(Ok(()));
                                    }
                                    if let Some(done) = pending_interrupt.take() {
                                        let _ = done.send(Ok(()));
                                    }
                                }
                                Some("task-failed") => {
                                    ready = false;
                                    let msg = dashscope::header_field(&v, "error_message")
                                        .unwrap_or_else(|| "task failed".into());
                                    let err = || DemoError::Tts(msg.clone());
                                    if let Some(done) = pending_finish.take() {
                                        let _ = done.send(Err(err()));
                                    }
                                    if let Some(done) = pending_interrupt.take() {
                                        let _ = done.send(Err(err()));
                                    }
                                    error!(error = %msg, "tts failed");
                                }
                                _ => {}
                            }
                        }
                    }
                    Some(Ok(Message::Close(_))) => {
                        info!("tts: server closed idle connection");
                        return Ok(());
                    }
                    Some(Err(e)) => {
                        return Err(DemoError::Tts(e.to_string()));
                    }
                    None => {
                        info!("tts: connection EOF");
                        return Ok(());
                    }
                    _ => {}
                }
            }
        }
    }
}

async fn start_new_task(
    write: &mut futures_util::stream::SplitSink<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
        Message,
    >,
    read: &mut futures_util::stream::SplitStream<
        tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    >,
    model: &str,
    voice: &str,
    sample_rate: u32,
    format: &str,
    language_hints: Option<&[String]>,
    task_id: &mut String,
    ready: &mut bool,
    audio_tx: &mpsc::Sender<TtsAudio>,
) -> Result<()> {
    *task_id = dashscope::task_id();
    *ready = false;
    let run = run_task_tts(task_id, model, voice, sample_rate, format, language_hints);
    write
        .send(Message::Text(run.to_string().into()))
        .await
        .map_err(|e| DemoError::Tts(e.to_string()))?;

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(15);
    while tokio::time::Instant::now() < deadline {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(t))) => {
                        if let Ok(v) = serde_json::from_str(&t) {
                            if event_name(&v).as_deref() == Some("task-started") {
                                *ready = true;
                                return Ok(());
                            }
                            if event_name(&v).as_deref() == Some("task-failed") {
                                return Err(DemoError::Tts(
                                    dashscope::header_field(&v, "error_message")
                                        .unwrap_or_else(|| "task failed".into()),
                                ));
                            }
                        }
                    }
                    Some(Ok(Message::Binary(b))) => {
                        let _ = audio_tx.send(TtsAudio { pcm: b.to_vec() }).await;
                    }
                    Some(Err(e)) => return Err(DemoError::Tts(e.to_string())),
                    _ => {}
                }
            }
            _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {}
        }
    }
    Err(DemoError::Tts("task-started timeout".into()))
}
