use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::thread::{self, JoinHandle};

use sherpa_onnx::{KeywordSpotter, KeywordSpotterConfig};
use tracing::{error, info, warn};

use crate::config::WakeConfig;
use crate::error::{DemoError, Result};
use crate::kws::keywords::encode_phrases;

pub struct WakeDetectorHandle {
    pcm_tx: SyncSender<Vec<f32>>,
    wake_rx: Receiver<()>,
    dropped_frames: AtomicU64,
    _thread: JoinHandle<()>,
}

impl WakeDetectorHandle {
    pub fn feed(&self, samples: &[f32]) {
        if samples.is_empty() {
            return;
        }
        match self.pcm_tx.try_send(samples.to_vec()) {
            Ok(()) => {}
            Err(mpsc::TrySendError::Full(_)) => {
                let dropped = self.dropped_frames.fetch_add(1, Ordering::Relaxed) + 1;
                if dropped == 1 || dropped % 20 == 0 {
                    warn!(
                        dropped,
                        "kws audio queue full; blocking (wake word may lag — consider wake.provider=cpu)"
                    );
                }
                let _ = self.pcm_tx.send(samples.to_vec());
            }
            Err(mpsc::TrySendError::Disconnected(_)) => {}
        }
    }

    pub fn try_recv_wake(&self) -> bool {
        self.wake_rx.try_recv().is_ok()
    }
}

pub fn start_wake_detector(cfg: &WakeConfig, sample_rate: u32) -> Result<WakeDetectorHandle> {
    let keywords_buf = encode_phrases(cfg)?;
    let phrases = cfg.effective_phrases();

    let mut kws_cfg = KeywordSpotterConfig::default();
    kws_cfg.model_config.transducer.encoder = Some(cfg.encoder.clone());
    kws_cfg.model_config.transducer.decoder = Some(cfg.decoder.clone());
    kws_cfg.model_config.transducer.joiner = Some(cfg.joiner.clone());
    kws_cfg.model_config.tokens = Some(cfg.tokens.clone());
    kws_cfg.model_config.provider = Some(cfg.provider.clone());
    kws_cfg.model_config.num_threads = cfg.num_threads;
    kws_cfg.keywords_buf = Some(keywords_buf);
    kws_cfg.keywords_score = cfg.boost_score;
    kws_cfg.keywords_threshold = cfg.trigger_threshold;

    let spotter = KeywordSpotter::create(&kws_cfg).ok_or_else(|| {
        DemoError::Config("failed to create KeywordSpotter (check model paths)".into())
    })?;

    let (pcm_tx, pcm_rx) = mpsc::sync_channel::<Vec<f32>>(256);
    let (wake_tx, wake_rx) = mpsc::channel();

    let phrase = phrases.join(", ");
    let thread = thread::spawn(move || {
        if let Err(e) = run_kws_loop(spotter, sample_rate, pcm_rx, wake_tx) {
            error!(error = %e, "kws thread exited");
        }
    });

    info!(phrase = %phrase, provider = %cfg.provider, "wake word detector started");
    Ok(WakeDetectorHandle {
        pcm_tx,
        wake_rx,
        dropped_frames: AtomicU64::new(0),
        _thread: thread,
    })
}

fn run_kws_loop(
    spotter: KeywordSpotter,
    sample_rate: u32,
    pcm_rx: mpsc::Receiver<Vec<f32>>,
    wake_tx: mpsc::Sender<()>,
) -> Result<()> {
    let stream = spotter.create_stream();
    while let Ok(samples) = pcm_rx.recv() {
        stream.accept_waveform(sample_rate as i32, &samples);
        while spotter.is_ready(&stream) {
            spotter.decode(&stream);
            if let Some(result) = spotter.get_result(&stream) {
                if !result.keyword.is_empty() {
                    info!(keyword = %result.keyword, "wake word detected");
                    let _ = wake_tx.send(());
                    spotter.reset(&stream);
                }
            }
        }
    }
    Ok(())
}
