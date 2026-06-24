use std::ffi::CString;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info, warn};

use crate::config::RockchipTtsConfig;
use crate::error::{DemoError, Result};

use super::TtsEngine;
use super::bailian::TtsAudio;

unsafe extern "C" {
    fn rktts_create() -> *mut std::ffi::c_void;
    fn rktts_init(
        handle: *mut std::ffi::c_void,
        auth_json: *const std::ffi::c_char,
        model_path: *const std::ffi::c_char,
        dicts_path: *const std::ffi::c_char,
        speaker_id: std::ffi::c_int,
        alpha: std::ffi::c_float,
        sample_rate: std::ffi::c_int,
        cb: extern "C" fn(*const i16, i32, i32, *mut std::ffi::c_void),
        userdata: *mut std::ffi::c_void,
    ) -> std::ffi::c_int;
    fn rktts_inference(
        handle: *mut std::ffi::c_void,
        text: *const std::ffi::c_char,
    ) -> std::ffi::c_int;
    fn rktts_release(handle: *mut std::ffi::c_void) -> std::ffi::c_int;
    fn rktts_destroy(handle: *mut std::ffi::c_void);
}

/// Wrapper around the opaque C handle that is safe to send between threads.
/// The underlying Rockchip TTS engine uses internal mutex + thread synchronization,
/// so the handle is inherently thread-safe.
struct RkTtsHandle(*mut std::ffi::c_void);

unsafe impl Send for RkTtsHandle {}
unsafe impl Sync for RkTtsHandle {}

struct CallbackContext {
    audio_tx: mpsc::Sender<TtsAudio>,
    infer_gen: Arc<AtomicU64>,
}

extern "C" fn audio_callback(
    data: *const i16,
    len: i32,
    is_last: i32,
    userdata: *mut std::ffi::c_void,
) {
    let ctx = unsafe { &*(userdata as *const CallbackContext) };
    let samples = unsafe { std::slice::from_raw_parts(data, len as usize) };
    let pcm: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
    let _ = ctx.audio_tx.try_send(TtsAudio { pcm });
    if is_last != 0 {
        // Signal that current inference generation is done.
        // The run_inference function waits for the gen to become even.
        ctx.infer_gen.fetch_add(1, Ordering::SeqCst);
    }
}

enum RkCommand {
    AppendText {
        text: String,
        done: oneshot::Sender<Result<()>>,
    },
    FinishTurn(oneshot::Sender<Result<()>>),
    InterruptTurn(oneshot::Sender<Result<()>>),
}

#[derive(Clone)]
pub struct RockchipTts {
    cmd_tx: mpsc::Sender<RkCommand>,
}

impl RockchipTts {
    pub async fn connect(config: &RockchipTtsConfig) -> Result<(Self, mpsc::Receiver<TtsAudio>)> {
        let (audio_tx, audio_rx) = mpsc::channel(128);
        let (cmd_tx, cmd_rx) = mpsc::channel::<RkCommand>(32);

        let cfg = config.clone();

        tokio::spawn(async move {
            if let Err(e) = run_rktts_driver(cfg, cmd_rx, audio_tx).await {
                error!(error = %e, "rktts driver exited");
            }
        });

        Ok((Self { cmd_tx }, audio_rx))
    }
}

#[async_trait]
impl TtsEngine for RockchipTts {
    async fn warmup(&self) -> Result<()> {
        Ok(())
    }

    async fn append_text(&self, text: &str) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(RkCommand::AppendText {
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
            .send(RkCommand::FinishTurn(tx))
            .await
            .map_err(|e| DemoError::Tts(e.to_string()))?;
        match tokio::time::timeout(std::time::Duration::from_secs(60), rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => Err(DemoError::Tts(e.to_string())),
            Err(_) => Err(DemoError::Tts("rktts finish-turn timeout".into())),
        }
    }

    async fn interrupt_turn(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(RkCommand::InterruptTurn(tx))
            .await
            .map_err(|e| DemoError::Tts(e.to_string()))?;
        match tokio::time::timeout(std::time::Duration::from_secs(10), rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => Err(DemoError::Tts(e.to_string())),
            Err(_) => Err(DemoError::Tts("rktts interrupt-turn timeout".into())),
        }
    }
}

async fn run_rktts_driver(
    config: RockchipTtsConfig,
    mut cmd_rx: mpsc::Receiver<RkCommand>,
    audio_tx: mpsc::Sender<TtsAudio>,
) -> Result<()> {
    let raw = unsafe { rktts_create() };
    if raw.is_null() {
        return Err(DemoError::Tts("rktts_create returned null".into()));
    }
    let handle = RkTtsHandle(raw);

    let infer_gen = Arc::new(AtomicU64::new(0));
    let ctx = Box::new(CallbackContext {
        audio_tx: audio_tx.clone(),
        infer_gen: infer_gen.clone(),
    });

    let auth = CString::new(config.auth_config.as_str())
        .map_err(|e| DemoError::Tts(format!("invalid auth_config: {e}")))?;
    let model = CString::new(config.model_path.as_str())
        .map_err(|e| DemoError::Tts(format!("invalid model_path: {e}")))?;
    let dicts = CString::new(config.dicts_path.as_str())
        .map_err(|e| DemoError::Tts(format!("invalid dicts_path: {e}")))?;

    let ret = unsafe {
        rktts_init(
            handle.0,
            auth.as_ptr(),
            model.as_ptr(),
            dicts.as_ptr(),
            config.speaker_id,
            config.alpha,
            24000,
            audio_callback,
            &*ctx as *const CallbackContext as *mut std::ffi::c_void,
        )
    };
    if ret != 0 {
        unsafe { rktts_destroy(handle.0) };
        return Err(DemoError::Tts(format!("rktts_init failed: {ret}")));
    }

    info!(
        "rktts initialized (speaker={}, alpha={})",
        config.speaker_id, config.alpha
    );

    let mut text_buf = String::new();

    loop {
        tokio::select! {
            cmd = cmd_rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    RkCommand::AppendText { text, done } => {
                        text_buf.push_str(&text);
                        let _ = done.send(Ok(()));
                    }
                    RkCommand::FinishTurn(done) => {
                        if text_buf.is_empty() {
                            let _ = done.send(Ok(()));
                            continue;
                        }

                        let text = std::mem::take(&mut text_buf);
                        let generation = infer_gen.load(Ordering::SeqCst);
                        let infer_done = run_inference(&handle, &text, &infer_gen, generation).await;
                        let _ = done.send(infer_done);
                    }
                    RkCommand::InterruptTurn(done) => {
                        text_buf.clear();
                        // Advance gen to unblock any waiting run_inference.
                        infer_gen.fetch_add(1, Ordering::SeqCst);
                        let _ = done.send(Ok(()));
                    }
                }
            }
        }
    }

    unsafe { rktts_release(handle.0) };
    unsafe { rktts_destroy(handle.0) };
    drop(ctx);
    info!("rktts driver shut down");
    Ok(())
}

async fn run_inference(
    handle: &RkTtsHandle,
    text: &str,
    infer_gen: &Arc<AtomicU64>,
    my_gen: u64,
) -> Result<()> {
    let c_text = match CString::new(text) {
        Ok(s) => s,
        Err(e) => return Err(DemoError::Tts(format!("invalid text: {e}"))),
    };

    let ret = unsafe { rktts_inference(handle.0, c_text.as_ptr()) };
    if ret != 0 {
        return Err(DemoError::Tts(format!("rktts_inference failed: {ret}")));
    }

    // Wait for callback to signal completion (infer_gen changes from my_gen)
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(60);
    while infer_gen.load(Ordering::SeqCst) == my_gen {
        if tokio::time::Instant::now() >= deadline {
            return Err(DemoError::Tts("rktts inference timeout".into()));
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    Ok(())
}
