use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error, info, warn};

use crate::aec::{self, AecEngine};
use crate::asr::{AsrBackend, AsrEngine, AsrEvent, create_asr};
use crate::audio::{AudioCapture, AudioPlayback, LinearResampler};
use crate::config::{Config, LlmConfig, OrchestratorConfig};
use crate::denoise::StreamingDenoiser;
use crate::error::Result;
use crate::kws::WakeDetectorHandle;
use crate::kws::start_wake_detector;
use crate::llm::{AccumulatedToolCall, ChatMessage, LlmClient, OpenAiCompatClient, ToolCall};
use crate::orchestrator::{
    SessionState, WakePhase, flush_remainder, matches_sleep_keyword, normalize_tts_text,
    take_early_chunk, take_sentence, texts_compatible,
};
use crate::speaker::SpeakerVerifier;
use crate::tools;
use crate::tools::hermes_queue::{HermesMessage, HermesQueue, HermesQueueSender, HermesWorkItem};
use crate::tts::{TtsBackend, TtsEngine, create_tts};
use crate::vad::{EndpointDetector, SileroVad, VadEngine, WebRtcVad};

pub struct Session {
    cfg: Config,
    hermes_work_tx: Option<mpsc::Sender<HermesWorkItem>>,
    hermes_msg_tx: Option<mpsc::Sender<HermesMessage>>,
    hermes_msg_rx: Option<mpsc::Receiver<HermesMessage>>,
}

/// Per-turn latency markers for KPI logs.
struct TurnLatency {
    asr_final_at: Option<Instant>,
    trigger_at: Instant,
    logged_first_pcm: Arc<AtomicBool>,
}

impl TurnLatency {
    fn log_first_pcm(&self) {
        if self.logged_first_pcm.swap(true, Ordering::SeqCst) {
            return;
        }
        let now = Instant::now();
        info!(
            trigger_to_first_pcm_ms = now.duration_since(self.trigger_at).as_millis(),
            "latency: first pcm"
        );
        if let Some(t) = self.asr_final_at {
            info!(
                asr_final_to_first_pcm_ms = now.duration_since(t).as_millis(),
                "latency: asr final -> first pcm"
            );
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SpeakerGate {
    Idle,
    Verifying,
    Passed,
    Rejected,
}

struct ActiveTurn {
    user_text: String,
    speculative: bool,
}

impl Session {
    pub fn new(cfg: Config) -> Self {
        Self {
            cfg,
            hermes_work_tx: None,
            hermes_msg_tx: None,
            hermes_msg_rx: None,
        }
    }

    pub fn with_hermes_work_tx(mut self, work_tx: mpsc::Sender<HermesWorkItem>) -> Self {
        self.hermes_work_tx = Some(work_tx);
        self
    }

    pub fn with_hermes_msg_channels(
        mut self,
        msg_tx: mpsc::Sender<HermesMessage>,
        msg_rx: mpsc::Receiver<HermesMessage>,
    ) -> Self {
        self.hermes_msg_tx = Some(msg_tx);
        self.hermes_msg_rx = Some(msg_rx);
        self
    }

    pub async fn run(self) -> Result<()> {
        self.run_inner().await
    }

    async fn run_inner(mut self) -> Result<()> {
        let orch = self.cfg.orchestrator.clone();
        let capture = AudioCapture::start(&self.cfg.audio, self.cfg.asr.chunk_ms)?;
        let playback = Arc::new(AudioPlayback::start(
            &self.cfg.audio,
            self.cfg.tts.sample_rate,
        )?);

        let wake_cfg = self.cfg.wake.clone();
        let wake_enabled = wake_cfg.enabled;
        let sleep_phrases = wake_cfg.effective_sleep_phrases();

        let asr_backend = AsrBackend::from_config(&self.cfg.asr);
        let asr_offline = matches!(asr_backend, AsrBackend::Sherpa);
        let (asr, mut asr_rx) = create_asr(
            &self.cfg.dashscope,
            &self.cfg.asr,
            wake_enabled,
            asr_backend,
        )
        .await?;
        let backend = TtsBackend::from_config(&self.cfg.tts);
        let (tts, mut tts_rx) = create_tts(&self.cfg.dashscope, &self.cfg.tts, backend).await?;
        // Inject OS info into system prompt so the LLM uses correct commands
        {
            let os = std::env::consts::OS;
            let os_hint = match os {
                "windows" => {
                    "[系统环境] 当前操作系统: Windows。shell命令使用方式：PowerShell cmdlet(如Get-Date)需用 powershell -Command \"...\"，cmd内置命令(如dir)需用 cmd /c ...，独立exe可直接用(如ping、ipconfig、systeminfo、findstr)。获取时间应使用 Get-Date，查文件用 dir 或 Get-ChildItem，搜索用 findstr 或 Select-String。"
                }
                "linux" => {
                    "[系统环境] 当前操作系统: Linux。命令直接执行(非shell)，参数按空格分割后传给程序。含空格的参数必须用引号包裹，如 date '+%Y-%m-%d %H:%M:%S'。date获取当前时间示例: date '+%Y年%m月%d日 %H:%M:%S' 或 date '+%Y-%m-%d %H:%M:%S'。其他常用命令：ls、grep、cat、uptime、free、ps等。"
                }
                "macos" => {
                    "[系统环境] 当前操作系统: macOS。命令直接执行(非shell)，参数按空格分割后传给程序。含空格的参数必须用引号包裹，如 date '+%Y-%m-%d %H:%M:%S'。date获取当前时间示例: date '+%Y年%m月%d日 %H:%M:%S' 或 date '+%Y-%m-%d %H:%M:%S'。其他常用命令：ls、grep、cat、uptime等。"
                }
                _ => "[系统环境] 当前操作系统: unix-like。shell命令使用标准POSIX语法。",
            };
            if !self.cfg.llm.system_prompt.contains(os_hint) {
                self.cfg.llm.system_prompt = format!("{os_hint}\n{}", self.cfg.llm.system_prompt);
            }
        }
        // Inject hermes guidance: call_hermes is async, hermes will reply later.
        {
            let hermes_hint = "[工具提示] call_hermes 是把请求发给 hermes（后台智能助手）异步处理，hermes 可能几秒或更久才会回复。调用后你收到的 tool result 只有入队确认，不代表任务完成。调用时 spoken 参数要填写给用户的自然口语播报，简述你正在帮用户处理什么（如'帮你查一下天气''我看看这个怎么弄'），不要用模板化开头。之后你应告诉用户'已帮你提交给 hermes 处理，稍后 hermes 会回复你'。严禁说'已经设置好了''已经完成了'等话。";
            if !self.cfg.llm.system_prompt.contains(hermes_hint) {
                self.cfg.llm.system_prompt =
                    format!("{hermes_hint}\n{}", self.cfg.llm.system_prompt);
            }
        }
        let llm: Arc<dyn LlmClient> = {
            let client = OpenAiCompatClient::new(self.cfg.llm.clone());
            if self.cfg.llm.warmup_on_start {
                if let Err(e) = client.warmup().await {
                    warn!(error = %e, "llm warmup failed");
                }
            }
            Arc::new(client)
        };

        let wake_detector: Option<WakeDetectorHandle> = if wake_enabled {
            let phrases = wake_cfg.effective_phrases();
            let phrase_str = phrases.join(", ");
            let detector = start_wake_detector(&wake_cfg, self.cfg.asr.sample_rate)?;
            info!(
                phrases = %phrase_str,
                boost_score = wake_cfg.boost_score,
                trigger_threshold = wake_cfg.trigger_threshold,
                grace_after_wake = wake_cfg.grace_after_wake_sec,
                idle_after_turn = wake_cfg.idle_after_turn_sec,
                barge_in_requires_wake = orch.barge_in_requires_wake,
                "wake: detector started"
            );
            Some(detector)
        } else {
            None
        };

        let mut wake_phase = if wake_enabled {
            let _ = asr.pause().await;
            WakePhase::Dormant
        } else {
            asr.resume().await?;
            WakePhase::Active
        };

        let (pcm_tx, mut pcm_rx) = mpsc::channel(64);
        let aec_ref_buf = aec::create_ref_buf(self.cfg.asr.sample_rate, 500);
        let aec_cfg = self.cfg.aec.clone();
        let aec_ref_clone = aec_ref_buf.clone();
        std::thread::spawn(move || {
            let mut aec_engine = AecEngine::new(&aec_cfg, aec_ref_clone);
            loop {
                if let Some(chunk) = capture.try_recv_chunk() {
                    let cleaned = aec_engine.process(&chunk.samples_f32);
                    let bytes = crate::audio::pcm::f32_to_i16_le(&cleaned);
                    let aec_chunk = crate::audio::capture::AudioChunk {
                        samples_f32: cleaned,
                        samples_i16_bytes: bytes,
                    };
                    let _ = pcm_tx.blocking_send(aec_chunk);
                } else {
                    std::thread::sleep(Duration::from_millis(5));
                }
            }
        });

        let play_gen = Arc::new(AtomicU64::new(0));
        let playback_tts = playback.clone();
        let play_gen_tts = play_gen.clone();
        let current_latency: Arc<std::sync::Mutex<Option<Arc<TurnLatency>>>> =
            Arc::new(std::sync::Mutex::new(None));
        let latency_for_tts = current_latency.clone();
        let turn_epoch = Arc::new(AtomicU64::new(0));
        let turn_epoch_tts = turn_epoch.clone();
        let aec_ref_tts = aec_ref_buf.clone();
        tokio::spawn(async move {
            let mut last_epoch = 0u64;
            let mut resampler = LinearResampler::new(24000, 16000);
            while let Some(audio) = tts_rx.recv().await {
                let epoch = turn_epoch_tts.load(Ordering::SeqCst);
                if epoch != last_epoch {
                    last_epoch = epoch;
                    while tts_rx.try_recv().is_ok() {}
                    // Keep draining for a short window to catch late-arriving
                    // audio from the old turn (still in-flight from TTS server).
                    let deadline = Instant::now() + Duration::from_millis(200);
                    loop {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        if remaining.is_zero() {
                            break;
                        }
                        match tokio::time::timeout(remaining, tts_rx.recv()).await {
                            Ok(Some(_)) => while tts_rx.try_recv().is_ok() {},
                            _ => break,
                        }
                    }
                    continue;
                }
                // Feed reference to AEC (resample 24k->16k)
                let f32_24k = crate::audio::pcm::i16_le_to_f32(&audio.pcm);
                let ref_16k = resampler.push(&f32_24k);
                aec::push_ref(&aec_ref_tts, &ref_16k, 16000, 500);
                // Forward to playback
                let g = play_gen_tts.load(Ordering::SeqCst);
                if let Ok(guard) = latency_for_tts.lock() {
                    if let Some(lat) = guard.as_ref() {
                        lat.log_first_pcm();
                    }
                }
                playback_tts.enqueue_pcm_i16(g, &audio.pcm);
            }
        });

        let mut vad = if let Some(sv) = SileroVad::create(
            &self.cfg.vad.model_path,
            self.cfg.asr.sample_rate as i32,
            self.cfg.vad.threshold,
            self.cfg.vad.min_silence_duration,
            self.cfg.vad.min_speech_duration,
            self.cfg.vad.max_speech_duration,
            orch.barge_in_sustain_frames,
            self.cfg.asr.chunk_ms,
        ) {
            info!(
                model = %self.cfg.vad.model_path,
                threshold = self.cfg.vad.threshold,
                min_silence = self.cfg.vad.min_silence_duration,
                min_speech = self.cfg.vad.min_speech_duration,
                max_speech = self.cfg.vad.max_speech_duration,
                "vad: silero started"
            );
            VadEngine::Silero(sv)
        } else {
            warn!(model = %self.cfg.vad.model_path, "vad: silero model not found, falling back to webrtc vad_mode={}", orch.vad_mode);
            VadEngine::WebRtc(WebRtcVad::new(
                self.cfg.asr.sample_rate,
                orch.barge_in_frames,
                orch.barge_in_sustain_frames,
                orch.vad_mode,
            ))
        };

        let mut denoiser = StreamingDenoiser::create(&self.cfg.denoise);
        let denoiser_enabled = denoiser.is_some();

        let speaker_verifier = SpeakerVerifier::create(&self.cfg.speaker);
        let speaker_enabled = speaker_verifier.is_some();
        let recent_audio_max = (self.cfg.asr.sample_rate * 3) as usize; // 3s buffer for speaker verify
        let mut recent_audio: VecDeque<f32> = VecDeque::with_capacity(recent_audio_max);

        let mut state = SessionState::Listening;
        let session_start = Instant::now();
        let cold_start = Duration::from_secs(orch.cold_start_sec);

        let mut messages: Vec<ChatMessage> = Vec::new();
        let mut last_final: Option<String> = None;
        let mut asr_final_at: Option<Instant> = None;
        let mut llm_cancel: Option<CancellationToken> = None;
        let mut active_turn: Option<ActiveTurn> = None;
        let mut last_barge_in_at: Option<Instant> = None;
        let mut _last_wake_at: Option<Instant> = None;
        let asr_settle_ms: u64 = 300;

        // Speculative partial tracking
        let mut last_partial = String::new();
        let mut partial_stable_since: Option<Instant> = None;
        let mut last_asr_event_at: Option<Instant> = None;
        let mut input_gated: bool = false;
        let mut utterance_active: bool = false;
        let mut pending_offline_flush: Option<Instant> = None;

        let (done_tx, mut done_rx) = mpsc::channel::<(String, u64, bool)>(4);

        let (_hermes_queue, mut hermes_msg_rx, hermes_sender_for_spawn) =
            if self.cfg.llm.tools_enabled {
                let aipc = self.cfg.llm.aipc_talk.clone();
                let external_msg = self.hermes_msg_tx.take().zip(self.hermes_msg_rx.take());
                let (queue, rx, _handle) = if aipc.uses_channel() {
                    let work_tx = self.hermes_work_tx.ok_or_else(|| {
                        crate::error::DemoError::Config(
                            "call_hermes channel transport requires embedded Hermes runtime \
                             (missing work channel)"
                                .to_string(),
                        )
                    })?;
                    if let Some((msg_tx, msg_rx)) = external_msg {
                        HermesQueue::new_channel_shared(aipc, work_tx, msg_tx, msg_rx)
                    } else {
                        let (q, rx, h, _push) = HermesQueue::new_channel(aipc, work_tx);
                        (q, rx, h)
                    }
                } else if let Some((msg_tx, msg_rx)) = external_msg {
                    HermesQueue::new_shared(aipc, msg_tx, msg_rx, None)
                } else {
                    let (q, rx, h, _push) = HermesQueue::new(aipc);
                    (q, rx, h)
                };
                let sender = queue.sender.clone();
                (Some(queue), rx, Some(sender))
            } else {
                let rx = self.hermes_msg_rx.take().unwrap_or_else(|| {
                    let (_tx, rx) = mpsc::channel::<HermesMessage>(1);
                    rx
                });
                (None, rx, None)
            };
        let mut pending_hermes_msgs: VecDeque<HermesMessage> = VecDeque::new();

        let mut speaker_gate = SpeakerGate::Idle;
        let mut speaker_verify_buffer: Vec<f32> = Vec::new();
        let speaker_verify_max = (self.cfg.asr.sample_rate as usize).saturating_mul(2); // 2s
        let speaker_verify_gate = speaker_verifier
            .as_ref()
            .is_some_and(|sv| sv.has_voiceprint());
        if !speaker_verify_gate {
            speaker_gate = SpeakerGate::Passed;
        }

        info!(
            cold_start_sec = orch.cold_start_sec,
            endpoint_silence_ms = orch.endpoint_silence_ms(),
            speculative_llm = orch.speculative_llm,
            wake_enabled,
            denoise_enabled = denoiser_enabled,
            speaker_enabled,
            min_rms_barge_in = orch.min_rms_barge_in,
            barge_in_sustain = orch.barge_in_sustain_frames,
            barge_in_cooldown_ms = orch.barge_in_cooldown_ms,
            speaker_verify_gate,
            "session ready"
        );
        if wake_enabled {
            info!(phrases = ?wake_cfg.effective_phrases(), "waiting for wake word");
        }
        if !sleep_phrases.is_empty() {
            info!(phrases = ?sleep_phrases, "sleep keywords enabled");
        }

        if let Err(e) = tts.warmup().await {
            warn!(error = %e, "tts warmup failed, will auto-start on first speech");
        }

        let grace_after_wake = Duration::from_secs(wake_cfg.grace_after_wake_sec);
        let idle_after_turn = Duration::from_secs(wake_cfg.idle_after_turn_sec);
        let mut diag_tick: u32 = 0;
        let mut last_barge_in_suppress_warn: Option<Instant> = None;
        let wake_ack_playing = Arc::new(AtomicBool::new(false));
        let (wake_ack_done_tx, mut wake_ack_done_rx) = mpsc::channel::<()>(1);

        loop {
            tokio::select! {
                    chunk = pcm_rx.recv() => {
                        let Some(chunk) = chunk else { break };
                        let raw_rms = rms_f32(&chunk.samples_f32);

                        let samples_f32 = if let Some(ref mut d) = denoiser {
                            let denoised = d.process(&chunk.samples_f32, self.cfg.asr.sample_rate);
                            if denoised.is_empty() {
                                chunk.samples_f32.clone()
                            } else {
                                denoised
                            }
                        } else {
                            chunk.samples_f32.clone()
                        };

                        if speaker_enabled {
                            for &s in &samples_f32 {
                                if recent_audio.len() >= recent_audio_max {
                                    recent_audio.pop_front();
                                }
                                recent_audio.push_back(s);
                            }
                        }

                        if let Some(ref det) = wake_detector {
                            det.feed(&samples_f32);
                        }
                        vad.feed(&samples_f32);

                        let speech_just_started = vad.speech_start();
                        // Diagnostic: log audio level + VAD state during AwakeGrace every ~500ms
                        if matches!(wake_phase, WakePhase::AwakeGrace { .. }) {
                            diag_tick += 1;
                            if diag_tick % 5 == 0 {
                                let denoised_rms = rms_f32(&samples_f32);
                                info!(
                                    raw_rms = format!("{:.6}", raw_rms),
                                    denoised_rms = format!("{:.6}", denoised_rms),
                                    vad_rms = format!("{:.6}", vad.last_rms()),
                                    vad_in_speech = vad.in_speech(),
                                    vad_speech_start = speech_just_started,
                                    speaker_gate = format!("{:?}", speaker_gate),
                                    "grace diag"
                                );
                            }
                        }
                        if speaker_verify_gate && speech_just_started && speaker_gate == SpeakerGate::Idle {
                            speaker_gate = SpeakerGate::Verifying;
                            speaker_verify_buffer.clear();
                        }

                        if wake_enabled && wake_detector.as_ref().is_some_and(|d| d.try_recv_wake()) {
                            _last_wake_at = Some(Instant::now());
                            if matches!(wake_phase, WakePhase::Dormant) {
                                info!("wake: waking from dormant — connecting ASR");
                                let has_ack = !wake_cfg.ack_reply.trim().is_empty();
                                let _ = asr.set_gate(true).await;
                                let _ = asr.reconnect().await;
                                utterance_active = false;
                                input_gated = has_ack;
                                if has_ack {
                                    wake_ack_playing.store(true, Ordering::SeqCst);
                                } else if !resume_asr_with_retry(asr.clone()).await {
                                    warn!("wake: ASR resume failed; staying dormant");
                                    continue;
                                } else {
                                    let _ = asr.set_gate(false).await;
                                }
                                let ack_extra = if has_ack {
                                    Duration::from_secs(3)
                                } else {
                                    Duration::ZERO
                                };
                                wake_phase = WakePhase::AwakeGrace {
                                    deadline: Instant::now() + grace_after_wake + ack_extra,
                                };
                                info!(
                                    grace_sec = wake_cfg.grace_after_wake_sec,
                                    ack = %wake_cfg.ack_reply,
                                    has_ack,
                                    "wake: accepted, now in AwakeGrace; speak within grace period"
                                );
                                if has_ack {
                                    let ack = wake_cfg.ack_reply.clone();
                                    let tts_ack = tts.clone();
                                    let playback_ack = playback.clone();
                                    let play_gen_ack = play_gen.clone();
                                    let done_tx = wake_ack_done_tx.clone();
                                    tokio::spawn(async move {
                                        play_wake_ack(&ack, tts_ack, &playback_ack, &play_gen_ack)
                                            .await;
                                        let _ = done_tx.send(()).await;
                                    });
                                }
                            } else if orch.barge_in_enabled {
                                if let Some(last) = last_barge_in_at {
                                    if last.elapsed().as_millis() < orch.barge_in_cooldown_ms as u128 {
                                        continue;
                                    }
                                }
                                info!("wake-word barge-in");
                                do_barge_in(
                                    &turn_epoch,
                                    &playback,
                                    &play_gen,
                                    &mut llm_cancel,
                                    &mut vad,
                                    tts.clone(),
                                    asr.clone(),
                                    wake_enabled,
                                    &mut wake_phase,
                                    &mut state,
                                    &mut active_turn,
                                    &current_latency,
                                    &mut last_partial,
                                    &mut partial_stable_since,
                                    &mut last_barge_in_at,
                                    &mut speaker_gate,
                                    &mut speaker_verify_buffer,
                                    speaker_verify_gate,
                                )
                                .await;
                                input_gated = false;
                            }
                        }

                        let ack_playing = wake_ack_playing.load(Ordering::SeqCst);

                        if !input_gated && !ack_playing && wake_phase.allows_asr() {
                            let do_send = match speaker_gate {
                                SpeakerGate::Idle => false,
                                SpeakerGate::Verifying => {
                                    speaker_verify_buffer.extend_from_slice(&samples_f32);
                                    if speaker_verify_buffer.len() >= speaker_verify_max {
                                        let buf = std::mem::take(&mut speaker_verify_buffer);
                                        let passed = speaker_verifier.as_ref().map_or(true, |sv| {
                                            sv.verify(&buf, self.cfg.asr.sample_rate)
                                        });
                                        if passed {
                                            speaker_gate = SpeakerGate::Passed;
                                            info!("speaker gate passed");
                                            let i16_bytes = f32_slice_to_i16_bytes(&buf);
                                            let _ = asr.send_audio(i16_bytes).await;
                                            true
                                        } else {
                                            speaker_gate = SpeakerGate::Rejected;
                                            vad.reset_barge_in_state();
                                            info!("speaker gate rejected");
                                            false
                                        }
                                    } else {
                                        false
                                    }
                                }
                                SpeakerGate::Passed => true,
                                SpeakerGate::Rejected => false,
                            };
                            if do_send {
                                let i16_bytes = f32_slice_to_i16_bytes(&samples_f32);
                                let _ = asr.send_audio(i16_bytes).await;
                            }
                        }

                        if !ack_playing {
                            if wake_phase.allows_asr() && (speech_just_started || vad.in_speech()) {
                                utterance_active = true;
                                pending_offline_flush = None;
                            }

                            if user_speech_activity(&mut vad, None, orch.min_final_chars, &wake_phase, orch.grace_min_final_chars) {
                                if promote_wake_on_speech(&mut wake_phase) {
                                    partial_stable_since = None;
                                    last_partial.clear();
                                }
                            }
                        }

                        if wake_phase.check_timeout(Instant::now()) {
                            enter_dormant(
                                asr.clone(),
                                &mut wake_phase,
                                &mut state,
                                &mut active_turn,
                                &mut last_final,
                                &mut asr_final_at,
                                &mut partial_stable_since,
                                &mut last_partial,
                                &mut llm_cancel,
                                &current_latency,
                                &mut asr_rx,
                                &mut speaker_gate,
                                &mut speaker_verify_buffer,
                                speaker_verify_gate,
                            )
                            .await;
                            continue;
                        }

                        if !wake_phase.allows_dialog() {
                            continue;
                        }

                        if !wake_enabled || !orch.barge_in_requires_wake {
                            if try_barge_in(
                                "vad",
                                &orch,
                                &mut state,
                                &mut vad,
                                &playback,
                                &play_gen,
                                &mut llm_cancel,
                                            &mut active_turn,
                                            &mut partial_stable_since,
                                            &mut last_partial,
                                            &current_latency,
                                            &turn_epoch,
                                            tts.clone(),
                                            None,
                                            &mut last_barge_in_at,
                                            &speaker_verifier,
                                            &recent_audio,
                                            &mut speaker_gate,
                                            &mut speaker_verify_buffer,
                                            speaker_verify_gate,
                                            asr.clone(),
                                            &mut wake_phase,
                                            wake_enabled,
                                        )
                                        .await
                                        {
                                input_gated = false;
                                continue;
                            }
                            let now = Instant::now();
                            if last_barge_in_suppress_warn
                                .is_none_or(|t| now.duration_since(t).as_secs() >= 3)
                            {
                                warn!(
                                    phrases = ?wake_cfg.effective_phrases(),
                                    "wake word required to barge-in"
                                );
                                last_barge_in_suppress_warn = Some(now);
                            }
                        }

                        let flush_now = if asr_offline {
                            update_pending_offline_flush(
                                utterance_active,
                                input_gated,
                                &vad,
                                speech_just_started,
                                orch.endpoint_silence_ms(),
                                orch.offline_continuation_ms,
                                &mut pending_offline_flush,
                            )
                        } else {
                            ready_to_flush_asr(
                                false,
                                input_gated,
                                &vad,
                                orch.endpoint_silence_ms(),
                                utterance_active,
                                &last_final,
                                orch.min_final_chars,
                            )
                        };

                        if flush_now {
                            pending_offline_flush = None;
                            input_gated = true;
                            info!("end of speech: gating audio, flushing ASR");
                            if let Err(e) = asr.finish_utterance().await {
                                warn!(error = %e, "finish_utterance failed");
                            }
                            utterance_active = false;
                            while let Ok(ev) = asr_rx.try_recv() {
                                match ev {
                                    AsrEvent::Final { text } => {
                                        info!(
                                            final_text = %text,
                                            last_final = ?last_final,
                                            state = ?state,
                                            wake_phase = ?wake_phase,
                                            "asr final (post-flush)"
                                        );
                                        last_asr_event_at = Some(Instant::now());
                                        let sep = if last_final.as_deref().is_some_and(|s| !s.ends_with(['\n', ' '])) { " " } else { "" };
                                        last_final = Some(match last_final.take() {
                                            Some(prev) => format!("{prev}{sep}{text}"),
                                            None => text,
                                        });
                                        asr_final_at = Some(Instant::now());
                                        partial_stable_since = None;
                                        last_partial.clear();
                                    }
                                    AsrEvent::Partial { text } => {
                                        debug!(partial = %text, "asr partial (post-flush)");
                                        last_asr_event_at = Some(Instant::now());
                                    }
                                    _ => {}
                                }
                            }
                            maybe_trigger(
                                &orch,
                                &mut state,
                                session_start,
                                cold_start,
                                &mut vad,
                                &mut last_final,
                                &mut asr_final_at,
                                &mut messages,
                                &llm,
                                tts.clone(),
                                &playback,
                                &play_gen,
                                &mut llm_cancel,
                                &mut active_turn,
                                &done_tx,
                                &current_latency,
                                &turn_epoch,
                                asr.clone(),
                                wake_enabled,
                                &sleep_phrases,
                                &self.cfg.llm,
                                hermes_sender_for_spawn.clone(),
                                &mut wake_phase,
                                &mut asr_rx,
                                &mut partial_stable_since,
                                &mut last_partial,
                                &mut speaker_gate,
                                &mut speaker_verify_buffer,
                                speaker_verify_gate,
                            )
                            .await;
                            continue;
                        }

                        if last_asr_event_at.map_or(true, |t| t.elapsed() >= Duration::from_millis(asr_settle_ms)) {
                            maybe_trigger(
                            &orch,
                            &mut state,
                            session_start,
                            cold_start,
                            &mut vad,
                            &mut last_final,
                            &mut asr_final_at,
                            &mut messages,
                            &llm,
                            tts.clone(),
                            &playback,
                            &play_gen,
                            &mut llm_cancel,
                            &mut active_turn,
                            &done_tx,
                            &current_latency,
                            &turn_epoch,
                            asr.clone(),
                            wake_enabled,
                            &sleep_phrases,
                            &self.cfg.llm,
                            hermes_sender_for_spawn.clone(),
                            &mut wake_phase,
                            &mut asr_rx,
                            &mut partial_stable_since,
                            &mut last_partial,
                            &mut speaker_gate,
                            &mut speaker_verify_buffer,
                            speaker_verify_gate,
                        )
                        .await;
                        }
                    }
                    ev = asr_rx.recv() => {
                        if let Some(ev) = ev {
                            match ev {
                                AsrEvent::Partial { text } => {
                                    debug!(
                                        partial = %text,
                                        state = ?state,
                                        wake_phase = ?wake_phase,
                                        "asr partial"
                                    );
                                    last_asr_event_at = Some(Instant::now());
                                    if speaker_verify_gate && vad.speech_start() && speaker_gate == SpeakerGate::Idle {
                                        speaker_gate = SpeakerGate::Verifying;
                                        speaker_verify_buffer.clear();
                                    }
                                    if user_speech_activity(
                                        &mut vad,
                                        Some(&text),
                                        orch.min_final_chars,
                                        &wake_phase,
                                        orch.grace_min_final_chars,
                                    ) {
                                        if promote_wake_on_speech(&mut wake_phase) {
                                            partial_stable_since = None;
                                            last_partial.clear();
                                        }
                                    }
                                    if !wake_phase.allows_dialog() {
                                        continue;
                                    }
                                    if !wake_enabled || !orch.barge_in_requires_wake {
                                        if try_barge_in(
                                            "asr-partial",
                                            &orch,
                                            &mut state,
                                            &mut vad,
                                            &playback,
                                            &play_gen,
                                            &mut llm_cancel,
                                            &mut active_turn,
                                            &mut partial_stable_since,
                                            &mut last_partial,
                                            &current_latency,
                                            &turn_epoch,
                                            tts.clone(),
                                            Some(text.as_str()),
                                            &mut last_barge_in_at,
                                            &speaker_verifier,
                                            &recent_audio,
                                            &mut speaker_gate,
                                            &mut speaker_verify_buffer,
                                            speaker_verify_gate,
                                            asr.clone(),
                                            &mut wake_phase,
                                            wake_enabled,
                                        )
                                        .await
                                        {
                                            input_gated = false;
                                            last_partial = text;
                                            continue;
                                        }
                                    } else {
                                        let now = Instant::now();
                                        if last_barge_in_suppress_warn
                                            .is_none_or(|t| now.duration_since(t).as_secs() >= 3)
                                        {
                                            warn!(
                                                phrases = ?wake_cfg.effective_phrases(),
                                                "wake word required to barge-in"
                                            );
                                            last_barge_in_suppress_warn = Some(now);
                                        }
                                    }
                                    if orch.speculative_llm
                                        && state == SessionState::Listening
                                        && wake_phase.allows_dialog()
                                    {
                                        if text == last_partial {
                                            // unchanged
                                        } else {
                                            last_partial = text.clone();
                                            partial_stable_since = Some(Instant::now());
                                        }
                                        if let Some(since) = partial_stable_since {
                                            if since.elapsed() >= Duration::from_millis(orch.speculative_stable_ms as u64)
                                                && text.trim().chars().count() >= orch.min_final_chars
                                                && active_turn.is_none()
                                            {
                                                if matches_sleep_keyword(&text, &sleep_phrases) {
                                                    apply_sleep_keyword(
                                                        &text,
                                                        wake_enabled,
                                                        asr.clone(),
                                                        tts.clone(),
                                                        &playback,
                                                        &play_gen,
                                                        &turn_epoch,
                                                        &mut llm_cancel,
                                                        &mut wake_phase,
                                                        &mut state,
                                                        &mut active_turn,
                                                        &mut last_final,
                                                        &mut asr_final_at,
                                                        &mut partial_stable_since,
                                                        &mut last_partial,
                                                        &current_latency,
                                                        &mut asr_rx,
                                                        &mut speaker_gate,
                                                        &mut speaker_verify_buffer,
                                                        speaker_verify_gate,
                                                    )
                                                    .await;
                                                    continue;
                                                }
                                                info!(%text, "speculative llm start");
                                                start_reply_turn(
                                                    text,
                                                    None,
                                                    false,
                                                    true,
                                                    &orch,
                                                    &mut state,
                                                    &mut messages,
                                                    &llm,
                                                    tts.clone(),
                                                    &playback,
                                                    &play_gen,
                                                    &mut llm_cancel,
                                                    &mut active_turn,
                                                    &done_tx,
                                                    &current_latency,
                                                    &turn_epoch,
                                                    asr.clone(),
                                                    wake_enabled,
                                                    &sleep_phrases,
                                                    &self.cfg.llm,
                                                    hermes_sender_for_spawn.clone(),
                                                    &mut wake_phase,
                                                    &mut asr_rx,
                                                    &mut last_final,
                                                    &mut asr_final_at,
                                                    &mut partial_stable_since,
                                                    &mut last_partial,
                                                    &mut speaker_gate,
                                                    &mut speaker_verify_buffer,
                                                    speaker_verify_gate,
                                                )
                                                .await;
                }
            }
                                    }
                                }
                                AsrEvent::Final { text } => {
                                    info!(
                                        final_text = %text,
                                        last_final = %last_final.as_deref().unwrap_or("none"),
                                        state = ?state,
                                        wake_phase = ?wake_phase,
                                        allows_dialog = wake_phase.allows_dialog(),
                                        "asr final"
                                    );
                                    if matches_sleep_keyword(&text, &sleep_phrases) {
                                        apply_sleep_keyword(
                                            &text,
                                            wake_enabled,
                                            asr.clone(),
                                            tts.clone(),
                                            &playback,
                                            &play_gen,
                                            &turn_epoch,
                                            &mut llm_cancel,
                                            &mut wake_phase,
                                            &mut state,
                                            &mut active_turn,
                                            &mut last_final,
                                            &mut asr_final_at,
                                            &mut partial_stable_since,
                                            &mut last_partial,
                                            &current_latency,
                                            &mut asr_rx,
                                            &mut speaker_gate,
                                            &mut speaker_verify_buffer,
                                            speaker_verify_gate,
                                        )
                                        .await;
                                        continue;
                                    }
                                    if speaker_verify_gate && vad.speech_start() && speaker_gate == SpeakerGate::Idle {
                                        speaker_gate = SpeakerGate::Verifying;
                                        speaker_verify_buffer.clear();
                                    }
                                    if user_speech_activity(
                                        &mut vad,
                                        Some(&text),
                                        orch.min_final_chars,
                                        &wake_phase,
                                        orch.grace_min_final_chars,
                                    ) {
                                        if promote_wake_on_speech(&mut wake_phase) {
                                            partial_stable_since = None;
                                            last_partial.clear();
                                        }
                                    }
                                    if wake_phase.check_timeout(Instant::now()) {
                                        enter_dormant(
                                            asr.clone(),
                                            &mut wake_phase,
                                            &mut state,
                                            &mut active_turn,
                                            &mut last_final,
                                            &mut asr_final_at,
                                            &mut partial_stable_since,
                                            &mut last_partial,
                                            &mut llm_cancel,
                                            &current_latency,
                                            &mut asr_rx,
                                            &mut speaker_gate,
                                            &mut speaker_verify_buffer,
                                            speaker_verify_gate,
                                        )
                                        .await;
                                        continue;
                                    }
                                    if !wake_phase.allows_dialog() {
                                        let sep = if last_final.as_deref().is_some_and(|s| !s.ends_with(['\n', ' '])) { " " } else { "" };
                                        last_final = Some(match last_final.take() {
                                            Some(prev) => format!("{prev}{sep}{text}"),
                                            None => text,
                                        });
                                        asr_final_at = Some(Instant::now());
                                        continue;
                                    }
                                    if !wake_enabled || !orch.barge_in_requires_wake {
                                        if try_barge_in(
                                            "asr-final",
                                            &orch,
                                            &mut state,
                                            &mut vad,
                                            &playback,
                                            &play_gen,
                                            &mut llm_cancel,
                                            &mut active_turn,
                                            &mut partial_stable_since,
                                            &mut last_partial,
                                            &current_latency,
                                            &turn_epoch,
                                            tts.clone(),
                                            Some(text.as_str()),
                                            &mut last_barge_in_at,
                                            &speaker_verifier,
                                            &recent_audio,
                                            &mut speaker_gate,
                                            &mut speaker_verify_buffer,
                                            speaker_verify_gate,
                                            asr.clone(),
                                            &mut wake_phase,
                                            wake_enabled,
                                        )
                                        .await
                                        {
                                            input_gated = false;
                                            // Accumulate rather than replace — user may still be speaking
                                            let sep = if last_final.as_deref().is_some_and(|s| !s.ends_with(['\n', ' '])) { " " } else { "" };
                                            last_final = Some(match last_final.take() {
                                                Some(prev) => format!("{prev}{sep}{text}"),
                                                None => text,
                                            });
                                            asr_final_at = Some(Instant::now());
                                            continue;
                                        }
                                    } else {
                                        let now = Instant::now();
                                        if last_barge_in_suppress_warn
                                            .is_none_or(|t| now.duration_since(t).as_secs() >= 3)
                                        {
                                            warn!(
                                                phrases = ?wake_cfg.effective_phrases(),
                                                "wake word required to barge-in"
                                            );
                                            last_barge_in_suppress_warn = Some(now);
                                        }
                                    }
                                    asr_final_at = Some(Instant::now());
                                    last_asr_event_at = Some(Instant::now());
                                    partial_stable_since = None;
                                    last_partial.clear();

                                    if let Some(ref turn) = active_turn {
                                        if turn.speculative && texts_compatible(&turn.user_text, &text) {
                                            info!("speculative text matches final");
                                            last_final = None;
                                        } else if turn.speculative {
                                            info!("speculative mismatch; restart with final");
                                            if let Some(c) = llm_cancel.take() {
                                                c.cancel();
                                            }
                                            if messages.last().map(|m| m.role.as_str()) == Some("user") {
                                                messages.pop();
                                            }
                                            active_turn = None;
                                            state = SessionState::Listening;
                                            input_gated = false;
                                            last_final = Some(text);
                                            maybe_trigger(
                                                &orch, &mut state, session_start, cold_start,
                                                &mut vad, &mut last_final, &mut asr_final_at,
                                                &mut messages, &llm, tts.clone(), &playback, &play_gen,
                                                &mut llm_cancel, &mut active_turn, &done_tx,
                                                &current_latency,
                                                &turn_epoch,
                                                asr.clone(),
                                                wake_enabled,
                                                &sleep_phrases,
                                                &self.cfg.llm,
                                                hermes_sender_for_spawn.clone(),
                                                &mut wake_phase,
                                                &mut asr_rx,
                                                &mut partial_stable_since,
                                                &mut last_partial,
                                                &mut speaker_gate,
                                                &mut speaker_verify_buffer,
                                                speaker_verify_gate,
                                            ).await;
                                        } else if !wake_enabled || !orch.barge_in_requires_wake {
                                            let prev_user_text = turn.user_text.clone();
                                            info!(prev = %prev_user_text, final_text = %text, "restarting turn with complete final text");
                                            if let Some(c) = llm_cancel.take() {
                                                c.cancel();
                                            }
                                            if messages.last().map(|m| m.role.as_str()) == Some("user") {
                                                messages.pop();
                                            }
                                            active_turn = None;
                                            state = SessionState::Listening;
                                            input_gated = false;
                                            let sep = if last_final.as_deref().is_some_and(|s| !s.ends_with(['\n', ' '])) { " " } else { "" };
                                            let combined = match last_final.take() {
                                                Some(prev) => format!("{prev}{sep}{text}"),
                                                None => {
                                                    let sep2 = if !text.starts_with(['\n', ' ']) && !prev_user_text.ends_with(['\n', ' ']) { " " } else { "" };
                                                    format!("{}{}{}", prev_user_text, sep2, text)
                                                }
                                            };
                                            last_final = Some(combined);
                                            maybe_trigger(
                                                &orch, &mut state, session_start, cold_start,
                                                &mut vad, &mut last_final, &mut asr_final_at,
                                                &mut messages, &llm, tts.clone(), &playback, &play_gen,
                                                &mut llm_cancel, &mut active_turn, &done_tx,
                                                &current_latency,
                                                &turn_epoch,
                                                asr.clone(),
                                                wake_enabled,
                                                &sleep_phrases,
                                                &self.cfg.llm,
                                                hermes_sender_for_spawn.clone(),
                                                &mut wake_phase,
                                                &mut asr_rx,
                                                &mut partial_stable_since,
                                                &mut last_partial,
                                                &mut speaker_gate,
                                                &mut speaker_verify_buffer,
                                                speaker_verify_gate,
                                            ).await;
                                        } else {
                                            // Wake word required to interrupt — save text for next turn
                                            let sep = if last_final.as_deref().is_some_and(|s| !s.ends_with(['\n', ' '])) { " " } else { "" };
                                            last_final = Some(match last_final.take() {
                                                Some(prev) => format!("{prev}{sep}{text}"),
                                                None => text,
                                            });
                                        }
                                    } else {
                                        // Accumulate ASR text regardless of state.
                                        // When Speaking/TTS playing, text is saved so the next
                                        // Listening cycle picks up the full utterance.
                                        let sep = if last_final.as_deref().is_some_and(|s| !s.ends_with(['\n', ' '])) { " " } else { "" };
                                        last_final = Some(match last_final.take() {
                                            Some(prev) => format!("{prev}{sep}{text}"),
                                            None => text,
                                        });
                                    }
                                    if state == SessionState::Listening
                                        && last_asr_event_at.map_or(true, |t| t.elapsed() >= Duration::from_millis(asr_settle_ms))
                                    {
                                        maybe_trigger(
                                            &orch, &mut state, session_start, cold_start,
                                            &mut vad, &mut last_final, &mut asr_final_at,
                                            &mut messages, &llm, tts.clone(), &playback, &play_gen,
                                            &mut llm_cancel, &mut active_turn, &done_tx,
                                            &current_latency, &turn_epoch,
                                            asr.clone(),
                                            wake_enabled,
                                            &sleep_phrases,
                                            &self.cfg.llm,
                                            hermes_sender_for_spawn.clone(),
                                            &mut wake_phase,
                                            &mut asr_rx,
                                            &mut partial_stable_since,
                                            &mut last_partial,
                                            &mut speaker_gate,
                                            &mut speaker_verify_buffer,
                                            speaker_verify_gate,
                                        ).await;
                                    }
                                }
                                AsrEvent::TaskFailed { message } => {
                        if wake_phase.allows_asr() && state == SessionState::Listening {
                                        warn!(%message, "asr failed");
                                    }
                                }
                                _ => {}
                            }
                        }
                    }
                    done = done_rx.recv() => {
                        if let Some((text, epoch, shutup_requested)) = done {
                            if !text.trim().is_empty() {
                                messages.push(ChatMessage {
                                    role: "assistant".to_string(),
                                    content: text,
                                    tool_calls: None,
                                    tool_call_id: None,
                                });
                                if orch.max_context_messages > 0 && messages.len() > orch.max_context_messages {
                                    let excess = messages.len() - orch.max_context_messages;
                                    messages.drain(..excess);
                                }
                            }
                            if epoch == turn_epoch.load(Ordering::SeqCst) {
                                state = SessionState::Listening;
                                active_turn = None;
                                last_final = None;
                                asr_final_at = None;
                                partial_stable_since = None;
                                last_partial.clear();
                                last_asr_event_at = None;
                                input_gated = false;
                                speaker_gate = SpeakerGate::Idle;
                                if !speaker_verify_gate {
                                    speaker_gate = SpeakerGate::Passed;
                                }
                                speaker_verify_buffer.clear();
                                *current_latency.lock().unwrap() = None;
                                if shutup_requested && wake_enabled {
                                    let _ = asr.set_gate(true).await;
                                    let _ = asr.pause().await;
                                    // drain pending ASR events
                                    while asr_rx.try_recv().is_ok() {}
                                    wake_phase = WakePhase::Dormant;
                                    info!("shutup requested -> dormant; say wake word to resume");
                                } else if wake_enabled {
                                    let _ = asr.set_gate(true).await;
                                    wake_phase = WakePhase::IdleAfterTurn {
                                        deadline: Instant::now() + idle_after_turn,
                                    };
                                    info!(
                                        idle_sec = wake_cfg.idle_after_turn_sec,
                                        "back to listening; idle timeout started"
                                    );
                                } else {
                                    wake_phase = WakePhase::Active;
                                    info!("back to listening");
                                }
                                // Process any pending hermes messages
                                if let Some(msg) = pending_hermes_msgs.pop_front() {
                                    info!(
                                        request_id = %msg.request_id,
                                        text = %msg.text,
                                        "hermes: processing pending message"
                                    );
                                    let was_dormant = false; // deferred from a busy state → never dormant
                                    handle_hermes_result(
                                        was_dormant,
                                        msg,
                                        &mut messages,
                                        &llm,
                                        tts.clone(),
                                        &playback,
                                        &play_gen,
                                        &turn_epoch,
                                        &done_tx,
                                        &mut state,
                                        &mut active_turn,
                                        &mut llm_cancel,
                                        &orch,
                                        &self.cfg.llm,
                                    )
                                    .await;
                                }
                            }
                        }
                    }
                    msg = hermes_msg_rx.recv() => {
                        if let Some(msg) = msg {
                            info!(
                                request_id = %msg.request_id,
                                status = %msg.status,
                                text = %msg.text,
                                "hermes: message received"
                            );
                            if state == SessionState::Listening && active_turn.is_none() {
                                let was_dormant = matches!(wake_phase, WakePhase::Dormant);
                                handle_hermes_result(
                                    was_dormant,
                                    msg,
                                    &mut messages,
                                    &llm,
                                    tts.clone(),
                                    &playback,
                                    &play_gen,
                                    &turn_epoch,
                                    &done_tx,
                                    &mut state,
                                    &mut active_turn,
                                    &mut llm_cancel,
                                    &orch,
                                    &self.cfg.llm,
                                )
                                .await;
                            } else {
                                info!(request_id = %msg.request_id, text = %msg.text, "hermes: deferring message (busy)");
                                pending_hermes_msgs.push_back(msg);
                            }
                        }
                    }
                    _ = wake_ack_done_rx.recv() => {
                        wake_ack_playing.store(false, Ordering::SeqCst);
                        if resume_asr_with_retry(asr.clone()).await {
                            let _ = asr.set_gate(false).await;
                            let _ = asr.reconnect().await;
                            input_gated = false;
                            utterance_active = false;
                            last_final = None;
                            asr_final_at = None;
                            partial_stable_since = None;
                            last_partial.clear();
                            vad.reset_barge_in_state();
                            pending_offline_flush = None;
                            if matches!(wake_phase, WakePhase::Active) {
                                wake_phase = WakePhase::AwakeGrace {
                                    deadline: Instant::now() + grace_after_wake,
                                };
                            }
                            info!("wake ack done; ASR listening for user speech");
                        } else {
                            warn!("wake ack done but ASR resume failed; returning to dormant");
                            enter_dormant(
                                asr.clone(),
                                &mut wake_phase,
                                &mut state,
                                &mut active_turn,
                                &mut last_final,
                                &mut asr_final_at,
                                &mut partial_stable_since,
                                &mut last_partial,
                                &mut llm_cancel,
                                &current_latency,
                                &mut asr_rx,
                                &mut speaker_gate,
                                &mut speaker_verify_buffer,
                                speaker_verify_gate,
                            )
                            .await;
                        }
                    }
                    _ = tokio::time::sleep(Duration::from_millis(100)) => {
                        if wake_phase.check_timeout(Instant::now()) {
                            enter_dormant(
                                asr.clone(),
                                &mut wake_phase,
                                &mut state,
                                &mut active_turn,
                                &mut last_final,
                                &mut asr_final_at,
                                &mut partial_stable_since,
                                &mut last_partial,
                                &mut llm_cancel,
                                &current_latency,
                                &mut asr_rx,
                                &mut speaker_gate,
                                &mut speaker_verify_buffer,
                                speaker_verify_gate,
                            )
                            .await;
                            continue;
                        }
                    }
                }
        }

        Ok(())
    }
}

fn user_speech_activity(
    vad: &mut VadEngine,
    text: Option<&str>,
    min_chars: usize,
    wake_phase: &WakePhase,
    grace_min_chars: usize,
) -> bool {
    if vad.speech_start() || vad.in_speech() {
        return true;
    }
    let in_grace = matches!(
        *wake_phase,
        WakePhase::AwakeGrace { .. } | WakePhase::IdleAfterTurn { .. }
    );
    let min = if in_grace { grace_min_chars } else { min_chars };
    text.is_some_and(|t| t.trim().chars().count() >= min)
}

fn update_pending_offline_flush(
    utterance_active: bool,
    input_gated: bool,
    vad: &VadEngine,
    speech_just_started: bool,
    endpoint_silence_ms: u32,
    continuation_ms: u32,
    pending: &mut Option<Instant>,
) -> bool {
    if !utterance_active || input_gated {
        *pending = None;
        return false;
    }
    if vad.in_speech() || speech_just_started {
        *pending = None;
        return false;
    }
    if vad.trailing_silence_ms() >= endpoint_silence_ms {
        pending.get_or_insert_with(|| {
            debug!(
                continuation_ms,
                trailing_silence_ms = vad.trailing_silence_ms(),
                "offline ASR: endpoint pause, waiting for user to resume utterance"
            );
            Instant::now() + Duration::from_millis(continuation_ms as u64)
        });
    } else {
        *pending = None;
    }
    pending.is_some_and(|deadline| Instant::now() >= deadline)
}

fn tool_calls_from_stream_map(map: &HashMap<u32, AccumulatedToolCall>) -> Vec<ToolCall> {
    let mut indices: Vec<u32> = map.keys().copied().collect();
    indices.sort();
    indices
        .into_iter()
        .filter_map(|idx| {
            map.get(&idx).map(|acc| ToolCall {
                id: if acc.id.is_empty() {
                    format!("call_{idx}")
                } else {
                    acc.id.clone()
                },
                r#type: "function".to_string(),
                function: crate::llm::ToolCallFunction {
                    name: acc.name.clone(),
                    arguments: acc.arguments.clone(),
                },
            })
        })
        .collect()
}

fn core_tool_call_to_talk(tc: hermes_core::ToolCall) -> ToolCall {
    ToolCall {
        id: tc.id,
        r#type: "function".to_string(),
        function: crate::llm::ToolCallFunction {
            name: tc.function.name,
            arguments: tc.function.arguments,
        },
    }
}

fn ready_to_flush_asr(
    asr_offline: bool,
    input_gated: bool,
    vad: &VadEngine,
    endpoint_silence_ms: u32,
    utterance_active: bool,
    last_final: &Option<String>,
    min_final_chars: usize,
) -> bool {
    if input_gated || vad.trailing_silence_ms() < endpoint_silence_ms {
        return false;
    }
    if asr_offline {
        utterance_active
    } else {
        last_final
            .as_ref()
            .is_some_and(|t| t.trim().chars().count() >= min_final_chars)
    }
}

fn promote_wake_on_speech(wake: &mut WakePhase) -> bool {
    match wake {
        WakePhase::AwakeGrace { .. } => {
            info!("wake grace -> active (user speech)");
            *wake = WakePhase::Active;
            true
        }
        WakePhase::IdleAfterTurn { .. } => {
            info!("idle after turn -> active (user speech)");
            *wake = WakePhase::Active;
            true
        }
        _ => false,
    }
}

#[allow(clippy::too_many_arguments)]
async fn apply_sleep_keyword(
    text: &str,
    wake_enabled: bool,
    asr: Arc<dyn AsrEngine>,
    tts: Arc<dyn TtsEngine>,
    playback: &Arc<AudioPlayback>,
    play_gen: &Arc<AtomicU64>,
    turn_epoch: &Arc<AtomicU64>,
    llm_cancel: &mut Option<CancellationToken>,
    wake_phase: &mut WakePhase,
    state: &mut SessionState,
    active_turn: &mut Option<ActiveTurn>,
    last_final: &mut Option<String>,
    asr_final_at: &mut Option<Instant>,
    partial_stable_since: &mut Option<Instant>,
    last_partial: &mut String,
    current_latency: &Arc<std::sync::Mutex<Option<Arc<TurnLatency>>>>,
    asr_rx: &mut mpsc::Receiver<AsrEvent>,
    speaker_gate: &mut SpeakerGate,
    speaker_verify_buffer: &mut Vec<f32>,
    speaker_verify_gate: bool,
) {
    info!(phrase = %text.trim(), "sleep keyword matched; skipping LLM");
    turn_epoch.fetch_add(1, Ordering::SeqCst);
    playback.stop_clear();
    play_gen.store(playback.current_generation(), Ordering::SeqCst);
    if let Some(c) = llm_cancel.take() {
        c.cancel();
    }
    let tts_int = tts.clone();
    tokio::spawn(async move {
        if let Err(e) = tts_int.interrupt_turn().await {
            warn!(error = %e, "tts interrupt on sleep keyword failed");
        }
    });
    *active_turn = None;
    if wake_enabled {
        enter_dormant(
            asr,
            wake_phase,
            state,
            active_turn,
            last_final,
            asr_final_at,
            partial_stable_since,
            last_partial,
            llm_cancel,
            current_latency,
            asr_rx,
            speaker_gate,
            speaker_verify_buffer,
            speaker_verify_gate,
        )
        .await;
    } else {
        *state = SessionState::Listening;
        *last_final = None;
        *asr_final_at = None;
        *partial_stable_since = None;
        last_partial.clear();
        *speaker_gate = SpeakerGate::Idle;
        if !speaker_verify_gate {
            *speaker_gate = SpeakerGate::Passed;
        }
        speaker_verify_buffer.clear();
        *current_latency.lock().unwrap() = None;
        info!("sleep keyword matched (wake disabled); back to listening");
    }
}

#[allow(clippy::too_many_arguments)]
async fn enter_dormant(
    asr: Arc<dyn AsrEngine>,
    wake_phase: &mut WakePhase,
    state: &mut SessionState,
    active_turn: &mut Option<ActiveTurn>,
    last_final: &mut Option<String>,
    asr_final_at: &mut Option<Instant>,
    partial_stable_since: &mut Option<Instant>,
    last_partial: &mut String,
    llm_cancel: &mut Option<CancellationToken>,
    current_latency: &Arc<std::sync::Mutex<Option<Arc<TurnLatency>>>>,
    asr_rx: &mut mpsc::Receiver<AsrEvent>,
    speaker_gate: &mut SpeakerGate,
    speaker_verify_buffer: &mut Vec<f32>,
    speaker_verify_gate: bool,
) {
    if let Some(c) = llm_cancel.take() {
        c.cancel();
    }
    let _ = asr.pause().await;
    let mut drained = 0usize;
    while asr_rx.try_recv().is_ok() {
        drained += 1;
    }
    *wake_phase = WakePhase::Dormant;
    *state = SessionState::Listening;
    *active_turn = None;
    *last_final = None;
    *asr_final_at = None;
    *partial_stable_since = None;
    last_partial.clear();
    *speaker_gate = SpeakerGate::Idle;
    if !speaker_verify_gate {
        *speaker_gate = SpeakerGate::Passed;
    }
    speaker_verify_buffer.clear();
    *current_latency.lock().unwrap() = None;
    info!(
        drained_asr_events = drained,
        "enter dormant; say wake word to resume"
    );
}

async fn resume_asr_with_retry(asr: Arc<dyn AsrEngine>) -> bool {
    let mut retries = 3u32;
    loop {
        match asr.resume().await {
            Ok(()) => return true,
            Err(e) => {
                retries = retries.saturating_sub(1);
                if retries == 0 {
                    error!(error = %e, "asr resume failed, giving up");
                    return false;
                }
                warn!(error = %e, remaining = retries, "asr resume failed, retrying");
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

async fn play_wake_ack(
    text: &str,
    tts: Arc<dyn TtsEngine>,
    playback: &Arc<AudioPlayback>,
    play_gen: &Arc<AtomicU64>,
) {
    let g = playback.bump_generation();
    playback.resume_playback();
    play_gen.store(g, Ordering::SeqCst);
    info!(reply = %text, "wake ack");
    if let Err(e) = tts.append_text(&normalize_tts_text(text)).await {
        warn!(error = %e, "wake ack tts append failed");
        return;
    }
    if let Err(e) = tts.finish_turn().await {
        warn!(error = %e, "wake ack tts finish failed");
    }
    playback.wait_drain(Duration::from_secs(15)).await;
}

fn is_output_busy(
    state: SessionState,
    playback: &AudioPlayback,
    active_turn: &Option<ActiveTurn>,
) -> bool {
    matches!(state, SessionState::Thinking | SessionState::Speaking)
        || active_turn.is_some()
        || playback.buffered_samples() > playback.sample_rate() as usize / 10
}

async fn do_barge_in(
    turn_epoch: &Arc<AtomicU64>,
    playback: &Arc<AudioPlayback>,
    play_gen: &Arc<AtomicU64>,
    llm_cancel: &mut Option<CancellationToken>,
    vad: &mut VadEngine,
    tts: Arc<dyn TtsEngine>,
    asr: Arc<dyn AsrEngine>,
    wake_enabled: bool,
    wake_phase: &mut WakePhase,
    state: &mut SessionState,
    active_turn: &mut Option<ActiveTurn>,
    current_latency: &Arc<std::sync::Mutex<Option<Arc<TurnLatency>>>>,
    last_partial: &mut String,
    partial_stable_since: &mut Option<Instant>,
    last_barge_in_at: &mut Option<Instant>,
    speaker_gate: &mut SpeakerGate,
    speaker_verify_buffer: &mut Vec<f32>,
    speaker_verify_gate: bool,
) {
    *last_barge_in_at = Some(Instant::now());
    turn_epoch.fetch_add(1, Ordering::SeqCst);
    playback.stop_clear();
    play_gen.store(playback.current_generation(), Ordering::SeqCst);
    if let Some(c) = llm_cancel.take() {
        c.cancel();
    }
    let tts_int = tts.clone();
    tokio::spawn(async move {
        if let Err(e) = tts_int.interrupt_turn().await {
            warn!(error = %e, "tts interrupt on barge-in failed");
        }
    });
    vad.reset_barge_in_state();
    *wake_phase = WakePhase::Active;
    *state = SessionState::Listening;
    *active_turn = None;
    *current_latency.lock().unwrap() = None;
    last_partial.clear();
    *partial_stable_since = None;
    *speaker_gate = SpeakerGate::Idle;
    if !speaker_verify_gate {
        *speaker_gate = SpeakerGate::Passed;
    }
    speaker_verify_buffer.clear();
    if wake_enabled {
        let _ = asr.set_gate(true).await;
    }
}

fn asr_indicates_barge_in(text: &str, active_turn: &Option<ActiveTurn>, min_chars: usize) -> bool {
    let t = text.trim();
    if t.chars().count() < min_chars {
        return false;
    }
    match active_turn {
        None => true,
        Some(turn) => !texts_compatible(&turn.user_text, t),
    }
}

#[allow(clippy::too_many_arguments)]
async fn try_barge_in(
    reason: &str,
    orch: &OrchestratorConfig,
    state: &mut SessionState,
    vad: &mut VadEngine,
    playback: &Arc<AudioPlayback>,
    play_gen: &Arc<AtomicU64>,
    llm_cancel: &mut Option<CancellationToken>,
    active_turn: &mut Option<ActiveTurn>,
    partial_stable_since: &mut Option<Instant>,
    last_partial: &mut String,
    current_latency: &Arc<std::sync::Mutex<Option<Arc<TurnLatency>>>>,
    turn_epoch: &Arc<AtomicU64>,
    tts: Arc<dyn TtsEngine>,
    asr_text: Option<&str>,
    last_barge_in_at: &mut Option<Instant>,
    speaker_verifier: &Option<SpeakerVerifier>,
    recent_audio: &VecDeque<f32>,
    speaker_gate: &mut SpeakerGate,
    speaker_verify_buffer: &mut Vec<f32>,
    speaker_verify_gate: bool,
    asr: Arc<dyn AsrEngine>,
    wake_phase: &mut WakePhase,
    wake_enabled: bool,
) -> bool {
    if !orch.barge_in_enabled || !is_output_busy(*state, playback, active_turn) {
        return false;
    }

    let vad_hit = vad.speech_start()
        || vad.user_speaking_during_playback()
        || (asr_text.is_none() && vad.in_speech());
    let asr_hit = asr_text
        .map(|t| asr_indicates_barge_in(t, active_turn, orch.min_final_chars))
        .unwrap_or(false);

    if !vad_hit && !asr_hit {
        return false;
    }

    if orch.min_rms_barge_in > 0.0 && vad.last_rms() < orch.min_rms_barge_in {
        return false;
    }

    if let Some(last) = *last_barge_in_at {
        if last.elapsed().as_millis() < orch.barge_in_cooldown_ms as u128 {
            return false;
        }
    }

    if let Some(sv) = speaker_verifier {
        if sv.has_voiceprint() {
            let sample_rate = 16000u32;
            let audio: Vec<f32> = recent_audio.iter().copied().collect();
            if !audio.is_empty() && !sv.verify(&audio, sample_rate) {
                return false;
            }
        }
    }

    info!(reason, vad_hit, asr_hit, "barge-in");
    do_barge_in(
        turn_epoch,
        playback,
        play_gen,
        llm_cancel,
        vad,
        tts,
        asr,
        wake_enabled,
        wake_phase,
        state,
        active_turn,
        current_latency,
        last_partial,
        partial_stable_since,
        last_barge_in_at,
        speaker_gate,
        speaker_verify_buffer,
        speaker_verify_gate,
    )
    .await;
    true
}

async fn maybe_trigger(
    orch: &OrchestratorConfig,
    state: &mut SessionState,
    session_start: Instant,
    cold_start: Duration,
    vad: &mut VadEngine,
    last_final: &mut Option<String>,
    asr_final_at: &mut Option<Instant>,
    messages: &mut Vec<ChatMessage>,
    llm: &Arc<dyn LlmClient>,
    tts: Arc<dyn TtsEngine>,
    playback: &Arc<AudioPlayback>,
    play_gen: &Arc<AtomicU64>,
    llm_cancel: &mut Option<CancellationToken>,
    active_turn: &mut Option<ActiveTurn>,
    done_tx: &mpsc::Sender<(String, u64, bool)>,
    current_latency: &Arc<std::sync::Mutex<Option<Arc<TurnLatency>>>>,
    turn_epoch: &Arc<AtomicU64>,
    asr: Arc<dyn AsrEngine>,
    wake_enabled: bool,
    sleep_phrases: &[String],
    llm_cfg: &LlmConfig,
    hermes_sender: Option<HermesQueueSender>,
    wake_phase: &mut WakePhase,
    asr_rx: &mut mpsc::Receiver<AsrEvent>,
    partial_stable_since: &mut Option<Instant>,
    last_partial: &mut String,
    speaker_gate: &mut SpeakerGate,
    speaker_verify_buffer: &mut Vec<f32>,
    speaker_verify_gate: bool,
) {
    if *state != SessionState::Listening || active_turn.is_some() {
        return;
    }
    if is_output_busy(*state, playback, active_turn) {
        return;
    }
    if session_start.elapsed() < cold_start {
        return;
    }
    let endpoint_silence = orch.endpoint_silence_ms();
    if vad.trailing_silence_ms() < endpoint_silence {
        return;
    }
    if last_final
        .as_ref()
        .map_or(true, |t| t.trim().chars().count() < orch.min_final_chars)
    {
        return;
    }
    let text = last_final.take().unwrap();
    let trimmed = text.trim();

    if matches_sleep_keyword(trimmed, sleep_phrases) {
        apply_sleep_keyword(
            trimmed,
            wake_enabled,
            asr.clone(),
            tts.clone(),
            playback,
            play_gen,
            turn_epoch,
            llm_cancel,
            wake_phase,
            state,
            active_turn,
            last_final,
            asr_final_at,
            partial_stable_since,
            last_partial,
            current_latency,
            asr_rx,
            speaker_gate,
            speaker_verify_buffer,
            speaker_verify_gate,
        )
        .await;
        return;
    }

    info!(
        trigger_text = %trimmed,
        chars = trimmed.chars().count(),
        state = ?state,
        "maybe_trigger: triggering LLM"
    );

    let final_at = asr_final_at.take();
    start_reply_turn(
        text,
        final_at,
        true,
        false,
        orch,
        state,
        messages,
        llm,
        tts,
        playback,
        play_gen,
        llm_cancel,
        active_turn,
        done_tx,
        current_latency,
        turn_epoch,
        asr,
        wake_enabled,
        sleep_phrases,
        llm_cfg,
        hermes_sender,
        wake_phase,
        asr_rx,
        last_final,
        asr_final_at,
        partial_stable_since,
        last_partial,
        speaker_gate,
        speaker_verify_buffer,
        speaker_verify_gate,
    )
    .await;
}

#[allow(clippy::too_many_arguments)]
async fn start_reply_turn(
    text: String,
    asr_final_at: Option<Instant>,
    log_asr_to_trigger: bool,
    speculative: bool,
    orch: &OrchestratorConfig,
    state: &mut SessionState,
    messages: &mut Vec<ChatMessage>,
    llm: &Arc<dyn LlmClient>,
    tts: Arc<dyn TtsEngine>,
    playback: &Arc<AudioPlayback>,
    play_gen: &Arc<AtomicU64>,
    llm_cancel: &mut Option<CancellationToken>,
    active_turn: &mut Option<ActiveTurn>,
    done_tx: &mpsc::Sender<(String, u64, bool)>,
    current_latency: &Arc<std::sync::Mutex<Option<Arc<TurnLatency>>>>,
    turn_epoch: &Arc<AtomicU64>,
    asr: Arc<dyn AsrEngine>,
    wake_enabled: bool,
    sleep_phrases: &[String],
    llm_cfg: &LlmConfig,
    hermes_sender: Option<HermesQueueSender>,
    wake_phase: &mut WakePhase,
    asr_rx: &mut mpsc::Receiver<AsrEvent>,
    last_final: &mut Option<String>,
    asr_final_at_slot: &mut Option<Instant>,
    partial_stable_since: &mut Option<Instant>,
    last_partial: &mut String,
    speaker_gate: &mut SpeakerGate,
    speaker_verify_buffer: &mut Vec<f32>,
    speaker_verify_gate: bool,
) {
    if *state != SessionState::Listening && !speculative {
        return;
    }
    if active_turn.is_some() && !speculative {
        return;
    }
    if !speculative && is_output_busy(*state, playback, active_turn) {
        return;
    }

    if matches_sleep_keyword(text.trim(), sleep_phrases) {
        apply_sleep_keyword(
            text.trim(),
            wake_enabled,
            asr.clone(),
            tts.clone(),
            playback,
            play_gen,
            turn_epoch,
            llm_cancel,
            wake_phase,
            state,
            active_turn,
            last_final,
            asr_final_at_slot,
            partial_stable_since,
            last_partial,
            current_latency,
            asr_rx,
            speaker_gate,
            speaker_verify_buffer,
            speaker_verify_gate,
        )
        .await;
        return;
    }

    let epoch_at_start = turn_epoch.load(Ordering::SeqCst);
    let trigger_at = Instant::now();
    if let Some(final_at) = asr_final_at {
        if log_asr_to_trigger {
            info!(
                asr_final_to_trigger_ms = trigger_at.duration_since(final_at).as_millis(),
                "latency: asr final -> trigger"
            );
        }
    }

    info!(
        user = %text,
        speculative,
        chars = text.chars().count(),
        state = ?state,
        "sending user message to LLM"
    );
    messages.push(ChatMessage {
        role: "user".to_string(),
        content: text.clone(),
        tool_calls: None,
        tool_call_id: None,
    });

    *active_turn = Some(ActiveTurn {
        user_text: text,
        speculative,
    });

    *state = SessionState::Thinking;
    let cancel = CancellationToken::new();
    *llm_cancel = Some(cancel.clone());

    let g = playback.bump_generation();
    playback.resume_playback();
    play_gen.store(g, Ordering::SeqCst);

    let latency = Arc::new(TurnLatency {
        asr_final_at,
        trigger_at,
        logged_first_pcm: Arc::new(AtomicBool::new(false)),
    });
    *current_latency.lock().unwrap() = Some(latency.clone());

    let llm = llm.clone();
    let tts = tts.clone();
    let msgs = messages.clone();
    let sentence_min = orch.sentence_min_len;
    let tts_first_chunk = orch.tts_first_chunk_chars;
    let done_tx = done_tx.clone();
    let playback_wait = playback.clone();
    let tools_enabled = llm_cfg.tools_enabled;
    let llm_cfg = llm_cfg.clone();
    let hermes_sender = hermes_sender.clone();

    tokio::spawn(async move {
        let mut msgs_local = msgs;
        let mut assistant_buf = String::new();
        let mut with_tools = tools_enabled;
        let mut should_go_dormant = false;
        let max_rounds: u32 = if tools_enabled { 2 } else { 1 };

        let tool_defs = if tools_enabled {
            Some(tools::get_tool_definitions())
        } else {
            None
        };

        for round in 0..max_rounds {
            let tools = if with_tools && round == 0 {
                tool_defs.as_deref()
            } else {
                None
            };
            with_tools = false;

            let stream_started = Instant::now();
            let mut stream = match llm.stream_chat(&msgs_local, tools, cancel.clone()).await {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "llm failed");
                    let _ = done_tx.send((String::new(), epoch_at_start, false)).await;
                    return;
                }
            };
            if round == 0 && !speculative {
                info!(
                    trigger_to_llm_stream_ms =
                        stream_started.duration_since(trigger_at).as_millis(),
                    "latency: trigger -> llm stream ready"
                );
            }

            let mut first_token = true;
            let mut sent_early = false;
            let mut buf = String::new();
            let mut tool_call_map: HashMap<u32, AccumulatedToolCall> = HashMap::new();

            while let Some(item) = stream.next().await {
                if cancel.is_cancelled() {
                    break;
                }
                let Ok(stream_item) = item else { continue };
                if first_token {
                    if round == 0 && !speculative {
                        info!(
                            trigger_to_llm_first_token_ms = trigger_at.elapsed().as_millis(),
                            "latency: trigger -> llm first token"
                        );
                    }
                    first_token = false;
                }

                if let Some(ref reasoning) = stream_item.reasoning_content {
                    eprint!("{}", reasoning);
                }

                // Accumulate tool_call deltas (always, even without tools)
                let has_tools = tools.is_some();
                for tc_delta in &stream_item.tool_calls {
                    let entry = tool_call_map.entry(tc_delta.index).or_insert_with(|| {
                        AccumulatedToolCall {
                            index: tc_delta.index,
                            id: String::new(),
                            name: String::new(),
                            arguments: String::new(),
                        }
                    });
                    if let Some(ref id) = tc_delta.id {
                        if !id.is_empty() {
                            entry.id = id.clone();
                        }
                    }
                    if let Some(ref name) = tc_delta.function_name {
                        entry.name.push_str(name);
                    }
                    if let Some(ref args) = tc_delta.function_arguments {
                        entry.arguments.push_str(args);
                    }
                }

                if let Some(ref token) = stream_item.content {
                    buf.push_str(token);
                    assistant_buf.push_str(token);

                    // Only stream TTS when tools are NOT enabled (round=1 or tools disabled)
                    if !has_tools {
                        if !sent_early && round == 0 {
                            if let Some(chunk) = take_early_chunk(&mut buf, tts_first_chunk) {
                                info!(
                                    ms = trigger_at.elapsed().as_millis(),
                                    %chunk,
                                    "tts early chunk"
                                );
                                if let Err(e) = tts.append_text(&normalize_tts_text(&chunk)).await {
                                    warn!(error = %e, "tts early append");
                                }
                                sent_early = true;
                            }
                        }

                        while let Some(sentence) = take_sentence(&mut buf, sentence_min) {
                            info!(%sentence, "tts sentence");
                            if let Err(e) = tts.append_text(&normalize_tts_text(&sentence)).await {
                                warn!(error = %e, "tts append");
                            }
                        }
                    }
                }
            }

            let mut tool_calls = tool_calls_from_stream_map(&tool_call_map);
            let mut speakable_buf = buf.clone();

            if tool_calls.is_empty() {
                let (plain, inline) = hermes_core::separate_text_and_calls(&buf);
                speakable_buf = plain;
                if !inline.is_empty() {
                    info!(
                        count = inline.len(),
                        "parsed inline tool_calls from assistant content"
                    );
                    tool_calls.extend(inline.into_iter().map(core_tool_call_to_talk));
                }
            }

            if tool_calls.is_empty() {
                if tools_enabled && round == 0 {
                    warn!(
                        chars = speakable_buf.chars().count(),
                        "tools enabled but no tool_calls parsed; suppressing assistant TTS"
                    );
                } else if let Some(rest) = flush_remainder(&mut speakable_buf) {
                    let _ = tts.append_text(&normalize_tts_text(&rest)).await;
                }
                if let Err(e) = tts.finish_turn().await {
                    warn!(error = %e, "tts finish");
                }
                playback_wait.wait_drain(Duration::from_secs(30)).await;
                let _ = done_tx
                    .send((assistant_buf, epoch_at_start, should_go_dormant))
                    .await;
                return;
            }

            // --- Tool call handling ---
            // Discard content buffer (tool call scaffolding / reasoning, not for TTS)
            buf.clear();

            let mut spoken_list: Vec<String> = Vec::new();

            for tc in &tool_calls {
                let mut has_spoken = false;
                if let Some(spoken) = tools::extract_spoken(&tc.function.arguments) {
                    spoken_list.push(spoken);
                    has_spoken = true;
                }
                if !has_spoken && tc.function.name == "call_hermes" {
                    if let Some(spoken) = tools::generate_hermes_spoken(&tc.function.arguments) {
                        spoken_list.push(spoken);
                    }
                }
            }

            // TTS spoken notifications — finish current task so audio plays
            // during tool execution, not deferred until after the tool result.
            if !spoken_list.is_empty() {
                for spoken in &spoken_list {
                    info!(%spoken, "tool: spoken notification");
                    if let Err(e) = tts.append_text(&normalize_tts_text(spoken)).await {
                        warn!(error = %e, "tts spoken append");
                    }
                }
                if let Err(e) = tts.finish_turn().await {
                    warn!(error = %e, "tts finish after spoken");
                }
            }

            // Push assistant message with tool_calls
            msgs_local.push(ChatMessage {
                role: "assistant".to_string(),
                content: String::new(),
                tool_calls: Some(tool_calls.clone()),
                tool_call_id: None,
            });

            // Execute tools and push results
            info!(
                count = tool_calls.len(),
                suppressed_chars = buf.len(),
                "llm returned tool_calls"
            );
            for tc in &tool_calls {
                info!(tool = %tc.function.name, args = %tc.function.arguments, "tool: calling");
                eprintln!(
                    "\n═══ LLM tool: {} ═══\n{}",
                    tc.function.name, tc.function.arguments
                );
                let result = match tools::execute_tool(
                    &tc.function.name,
                    &tc.function.arguments,
                    &llm_cfg,
                    hermes_sender.as_ref(),
                )
                .await
                {
                    Ok(r) => r,
                    Err(e) => format!("error: {e}"),
                };
                info!(tool = %tc.function.name, result_len = result.len(), "tool: result");
                eprintln!("═══ tool result: {} ═══\n{}", tc.function.name, result);
                if tc.function.name == "shutup" {
                    should_go_dormant = true;
                }
                msgs_local.push(ChatMessage {
                    role: "tool".to_string(),
                    content: result,
                    tool_calls: None,
                    tool_call_id: Some(tc.id.clone()),
                });
            }
            if should_go_dormant {
                break;
            }
        }

        // Should not reach here (max_rounds exhausted)
        if let Err(e) = tts.finish_turn().await {
            warn!(error = %e, "tts finish");
        }
        playback_wait.wait_drain(Duration::from_secs(30)).await;
        let _ = done_tx
            .send((assistant_buf, epoch_at_start, should_go_dormant))
            .await;
    });

    *state = SessionState::Speaking;
    if wake_enabled {
        let _ = asr.set_gate(false).await;
    }
}

async fn handle_hermes_result(
    was_dormant: bool,
    msg: HermesMessage,
    messages: &mut Vec<ChatMessage>,
    llm: &Arc<dyn LlmClient>,
    tts: Arc<dyn TtsEngine>,
    playback: &Arc<AudioPlayback>,
    play_gen: &Arc<AtomicU64>,
    turn_epoch: &Arc<AtomicU64>,
    done_tx: &mpsc::Sender<(String, u64, bool)>,
    state: &mut SessionState,
    active_turn: &mut Option<ActiveTurn>,
    llm_cancel: &mut Option<CancellationToken>,
    orch: &OrchestratorConfig,
    _llm_cfg: &LlmConfig,
) {
    eprintln!(
        "\n══════════ hermes 返回 ══════════\n{}\n══════════════════════════",
        msg.text
    );

    if msg.status != "final" && msg.status != "error" && msg.status != "ok" {
        info!(
            request_id = %msg.request_id,
            status = %msg.status,
            "hermes: skipping non-final message"
        );
        return;
    }

    messages.push(ChatMessage {
        role: "tool".to_string(),
        content: msg.text.clone(),
        tool_calls: None,
        tool_call_id: Some(msg.request_id.clone()),
    });
    messages.push(ChatMessage {
        role: "system".to_string(),
        content: format!(
            "hermes 返回了查询结果（request_id={}），请用自然口语向用户播报这个结果",
            msg.request_id
        ),
        tool_calls: None,
        tool_call_id: None,
    });

    *state = SessionState::Thinking;
    *active_turn = Some(ActiveTurn {
        user_text: String::new(),
        speculative: false,
    });

    let g = playback.bump_generation();
    playback.resume_playback();
    play_gen.store(g, Ordering::SeqCst);

    let cancel = CancellationToken::new();
    *llm_cancel = Some(cancel.clone());

    let msgs = messages.clone();
    let llm = llm.clone();
    let tts = tts.clone();
    let playback = playback.clone();
    let turn_epoch = turn_epoch.clone();
    let done_tx = done_tx.clone();
    let sentence_min = orch.sentence_min_len;
    let tts_first_chunk = orch.tts_first_chunk_chars;
    let epoch_at_start = turn_epoch.load(std::sync::atomic::Ordering::SeqCst);
    let go_dormant = was_dormant;

    tokio::spawn(async move {
        let mut assistant_buf = String::new();

        let mut stream = match llm.stream_chat(&msgs, None, cancel.clone()).await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "hermes replay llm failed");
                let _ = done_tx
                    .send((String::new(), epoch_at_start, go_dormant))
                    .await;
                return;
            }
        };

        let mut buf = String::new();
        let mut sent_early = false;
        use futures_util::StreamExt;
        while let Some(item) = stream.next().await {
            if cancel.is_cancelled() {
                break;
            }
            let Ok(stream_item) = item else { continue };
            if let Some(ref token) = stream_item.content {
                buf.push_str(token);
                assistant_buf.push_str(token);
                if !sent_early {
                    if let Some(chunk) = take_early_chunk(&mut buf, tts_first_chunk) {
                        let _ = tts.append_text(&normalize_tts_text(&chunk)).await;
                        sent_early = true;
                    }
                }
                while let Some(sentence) = take_sentence(&mut buf, sentence_min) {
                    let _ = tts.append_text(&normalize_tts_text(&sentence)).await;
                }
            }
        }

        if let Some(rest) = flush_remainder(&mut buf) {
            let _ = tts.append_text(&normalize_tts_text(&rest)).await;
        }
        if let Err(e) = tts.finish_turn().await {
            warn!(error = %e, "tts finish");
        }
        playback.wait_drain(Duration::from_secs(30)).await;
        let _ = done_tx
            .send((assistant_buf, epoch_at_start, go_dormant))
            .await;
    });

    *state = SessionState::Speaking;
}

fn f32_slice_to_i16_bytes(samples: &[f32]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|&s| {
            let v = (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
            v.to_le_bytes()
        })
        .collect()
}

fn rms_f32(samples: &[f32]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    ((sum_sq / samples.len() as f64).sqrt()) as f32
}
