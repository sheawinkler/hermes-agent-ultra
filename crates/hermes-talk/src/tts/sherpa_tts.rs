//! Offline Kokoro TTS via sherpa-onnx (Windows / x86 CPU).

use async_trait::async_trait;
use sherpa_onnx::{GenerationConfig, OfflineTts, OfflineTtsConfig, OfflineTtsKokoroModelConfig};
use tokio::sync::{mpsc, oneshot};
use tracing::{error, info};

use crate::config::SherpaTtsConfig;
use crate::error::{DemoError, Result};
use crate::tts::{TtsEngine, bailian::TtsAudio};

enum TtsCommand {
    AppendText {
        text: String,
        done: oneshot::Sender<Result<()>>,
    },
    FinishTurn(oneshot::Sender<Result<()>>),
    InterruptTurn(oneshot::Sender<Result<()>>),
}

pub struct SherpaTts {
    cmd_tx: mpsc::Sender<TtsCommand>,
}

impl SherpaTts {
    pub async fn connect(cfg: &SherpaTtsConfig) -> Result<(Self, mpsc::Receiver<TtsAudio>)> {
        let (audio_tx, audio_rx) = mpsc::channel(128);
        let (cmd_tx, cmd_rx) = mpsc::channel::<TtsCommand>(32);
        let cfg = cfg.clone();

        tokio::task::spawn_blocking(move || {
            if let Err(e) = run_kokoro_driver(cfg, cmd_rx, audio_tx) {
                error!(error = %e, "sherpa kokoro tts driver exited");
            }
        });

        Ok((Self { cmd_tx }, audio_rx))
    }
}

#[async_trait]
impl TtsEngine for SherpaTts {
    async fn warmup(&self) -> Result<()> {
        Ok(())
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
        match tokio::time::timeout(std::time::Duration::from_secs(120), rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(e)) => Err(DemoError::Tts(e.to_string())),
            Err(_) => Err(DemoError::Tts("kokoro finish-turn timeout".into())),
        }
    }

    async fn interrupt_turn(&self) -> Result<()> {
        let (tx, rx) = oneshot::channel();
        self.cmd_tx
            .send(TtsCommand::InterruptTurn(tx))
            .await
            .map_err(|e| DemoError::Tts(e.to_string()))?;
        rx.await.map_err(|e| DemoError::Tts(e.to_string()))?
    }
}

fn run_kokoro_driver(
    cfg: SherpaTtsConfig,
    mut cmd_rx: mpsc::Receiver<TtsCommand>,
    audio_tx: mpsc::Sender<TtsAudio>,
) -> Result<()> {
    let kokoro = OfflineTtsKokoroModelConfig {
        model: Some(cfg.model.clone()),
        voices: Some(cfg.voices.clone()),
        tokens: Some(cfg.tokens.clone()),
        data_dir: Some(cfg.data_dir.clone()),
        dict_dir: Some(cfg.dict_dir.clone()),
        lexicon: Some(cfg.lexicon.clone()),
        length_scale: cfg.length_scale,
        lang: cfg.lang.clone(),
    };

    let tts_config = OfflineTtsConfig {
        model: sherpa_onnx::OfflineTtsModelConfig {
            kokoro,
            num_threads: cfg.num_threads,
            provider: Some(cfg.provider.clone()),
            debug: false,
            ..Default::default()
        },
        ..Default::default()
    };

    let tts = OfflineTts::create(&tts_config).ok_or_else(|| {
        DemoError::Config(format!(
            "failed to create Kokoro TTS (check tts.sherpa model paths): model={}",
            cfg.model
        ))
    })?;

    info!(
        model = %cfg.model,
        sample_rate = tts.sample_rate(),
        speakers = tts.num_speakers(),
        sid = cfg.sid,
        "sherpa Kokoro TTS ready"
    );

    let mut text_buf = String::new();

    while let Some(cmd) = cmd_rx.blocking_recv() {
        match cmd {
            TtsCommand::AppendText { text, done } => {
                text_buf.push_str(&text);
                let _ = done.send(Ok(()));
            }
            TtsCommand::FinishTurn(done) => {
                if text_buf.is_empty() {
                    let _ = done.send(Ok(()));
                    continue;
                }
                let text = std::mem::take(&mut text_buf);
                let result = synthesize_turn(&tts, &cfg, &text, &audio_tx);
                let _ = done.send(result);
            }
            TtsCommand::InterruptTurn(done) => {
                text_buf.clear();
                let _ = done.send(Ok(()));
            }
        }
    }
    Ok(())
}

fn synthesize_turn(
    tts: &OfflineTts,
    cfg: &SherpaTtsConfig,
    text: &str,
    audio_tx: &mpsc::Sender<TtsAudio>,
) -> Result<()> {
    let gen_config = GenerationConfig {
        sid: cfg.sid,
        speed: cfg.speed,
        ..Default::default()
    };

    let audio = tts
        .generate_with_config(text, &gen_config, Option::<fn(&[f32], f32) -> bool>::None)
        .ok_or_else(|| DemoError::Tts("kokoro generate failed".into()))?;

    let pcm = f32_to_i16_pcm_bytes(audio.samples());
    if !pcm.is_empty() {
        let _ = audio_tx.blocking_send(TtsAudio { pcm });
    }
    Ok(())
}

fn f32_to_i16_pcm_bytes(samples: &[f32]) -> Vec<u8> {
    samples
        .iter()
        .flat_map(|&s| {
            let clamped = s.clamp(-1.0, 1.0);
            let i = (clamped * i16::MAX as f32) as i16;
            i.to_le_bytes()
        })
        .collect()
}
