use std::ffi::CString;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, error, info, warn};

use crate::asr::{AsrEngine, AsrEvent};
use crate::config::RockchipAsrConfig;
use crate::error::{DemoError, Result};

unsafe extern "C" fn _dummy_output_cb(
    _name: *const std::ffi::c_char,
    _packet: ffi::RockXPacket,
    _userdata: *mut std::ffi::c_void,
) {
}

fn check_ret(ret: i32) -> Result<()> {
    if ret == ffi::ROCKX_RET_SUCCESS {
        Ok(())
    } else {
        Err(DemoError::Asr(format!("rkasr call failed: {ret}")))
    }
}

mod ffi {
    use std::ffi::c_void;

    pub type RockXHandle = *mut c_void;
    pub type RockXInput = *mut c_void;
    pub type RockXPacket = *mut c_void;

    pub const ROCKX_RET_SUCCESS: i32 = 0;
    pub const ROCKX_DATA_TYPE_INT16: i32 = 3;
    pub const ROCKX_STREAM_STATE_FIRST: i32 = 0;
    pub const ROCKX_STREAM_STATE_CONTINUE: i32 = 1;
    pub const ROCKX_STREAM_STATE_LAST: i32 = 2;
    pub const ASR_STATE_NONE: i32 = 0;
    pub const ASR_STATE_FIRST: i32 = 1;
    pub const ASR_STATE_RUNNING: i32 = 2;
    pub const ASR_STATE_FINISH: i32 = 3;
    pub const ASR_STATE_COMPLETE: i32 = 4;
    pub const ROCKASR_MODE_STREAM: i32 = 0;
    pub const ROCKASR_LANG_CHINESE: i32 = 1;
    pub const ROCKASR_LANG_ENGLISH: i32 = 0;
    pub const ROCKX_CALLBACK_ON_SCHED_THREAD: i32 = 1;

    #[repr(C)]
    pub struct RockAsrLangMode {
        pub source: i32,
        pub target: i32,
    }

    #[repr(C)]
    pub struct RockAsrResult {
        pub r#type: u32,
        pub state: i32,
        pub ts_start: u64,
        pub ts_end: u64,
        pub speaker_id: i32,
        pub result: *const std::ffi::c_char,
        pub new_result: *const std::ffi::c_char,
        pub trans_result: *const std::ffi::c_char,
        pub audio_result: *mut c_void,
    }

    #[repr(C)]
    pub struct RockXAsrInitParam {
        pub model_name: *const std::ffi::c_char,
        pub work_mode: i32,
        pub lang_mode: RockAsrLangMode,
        pub sample_rate: i32,
        pub userdata: *mut c_void,
        pub result_callback: Option<unsafe extern "C" fn(*const RockAsrResult, i32, *mut c_void)>,
    }

    unsafe extern "C" {
        pub fn RockXCreate() -> RockXHandle;
        pub fn RockXDestroy(handle: RockXHandle) -> i32;
        pub fn RockXInit(handle: RockXHandle) -> i32;
        pub fn RockXSetParamString(
            handle: RockXHandle,
            node_name: *const std::ffi::c_char,
            param_name: *const std::ffi::c_char,
            value: *const std::ffi::c_char,
            need_release: i32,
        ) -> i32;
        pub fn RockXSetParamInt(
            handle: RockXHandle,
            node_name: *const std::ffi::c_char,
            param_name: *const std::ffi::c_char,
            value: i32,
        ) -> i32;
        pub fn RockXSetParamPointer(
            handle: RockXHandle,
            node_name: *const std::ffi::c_char,
            param_name: *const std::ffi::c_char,
            value: *mut c_void,
            need_release: i32,
            callback: Option<unsafe extern "C" fn()>,
        ) -> i32;
        pub fn RockXSetOutputInfoDefault(
            handle: RockXHandle,
            name: *const std::ffi::c_char,
            callback: Option<
                unsafe extern "C" fn(*const std::ffi::c_char, RockXPacket, *mut c_void),
            >,
            mode: i32,
            userdata: *mut c_void,
        ) -> i32;
        pub fn RockXInputCreate(handle: RockXHandle) -> RockXInput;
        pub fn RockXInputDestroy(handle: RockXHandle, input: RockXInput) -> i32;
        pub fn RockXInputGetPacketAt(input: RockXInput, index: i32) -> RockXPacket;
        pub fn RockXPacketSetAudio2(
            packet: RockXPacket,
            state: i32,
            sample_rate: i32,
            channels: i32,
            sample_num: i32,
            dtype: i32,
            data: *mut c_void,
        ) -> i32;
        pub fn RockXProcessAsync(handle: RockXHandle, input: RockXInput, wait: *mut c_void) -> i32;

    }
}

struct CallbackContext {
    event_tx: mpsc::Sender<AsrEvent>,
    frame_done: Arc<AtomicBool>,
}

unsafe extern "C" fn asr_result_callback(
    result: *const ffi::RockAsrResult,
    result_count: i32,
    userdata: *mut std::ffi::c_void,
) {
    let ctx = &*(userdata as *const CallbackContext);
    let r = &*result;
    let state = r.state;

    let state_name = match state {
        ffi::ASR_STATE_NONE => "NONE",
        ffi::ASR_STATE_FIRST => "FIRST",
        ffi::ASR_STATE_RUNNING => "RUNNING",
        ffi::ASR_STATE_FINISH => "FINISH",
        ffi::ASR_STATE_COMPLETE => "COMPLETE",
        _ => "UNKNOWN",
    };

    let new_txt = if !r.new_result.is_null() {
        std::ffi::CStr::from_ptr(r.new_result)
            .to_str()
            .ok()
            .map(|s| s.to_string())
    } else {
        None
    };
    let full_txt = if !r.result.is_null() {
        std::ffi::CStr::from_ptr(r.result)
            .to_str()
            .ok()
            .map(|s| s.to_string())
    } else {
        None
    };

    debug!(
        state = state_name,
        state_code = state,
        result_count = result_count,
        new = %new_txt.as_deref().unwrap_or("null"),
        full = %full_txt.as_deref().unwrap_or("null"),
        "rkasr callback"
    );

    let text = new_txt.or(full_txt);
    let Some(text) = text else { return };
    if text.is_empty() {
        return;
    }

    let event = match state {
        ffi::ASR_STATE_FINISH => {
            info!(%text, "rkasr final result");
            AsrEvent::Final { text }
        }
        _ => {
            info!(%text, "rkasr partial result");
            AsrEvent::Partial { text }
        }
    };
    let _ = ctx.event_tx.try_send(event);
    if state == ffi::ASR_STATE_FINISH {
        ctx.frame_done.store(true, Ordering::SeqCst);
    }
}

enum RkAsrCommand {
    Audio(Vec<u8>),
    Pause(oneshot::Sender<Result<()>>),
    Resume(oneshot::Sender<Result<()>>),
    FinishUtterance(oneshot::Sender<Result<()>>),
}

#[derive(Clone)]
pub struct RockchipAsr {
    cmd_tx: mpsc::Sender<RkAsrCommand>,
}

struct RkAsrHandle(ffi::RockXHandle);

unsafe impl Send for RkAsrHandle {}
unsafe impl Sync for RkAsrHandle {}

impl RockchipAsr {
    pub async fn connect(
        config: &RockchipAsrConfig,
        start_paused: bool,
    ) -> Result<(Self, mpsc::Receiver<AsrEvent>)> {
        let (event_tx, event_rx) = mpsc::channel(64);
        let (cmd_tx, cmd_rx) = mpsc::channel::<RkAsrCommand>(128);
        let cfg = config.clone();
        tokio::spawn(async move {
            if let Err(e) = run_rkasr_driver(cfg, cmd_rx, event_tx, start_paused).await {
                error!(error = %e, "rkasr driver exited");
            }
        });
        Ok((Self { cmd_tx }, event_rx))
    }
}

#[async_trait]
impl AsrEngine for RockchipAsr {
    async fn send_audio(&self, pcm: Vec<u8>) -> Result<()> {
        debug!(bytes = pcm.len(), "rkasr send_audio");
        self.cmd_tx
            .send(RkAsrCommand::Audio(pcm))
            .await
            .map_err(|e| DemoError::Asr(format!("send audio: {e}")))
    }
    async fn pause(&self) -> Result<()> {
        info!("rkasr pause");
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(RkAsrCommand::Pause(tx))
            .await
            .map_err(|e| DemoError::Asr(format!("pause: {e}")))?;
        rx.await
            .map_err(|e| DemoError::Asr(format!("pause response: {e}")))?
    }
    async fn resume(&self) -> Result<()> {
        info!("rkasr resume");
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(RkAsrCommand::Resume(tx))
            .await
            .map_err(|e| DemoError::Asr(format!("resume: {e}")))?;
        rx.await
            .map_err(|e| DemoError::Asr(format!("resume response: {e}")))?
    }
    async fn set_gate(&self, on: bool) -> Result<()> {
        if on {
            // Gate on = unpause (same as resume), ensures audio flows after barge-in
            self.resume().await
        } else {
            Ok(())
        }
    }
    async fn reconnect(&self) -> Result<()> {
        Ok(())
    }
    async fn finish_utterance(&self) -> Result<()> {
        info!("rkasr finish_utterance");
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(RkAsrCommand::FinishUtterance(tx))
            .await
            .map_err(|e| DemoError::Asr(format!("finish_utterance: {e}")))?;
        rx.await
            .map_err(|e| DemoError::Asr(format!("finish_utterance response: {e}")))?
    }
}

fn write_auth_config(json: &str) -> std::io::Result<std::path::PathBuf> {
    let dir = std::env::temp_dir().join("rkauth");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join("asr_auth.json");
    std::fs::write(&path, json)?;
    Ok(path)
}

// ============================================================================
// Driver
// ============================================================================

async fn run_rkasr_driver(
    config: RockchipAsrConfig,
    mut cmd_rx: mpsc::Receiver<RkAsrCommand>,
    event_tx: mpsc::Sender<AsrEvent>,
    start_paused: bool,
) -> Result<()> {
    let paused = Arc::new(AtomicBool::new(start_paused));
    let frame_done = Arc::new(AtomicBool::new(true));

    info!(
        data_path = %config.data_path,
        auth = %config.auth_config,
        start_paused = start_paused,
        "rkasr driver starting"
    );

    let handle = {
        let h = unsafe { ffi::RockXCreate() };
        if h.is_null() {
            return Err(DemoError::Asr("RockXCreate returned null".into()));
        }
        info!("rkasr RockXCreate ok");
        RkAsrHandle(h)
    };

    let module_name = CString::new("LLMASR").unwrap();
    let data_path = CString::new(config.data_path.as_str())
        .map_err(|e| DemoError::Asr(format!("data_path: {e}")))?;

    let auth_path_buf = write_auth_config(&config.auth_config)
        .map_err(|e| DemoError::Asr(format!("write auth config: {e}")))?;
    let auth_path_str = auth_path_buf.to_string_lossy();
    let auth_path = CString::new(auth_path_str.as_ref())
        .map_err(|e| DemoError::Asr(format!("auth_path: {e}")))?;

    let c_mn = CString::new("module_name").unwrap();
    let c_dp = CString::new("data_path").unwrap();
    let c_ac = CString::new("auth_config").unwrap();
    let c_param = CString::new("param").unwrap();
    let c_sr = CString::new("src_sample_rate").unwrap();
    let c_cs = CString::new("chunk_size").unwrap();
    let c_out = CString::new("out").unwrap();

    unsafe {
        let n = std::ptr::null();
        check_ret(ffi::RockXSetParamString(
            handle.0,
            n,
            c_mn.as_ptr(),
            module_name.as_ptr(),
            0,
        ))?;
        debug!("rkasr set module_name ok");
        check_ret(ffi::RockXSetParamString(
            handle.0,
            n,
            c_dp.as_ptr(),
            data_path.as_ptr(),
            0,
        ))?;
        debug!("rkasr set data_path ok");
        check_ret(ffi::RockXSetParamString(
            handle.0,
            n,
            c_ac.as_ptr(),
            auth_path.as_ptr(),
            0,
        ))?;
        debug!("rkasr set auth_config ok");
    }

    let callback_ctx = Box::new(CallbackContext {
        event_tx: event_tx.clone(),
        frame_done: frame_done.clone(),
    });

    let init_param = Box::new(ffi::RockXAsrInitParam {
        model_name: std::ptr::null(),
        work_mode: ffi::ROCKASR_MODE_STREAM,
        lang_mode: ffi::RockAsrLangMode {
            source: ffi::ROCKASR_LANG_CHINESE,
            target: ffi::ROCKASR_LANG_ENGLISH,
        },
        sample_rate: 16000,
        userdata: &*callback_ctx as *const CallbackContext as *mut std::ffi::c_void,
        result_callback: Some(asr_result_callback),
    });

    unsafe {
        check_ret(ffi::RockXSetParamPointer(
            handle.0,
            module_name.as_ptr(),
            c_param.as_ptr(),
            Box::into_raw(init_param) as *mut std::ffi::c_void,
            1,
            None,
        ))?;
        debug!("rkasr set param pointer ok");

        let n = std::ptr::null();
        check_ret(ffi::RockXSetParamInt(handle.0, n, c_sr.as_ptr(), 16000))?;
        debug!("rkasr set sample_rate ok");
        check_ret(ffi::RockXSetParamInt(
            handle.0,
            module_name.as_ptr(),
            c_cs.as_ptr(),
            3,
        ))?;
        debug!("rkasr set chunk_size=3 ok");

        check_ret(ffi::RockXInit(handle.0))?;
        info!("rkasr RockXInit ok");

        // Force ASR shared libs load (their ELF constructors register LLMASR)
        unsafe extern "C" {
            fn force_asr_libs_init();
        }
        unsafe {
            force_asr_libs_init();
        }
        info!("rkasr force-asr-libs ok");

        check_ret(ffi::RockXSetOutputInfoDefault(
            handle.0,
            c_out.as_ptr(),
            Some(_dummy_output_cb),
            ffi::ROCKX_CALLBACK_ON_SCHED_THREAD,
            std::ptr::null_mut(),
        ))?;
        debug!("rkasr set output info ok");
    }

    info!("rkasr initialized");
    let _ = event_tx.send(AsrEvent::TaskStarted).await;
    debug!("rkasr TaskStarted sent");

    let mut first = Arc::new(AtomicBool::new(true));

    loop {
        if paused.load(Ordering::SeqCst) {
            match cmd_rx.recv().await {
                Some(RkAsrCommand::Resume(done)) => {
                    info!("rkasr resume (from paused)");
                    paused.store(false, Ordering::SeqCst);
                    first.store(true, Ordering::SeqCst);
                    let _ = event_tx.send(AsrEvent::TaskStarted).await;
                    let _ = done.send(Ok(()));
                    continue;
                }
                Some(RkAsrCommand::Pause(done)) => {
                    let _ = done.send(Ok(()));
                    continue;
                }
                Some(RkAsrCommand::FinishUtterance(done)) => {
                    let _ = done.send(Ok(()));
                    continue;
                }
                Some(RkAsrCommand::Audio(_)) => continue,
                None => break,
            }
        }

        tokio::select! {
            cmd = cmd_rx.recv() => {
                match cmd {
                    Some(RkAsrCommand::Audio(bytes)) => {
                        let samples = bytes.len() as i32 / 2;
                        if samples == 0 { continue; }

                        let is_first = first.swap(false, Ordering::SeqCst);
                        let state = if is_first {
                            ffi::ROCKX_STREAM_STATE_FIRST
                        } else {
                            ffi::ROCKX_STREAM_STATE_CONTINUE
                        };

                        debug!(
                            bytes = bytes.len(),
                            samples = samples,
                            first = is_first,
                            "rkasr sending audio"
                        );

                        let buf = bytes.to_vec();
                        let buf_ptr = buf.as_ptr() as *mut std::ffi::c_void;

                        let input = unsafe { ffi::RockXInputCreate(handle.0) };
                        if input.is_null() {
                            warn!("rkasr RockXInputCreate failed");
                            continue;
                        }

                        let pkt = unsafe { ffi::RockXInputGetPacketAt(input, 0) };

                        unsafe {
                            ffi::RockXPacketSetAudio2(
                                pkt, state, 16000, 1, samples,
                                ffi::ROCKX_DATA_TYPE_INT16, buf_ptr,
                            );
                        }

                        std::mem::forget(buf);
                        unsafe { ffi::RockXProcessAsync(handle.0, input, std::ptr::null_mut()); }
                    }
                    Some(RkAsrCommand::Pause(done)) => {
                        info!("rkasr pause");
                        paused.store(true, Ordering::SeqCst);
                        // Send LAST with silence to flush
                        let silence = vec![0i16; 1600];
                        let input = unsafe { ffi::RockXInputCreate(handle.0) };
                        if !input.is_null() {
                            let pkt = unsafe { ffi::RockXInputGetPacketAt(input, 0) };
                            unsafe {
                                ffi::RockXPacketSetAudio2(
                                    pkt, ffi::ROCKX_STREAM_STATE_LAST,
                                    16000, 1, 1600, ffi::ROCKX_DATA_TYPE_INT16,
                                    silence.as_ptr() as *mut std::ffi::c_void,
                                );
                                std::mem::forget(silence);
                                ffi::RockXProcessAsync(handle.0, input, std::ptr::null_mut());
                            }
                        }
                        let _ = done.send(Ok(()));
                    }
                    Some(RkAsrCommand::Resume(done)) => {
                        info!("rkasr resume (already active)");
                        first.store(true, Ordering::SeqCst);
                        let _ = done.send(Ok(()));
                    }
                    Some(RkAsrCommand::FinishUtterance(done)) => {
                        info!("rkasr finish_utterance: sending LAST to flush");
                        frame_done.store(false, Ordering::SeqCst);
                        let silence = vec![0i16; 1600];
                        let input = unsafe { ffi::RockXInputCreate(handle.0) };
                        if !input.is_null() {
                            let pkt = unsafe { ffi::RockXInputGetPacketAt(input, 0) };
                            unsafe {
                                ffi::RockXPacketSetAudio2(
                                    pkt, ffi::ROCKX_STREAM_STATE_LAST,
                                    16000, 1, 1600, ffi::ROCKX_DATA_TYPE_INT16,
                                    silence.as_ptr() as *mut std::ffi::c_void,
                                );
                                std::mem::forget(silence);
                                ffi::RockXProcessAsync(handle.0, input, std::ptr::null_mut());
                            }
                        }
                        let deadline = tokio::time::Instant::now() + Duration::from_millis(1500);
                        while !frame_done.load(Ordering::SeqCst) {
                            if tokio::time::Instant::now() >= deadline {
                                warn!("rkasr finish_utterance: flush timed out");
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(5)).await;
                        }
                        info!("rkasr finish_utterance: flush complete");
                        first.store(true, Ordering::SeqCst);
                        let _ = done.send(Ok(()));
                    }
                    None => break,
                }
            }
        }
    }

    unsafe { ffi::RockXDestroy(handle.0) };
    drop(callback_ctx);
    info!("rkasr driver shut down");
    Ok(())
}
