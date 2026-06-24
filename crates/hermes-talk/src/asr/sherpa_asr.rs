//! Offline SenseVoice ASR via sherpa-onnx (Windows / x86 CPU).

use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};

use async_trait::async_trait;
use sherpa_onnx::{OfflineRecognizer, OfflineRecognizerConfig, OfflineSenseVoiceModelConfig};
use tokio::sync::mpsc as async_mpsc;
use tracing::{error, info, warn};

use crate::asr::{AsrEngine, AsrEvent};
use crate::config::SherpaAsrConfig;
use crate::error::{DemoError, Result};

enum AsrCommand {
    SendAudio(Vec<u8>),
    FinishUtterance,
    SetPaused(bool),
    SetGate(bool),
    ResetBuffer,
}

struct DriverState {
    recognizer: OfflineRecognizer,
    sample_rate: u32,
    buffer: Vec<i16>,
    paused: bool,
    gated: bool,
}

pub struct SherpaAsr {
    cmd_tx: SyncSender<AsrCommand>,
    _thread: JoinHandle<()>,
}

impl SherpaAsr {
    pub async fn connect(
        cfg: &SherpaAsrConfig,
        sample_rate: u32,
        start_paused: bool,
    ) -> Result<(Self, async_mpsc::Receiver<AsrEvent>)> {
        let mut recognizer_config = OfflineRecognizerConfig::default();
        recognizer_config.model_config.sense_voice = OfflineSenseVoiceModelConfig {
            model: Some(cfg.model.clone()),
            language: Some(cfg.language.clone()),
            use_itn: cfg.use_itn,
        };
        recognizer_config.model_config.tokens = Some(cfg.tokens.clone());
        recognizer_config.model_config.provider = Some(cfg.provider.clone());
        recognizer_config.model_config.num_threads = cfg.num_threads;

        let recognizer = OfflineRecognizer::create(&recognizer_config).ok_or_else(|| {
            DemoError::Config(format!(
                "failed to create SenseVoice recognizer (check asr.sherpa model paths): model={}",
                cfg.model
            ))
        })?;

        let (cmd_tx, cmd_rx) = mpsc::sync_channel::<AsrCommand>(128);
        let (event_tx, event_rx) = async_mpsc::channel(64);

        let mut state = DriverState {
            recognizer,
            sample_rate,
            buffer: Vec::new(),
            paused: start_paused,
            gated: false,
        };

        let thread = thread::spawn(move || {
            if let Err(e) = run_asr_loop(&mut state, cmd_rx, event_tx) {
                error!(error = %e, "sherpa asr thread exited");
            }
        });

        info!(
            model = %cfg.model,
            language = %cfg.language,
            sample_rate,
            "sherpa SenseVoice ASR ready"
        );

        Ok((
            Self {
                cmd_tx,
                _thread: thread,
            },
            event_rx,
        ))
    }

    fn send_cmd(&self, cmd: AsrCommand) -> Result<()> {
        self.cmd_tx
            .send(cmd)
            .map_err(|e| DemoError::Asr(format!("sherpa asr command channel: {e}")))
    }
}

#[async_trait]
impl AsrEngine for SherpaAsr {
    async fn send_audio(&self, pcm: Vec<u8>) -> Result<()> {
        self.send_cmd(AsrCommand::SendAudio(pcm))
    }

    async fn pause(&self) -> Result<()> {
        self.send_cmd(AsrCommand::SetPaused(true))
    }

    async fn resume(&self) -> Result<()> {
        self.send_cmd(AsrCommand::SetPaused(false))
    }

    async fn set_gate(&self, on: bool) -> Result<()> {
        self.send_cmd(AsrCommand::SetGate(on))
    }

    async fn reconnect(&self) -> Result<()> {
        self.send_cmd(AsrCommand::ResetBuffer)
    }

    async fn finish_utterance(&self) -> Result<()> {
        self.send_cmd(AsrCommand::FinishUtterance)
    }
}

fn run_asr_loop(
    state: &mut DriverState,
    cmd_rx: Receiver<AsrCommand>,
    event_tx: async_mpsc::Sender<AsrEvent>,
) -> Result<()> {
    while let Ok(cmd) = cmd_rx.recv() {
        match cmd {
            AsrCommand::SendAudio(pcm) if !state.paused && !state.gated => {
                append_pcm_i16(&mut state.buffer, &pcm);
            }
            AsrCommand::SendAudio(_) => {}
            AsrCommand::SetPaused(on) => state.paused = on,
            AsrCommand::SetGate(on) => {
                state.gated = on;
                if on {
                    state.buffer.clear();
                }
            }
            AsrCommand::ResetBuffer => state.buffer.clear(),
            AsrCommand::FinishUtterance => {
                if state.buffer.is_empty() {
                    continue;
                }
                let samples: Vec<f32> = state
                    .buffer
                    .iter()
                    .map(|&s| s as f32 / i16::MAX as f32)
                    .collect();
                state.buffer.clear();

                let stream = state.recognizer.create_stream();
                stream.accept_waveform(state.sample_rate as i32, &samples);
                state.recognizer.decode(&stream);
                let text = stream
                    .get_result()
                    .map(|r| r.text.trim().to_string())
                    .unwrap_or_default();

                if text.is_empty() {
                    warn!("sherpa asr: empty decode result");
                    continue;
                }

                info!(text = %text, "sherpa asr final");
                let _ = event_tx.blocking_send(AsrEvent::Final { text });
            }
        }
    }
    Ok(())
}

fn append_pcm_i16(buf: &mut Vec<i16>, pcm: &[u8]) {
    let mut iter = pcm.chunks_exact(2);
    for chunk in iter.by_ref() {
        buf.push(i16::from_le_bytes([chunk[0], chunk[1]]));
    }
}
