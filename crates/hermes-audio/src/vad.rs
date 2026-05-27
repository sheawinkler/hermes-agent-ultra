//! Voice Activity Detection (VAD) implementations.
//!
//! # Two backends
//!
//! | Feature flag    | Backend                | Notes |
//! |-----------------|------------------------|-------|
//! | `silero-vad`    | Silero ONNX model      | High accuracy, requires `assets/silero_vad.onnx` |
//! | *(default)*     | RMS + ZCR energy model | Lightweight, no model file needed |
//!
//! Both backends implement `VadBackend`, so call sites are identical.
//!
//! # Silero VAD model
//!
//! Download from: <https://github.com/snakers4/silero-vad/raw/master/files/silero_vad.onnx>
//! and place at `assets/silero_vad.onnx` (relative to the binary's working directory) or
//! set `SILERO_VAD_MODEL_PATH` env var.

use serde::{Deserialize, Serialize};
#[allow(unused_imports)]
use tracing::{debug, warn};

// ---------------------------------------------------------------------------
// Shared config
// ---------------------------------------------------------------------------

/// Tuning parameters shared by both VAD backends.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VadConfig {
    /// Probability threshold above which a frame is considered speech (Silero).
    /// For the energy backend this maps to `energy_threshold`.
    pub threshold: f32,
    /// Minimum consecutive voiced frames before speech_active → true.
    pub min_speech_frames: usize,
    /// Silence duration (ms) after last speech before speech_active → false.
    pub silence_timeout_ms: u64,
    /// Frame size in samples (e.g. 512 @ 16kHz = 32ms — Silero default).
    pub frame_size: usize,
    /// Zero-crossing rate upper bound (energy backend only).
    pub max_zcr: f32,
}

impl Default for VadConfig {
    fn default() -> Self {
        Self {
            threshold: 0.5,
            min_speech_frames: 3,
            silence_timeout_ms: 800,
            frame_size: 512,
            max_zcr: 0.5,
        }
    }
}

impl VadConfig {
    pub fn for_meeting() -> Self {
        Self {
            threshold: 0.4,
            min_speech_frames: 5,
            // longer timeout suits meeting pauses (thinking, PPT switching)
            silence_timeout_ms: 1500,
            frame_size: 512,
            max_zcr: 0.6,
        }
    }
}

// ---------------------------------------------------------------------------
// VadBackend trait
// ---------------------------------------------------------------------------

/// Stateful frame-by-frame VAD processor.
pub trait VadBackend: Send {
    /// Process one frame of mono f32 PCM (normalized to [-1,1]).
    /// Returns `true` if the detector considers speech currently active.
    fn process_frame(&mut self, samples: &[f32]) -> bool;

    /// Reset internal state (e.g. between utterances or recording sessions).
    fn reset(&mut self);

    /// Whether speech is currently active.
    fn is_speech_active(&self) -> bool;
}

// ---------------------------------------------------------------------------
// Energy-based backend (always available)
// ---------------------------------------------------------------------------

/// Lightweight RMS + zero-crossing VAD.  No external dependencies.
pub struct EnergyVad {
    config: VadConfig,
    consecutive_speech: usize,
    speech_active: bool,
    last_speech_ms: Option<std::time::Instant>,
}

impl EnergyVad {
    pub fn new(config: VadConfig) -> Self {
        Self {
            config,
            consecutive_speech: 0,
            speech_active: false,
            last_speech_ms: None,
        }
    }

    fn rms(samples: &[f32]) -> f32 {
        if samples.is_empty() {
            return 0.0;
        }
        let sum_sq: f32 = samples.iter().map(|s| s * s).sum();
        (sum_sq / samples.len() as f32).sqrt()
    }

    fn zcr(samples: &[f32]) -> f32 {
        if samples.len() < 2 {
            return 0.0;
        }
        let crossings = samples
            .windows(2)
            .filter(|w| (w[0] >= 0.0) != (w[1] >= 0.0))
            .count();
        crossings as f32 / (samples.len() - 1) as f32
    }
}

impl VadBackend for EnergyVad {
    fn process_frame(&mut self, samples: &[f32]) -> bool {
        if samples.is_empty() {
            return self.speech_active;
        }
        let rms = Self::rms(samples);
        let zcr = Self::zcr(samples);
        let is_voiced = rms >= self.config.threshold && zcr <= self.config.max_zcr;

        if is_voiced {
            self.consecutive_speech += 1;
            self.last_speech_ms = Some(std::time::Instant::now());
            if self.consecutive_speech >= self.config.min_speech_frames {
                self.speech_active = true;
            }
        } else {
            self.consecutive_speech = 0;
            if self.speech_active
                && let Some(last) = self.last_speech_ms
                && last.elapsed() > std::time::Duration::from_millis(self.config.silence_timeout_ms)
            {
                self.speech_active = false;
            }
        }
        self.speech_active
    }

    fn reset(&mut self) {
        self.consecutive_speech = 0;
        self.speech_active = false;
        self.last_speech_ms = None;
    }

    fn is_speech_active(&self) -> bool {
        self.speech_active
    }
}

// ---------------------------------------------------------------------------
// Silero ONNX backend (feature = "silero-vad")
// ---------------------------------------------------------------------------

#[cfg(feature = "silero-vad")]
mod silero {
    use super::{VadBackend, VadConfig};
    use ort::{inputs, session::Session};
    use tracing::{debug, warn};

    const DEFAULT_MODEL_PATH: &str = "assets/silero_vad.onnx";
    const SAMPLE_RATE: i64 = 16_000;

    pub struct SileroVad {
        session: Session,
        config: VadConfig,
        h: Vec<f32>,
        c: Vec<f32>,
        speech_active: bool,
        consecutive_speech: usize,
        last_speech_ms: Option<std::time::Instant>,
    }

    impl SileroVad {
        /// Load the Silero VAD model from `path` (or env `SILERO_VAD_MODEL_PATH`).
        pub fn load(config: VadConfig) -> Result<Self, String> {
            let model_path = std::env::var("SILERO_VAD_MODEL_PATH")
                .unwrap_or_else(|_| DEFAULT_MODEL_PATH.to_string());

            if !std::path::Path::new(&model_path).exists() {
                return Err(format!(
                    "Silero VAD model not found at '{model_path}'. \
                     Download from https://github.com/snakers4/silero-vad/raw/master/files/silero_vad.onnx \
                     and set SILERO_VAD_MODEL_PATH or place at assets/silero_vad.onnx"
                ));
            }

            let session = Session::builder()
                .map_err(|e| e.to_string())?
                .commit_from_file(&model_path)
                .map_err(|e| e.to_string())?;

            debug!("SileroVad: loaded model from {model_path}");
            Ok(Self {
                session,
                config,
                // Silero v4 hidden state: 2 x 1 x 64
                h: vec![0.0f32; 2 * 1 * 64],
                c: vec![0.0f32; 2 * 1 * 64],
                speech_active: false,
                consecutive_speech: 0,
                last_speech_ms: None,
            })
        }

        fn run_inference(&mut self, frame: &[f32]) -> f32 {
            let input_tensor = ndarray::Array2::from_shape_vec(
                (1, frame.len()),
                frame.to_vec(),
            );
            let h_tensor = ndarray::Array3::from_shape_vec(
                (2, 1, 64),
                self.h.clone(),
            );
            let c_tensor = ndarray::Array3::from_shape_vec(
                (2, 1, 64),
                self.c.clone(),
            );
            let sr_tensor = ndarray::Array1::from_vec(vec![SAMPLE_RATE]);

            let (input_t, h_t, c_t, sr_t) = match (input_tensor, h_tensor, c_tensor) {
                (Ok(i), Ok(h), Ok(c)) => (i, h, c, sr_tensor),
                _ => {
                    warn!("SileroVad: tensor shape mismatch");
                    return 0.0;
                }
            };

            let outputs = self.session.run(
                inputs![input_t, sr_t, h_t, c_t].unwrap_or_default()
            );
            match outputs {
                Ok(outs) => {
                    // Output order: speech_prob, hn, cn
                    if let Some(prob_tensor) = outs.get(0) {
                        if let Ok(view) = prob_tensor.try_extract_tensor::<f32>() {
                            let prob = view.iter().next().copied().unwrap_or(0.0);
                            // Update hidden states
                            if let (Some(hn), Some(cn)) = (outs.get(1), outs.get(2)) {
                                if let (Ok(hv), Ok(cv)) = (
                                    hn.try_extract_tensor::<f32>(),
                                    cn.try_extract_tensor::<f32>(),
                                ) {
                                    self.h = hv.iter().copied().collect();
                                    self.c = cv.iter().copied().collect();
                                }
                            }
                            return prob;
                        }
                    }
                    0.0
                }
                Err(e) => {
                    warn!("SileroVad inference error: {e}");
                    0.0
                }
            }
        }
    }

    impl VadBackend for SileroVad {
        fn process_frame(&mut self, samples: &[f32]) -> bool {
            if samples.is_empty() {
                return self.speech_active;
            }

            let prob = self.run_inference(samples);
            let is_speech = prob >= self.config.threshold;

            if is_speech {
                self.consecutive_speech += 1;
                self.last_speech_ms = Some(std::time::Instant::now());
                if self.consecutive_speech >= self.config.min_speech_frames {
                    self.speech_active = true;
                }
            } else {
                self.consecutive_speech = 0;
                if self.speech_active {
                    if let Some(last) = self.last_speech_ms {
                        if last.elapsed()
                            > std::time::Duration::from_millis(self.config.silence_timeout_ms)
                        {
                            self.speech_active = false;
                        }
                    }
                }
            }
            self.speech_active
        }

        fn reset(&mut self) {
            self.h = vec![0.0f32; 2 * 1 * 64];
            self.c = vec![0.0f32; 2 * 1 * 64];
            self.speech_active = false;
            self.consecutive_speech = 0;
            self.last_speech_ms = None;
        }

        fn is_speech_active(&self) -> bool {
            self.speech_active
        }
    }
}

#[cfg(feature = "silero-vad")]
pub use silero::SileroVad;

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Create the best available VAD backend for the given config.
///
/// With `silero-vad` feature and model file present: returns `SileroVad`.
/// Otherwise falls back to `EnergyVad` with a warning.
pub fn create_vad(config: VadConfig) -> Box<dyn VadBackend> {
    #[cfg(feature = "silero-vad")]
    {
        match silero::SileroVad::load(config.clone()) {
            Ok(vad) => {
                debug!("Using Silero VAD backend");
                return Box::new(vad);
            }
            Err(e) => {
                warn!("Silero VAD unavailable ({e}), falling back to energy VAD");
            }
        }
    }
    debug!("Using energy (RMS+ZCR) VAD backend");
    Box::new(EnergyVad::new(config))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn energy_vad_silence_stays_false() {
        let cfg = VadConfig { threshold: 0.02, min_speech_frames: 3, ..Default::default() };
        let mut vad = EnergyVad::new(cfg);
        let silent = vec![0.0f32; 512];
        for _ in 0..10 {
            assert!(!vad.process_frame(&silent));
        }
    }

    #[test]
    fn energy_vad_loud_activates() {
        let cfg = VadConfig {
            threshold: 0.01,
            min_speech_frames: 2,
            silence_timeout_ms: 800,
            frame_size: 512,
            max_zcr: 1.0, // allow any ZCR
        };
        let mut vad = EnergyVad::new(cfg);
        let loud = vec![0.5f32; 512];
        vad.process_frame(&loud);
        vad.process_frame(&loud);
        assert!(vad.is_speech_active());
    }

    #[test]
    fn energy_vad_reset_clears_state() {
        let cfg = VadConfig {
            threshold: 0.01,
            min_speech_frames: 1,
            silence_timeout_ms: 800,
            frame_size: 512,
            max_zcr: 1.0,
        };
        let mut vad = EnergyVad::new(cfg);
        let loud = vec![0.5f32; 512];
        vad.process_frame(&loud);
        assert!(vad.is_speech_active());
        vad.reset();
        assert!(!vad.is_speech_active());
    }

    #[test]
    fn create_vad_returns_energy_fallback_without_model() {
        let vad = create_vad(VadConfig::default());
        // Just ensure it compiles and runs without panic
        drop(vad);
    }
}
