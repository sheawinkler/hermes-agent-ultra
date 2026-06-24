use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::sync::{Mutex, mpsc, oneshot};
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http;
use tracing::{error, info, warn};

use crate::asr::{AsrEngine, AsrEvent};
use crate::config::{AsrConfig, DashscopeConfig};
use crate::dashscope::{self, event_name, finish_task, run_task_asr};
use crate::error::{DemoError, Result};

enum AsrCommand {
    Audio(Vec<u8>),
    Pause(oneshot::Sender<Result<()>>),
    Resume(oneshot::Sender<Result<()>>),
    Reconnect(oneshot::Sender<Result<()>>),
    Gate(bool),
}

struct LoopState {
    paused: AtomicBool,
    pending_resume: Mutex<Vec<oneshot::Sender<Result<()>>>>,
}

#[derive(Clone)]
pub struct BailianAsr {
    cmd_tx: mpsc::Sender<AsrCommand>,
}

impl BailianAsr {
    pub async fn connect(
        dashscope: &DashscopeConfig,
        asr: &AsrConfig,
        start_paused: bool,
    ) -> Result<(Self, mpsc::Receiver<AsrEvent>)> {
        let (event_tx, event_rx) = mpsc::channel(64);
        let (cmd_tx, cmd_rx) = mpsc::channel::<AsrCommand>(128);

        let url = dashscope.ws_url.clone();
        let api_key = dashscope.api_key.clone();
        let model = asr.model.clone();
        let sample_rate = asr.sample_rate;
        let format = asr.format.clone();
        let language_hints = asr.language_hints.clone();

        tokio::spawn(async move {
            if let Err(e) = run_asr_loop(
                &url,
                &api_key,
                &model,
                sample_rate,
                &format,
                language_hints.as_deref(),
                cmd_rx,
                &event_tx,
                start_paused,
            )
            .await
            {
                error!(error = %e, "asr loop ended");
                let _ = event_tx
                    .send(AsrEvent::TaskFailed {
                        message: e.to_string(),
                    })
                    .await;
            }
        });

        Ok((Self { cmd_tx }, event_rx))
    }
}

#[async_trait]
impl AsrEngine for BailianAsr {
    async fn send_audio(&self, pcm: Vec<u8>) -> Result<()> {
        self.cmd_tx
            .send(AsrCommand::Audio(pcm))
            .await
            .map_err(|e| DemoError::Asr(format!("send audio: {e}")))
    }

    async fn pause(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AsrCommand::Pause(tx))
            .await
            .map_err(|e| DemoError::Asr(format!("pause: {e}")))?;
        rx.await
            .map_err(|e| DemoError::Asr(format!("pause response: {e}")))?
    }

    async fn resume(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AsrCommand::Resume(tx))
            .await
            .map_err(|e| DemoError::Asr(format!("resume: {e}")))?;
        rx.await
            .map_err(|e| DemoError::Asr(format!("resume response: {e}")))?
    }

    async fn set_gate(&self, on: bool) -> Result<()> {
        self.cmd_tx
            .send(AsrCommand::Gate(on))
            .await
            .map_err(|e| DemoError::Asr(format!("gate: {e}")))
    }

    async fn reconnect(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(AsrCommand::Reconnect(tx))
            .await
            .map_err(|e| DemoError::Asr(format!("reconnect: {e}")))?;
        rx.await
            .map_err(|e| DemoError::Asr(format!("reconnect response: {e}")))?
    }

    async fn finish_utterance(&self) -> Result<()> {
        Ok(())
    }
}

async fn run_asr_loop(
    url: &str,
    api_key: &str,
    model: &str,
    sample_rate: u32,
    format: &str,
    language_hints: Option<&[String]>,
    mut cmd_rx: mpsc::Receiver<AsrCommand>,
    event_tx: &mpsc::Sender<AsrEvent>,
    start_paused: bool,
) -> Result<()> {
    let loop_state = LoopState {
        paused: AtomicBool::new(start_paused),
        pending_resume: Mutex::new(Vec::new()),
    };

    loop {
        if loop_state.paused.load(Ordering::SeqCst) {
            match cmd_rx.recv().await {
                Some(AsrCommand::Resume(done)) => {
                    loop_state.paused.store(false, Ordering::SeqCst);
                    loop_state.pending_resume.lock().await.push(done);
                    continue;
                }
                Some(AsrCommand::Pause(done)) => {
                    let _ = done.send(Ok(()));
                    continue;
                }
                Some(AsrCommand::Reconnect(done)) => {
                    let _ = done.send(Ok(()));
                    continue;
                }
                Some(AsrCommand::Gate(_)) | Some(AsrCommand::Audio(_)) => continue,
                None => break Ok(()),
            }
        }

        match run_asr_session(
            url,
            api_key,
            model,
            sample_rate,
            format,
            language_hints,
            &mut cmd_rx,
            event_tx,
            &loop_state,
        )
        .await
        {
            Ok(()) => {
                if loop_state.paused.load(Ordering::SeqCst) {
                    info!("asr: paused, waiting for resume");
                    continue;
                }
                warn!("asr session closed cleanly; reconnecting");
            }
            Err(e) => {
                warn!(error = %e, "asr session error");
            }
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

async fn run_asr_session(
    url: &str,
    api_key: &str,
    model: &str,
    sample_rate: u32,
    format: &str,
    language_hints: Option<&[String]>,
    cmd_rx: &mut mpsc::Receiver<AsrCommand>,
    event_tx: &mpsc::Sender<AsrEvent>,
    loop_state: &LoopState,
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

    info!("asr connected");

    let mut task_id = dashscope::task_id();
    let mut started = false;
    let mut paused = false;

    async fn start_task(
        write: &mut futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
        task_id: &mut String,
        model: &str,
        sample_rate: u32,
        format: &str,
        language_hints: Option<&[String]>,
    ) -> Result<()> {
        *task_id = dashscope::task_id();
        let run = run_task_asr(task_id, model, sample_rate, format, language_hints);
        write
            .send(Message::Text(run.to_string().into()))
            .await
            .map_err(|e| DemoError::Asr(e.to_string()))?;
        Ok(())
    }

    start_task(
        &mut write,
        &mut task_id,
        model,
        sample_rate,
        format,
        language_hints,
    )
    .await?;

    let silence_frame_size = (sample_rate as usize * 20 / 1000) * 2;
    let keepalive_interval = Duration::from_secs(5);
    let mut keepalive_tick = tokio::time::interval(keepalive_interval);
    keepalive_tick.tick().await;
    let mut last_audio_at = Instant::now();

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(AsrCommand::Audio(bytes)) => {
                        if started && !paused {
                            write.send(Message::Binary(bytes.into())).await
                                .map_err(|e| DemoError::Asr(e.to_string()))?;
                            last_audio_at = Instant::now();
                        }
                    }
                    Some(AsrCommand::Pause(done)) => {
                        if started {
                            let fin = finish_task(&task_id);
                            let _ = write.send(Message::Text(fin.to_string().into())).await;
                        }
                        loop_state.paused.store(true, Ordering::SeqCst);
                        resolve_pending_err(&loop_state.pending_resume, "session ended before task started").await;
                        let _ = done.send(Ok(()));
                        info!("asr paused — disconnecting");
                        return Ok(());
                    }
                    Some(AsrCommand::Resume(done)) => {
                        let _ = done.send(Ok(()));
                    }
                    Some(AsrCommand::Gate(on)) => {
                        paused = !on;
                    }
                    Some(AsrCommand::Reconnect(done)) => {
                        info!("asr: forced reconnect — closing current session");
                        if started {
                            let fin = finish_task(&task_id);
                            let _ = write.send(Message::Text(fin.to_string().into())).await;
                        }
                        resolve_pending_err(&loop_state.pending_resume, "session ended before task started").await;
                        let _ = done.send(Ok(()));
                        return Ok(());
                    }
                    None => break,
                }
            }
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Text(t))) => {
                        let v: Value = serde_json::from_str(&t)
                            .map_err(|e| DemoError::Asr(e.to_string()))?;
                        if let Some(ev) = parse_asr_event(&v) {
                            if matches!(ev, AsrEvent::TaskStarted) {
                                started = true;
                                resolve_pending_ok(&loop_state.pending_resume).await;
                            }
                            let _ = event_tx.send(ev).await;
                        }
                    }
                    Some(Ok(Message::Close(_))) => break,
                    Some(Err(e)) => return Err(DemoError::Asr(e.to_string())),
                    None => break,
                    _ => {}
                }
            }
            _ = keepalive_tick.tick() => {
                if started && last_audio_at.elapsed() >= keepalive_interval {
                    let silence = vec![0u8; silence_frame_size];
                    if write.send(Message::Binary(silence.into())).await.is_err() {
                        break;
                    }
                    last_audio_at = Instant::now();
                }
            }
        }
    }

    resolve_pending_err(
        &loop_state.pending_resume,
        "session ended before task started",
    )
    .await;

    if started {
        let fin = finish_task(&task_id);
        let _ = write.send(Message::Text(fin.to_string().into())).await;
    }
    info!("asr session closed");
    Ok(())
}

async fn resolve_pending_ok(pending: &Mutex<Vec<oneshot::Sender<Result<()>>>>) {
    let mut waiters = pending.lock().await;
    for w in waiters.drain(..) {
        let _ = w.send(Ok(()));
    }
}

async fn resolve_pending_err(pending: &Mutex<Vec<oneshot::Sender<Result<()>>>>, msg: &str) {
    let mut waiters = pending.lock().await;
    for w in waiters.drain(..) {
        let _ = w.send(Err(DemoError::Asr(msg.to_string())));
    }
}

fn parse_asr_event(msg: &Value) -> Option<AsrEvent> {
    let event = event_name(msg)?;
    match event.as_str() {
        "task-started" => Some(AsrEvent::TaskStarted),
        "task-failed" => {
            let message =
                dashscope::header_field(msg, "error_message").unwrap_or_else(|| "unknown".into());
            Some(AsrEvent::TaskFailed { message })
        }
        "result-generated" => {
            let sentence = msg.get("payload")?.get("output")?.get("sentence")?;
            if sentence.get("heartbeat").and_then(|v| v.as_bool()) == Some(true) {
                return None;
            }
            let text = sentence.get("text")?.as_str()?.to_string();
            if text.is_empty() {
                return None;
            }
            let sentence_end = sentence
                .get("sentence_end")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if sentence_end {
                Some(AsrEvent::Final { text })
            } else {
                Some(AsrEvent::Partial { text })
            }
        }
        _ => None,
    }
}
