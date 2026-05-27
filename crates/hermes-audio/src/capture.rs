//! `AudioCaptureSource` trait — the unified abstraction for any audio input.
//!
//! Implementors:
//! - CLI mic source (wraps `cpal` device)
//! - Loopback source (WASAPI loopback on Windows, virtual device on macOS)
//! - Test stub (in-memory PCM replay)

use async_trait::async_trait;

/// A source of raw PCM audio frames.
///
/// All implementations produce **mono, f32 normalized to [-1, 1]** samples at
/// a fixed sample rate.  The caller is responsible for resampling if mixing
/// sources with different rates.
#[async_trait]
pub trait AudioCaptureSource: Send + Sync {
    /// Read the next chunk of PCM audio samples.
    ///
    /// Returns `None` when the source has been closed or exhausted (e.g. end
    /// of a pre-recorded file in tests).
    async fn read_chunk(&self) -> Option<Vec<f32>>;

    /// Sample rate in Hz (e.g. 16_000 or 44_100).
    fn sample_rate(&self) -> u32;

    /// Number of channels produced by this source.
    /// Convention: callers should mono-mix multi-channel output before VAD.
    fn channels(&self) -> u16;

    /// Human-readable label used in logs and transcripts (e.g. "mic", "loopback").
    fn label(&self) -> &str;
}

// ---------------------------------------------------------------------------
// Test / stub implementations
// ---------------------------------------------------------------------------

/// A replay source that feeds pre-recorded PCM samples.  Useful in unit tests
/// and parity fixtures.
pub struct PcmReplaySource {
    label: String,
    sample_rate: u32,
    chunks: tokio::sync::Mutex<std::collections::VecDeque<Vec<f32>>>,
}

impl PcmReplaySource {
    /// Create from a flat PCM buffer, split into `chunk_size`-sample chunks.
    pub fn new(label: impl Into<String>, sample_rate: u32, pcm: Vec<f32>, chunk_size: usize) -> Self {
        let chunks: std::collections::VecDeque<Vec<f32>> = pcm
            .chunks(chunk_size.max(1))
            .map(|c| c.to_vec())
            .collect();
        Self {
            label: label.into(),
            sample_rate,
            chunks: tokio::sync::Mutex::new(chunks),
        }
    }
}

#[async_trait]
impl AudioCaptureSource for PcmReplaySource {
    async fn read_chunk(&self) -> Option<Vec<f32>> {
        self.chunks.lock().await.pop_front()
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn channels(&self) -> u16 {
        1
    }

    fn label(&self) -> &str {
        &self.label
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn replay_source_yields_chunks_then_none() {
        let pcm = vec![0.1f32; 100];
        let src = PcmReplaySource::new("test", 16_000, pcm, 32);
        // first 3 full chunks + one partial
        assert!(src.read_chunk().await.is_some());
        assert!(src.read_chunk().await.is_some());
        assert!(src.read_chunk().await.is_some());
        assert!(src.read_chunk().await.is_some()); // partial (4 samples)
        assert!(src.read_chunk().await.is_none());
    }
}
