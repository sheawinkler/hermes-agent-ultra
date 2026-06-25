use sherpa_onnx::{
    OfflineSpeechDenoiserDpdfNetModelConfig, OfflineSpeechDenoiserGtcrnModelConfig,
    OfflineSpeechDenoiserModelConfig, OnlineSpeechDenoiser, OnlineSpeechDenoiserConfig,
};
use tracing::info;

use crate::config::DenoiseConfig;

pub struct StreamingDenoiser {
    inner: OnlineSpeechDenoiser,
}

impl StreamingDenoiser {
    pub fn create(cfg: &DenoiseConfig) -> Option<Self> {
        if !cfg.enabled {
            return None;
        }
        let model_path = cfg.resolve_model_path();
        if model_path.is_empty() || !std::path::Path::new(&model_path).exists() {
            tracing::warn!(%model_path, "denoise model not found, denoise disabled");
            return None;
        }

        let is_gtcrn = cfg.variant.starts_with("gtcrn");
        let model = OfflineSpeechDenoiserModelConfig {
            dpdfnet: if !is_gtcrn {
                OfflineSpeechDenoiserDpdfNetModelConfig {
                    model: Some(model_path.clone()),
                }
            } else {
                Default::default()
            },
            gtcrn: if is_gtcrn {
                OfflineSpeechDenoiserGtcrnModelConfig {
                    model: Some(model_path.clone()),
                }
            } else {
                Default::default()
            },
            provider: Some(cfg.provider.clone()),
            ..Default::default()
        };

        let config = OnlineSpeechDenoiserConfig { model };
        let inner = OnlineSpeechDenoiser::create(&config)?;

        info!(
            model = %model_path,
            variant = %cfg.variant,
            sample_rate = inner.sample_rate(),
            frame_shift = inner.frame_shift_in_samples(),
            "denoiser started"
        );
        Some(Self { inner })
    }

    /// Process an audio chunk. Returns denoised f32 samples.
    /// The output may be shorter or longer than input due to frame boundaries;
    /// buffering is handled internally by the denoiser.
    pub fn process(&mut self, samples: &[f32], sample_rate: u32) -> Vec<f32> {
        let wave = self.inner.run(samples, sample_rate as i32);
        wave.samples
    }

    /// Flush remaining frames at end of stream.
    pub fn flush(&mut self) -> Vec<f32> {
        self.inner.flush().samples
    }

    pub fn sample_rate(&self) -> u32 {
        self.inner.sample_rate() as u32
    }
}
