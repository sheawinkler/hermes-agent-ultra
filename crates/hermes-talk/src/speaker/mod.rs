use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sherpa_onnx::{SpeakerEmbeddingExtractor, SpeakerEmbeddingExtractorConfig};
use tracing::{info, warn};

use crate::config::SpeakerConfig;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct VoiceprintData {
    embedding: Vec<f32>,
    dim: usize,
}

pub struct SpeakerVerifier {
    extractor: SpeakerEmbeddingExtractor,
    voiceprint: Option<Vec<f32>>,
    threshold: f32,
}

impl SpeakerVerifier {
    pub fn create(cfg: &SpeakerConfig) -> Option<Self> {
        if !cfg.enabled {
            return None;
        }
        let model_path = cfg.resolve_model_path();
        if model_path.is_empty() || !Path::new(&model_path).exists() {
            warn!(%model_path, "speaker model not found, speaker verification disabled");
            return None;
        }

        let config = SpeakerEmbeddingExtractorConfig {
            model: Some(model_path.clone()),
            num_threads: 1,
            debug: false,
            provider: Some(cfg.provider.clone()),
        };
        let extractor = SpeakerEmbeddingExtractor::create(&config)?;

        let voiceprint = load_voiceprint(&cfg.voiceprint_path);
        let status = if voiceprint.is_some() {
            "voiceprint loaded"
        } else {
            "no voiceprint; run `hermes talk enroll` first"
        };
        info!(
            model = %model_path,
            dim = extractor.dim(),
            threshold = cfg.threshold,
            voiceprint_path = %cfg.voiceprint_path,
            "{status}"
        );

        Some(Self {
            extractor,
            voiceprint,
            threshold: cfg.threshold,
        })
    }

    pub fn has_voiceprint(&self) -> bool {
        self.voiceprint.is_some()
    }

    /// Extract speaker embedding from audio samples.
    pub fn extract_embedding(&self, samples: &[f32], sample_rate: u32) -> Option<Vec<f32>> {
        let stream = self.extractor.create_stream()?;
        stream.accept_waveform(sample_rate as i32, samples);
        stream.input_finished();
        if !self.extractor.is_ready(&stream) {
            warn!("speaker audio too short for embedding extraction");
            return None;
        }
        self.extractor.compute(&stream)
    }

    /// Verify that the audio matches the enrolled voiceprint.
    /// Returns true if the speaker matches, false otherwise.
    /// If no voiceprint is enrolled, returns true (pass-through mode).
    pub fn verify(&self, samples: &[f32], sample_rate: u32) -> bool {
        let Some(ref voiceprint) = self.voiceprint else {
            return true;
        };
        let Some(embedding) = self.extract_embedding(samples, sample_rate) else {
            return false;
        };
        let score = cosine_similarity(voiceprint, &embedding);
        let passed = score >= self.threshold;
        if !passed {
            info!(
                score,
                threshold = self.threshold,
                "speaker verification rejected"
            );
        }
        passed
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    assert_eq!(a.len(), b.len(), "embedding dimensions must match");
    let (dot, norm_a, norm_b) = a
        .iter()
        .zip(b.iter())
        .fold((0.0f32, 0.0f32, 0.0f32), |(d, na, nb), (&x, &y)| {
            (d + x * y, na + x * x, nb + y * y)
        });
    let denom = (norm_a * norm_b).sqrt();
    if denom < f32::EPSILON {
        return 0.0;
    }
    dot / denom
}

/// Enroll a voiceprint from audio samples and save to file.
pub fn enroll_voiceprint(
    cfg: &SpeakerConfig,
    samples: &[f32],
    sample_rate: u32,
) -> Result<(), String> {
    let model_path = cfg.resolve_model_path();
    if model_path.is_empty() || !Path::new(&model_path).exists() {
        return Err(format!("speaker model not found: {model_path}"));
    }

    let config = SpeakerEmbeddingExtractorConfig {
        model: Some(model_path.clone()),
        num_threads: 1,
        debug: false,
        provider: Some(cfg.provider.clone()),
    };
    let extractor = SpeakerEmbeddingExtractor::create(&config)
        .ok_or_else(|| "failed to create SpeakerEmbeddingExtractor".to_string())?;

    let embedding = {
        let stream = extractor
            .create_stream()
            .ok_or_else(|| "failed to create speaker stream".to_string())?;
        stream.accept_waveform(sample_rate as i32, samples);
        stream.input_finished();
        if !extractor.is_ready(&stream) {
            return Err(
                "audio too short for voiceprint enrollment (need ~2-3s of speech)".to_string(),
            );
        }
        extractor
            .compute(&stream)
            .ok_or_else(|| "failed to compute embedding".to_string())?
    };

    let data = VoiceprintData {
        dim: embedding.len(),
        embedding,
    };
    let json = serde_json::to_string_pretty(&data).map_err(|e| e.to_string())?;
    fs::write(&cfg.voiceprint_path, json).map_err(|e| e.to_string())?;
    info!(
        path = %cfg.voiceprint_path,
        dim = data.dim,
        "voiceprint enrolled successfully"
    );
    Ok(())
}

fn load_voiceprint(path: &str) -> Option<Vec<f32>> {
    let content = fs::read_to_string(path).ok()?;
    let data: VoiceprintData = serde_json::from_str(&content).ok()?;
    if data.embedding.len() != data.dim || data.embedding.is_empty() {
        warn!("voiceprint file corrupted");
        return None;
    }
    Some(data.embedding)
}
