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
#[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
use crate::orchestrator::StreamingThinkTtsGate;
#[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
use crate::orchestrator::matches_sleep_keyword;
use crate::orchestrator::{
    IncrementalThinkStripper, SessionState, WakePhase, extract_inline_thinking, flush_remainder,
    normalize_asr_transcript, normalize_tts_text, strip_think_blocks, take_early_chunk,
    take_sentence, texts_compatible,
};
#[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
use crate::orchestrator::{
    UtterancePipeline, UtteranceTranscript, bump_longest_transcript,
    resolve_utterance_text_with_best, spawn_ordered_asr_feeder, wait_utterance_fed,
};
use crate::speaker::SpeakerVerifier;
#[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
use crate::stream_turn;
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
    /// `messages.len()` before this turn's hermes merge / user append.
    context_checkpoint: usize,
}

fn rollback_turn_context(messages: &mut Vec<ChatMessage>, checkpoint: usize) {
    if messages.len() > checkpoint {
        messages.truncate(checkpoint);
        info!(
            checkpoint,
            remaining = messages.len(),
            "rolled back unspoken turn from context"
        );
    }
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
        let asr_offline = {
            #[cfg(feature = "sherpa-asr-tts")]
            {
                matches!(asr_backend, AsrBackend::Sherpa)
            }
            #[cfg(not(feature = "sherpa-asr-tts"))]
            {
                false
            }
        };
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
            let hermes_hint = "[工具提示] call_hermes 是把请求发给 hermes（后台智能助手）异步处理，hermes 可能几秒或更久才会回复。调用后你收到的 tool result 只有入队确认，不代表任务完成。调用时 spoken 须分两段：①准确精炼复述用户这一次的具体诉求；②用自然亲切的语气说明已交给 hermes、请用户稍候（如「我这就帮你安排」「好了已经交给后台了，你稍等一下」）。禁止空洞套话（如单独一句「帮你查一下」「我看看」）；spoken 播完后无需再说确认语。hermes 完成后会主动推送结果，你届时再用口语向用户播报真实结果。严禁在结果未返回前说「已经设置好了」「已经完成了」等话。";
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
        let denoise_cfg = self.cfg.denoise.clone();
        let capture_sample_rate = self.cfg.asr.sample_rate;
        let aec_ref_clone = aec_ref_buf.clone();
        std::thread::spawn(move || {
            let mut aec_engine = AecEngine::new(&aec_cfg, aec_ref_clone);
            let mut denoiser = StreamingDenoiser::create(&denoise_cfg);
            loop {
                if let Some(chunk) = capture.try_recv_chunk() {
                    // Pipeline order: AEC → denoise → (VAD/ASR in session loop)
                    let after_aec = aec_engine.process(&chunk.samples_f32);
                    let cleaned = if let Some(ref mut d) = denoiser {
                        let out = d.process(&after_aec, capture_sample_rate);
                        if out.is_empty() { after_aec } else { out }
                    } else {
                        after_aec
                    };
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
                    // Do not `continue` here: the current frame may be the first packet
                    // of a barge-in ack after interrupt; enqueue_pcm_i16 drops stale gen.
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
            &self.cfg.vad.provider,
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

        let denoiser_enabled = self.cfg.denoise.enabled;
        let aec_enabled = self.cfg.aec.enabled;

        #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
        let utterance_feeder = spawn_ordered_asr_feeder(asr.clone());
        #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
        let mut utterance_pipeline = UtterancePipeline::new(utterance_feeder.cmd_tx);
        #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
        let mut feed_done_rx = utterance_feeder.feed_done_rx;
        #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
        let mut feed_ack_rx = utterance_feeder.feed_ack_rx;
        #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
        let mut utterance_transcript = UtteranceTranscript::default();
        #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
        let mut sealed_utterance_id: Option<u64> = None;

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
        #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
        let mut asr_echo_cooldown_until: Option<Instant> = None;

        let (done_tx, mut done_rx) = mpsc::channel::<stream_turn::TurnDone>(4);

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
            aec_enabled,
            audio_pipeline = "capture -> aec -> denoise -> vad",
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

        loop {
            tokio::select! {
                chunk = pcm_rx.recv() => {
                    let Some(chunk) = chunk else { break };
                    let raw_rms = rms_f32(&chunk.samples_f32);
                    let samples_f32 = chunk.samples_f32;

                    #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
                    drain_feed_acks(
                        &mut feed_ack_rx,
                        &utterance_pipeline,
                        &mut utterance_transcript,
                    );

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
                            #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
                            let asr_ok = stream_turn::resume_asr_with_retry(asr.clone()).await;
                            #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
                            let asr_ok = {
                                if !resume_asr_with_retry(asr.clone()).await {
                                    false
                                } else {
                                    let _ = asr.set_gate(false).await;
                                    asr.reconnect().await.is_ok()
                                }
                            };
                            if !asr_ok {
                                warn!("wake: ASR resume failed; staying dormant");
                                continue;
                            }
                            #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
                            {
                                utterance_active = false;
                            }
                            let ack_extra = if wake_cfg.ack_reply.trim().is_empty() {
                                Duration::ZERO
                            } else {
                                Duration::from_secs(3)
                            };
                            wake_phase = WakePhase::AwakeGrace {
                                deadline: Instant::now() + grace_after_wake + ack_extra,
                            };
                            info!(
                                grace_sec = wake_cfg.grace_after_wake_sec,
                                ack = %wake_cfg.ack_reply,
                                "wake: accepted, now in AwakeGrace; speak within grace period"
                            );
                            if !wake_cfg.ack_reply.trim().is_empty() {
                                spawn_wake_ack(
                                    wake_cfg.ack_reply.clone(),
                                    tts.clone(),
                                    playback.clone(),
                                    play_gen.clone(),
                                );
                            }
                        } else if orch.barge_in_enabled
                            && is_output_busy(state, &playback, &active_turn)
                        {
                            if let Some(last) = last_barge_in_at {
                                if last.elapsed().as_millis() < orch.barge_in_cooldown_ms as u128 {
                                    continue;
                                }
                            }
                            if is_llm_turn_busy(state, &active_turn) {
                                info!("wake-word barge-in (kws)");
                                let ack_reply = barge_in_ack_reply(
                                    wake_enabled,
                                    &wake_cfg.ack_reply,
                                    active_turn.is_some(),
                                );
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
                                    &mut messages,
                                    &mut active_turn,
                                    &current_latency,
                                    &mut last_partial,
                                    &mut partial_stable_since,
                                    &mut last_barge_in_at,
                                    &mut speaker_gate,
                                    &mut speaker_verify_buffer,
                                    speaker_verify_gate,
                                    ack_reply,
                                    #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
                                    RockchipBargeInReset {
                                        input_gated: &mut input_gated,
                                        utterance_active: &mut utterance_active,
                                        asr_echo_cooldown_until: &mut asr_echo_cooldown_until,
                                        utterance_pipeline: &mut utterance_pipeline,
                                        utterance_transcript: &mut utterance_transcript,
                                        sealed_utterance_id: &mut sealed_utterance_id,
                                        last_final: &mut last_final,
                                        asr_rx: &mut asr_rx,
                                    },
                                )
                                .await;
                            } else {
                                info!("wake-word: interrupt playback for user speech");
                                interrupt_playback_for_user_speech(
                                    &playback,
                                    &play_gen,
                                    tts.clone(),
                                    &mut vad,
                                    asr.clone(),
                                    wake_enabled,
                                    &mut wake_phase,
                                    &mut state,
                                    &mut last_partial,
                                    &mut partial_stable_since,
                                    &mut speaker_gate,
                                    &mut speaker_verify_buffer,
                                    speaker_verify_gate,
                                    #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
                                    RockchipBargeInReset {
                                        input_gated: &mut input_gated,
                                        utterance_active: &mut utterance_active,
                                        asr_echo_cooldown_until: &mut asr_echo_cooldown_until,
                                        utterance_pipeline: &mut utterance_pipeline,
                                        utterance_transcript: &mut utterance_transcript,
                                        sealed_utterance_id: &mut sealed_utterance_id,
                                        last_final: &mut last_final,
                                        asr_rx: &mut asr_rx,
                                    },
                                )
                                .await;
                            }
                        } else {
                            #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
                            match &mut wake_phase {
                                WakePhase::AwakeGrace { deadline } => {
                                    *deadline = Instant::now() + grace_after_wake;
                                    info!("wake kws during grace; extended grace window");
                                }
                                WakePhase::IdleAfterTurn { .. } => {
                                    promote_wake_on_speech(&mut wake_phase);
                                    if !speaker_verify_gate {
                                        speaker_gate = SpeakerGate::Passed;
                                    }
                                    let _ =
                                        open_asr_for_user_speech(asr.clone(), wake_enabled)
                                            .await;
                                }
                                WakePhase::Active => {
                                    debug!("wake kws while listening; ignored");
                                }
                                WakePhase::Dormant => {}
                            }
                        }
                    }

                    #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
                    if wake_phase.allows_asr()
                        && (speech_just_started || vad.in_speech())
                        && matches!(wake_phase, WakePhase::IdleAfterTurn { .. })
                    {
                        info!("idle after turn -> active (speech start)");
                        wake_phase = WakePhase::Active;
                        if !speaker_verify_gate {
                            speaker_gate = SpeakerGate::Passed;
                        }
                        let _ = open_asr_for_user_speech(asr.clone(), wake_enabled).await;
                    }

                    if !input_gated && wake_phase.allows_asr() {
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
                                        #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
                                        {
                                            rockchip_open_utterance(
                                                &asr,
                                                &mut utterance_pipeline,
                                                &mut utterance_transcript,
                                                &mut sealed_utterance_id,
                                                &mut last_final,
                                                &mut last_partial,
                                                &mut asr_rx,
                                            )
                                            .await;
                                            utterance_pipeline.push_pcm(i16_bytes);
                                        }
                                        #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
                                        {
                                            let _ = asr.send_audio(i16_bytes).await;
                                        }
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
                            #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
                            {
                                rockchip_open_utterance(
                                    &asr,
                                    &mut utterance_pipeline,
                                    &mut utterance_transcript,
                                    &mut sealed_utterance_id,
                                    &mut last_final,
                                    &mut last_partial,
                                    &mut asr_rx,
                                )
                                .await;
                                utterance_pipeline.push_pcm(i16_bytes);
                            }
                            #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
                            {
                                let _ = asr.send_audio(i16_bytes).await;
                            }
                        }
                    }

                    if wake_phase.allows_asr() && (speech_just_started || vad.in_speech()) {
                        utterance_active = true;
                        pending_offline_flush = None;
                        #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
                        if speech_just_started {
                            rockchip_open_utterance(
                                &asr,
                                &mut utterance_pipeline,
                                &mut utterance_transcript,
                                &mut sealed_utterance_id,
                                &mut last_final,
                                &mut last_partial,
                                &mut asr_rx,
                            )
                            .await;
                        }
                    }

                    if user_speech_activity(&mut vad, None, orch.min_final_chars, &wake_phase, orch.grace_min_final_chars) {
                        if promote_wake_on_speech_with_asr(
                            &mut wake_phase,
                            asr.clone(),
                            wake_enabled,
                        )
                        .await
                        {
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
                        continue;
                    }

                    if orch.barge_in_enabled
                        && is_output_busy(state, &playback, &active_turn)
                    {
                        if !wake_enabled || !orch.barge_in_requires_wake {
                            let ack_reply = barge_in_ack_reply(
                                wake_enabled,
                                &wake_cfg.ack_reply,
                                active_turn.is_some(),
                            );
                            if try_barge_in(
                                "vad",
                                &orch,
                                &mut state,
                                &mut vad,
                                &playback,
                                &play_gen,
                                &mut llm_cancel,
                                &mut messages,
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
                                ack_reply,
                                #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
                                RockchipBargeInReset {
                                    input_gated: &mut input_gated,
                                    utterance_active: &mut utterance_active,
                                    asr_echo_cooldown_until: &mut asr_echo_cooldown_until,
                                    utterance_pipeline: &mut utterance_pipeline,
                                    utterance_transcript: &mut utterance_transcript,
                                    sealed_utterance_id: &mut sealed_utterance_id,
                                    last_final: &mut last_final,
                                    asr_rx: &mut asr_rx,
                                },
                            )
                            .await
                            {
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
                    }

                    #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
                    let flush_now = stream_turn::should_flush_asr_partial(
                        input_gated,
                        vad.trailing_silence_ms(),
                        orch.endpoint_silence_ms(),
                        &last_partial,
                        orch.min_final_chars,
                        utterance_pipeline.is_open() || utterance_pipeline.is_sealed(),
                    ) || stream_turn::should_flush_asr_partial_complete(
                        input_gated,
                        vad.trailing_silence_ms(),
                        orch.early_endpoint_silence_ms(),
                        &last_partial,
                        orch.min_final_chars,
                        utterance_pipeline.is_open() || utterance_pipeline.is_sealed(),
                        partial_stable_since,
                        orch.speculative_stable_ms,
                    );
                    #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
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
                        #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
                        {
                            let utt_id = utterance_pipeline.utterance_id();
                            if utterance_pipeline.is_open() {
                                utterance_pipeline.seal();
                                sealed_utterance_id = Some(utt_id);
                            }
                            if !wait_utterance_fed(
                                &mut feed_done_rx,
                                utt_id,
                                500,
                            )
                            .await
                            {
                                warn!(
                                    utterance_id = utt_id,
                                    "utterance feed: timeout waiting for queue drain"
                                );
                            }
                            if let Err(e) = asr.finish_utterance().await {
                                warn!(error = %e, "finish_utterance failed");
                            }
                            utterance_active = false;
                            drain_feed_acks(
                                &mut feed_ack_rx,
                                &utterance_pipeline,
                                &mut utterance_transcript,
                            );
                            let queued = utterance_transcript.best_transcript();
                            let flush_wait_ms =
                                stream_turn::rockchip_asr_flush_wait_ms(&queued, orch.min_final_chars);
                            let final_text =
                                wait_rockchip_asr_final(&mut asr_rx, flush_wait_ms).await;
                            let asr_final_log = final_text.clone();
                            let best_full = utterance_transcript.best_full();
                            let peak_partial = last_partial.trim();
                            let resolved = resolve_utterance_text_with_best(
                                &queued,
                                final_text.as_deref(),
                                if best_full.is_empty() {
                                    None
                                } else {
                                    Some(best_full)
                                },
                                if peak_partial.is_empty() {
                                    None
                                } else {
                                    Some(peak_partial)
                                },
                            );
                            if let Some(text) = resolved {
                                let normalized = normalize_asr_transcript(&text);
                                info!(
                                    utterance_id = utt_id,
                                    text = %normalized,
                                    asr_final = ?asr_final_log,
                                    "utterance complete: queue text concat"
                                );
                                last_final = Some(normalized);
                                asr_final_at = Some(Instant::now());
                                last_partial.clear();
                                utterance_transcript.clear();
                                if matches!(wake_phase, WakePhase::AwakeGrace { .. }) {
                                    wake_phase = WakePhase::Active;
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
                                    &mut pending_hermes_msgs,
                                )
                                .await;
                                if has_pending_utterance_trigger(
                                    &last_final,
                                    &asr_final_at,
                                    &active_turn,
                                ) {
                                    info!(
                                        user = last_final.as_deref().unwrap_or(""),
                                        output_busy =
                                            is_output_busy(state, &playback, &active_turn),
                                        wake_phase = ?wake_phase,
                                        "utterance ready: deferred LLM trigger (retry when playback idle)"
                                    );
                                }
                                if active_turn.is_none() {
                                    try_play_one_pending_hermes(
                                        &mut pending_hermes_msgs,
                                        false,
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
                                utterance_pipeline.clear_after_llm();
                                utterance_transcript.clear();
                                sealed_utterance_id = None;
                            } else {
                                warn!(
                                    utterance_id = utt_id,
                                    "utterance complete: no ASR final after queue flush"
                                );
                                last_final = None;
                                sealed_utterance_id = None;
                                utterance_pipeline.clear_after_llm();
                                if active_turn.is_none() {
                                    try_play_one_pending_hermes(
                                        &mut pending_hermes_msgs,
                                        false,
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
                            drain_stale_asr_events(&mut asr_rx);
                            if state == SessionState::Listening && active_turn.is_none() {
                                input_gated = false;
                            }
                            continue;
                        }
                        #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
                        {
                        if let Err(e) = asr.finish_utterance().await {
                            warn!(error = %e, "finish_utterance failed");
                        }
                        utterance_active = false;
                        settle_asr_after_flush(
                            &mut asr_rx,
                            &mut last_final,
                            &mut asr_final_at,
                            &mut last_partial,
                            &mut partial_stable_since,
                            &mut last_asr_event_at,
                            asr_settle_ms,
                        )
                        .await;
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
                            &mut pending_hermes_msgs,
                        )
                        .await;
                        if active_turn.is_none() {
                            try_play_one_pending_hermes(
                                &mut pending_hermes_msgs,
                                false,
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
                        if state == SessionState::Listening && active_turn.is_none() {
                            input_gated = false;
                        }
                        continue;
                        }
                    }

                    #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
                    let periodic_trigger_ok = has_pending_utterance_trigger(
                        &last_final,
                        &asr_final_at,
                        &active_turn,
                    );
                    #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
                    let periodic_trigger_ok = true;

                    if periodic_trigger_ok
                        && last_asr_event_at
                            .map_or(true, |t| t.elapsed() >= Duration::from_millis(asr_settle_ms))
                    {
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
                        &mut pending_hermes_msgs,
                    )
                    .await;
                    }
                }
                ev = asr_rx.recv() => {
                    if let Some(ev) = ev {
                        match ev {
                            AsrEvent::Partial { text, full } => {
                                #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
                                if asr_echo_cooldown_until
                                    .is_some_and(|t| Instant::now() < t)
                                {
                                    continue;
                                }
                                if utterance_active && wake_phase.allows_asr() {
                                    info!(
                                        partial = %text,
                                        state = ?state,
                                        wake_phase = ?wake_phase,
                                        "asr partial"
                                    );
                                } else {
                                    debug!(
                                        partial = %text,
                                        state = ?state,
                                        wake_phase = ?wake_phase,
                                        "asr partial"
                                    );
                                }
                                last_asr_event_at = Some(Instant::now());
                                #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
                                {
                                    if !utterance_pipeline.is_open() {
                                        continue;
                                    }
                                    drain_feed_acks(
                                        &mut feed_ack_rx,
                                        &utterance_pipeline,
                                        &mut utterance_transcript,
                                    );
                                    utterance_transcript.append_hypothesis(&text, full.as_deref());
                                    let assembled = utterance_transcript.concat_transcript();
                                    let prev_len = last_partial.chars().count();
                                    bump_longest_transcript(
                                        &mut last_partial,
                                        &[&text, full.as_deref().unwrap_or(""), &assembled],
                                    );
                                    if last_partial.chars().count() != prev_len {
                                        partial_stable_since = Some(Instant::now());
                                    }
                                }
                                #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
                                {
                                    last_partial = text.clone();
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
                                    if promote_wake_on_speech_with_asr(
                                        &mut wake_phase,
                                        asr.clone(),
                                        wake_enabled,
                                    )
                                    .await
                                    {
                                        partial_stable_since = None;
                                        last_partial.clear();
                                    }
                                }
                                if !wake_phase.allows_dialog() {
                                    continue;
                                }
                                if orch.barge_in_enabled
                                    && is_output_busy(state, &playback, &active_turn)
                                {
                                    if !wake_enabled || !orch.barge_in_requires_wake {
                                        let ack_reply = barge_in_ack_reply(
                                            wake_enabled,
                                            &wake_cfg.ack_reply,
                                            active_turn.is_some(),
                                        );
                                        if try_barge_in(
                                            "asr-partial",
                                            &orch,
                                            &mut state,
                                            &mut vad,
                                            &playback,
                                            &play_gen,
                                            &mut llm_cancel,
                                            &mut messages,
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
                                            ack_reply,
                                            #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
                                            RockchipBargeInReset {
                                                input_gated: &mut input_gated,
                                                utterance_active: &mut utterance_active,
                                                asr_echo_cooldown_until: &mut asr_echo_cooldown_until,
                                                utterance_pipeline: &mut utterance_pipeline,
                                                utterance_transcript: &mut utterance_transcript,
                                                sealed_utterance_id: &mut sealed_utterance_id,
                                                last_final: &mut last_final,
                                                asr_rx: &mut asr_rx,
                                            },
                                        )
                                        .await
                                        {
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
                                }
                                if orch.speculative_llm
                                    && state == SessionState::Listening
                                    && wake_phase.allows_dialog()
                                {
                                    #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
                                    {
                                        if text == last_partial {
                                            // unchanged
                                        } else {
                                            last_partial = text.clone();
                                            partial_stable_since = Some(Instant::now());
                                        }
                                    }
                                    let spec_text = last_partial.as_str();
                                    if let Some(since) = partial_stable_since {
                                        if since.elapsed()
                                            >= Duration::from_millis(orch.speculative_stable_ms as u64)
                                            && spec_text.trim().chars().count() >= orch.min_final_chars
                                            && active_turn.is_none()
                                        {
                                            #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
                                            if matches_sleep_keyword(spec_text, &sleep_phrases) {
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
                                            info!(text = %spec_text, "speculative llm start");
                                            let context_checkpoint = messages.len();
                                            start_reply_turn(
                                                spec_text.to_string(),
                                                None,
                                                false,
                                                true,
                                                context_checkpoint,
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
                            AsrEvent::SegmentFinish { text } => {
                                last_asr_event_at = Some(Instant::now());
                                #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
                                {
                                    if asr_echo_cooldown_until
                                        .is_some_and(|t| Instant::now() < t)
                                    {
                                        continue;
                                    }
                                    if !utterance_pipeline.is_open() {
                                        continue;
                                    }
                                    debug!(
                                        segment = %text,
                                        state = ?state,
                                        wake_phase = ?wake_phase,
                                        "asr segment finish"
                                    );
                                    drain_feed_acks(
                                        &mut feed_ack_rx,
                                        &utterance_pipeline,
                                        &mut utterance_transcript,
                                    );
                                    utterance_transcript.commit_segment(&text);
                                    let assembled = utterance_transcript.concat_transcript();
                                    let prev_len = last_partial.chars().count();
                                    bump_longest_transcript(
                                        &mut last_partial,
                                        &[&text, &assembled],
                                    );
                                    if last_partial.chars().count() != prev_len {
                                        partial_stable_since = Some(Instant::now());
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
                                #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
                                {
                                    debug!(
                                        final_text = %text,
                                        state = ?state,
                                        sealed = ?sealed_utterance_id,
                                        "asr final (rockchip): ignored; LLM only after queue+flush"
                                    );
                                    continue;
                                }
                                #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
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
                                    if promote_wake_on_speech_with_asr(
                                        &mut wake_phase,
                                        asr.clone(),
                                        wake_enabled,
                                    )
                                    .await
                                    {
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
                                    last_final = Some(normalize_asr_transcript(&text));
                                    asr_final_at = Some(Instant::now());
                                    continue;
                                }
                                if orch.barge_in_enabled
                                    && is_output_busy(state, &playback, &active_turn)
                                {
                                    if !wake_enabled || !orch.barge_in_requires_wake {
                                        let ack_reply = barge_in_ack_reply(
                                            wake_enabled,
                                            &wake_cfg.ack_reply,
                                            active_turn.is_some(),
                                        );
                                        if try_barge_in(
                                            "asr-final",
                                            &orch,
                                            &mut state,
                                            &mut vad,
                                            &playback,
                                            &play_gen,
                                            &mut llm_cancel,
                                            &mut messages,
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
                                            ack_reply,
                                            #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
                                            RockchipBargeInReset {
                                                input_gated: &mut input_gated,
                                                utterance_active: &mut utterance_active,
                                                asr_echo_cooldown_until: &mut asr_echo_cooldown_until,
                                                utterance_pipeline: &mut utterance_pipeline,
                                                utterance_transcript: &mut utterance_transcript,
                                                sealed_utterance_id: &mut sealed_utterance_id,
                                                last_final: &mut last_final,
                                                asr_rx: &mut asr_rx,
                                            },
                                        )
                                        .await
                                        {
                                            // Accumulate rather than replace — user may still be speaking
                                            last_final = Some(normalize_asr_transcript(&text));
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
                                }
                                asr_final_at = Some(Instant::now());
                                last_asr_event_at = Some(Instant::now());
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
                                        last_final = Some(normalize_asr_transcript(&text));
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
                                            &mut pending_hermes_msgs,
                                        ).await;
                                    } else if !wake_enabled
                                        || !orch.barge_in_requires_wake
                                        || crate::orchestrator::utterance_likely_incomplete(&turn.user_text)
                                    {
                                        let prev_user_text = turn.user_text.clone();
                                        let continuing =
                                            crate::orchestrator::utterance_likely_incomplete(&prev_user_text);
                                        info!(
                                            prev = %prev_user_text,
                                            final_text = %text,
                                            continuing,
                                            "restarting turn with complete final text"
                                        );
                                        if let Some(c) = llm_cancel.take() {
                                            c.cancel();
                                        }
                                        turn_epoch.fetch_add(1, Ordering::SeqCst);
                                        play_gen.fetch_add(1, Ordering::SeqCst);
                                        if messages.last().map(|m| m.role.as_str()) == Some("user") {
                                            messages.pop();
                                        }
                                        active_turn = None;
                                        state = SessionState::Listening;
                                        input_gated = false;
                                        let combined = normalize_asr_transcript(&text);
                                        info!(combined = %combined, "utterance text for retrigger");
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
                                            &mut pending_hermes_msgs,
                                        ).await;
                                    } else {
                                        // Wake word required to interrupt — save text for next turn
                                        last_final = Some(normalize_asr_transcript(&text));
                                    }
                                } else {
                                    // Accumulate ASR text regardless of state.
                                    // When Speaking/TTS playing, text is saved so the next
                                    // Listening cycle picks up the full utterance.
                                    last_final = Some(normalize_asr_transcript(&text));
                                }
                                if state == SessionState::Listening && active_turn.is_none() {
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
                                        &mut pending_hermes_msgs,
                                    ).await;
                                }
                                } // non-rockchip asr final
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
                    if let Some(done) = done {
                        let stream_turn::TurnDone {
                            assistant_text,
                            epoch,
                            shutup,
                            tts_spoken,
                        } = done;
                        let context_checkpoint =
                            active_turn.as_ref().map(|t| t.context_checkpoint);
                        if epoch == turn_epoch.load(Ordering::SeqCst) {
                            if tts_spoken && !assistant_text.trim().is_empty() {
                                messages.push(ChatMessage {
                                    role: "assistant".to_string(),
                                    content: assistant_text,
                                    tool_calls: None,
                                    tool_call_id: None,
                                });
                                if orch.max_context_messages > 0
                                    && messages.len() > orch.max_context_messages
                                {
                                    let excess = messages.len() - orch.max_context_messages;
                                    messages.drain(..excess);
                                }
                            } else if !tts_spoken {
                                if let Some(cp) = context_checkpoint {
                                    rollback_turn_context(&mut messages, cp);
                                }
                            }
                            state = SessionState::Listening;
                            active_turn = None;
                            last_final = None;
                            asr_final_at = None;
                            partial_stable_since = None;
                            last_partial.clear();
                            last_asr_event_at = None;
                            input_gated = false;
                            speaker_gate = SpeakerGate::Idle;
                            #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
                            {
                                utterance_pipeline.clear_after_llm();
                                utterance_transcript.clear();
                                sealed_utterance_id = None;
                                asr_echo_cooldown_until = Some(
                                    Instant::now()
                                        + Duration::from_millis(
                                            stream_turn::ROCKCHIP_POST_TURN_ASR_COOLDOWN_MS,
                                        ),
                                );
                                drain_stale_asr_events(&mut asr_rx);
                            }
                            if !speaker_verify_gate {
                                speaker_gate = SpeakerGate::Passed;
                            }
                            speaker_verify_buffer.clear();
                            *current_latency.lock().unwrap() = None;
                            if (shutup || wake_cfg.idle_after_turn_sec == 0) && wake_enabled {
                                let _ = asr.set_gate(true).await;
                                let _ = asr.pause().await;
                                // drain pending ASR events
                                while asr_rx.try_recv().is_ok() {}
                                wake_phase = WakePhase::Dormant;
                                if shutup {
                                    info!("shutup requested -> dormant; say wake word to resume");
                                } else {
                                    info!(
                                        "turn complete -> dormant (idle_after_turn_sec=0); say wake word to resume"
                                    );
                                }
                            } else if wake_enabled {
                                wake_phase = WakePhase::IdleAfterTurn {
                                    deadline: Instant::now() + idle_after_turn,
                                };
                                #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
                                stream_turn::reopen_asr_after_turn(asr.clone(), wake_enabled).await;
                                #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
                                if !open_asr_for_user_speech(asr.clone(), wake_enabled).await {
                                    warn!(
                                        "IdleAfterTurn: failed to reopen ASR for follow-up speech"
                                    );
                                }
                                info!(
                                    idle_sec = wake_cfg.idle_after_turn_sec,
                                    "back to listening; idle timeout started"
                                );
                            } else {
                                wake_phase = WakePhase::Active;
                                info!("back to listening");
                            }
                            // Process any pending hermes messages
                            try_play_one_pending_hermes(
                                &mut pending_hermes_msgs,
                                false,
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
                msg = hermes_msg_rx.recv() => {
                    if let Some(msg) = msg {
                        info!(
                            request_id = %msg.request_id,
                            status = %msg.status,
                            text = %msg.text,
                            "hermes: message received"
                        );
                        #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
                        let user_speaking = user_speech_in_progress_rockchip(
                            utterance_active,
                            &vad,
                            &utterance_pipeline,
                        );
                        #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
                        let user_speaking = user_speech_in_progress(utterance_active, &vad);
                        if should_defer_hermes(user_speaking, state, &active_turn) {
                            info!(
                                request_id = %msg.request_id,
                                text = %msg.text,
                                user_speaking,
                                state = ?state,
                                "hermes: deferring message"
                            );
                            pending_hermes_msgs.push_back(msg);
                        } else {
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
                        }
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
    let in_grace = matches!(
        *wake_phase,
        WakePhase::AwakeGrace { .. } | WakePhase::IdleAfterTurn { .. }
    );
    #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
    {
        if !in_grace && (vad.speech_start() || vad.in_speech()) {
            return true;
        }
    }
    #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
    {
        if vad.speech_start() || vad.in_speech() {
            return true;
        }
    }
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
            let acc = map.get(&idx)?;
            if acc.name.trim().is_empty() {
                return None;
            }
            Some(ToolCall {
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

fn has_actionable_tool_deltas(map: &HashMap<u32, AccumulatedToolCall>) -> bool {
    map.values().any(|acc| !acc.name.trim().is_empty())
}

fn core_tool_call_to_talk(tc: hermes_core::ToolCall) -> ToolCall {
    let name = match tc.function.name.as_str() {
        "execute_command" => "execute",
        other => other,
    };
    ToolCall {
        id: tc.id,
        r#type: "function".to_string(),
        function: crate::llm::ToolCallFunction {
            name: name.to_string(),
            arguments: tc.function.arguments,
        },
    }
}

async fn append_tts_text(tts: &Arc<dyn TtsEngine>, text: &str, tts_sent: &mut bool) {
    let normalized = normalize_tts_text(text);
    if normalized.trim().is_empty() {
        return;
    }
    match tts.append_text(&normalized).await {
        Ok(()) => *tts_sent = true,
        Err(e) => warn!(error = %e, %normalized, "tts append failed"),
    }
}

async fn drain_tts_buf(
    tts: &Arc<dyn TtsEngine>,
    tts_buf: &mut String,
    tts_first_chunk: usize,
    sentence_min: usize,
    sent_early: &mut bool,
    tts_sent: &mut bool,
) {
    if !*sent_early {
        if let Some(chunk) = take_early_chunk(tts_buf, tts_first_chunk) {
            info!(%chunk, "tts early chunk");
            append_tts_text(tts, &chunk, tts_sent).await;
            *sent_early = true;
        }
    }
    while let Some(sentence) = take_sentence(tts_buf, sentence_min) {
        info!(%sentence, "tts sentence");
        append_tts_text(tts, &sentence, tts_sent).await;
    }
    if let Some(rest) = flush_remainder(tts_buf) {
        append_tts_text(tts, &rest, tts_sent).await;
    }
}

async fn speak_plain_assistant_reply(
    tts: &Arc<dyn TtsEngine>,
    plain: &str,
    tts_first_chunk: usize,
    sentence_min: usize,
    tts_sent: &mut bool,
) {
    let plain = strip_think_blocks(plain);
    if plain.trim().is_empty() {
        return;
    }
    let mut stripper = IncrementalThinkStripper::new();
    let cleaned = stripper.push(&plain);
    let tail = stripper.flush();
    let mut buf = format!("{cleaned}{tail}");
    if buf.trim().is_empty() {
        return;
    }
    info!(
        chars = buf.chars().count(),
        "tts speaking plain assistant reply"
    );
    let mut sent_early = false;
    drain_tts_buf(
        tts,
        &mut buf,
        tts_first_chunk,
        sentence_min,
        &mut sent_early,
        tts_sent,
    )
    .await;
}

fn flush_llm_reasoning_log(round: u32, reasoning_buf: &mut String, emitted: &mut bool) {
    if *emitted || reasoning_buf.trim().is_empty() {
        reasoning_buf.clear();
        return;
    }
    info!(
        round,
        chars = reasoning_buf.chars().count(),
        reasoning = %reasoning_buf.trim(),
        "llm reasoning"
    );
    *emitted = true;
    reasoning_buf.clear();
}

fn flush_llm_content_log(round: u32, content: &str, emitted: &mut bool) {
    if *emitted || content.trim().is_empty() {
        return;
    }
    info!(
        round,
        chars = content.chars().count(),
        content = %content.trim(),
        "llm assistant content"
    );
    *emitted = true;
}

/// Split assistant `content` into reasoning (inline think blocks) and TTS/log speakable text.
fn prepare_llm_speakable_text(
    raw: &str,
    reasoning_buf: &mut String,
    reasoning_log_emitted: &mut bool,
    round: u32,
) -> String {
    let inline = extract_inline_thinking(raw);
    if !inline.trim().is_empty() {
        if !reasoning_buf.trim().is_empty() {
            reasoning_buf.push('\n');
        }
        reasoning_buf.push_str(inline.trim());
        flush_llm_reasoning_log(round, reasoning_buf, reasoning_log_emitted);
    }
    strip_think_blocks(raw)
}

fn log_llm_tool_calls(round: u32, tool_calls: &[ToolCall]) {
    for tc in tool_calls {
        info!(
            round,
            tool = %tc.function.name,
            args = %tc.function.arguments,
            "llm tool_call"
        );
    }
}

fn resolve_asr_last_final(last_final: &mut Option<String>, last_partial: &str) {
    if last_partial.trim().is_empty() {
        return;
    }
    let normalized = normalize_asr_transcript(last_partial);
    if last_final.as_deref() != Some(normalized.as_str()) {
        if let Some(prev) = last_final.as_ref() {
            info!(prev = %prev, resolved = %normalized, "asr: use latest partial");
        }
        *last_final = Some(normalized);
    }
}

#[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
fn drain_stale_asr_events(asr_rx: &mut mpsc::Receiver<AsrEvent>) {
    let mut dropped = 0usize;
    while asr_rx.try_recv().is_ok() {
        dropped += 1;
    }
    if dropped > 0 {
        debug!(dropped, "asr: drained stale events between utterances");
    }
}

#[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
fn drain_feed_acks(
    feed_ack_rx: &mut mpsc::Receiver<(u64, u64)>,
    utterance_pipeline: &UtterancePipeline,
    utterance_transcript: &mut UtteranceTranscript,
) {
    while let Ok((id, seq)) = feed_ack_rx.try_recv() {
        if id == utterance_pipeline.utterance_id() {
            utterance_transcript.on_slice_fed(id, seq);
        }
    }
}

#[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
async fn rockchip_open_utterance(
    asr: &Arc<dyn AsrEngine>,
    pipeline: &mut UtterancePipeline,
    transcript: &mut UtteranceTranscript,
    sealed_utterance_id: &mut Option<u64>,
    last_final: &mut Option<String>,
    last_partial: &mut String,
    asr_rx: &mut mpsc::Receiver<AsrEvent>,
) {
    if pipeline.is_open() {
        return;
    }
    drain_stale_asr_events(asr_rx);
    pipeline.begin();
    transcript.reset(pipeline.utterance_id());
    *sealed_utterance_id = None;
    *last_final = None;
    last_partial.clear();
    if let Err(e) = asr.begin_utterance().await {
        warn!(error = %e, "begin_utterance failed");
    }
}

/// Block until RK streaming ASR emits Final after `finish_utterance`.
async fn wait_rockchip_asr_final(
    asr_rx: &mut mpsc::Receiver<AsrEvent>,
    timeout_ms: u64,
) -> Option<String> {
    let deadline = Instant::now() + Duration::from_millis(timeout_ms);
    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match tokio::time::timeout(remaining, asr_rx.recv()).await {
            Ok(Some(AsrEvent::Final { text })) => return Some(text),
            Ok(Some(AsrEvent::Partial { text, .. })) => {
                debug!(partial = %text, "asr partial while waiting for final");
            }
            Ok(Some(AsrEvent::SegmentFinish { text })) => {
                debug!(segment = %text, "asr segment finish while waiting for final");
            }
            Ok(Some(AsrEvent::TaskFailed { message })) => {
                warn!(%message, "asr failed while waiting for final");
                return None;
            }
            Ok(Some(AsrEvent::TaskStarted)) | Ok(None) => return None,
            Err(_) => return None,
        }
    }
    None
}

async fn settle_asr_after_flush(
    asr_rx: &mut mpsc::Receiver<AsrEvent>,
    last_final: &mut Option<String>,
    asr_final_at: &mut Option<Instant>,
    last_partial: &mut String,
    partial_stable_since: &mut Option<Instant>,
    last_asr_event_at: &mut Option<Instant>,
    settle_ms: u64,
) {
    let deadline = Instant::now() + Duration::from_millis(settle_ms);
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, asr_rx.recv()).await {
            Ok(Some(AsrEvent::Final { text })) => {
                info!(
                    final_text = %text,
                    last_final = ?last_final,
                    "asr final (post-flush settle)"
                );
                *last_final = Some(normalize_asr_transcript(&text));
                *asr_final_at = Some(Instant::now());
                *last_asr_event_at = Some(Instant::now());
                *partial_stable_since = None;
                last_partial.clear();
            }
            Ok(Some(AsrEvent::Partial { text, .. })) => {
                debug!(partial = %text, "asr partial (post-flush settle)");
                *last_partial = text;
                *last_asr_event_at = Some(Instant::now());
            }
            Ok(Some(AsrEvent::SegmentFinish { text })) => {
                debug!(segment = %text, "asr segment finish (post-flush settle)");
                *last_partial = text;
                *last_asr_event_at = Some(Instant::now());
            }
            Ok(Some(AsrEvent::TaskFailed { .. })) | Ok(None) => break,
            Err(_) => break,
            _ => {}
        }
    }
    resolve_asr_last_final(last_final, last_partial);
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

async fn promote_wake_on_speech_with_asr(
    wake: &mut WakePhase,
    asr: Arc<dyn AsrEngine>,
    wake_enabled: bool,
) -> bool {
    if promote_wake_on_speech(wake) {
        #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
        {
            let _ = open_asr_for_user_speech(asr, wake_enabled).await;
        }
        true
    } else {
        false
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

/// Reopen ASR after echo guard or barge-in (`set_gate(true)` blocks `send_audio`).
async fn open_asr_for_user_speech(asr: Arc<dyn AsrEngine>, wake_enabled: bool) -> bool {
    if wake_enabled && !resume_asr_with_retry(asr.clone()).await {
        return false;
    }
    if let Err(e) = asr.set_gate(false).await {
        warn!(error = %e, "asr set_gate(false) failed");
        return false;
    }
    if let Err(e) = asr.reconnect().await {
        warn!(error = %e, "asr reconnect after opening gate failed");
    }
    info!("ASR gate open for user speech");
    true
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

fn spawn_wake_ack(
    text: String,
    tts: Arc<dyn TtsEngine>,
    playback: Arc<AudioPlayback>,
    play_gen: Arc<AtomicU64>,
) {
    tokio::spawn(async move {
        play_wake_ack(&text, tts, &playback, &play_gen).await;
    });
}

fn is_llm_turn_busy(state: SessionState, active_turn: &Option<ActiveTurn>) -> bool {
    active_turn.is_some() || matches!(state, SessionState::Thinking | SessionState::Speaking)
}

fn is_output_busy(
    state: SessionState,
    playback: &AudioPlayback,
    active_turn: &Option<ActiveTurn>,
) -> bool {
    is_llm_turn_busy(state, active_turn)
        || playback.buffered_samples() > playback.sample_rate() as usize / 10
}

/// Rockchip: utterance flush produced text but LLM has not started yet.
fn has_pending_utterance_trigger(
    last_final: &Option<String>,
    asr_final_at: &Option<Instant>,
    active_turn: &Option<ActiveTurn>,
) -> bool {
    last_final.is_some() && asr_final_at.is_some() && active_turn.is_none()
}

fn barge_in_ack_reply<'a>(
    wake_enabled: bool,
    ack: &'a str,
    llm_turn_busy: bool,
) -> Option<&'a str> {
    if wake_enabled && llm_turn_busy && !ack.trim().is_empty() {
        Some(ack)
    } else {
        None
    }
}

#[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
struct RockchipBargeInReset<'a> {
    input_gated: &'a mut bool,
    utterance_active: &'a mut bool,
    asr_echo_cooldown_until: &'a mut Option<Instant>,
    utterance_pipeline: &'a mut UtterancePipeline,
    utterance_transcript: &'a mut UtteranceTranscript,
    sealed_utterance_id: &'a mut Option<u64>,
    last_final: &'a mut Option<String>,
    asr_rx: &'a mut mpsc::Receiver<AsrEvent>,
}

#[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
fn rockchip_reset_after_barge_in(rk: RockchipBargeInReset<'_>, echo_cooldown: bool) {
    *rk.input_gated = false;
    *rk.utterance_active = false;
    rk.utterance_pipeline.clear_after_llm();
    rk.utterance_transcript.clear();
    *rk.sealed_utterance_id = None;
    *rk.last_final = None;
    drain_stale_asr_events(rk.asr_rx);
    *rk.asr_echo_cooldown_until = echo_cooldown.then(|| {
        Instant::now() + Duration::from_millis(stream_turn::ROCKCHIP_POST_TURN_ASR_COOLDOWN_MS)
    });
}

#[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
async fn rockchip_reopen_asr_after_barge_in(
    asr: Arc<dyn AsrEngine>,
    wake_enabled: bool,
    echo_cooldown: bool,
    rk: RockchipBargeInReset<'_>,
) {
    rockchip_reset_after_barge_in(rk, echo_cooldown);
    if wake_enabled {
        let _ = asr.set_gate(true).await;
    }
    if let Err(e) = asr.begin_utterance().await {
        warn!(error = %e, "begin_utterance after barge-in failed");
    }
    info!(echo_cooldown, "rockchip ASR reopened for user speech");
}

/// Stop wake ack / leftover playback so mic+ASR can capture the user — no LLM cancel, no ack replay.
#[allow(clippy::too_many_arguments)]
async fn interrupt_playback_for_user_speech(
    playback: &Arc<AudioPlayback>,
    play_gen: &Arc<AtomicU64>,
    tts: Arc<dyn TtsEngine>,
    vad: &mut VadEngine,
    asr: Arc<dyn AsrEngine>,
    wake_enabled: bool,
    wake_phase: &mut WakePhase,
    state: &mut SessionState,
    last_partial: &mut String,
    partial_stable_since: &mut Option<Instant>,
    speaker_gate: &mut SpeakerGate,
    speaker_verify_buffer: &mut Vec<f32>,
    speaker_verify_gate: bool,
    #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))] rk: RockchipBargeInReset<'_>,
) {
    playback.stop_clear();
    play_gen.store(playback.current_generation(), Ordering::SeqCst);
    if let Err(e) = tts.interrupt_turn().await {
        warn!(error = %e, "tts interrupt on playback-only interrupt failed");
    }
    vad.reset_barge_in_state();
    if matches!(wake_phase, WakePhase::AwakeGrace { .. }) {
        *wake_phase = WakePhase::Active;
    }
    *state = SessionState::Listening;
    last_partial.clear();
    *partial_stable_since = None;
    *speaker_gate = SpeakerGate::Idle;
    if !speaker_verify_gate {
        *speaker_gate = SpeakerGate::Passed;
    }
    speaker_verify_buffer.clear();
    #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
    {
        rockchip_reopen_asr_after_barge_in(asr.clone(), wake_enabled, false, rk).await;
    }
    #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
    {
        let _ = open_asr_for_user_speech(asr, wake_enabled).await;
    }
    info!("playback interrupted; ready for user speech");
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
    messages: &mut Vec<ChatMessage>,
    active_turn: &mut Option<ActiveTurn>,
    current_latency: &Arc<std::sync::Mutex<Option<Arc<TurnLatency>>>>,
    last_partial: &mut String,
    partial_stable_since: &mut Option<Instant>,
    last_barge_in_at: &mut Option<Instant>,
    speaker_gate: &mut SpeakerGate,
    speaker_verify_buffer: &mut Vec<f32>,
    speaker_verify_gate: bool,
    ack_reply: Option<&str>,
    #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))] rk: RockchipBargeInReset<'_>,
) {
    // 1. Stop speaker output immediately — before any await or other work.
    playback.stop_clear();
    play_gen.store(playback.current_generation(), Ordering::SeqCst);

    *last_barge_in_at = Some(Instant::now());

    if let Some(turn) = active_turn.as_ref() {
        rollback_turn_context(messages, turn.context_checkpoint);
    }

    // 2. Stop in-flight LLM stream.
    if let Some(c) = llm_cancel.take() {
        c.cancel();
    }

    // 3. Stop TTS synthesis and discard buffered text.
    if let Err(e) = tts.interrupt_turn().await {
        warn!(error = %e, "tts interrupt on barge-in failed");
    }

    // 4. Drop stale PCM still in the TTS pump channel (playback stays stopped).
    turn_epoch.fetch_add(1, Ordering::SeqCst);

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

    // 5. Ack TTS+playback on a background task; do not block ASR reopen.
    if let Some(ack) = ack_reply.filter(|s| !s.trim().is_empty()) {
        info!(reply = %ack.trim(), "barge-in ack (spawned)");
        spawn_wake_ack(
            ack.trim().to_string(),
            tts.clone(),
            playback.clone(),
            play_gen.clone(),
        );
    }

    // 6. Reopen mic/ASR immediately (echo cooldown only when barge-in ack will play).
    let echo_cooldown = ack_reply.is_some();
    #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
    {
        rockchip_reopen_asr_after_barge_in(asr.clone(), wake_enabled, echo_cooldown, rk).await;
    }
    #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
    {
        let _ = open_asr_for_user_speech(asr, wake_enabled).await;
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
    messages: &mut Vec<ChatMessage>,
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
    ack_reply: Option<&str>,
    #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))] rk: RockchipBargeInReset<'_>,
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

    if !is_llm_turn_busy(*state, active_turn) {
        info!(reason, vad_hit, asr_hit, "playback-only interrupt");
        interrupt_playback_for_user_speech(
            playback,
            play_gen,
            tts,
            vad,
            asr,
            wake_enabled,
            wake_phase,
            state,
            last_partial,
            partial_stable_since,
            speaker_gate,
            speaker_verify_buffer,
            speaker_verify_gate,
            #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
            rk,
        )
        .await;
        return true;
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
        messages,
        active_turn,
        current_latency,
        last_partial,
        partial_stable_since,
        last_barge_in_at,
        speaker_gate,
        speaker_verify_buffer,
        speaker_verify_gate,
        ack_reply,
        #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
        rk,
    )
    .await;
    true
}

fn hermes_message_accepted(status: &str) -> bool {
    #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
    {
        stream_turn::hermes_status_accepted(status)
    }
    #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
    {
        status == "final" || status == "error" || status == "ok"
    }
}

fn user_speech_in_progress(utterance_active: bool, vad: &VadEngine) -> bool {
    utterance_active || vad.in_speech()
}

#[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
fn user_speech_in_progress_rockchip(
    utterance_active: bool,
    vad: &VadEngine,
    utterance_pipeline: &UtterancePipeline,
) -> bool {
    user_speech_in_progress(utterance_active, vad) || utterance_pipeline.is_open()
}

fn should_defer_hermes(
    user_speaking: bool,
    state: SessionState,
    active_turn: &Option<ActiveTurn>,
) -> bool {
    user_speaking || state != SessionState::Listening || active_turn.is_some()
}

fn append_hermes_to_messages(
    messages: &mut Vec<ChatMessage>,
    msg: &HermesMessage,
    merged_with_user: bool,
) -> bool {
    if !hermes_message_accepted(&msg.status) {
        return false;
    }
    messages.push(ChatMessage {
        role: "tool".to_string(),
        content: msg.text.clone(),
        tool_calls: None,
        tool_call_id: Some(msg.request_id.clone()),
    });
    let system_content = if merged_with_user {
        format!(
            "hermes 返回了查询结果（request_id={}），请用自然口语向用户播报这个结果；若用户刚才也说了话，请一并回应",
            msg.request_id
        )
    } else {
        format!(
            "hermes 返回了查询结果（request_id={}），请用自然口语向用户播报这个结果",
            msg.request_id
        )
    };
    messages.push(ChatMessage {
        role: "system".to_string(),
        content: system_content,
        tool_calls: None,
        tool_call_id: None,
    });
    true
}

fn take_one_pending_hermes_for_turn(
    pending: &mut VecDeque<HermesMessage>,
    messages: &mut Vec<ChatMessage>,
) -> bool {
    while let Some(msg) = pending.pop_front() {
        if append_hermes_to_messages(messages, &msg, true) {
            return true;
        }
        warn!(
            request_id = %msg.request_id,
            status = %msg.status,
            "hermes: skipping non-final pending message"
        );
    }
    false
}

#[allow(clippy::too_many_arguments)]
async fn try_play_one_pending_hermes(
    pending: &mut VecDeque<HermesMessage>,
    was_dormant: bool,
    messages: &mut Vec<ChatMessage>,
    llm: &Arc<dyn LlmClient>,
    tts: Arc<dyn TtsEngine>,
    playback: &Arc<AudioPlayback>,
    play_gen: &Arc<AtomicU64>,
    turn_epoch: &Arc<AtomicU64>,
    done_tx: &mpsc::Sender<stream_turn::TurnDone>,
    state: &mut SessionState,
    active_turn: &mut Option<ActiveTurn>,
    llm_cancel: &mut Option<CancellationToken>,
    orch: &OrchestratorConfig,
    llm_cfg: &LlmConfig,
) -> bool {
    while let Some(msg) = pending.pop_front() {
        if !hermes_message_accepted(&msg.status) {
            warn!(
                request_id = %msg.request_id,
                status = %msg.status,
                "hermes: skipping non-final pending message"
            );
            continue;
        }
        handle_hermes_result(
            was_dormant,
            msg,
            messages,
            llm,
            tts,
            playback,
            play_gen,
            turn_epoch,
            done_tx,
            state,
            active_turn,
            llm_cancel,
            orch,
            llm_cfg,
        )
        .await;
        return true;
    }
    false
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
    done_tx: &mpsc::Sender<stream_turn::TurnDone>,
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
    pending_hermes_msgs: &mut VecDeque<HermesMessage>,
) {
    #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
    if asr_final_at.is_none() {
        return;
    }
    if *state != SessionState::Listening || active_turn.is_some() {
        return;
    }
    if matches!(wake_phase, WakePhase::Dormant) {
        return;
    }
    if is_output_busy(*state, playback, active_turn) {
        if last_final.is_some() {
            debug!(
                wake_phase = ?wake_phase,
                "maybe_trigger: output busy, utterance pending"
            );
        }
        return;
    }
    if session_start.elapsed() < cold_start {
        return;
    }
    #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
    resolve_asr_last_final(last_final, last_partial);
    #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
    let endpoint_silence = orch.endpoint_silence_ms();
    #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
    {
        let _ = orch.endpoint_silence_ms();
    }
    #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
    let utterance_flush_ready = true;
    #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
    let utterance_flush_ready = vad.trailing_silence_ms() >= endpoint_silence;
    if !utterance_flush_ready {
        return;
    }
    if last_final
        .as_ref()
        .map_or(true, |t| t.trim().chars().count() < orch.min_final_chars)
    {
        return;
    }
    let text = normalize_asr_transcript(&last_final.take().unwrap());
    last_partial.clear();
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }

    #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
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

    let context_checkpoint = messages.len();
    let merged = take_one_pending_hermes_for_turn(pending_hermes_msgs, messages);
    if merged {
        info!(user = %trimmed, "hermes: merged one message into user turn");
    }

    let final_at = asr_final_at.take();
    start_reply_turn(
        text.clone(),
        final_at,
        true,
        false,
        context_checkpoint,
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
    context_checkpoint: usize,
    orch: &OrchestratorConfig,
    state: &mut SessionState,
    messages: &mut Vec<ChatMessage>,
    llm: &Arc<dyn LlmClient>,
    tts: Arc<dyn TtsEngine>,
    playback: &Arc<AudioPlayback>,
    play_gen: &Arc<AtomicU64>,
    llm_cancel: &mut Option<CancellationToken>,
    active_turn: &mut Option<ActiveTurn>,
    done_tx: &mpsc::Sender<stream_turn::TurnDone>,
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

    #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
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
        context_checkpoint,
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

    #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
    stream_turn::spawn_reply_turn(
        cancel,
        trigger_at,
        speculative,
        epoch_at_start,
        msgs,
        llm,
        tts,
        playback_wait,
        done_tx,
        sentence_min,
        tts_first_chunk,
        tools_enabled,
        llm_cfg,
        hermes_sender,
    );

    #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
    tokio::spawn(async move {
        let mut msgs_local = msgs;
        let mut assistant_buf = String::new();
        let mut should_go_dormant = false;
        let mut turn_tts_spoken = false;
        let max_rounds: u32 = if tools_enabled { 2 } else { 1 };

        for round in 0..max_rounds {
            let tools = if tools_enabled && round == 0 {
                Some(tools::get_tool_definitions())
            } else {
                None
            };

            let stream_started = Instant::now();
            let mut stream = match llm
                .stream_chat(&msgs_local, tools.as_deref(), cancel.clone())
                .await
            {
                Ok(s) => s,
                Err(e) => {
                    warn!(error = %e, "llm failed");
                    stream_turn::send_turn_done(
                        &done_tx,
                        String::new(),
                        epoch_at_start,
                        false,
                        false,
                    )
                    .await;
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
            let mut tts_sent = false;
            let mut buf = String::new();
            let mut tts_buf = String::new();
            let mut reasoning_buf = String::new();
            let mut reasoning_log_emitted = false;
            let mut content_log_emitted = false;
            let mut think_strip = StreamingThinkTtsGate::new(llm_cfg.thinking_enabled);
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
                    reasoning_buf.push_str(reasoning);
                    eprint!("{}", reasoning);
                }

                // Accumulate tool_call deltas (always, even without tools)
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
                if has_actionable_tool_deltas(&tool_call_map) {
                    flush_llm_reasoning_log(round, &mut reasoning_buf, &mut reasoning_log_emitted);
                }

                if let Some(ref token) = stream_item.content {
                    flush_llm_reasoning_log(round, &mut reasoning_buf, &mut reasoning_log_emitted);
                    buf.push_str(token);
                    assistant_buf.push_str(token);

                    let actionable = has_actionable_tool_deltas(&tool_call_map);
                    let speakable = think_strip.push(token);
                    if crate::orchestrator::append_speakable_stream_delta(
                        &mut tts_buf,
                        &speakable,
                        actionable,
                    ) {
                        drain_tts_buf(
                            &tts,
                            &mut tts_buf,
                            tts_first_chunk,
                            sentence_min,
                            &mut sent_early,
                            &mut tts_sent,
                        )
                        .await;
                    }
                }
            }

            flush_llm_reasoning_log(round, &mut reasoning_buf, &mut reasoning_log_emitted);

            let mut tool_calls = tool_calls_from_stream_map(&tool_call_map);
            let (plain, inline) = hermes_core::separate_text_and_calls(&buf);
            let speakable_buf = prepare_llm_speakable_text(
                &plain,
                &mut reasoning_buf,
                &mut reasoning_log_emitted,
                round,
            );
            if tool_calls.is_empty() && !inline.is_empty() {
                info!(
                    count = inline.len(),
                    "parsed inline tool_calls from assistant content"
                );
                tool_calls.extend(inline.into_iter().map(core_tool_call_to_talk));
            }
            tool_calls.retain(|tc| !tc.function.name.trim().is_empty());
            flush_llm_content_log(round, &speakable_buf, &mut content_log_emitted);
            log_llm_tool_calls(round, &tool_calls);

            if tool_calls.is_empty() {
                let tail = think_strip.flush();
                if crate::orchestrator::append_speakable_stream_delta(&mut tts_buf, &tail, false) {
                    drain_tts_buf(
                        &tts,
                        &mut tts_buf,
                        tts_first_chunk,
                        sentence_min,
                        &mut sent_early,
                        &mut tts_sent,
                    )
                    .await;
                }
                if !tts_sent {
                    speak_plain_assistant_reply(
                        &tts,
                        &speakable_buf,
                        tts_first_chunk,
                        sentence_min,
                        &mut tts_sent,
                    )
                    .await;
                }
                if !tts_sent && !speakable_buf.trim().is_empty() {
                    warn!(
                        chars = speakable_buf.chars().count(),
                        "assistant reply had text but nothing was sent to TTS"
                    );
                }
                if let Err(e) = tts.finish_turn().await {
                    warn!(error = %e, "tts finish");
                }
                playback_wait.wait_drain(Duration::from_secs(30)).await;
                turn_tts_spoken |= tts_sent;
                stream_turn::send_turn_done(
                    &done_tx,
                    assistant_buf,
                    epoch_at_start,
                    should_go_dormant,
                    turn_tts_spoken,
                )
                .await;
                return;
            }

            // --- Tool call handling ---
            // Discard content buffer (tool call scaffolding / reasoning, not for TTS)
            buf.clear();

            let shutup_turn = tools::tool_calls_include_shutup(
                tool_calls.iter().map(|tc| tc.function.name.as_str()),
            );

            let mut spoken_list: Vec<String> = Vec::new();

            for tc in &tool_calls {
                if let Some(spoken) =
                    tools::extract_tool_spoken(&tc.function.name, &tc.function.arguments)
                {
                    spoken_list.push(spoken);
                }
            }

            // TTS spoken notifications — finish current task so audio plays
            // during tool execution, not deferred until after the tool result.
            if !shutup_turn && !spoken_list.is_empty() {
                for spoken in &spoken_list {
                    info!(%spoken, "tool: spoken notification");
                    if let Err(e) = tts.append_text(&normalize_tts_text(spoken)).await {
                        warn!(error = %e, "tts spoken append");
                    } else {
                        turn_tts_spoken = true;
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
            let mut tool_results: Vec<String> = Vec::with_capacity(tool_calls.len());
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
                if tools::is_shutup_tool(&tc.function.name) {
                    should_go_dormant = true;
                }
                tool_results.push(result.clone());
                msgs_local.push(ChatMessage {
                    role: "tool".to_string(),
                    content: result,
                    tool_calls: None,
                    tool_call_id: Some(tc.id.clone()),
                });
            }
            if should_go_dormant {
                stream_turn::complete_shutup_turn(&tts, &playback_wait, &done_tx, epoch_at_start)
                    .await;
                return;
            }
            turn_tts_spoken |= tts_sent;
            if tools::should_skip_call_hermes_confirmation(
                tool_calls.iter().map(|tc| tc.function.name.as_str()),
                &tool_results,
            ) {
                info!("call_hermes enqueued: skipping follow-up LLM round");
                playback_wait.wait_drain(Duration::from_secs(30)).await;
                stream_turn::send_turn_done(
                    &done_tx,
                    assistant_buf,
                    epoch_at_start,
                    should_go_dormant,
                    turn_tts_spoken,
                )
                .await;
                return;
            }
        }

        // Should not reach here (max_rounds exhausted)
        if let Err(e) = tts.finish_turn().await {
            warn!(error = %e, "tts finish");
        }
        playback_wait.wait_drain(Duration::from_secs(30)).await;
        stream_turn::send_turn_done(
            &done_tx,
            assistant_buf,
            epoch_at_start,
            should_go_dormant,
            turn_tts_spoken,
        )
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
    done_tx: &mpsc::Sender<stream_turn::TurnDone>,
    state: &mut SessionState,
    active_turn: &mut Option<ActiveTurn>,
    llm_cancel: &mut Option<CancellationToken>,
    orch: &OrchestratorConfig,
    llm_cfg: &LlmConfig,
) {
    eprintln!(
        "\n══════════ hermes 返回 ══════════\n{}\n══════════════════════════",
        msg.text
    );

    let context_checkpoint = messages.len();
    if !append_hermes_to_messages(messages, &msg, false) {
        info!(
            request_id = %msg.request_id,
            status = %msg.status,
            "hermes: skipping non-final message"
        );
        return;
    }

    *state = SessionState::Thinking;
    *active_turn = Some(ActiveTurn {
        user_text: String::new(),
        speculative: false,
        context_checkpoint,
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
    let llm_cfg = llm_cfg.clone();

    #[cfg(all(feature = "rockchip", not(feature = "sherpa-asr-tts")))]
    stream_turn::spawn_hermes_replay(
        cancel,
        epoch_at_start,
        go_dormant,
        msgs,
        llm,
        tts,
        playback,
        done_tx,
        sentence_min,
        tts_first_chunk,
        llm_cfg.clone(),
    );

    #[cfg(not(all(feature = "rockchip", not(feature = "sherpa-asr-tts"))))]
    tokio::spawn(async move {
        let mut assistant_buf = String::new();
        let mut buf = String::new();
        let mut reasoning_buf = String::new();
        let mut reasoning_log_emitted = false;
        let mut content_log_emitted = false;

        let mut stream = match llm.stream_chat(&msgs, None, cancel.clone()).await {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "hermes replay llm failed");
                stream_turn::send_turn_done(
                    &done_tx,
                    String::new(),
                    epoch_at_start,
                    go_dormant,
                    false,
                )
                .await;
                return;
            }
        };

        let mut tts_buf = String::new();
        let mut tts_gate = StreamingThinkTtsGate::new(llm_cfg.thinking_enabled);
        let mut sent_early = false;
        let mut tts_sent = false;
        use futures_util::StreamExt;
        while let Some(item) = stream.next().await {
            if cancel.is_cancelled() {
                break;
            }
            let Ok(stream_item) = item else { continue };
            if let Some(ref reasoning) = stream_item.reasoning_content {
                reasoning_buf.push_str(reasoning);
                eprint!("{}", reasoning);
            }
            if let Some(ref token) = stream_item.content {
                flush_llm_reasoning_log(1, &mut reasoning_buf, &mut reasoning_log_emitted);
                buf.push_str(token);
                assistant_buf.push_str(token);

                let speakable = tts_gate.push(token);
                if crate::orchestrator::append_speakable_stream_delta(
                    &mut tts_buf,
                    &speakable,
                    false,
                ) {
                    drain_tts_buf(
                        &tts,
                        &mut tts_buf,
                        tts_first_chunk,
                        sentence_min,
                        &mut sent_early,
                        &mut tts_sent,
                    )
                    .await;
                }
            }
        }

        flush_llm_reasoning_log(1, &mut reasoning_buf, &mut reasoning_log_emitted);
        let (plain, _inline) = hermes_core::separate_text_and_calls(&buf);
        let speakable_buf =
            prepare_llm_speakable_text(&plain, &mut reasoning_buf, &mut reasoning_log_emitted, 1);
        flush_llm_content_log(1, &speakable_buf, &mut content_log_emitted);

        let tail = tts_gate.flush();
        if crate::orchestrator::append_speakable_stream_delta(&mut tts_buf, &tail, false) {
            drain_tts_buf(
                &tts,
                &mut tts_buf,
                tts_first_chunk,
                sentence_min,
                &mut sent_early,
                &mut tts_sent,
            )
            .await;
        }
        if !tts_sent {
            speak_plain_assistant_reply(
                &tts,
                &speakable_buf,
                tts_first_chunk,
                sentence_min,
                &mut tts_sent,
            )
            .await;
        }
        if let Err(e) = tts.finish_turn().await {
            warn!(error = %e, "tts finish");
        }
        playback.wait_drain(Duration::from_secs(30)).await;
        stream_turn::send_turn_done(
            &done_tx,
            assistant_buf,
            epoch_at_start,
            go_dormant,
            tts_sent,
        )
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

#[cfg(test)]
mod hermes_pending_tests {
    use super::*;

    fn sample_msg(id: &str, status: &str) -> HermesMessage {
        HermesMessage {
            request_id: id.to_string(),
            text: format!("result-{id}"),
            status: status.to_string(),
        }
    }

    #[test]
    fn take_one_empty_pending_no_op() {
        let mut pending = VecDeque::new();
        let mut messages = Vec::new();
        assert!(!take_one_pending_hermes_for_turn(
            &mut pending,
            &mut messages
        ));
        assert!(messages.is_empty());
    }

    #[test]
    fn take_one_appends_single_accepted_message() {
        let mut pending = VecDeque::from([sample_msg("r1", "final")]);
        let mut messages = Vec::new();
        assert!(take_one_pending_hermes_for_turn(
            &mut pending,
            &mut messages
        ));
        assert!(pending.is_empty());
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].role, "tool");
        assert_eq!(messages[0].content, "result-r1");
        assert_eq!(messages[1].role, "system");
        assert!(messages[1].content.contains("若用户刚才也说了话"));
    }

    #[test]
    fn take_one_from_many_leaves_remainder() {
        let mut pending = VecDeque::from([sample_msg("r1", "final"), sample_msg("r2", "final")]);
        let mut messages = Vec::new();
        assert!(take_one_pending_hermes_for_turn(
            &mut pending,
            &mut messages
        ));
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].request_id, "r2");
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[0].tool_call_id.as_deref(), Some("r1"));
    }

    #[test]
    fn take_one_skips_non_final_at_front() {
        let mut pending =
            VecDeque::from([sample_msg("bad", "partial"), sample_msg("good", "final")]);
        let mut messages = Vec::new();
        assert!(take_one_pending_hermes_for_turn(
            &mut pending,
            &mut messages
        ));
        assert!(pending.is_empty());
        assert_eq!(messages[0].tool_call_id.as_deref(), Some("good"));
    }
}
