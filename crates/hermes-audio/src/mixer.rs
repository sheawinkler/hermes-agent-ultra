//! `DualTrackMixer` — interleaves mic and loopback frames with channel tags.
//!
//! The mixer drives two `AudioCaptureSource` instances concurrently and
//! produces a stream of `TaggedFrame` values.  Downstream consumers see a
//! single chronologically-ordered stream while retaining the original channel
//! identity for speaker attribution.
//!
//! # Design
//!
//! Both sources are polled via `tokio::select!` so whichever produces data
//! first is forwarded immediately. When one source closes, the mixer continues
//! with the remaining source (graceful single-source degradation).

use std::sync::Arc;

use tokio::sync::mpsc;
use tracing::debug;

use crate::capture::AudioCaptureSource;
use crate::frame::{AudioChannel, TaggedFrame};

/// Concurrently reads from a mic source and a loopback source, producing a
/// merged stream of `TaggedFrame` values.
pub struct DualTrackMixer {
    mic: Arc<dyn AudioCaptureSource>,
    loopback: Arc<dyn AudioCaptureSource>,
}

impl DualTrackMixer {
    pub fn new(
        mic: Arc<dyn AudioCaptureSource>,
        loopback: Arc<dyn AudioCaptureSource>,
    ) -> Self {
        Self { mic, loopback }
    }

    /// Start mixing and return a `Receiver` that yields `TaggedFrame` values.
    ///
    /// The mixer spawns two background tasks (one per source).  When both
    /// sources are exhausted, the sender is dropped and the receiver returns
    /// `None`, signalling end-of-stream.
    ///
    /// `buffer` controls the channel capacity (default 64 is fine for most
    /// meeting-recorder use cases).
    pub fn into_stream(self, buffer: usize) -> mpsc::Receiver<TaggedFrame> {
        let (tx, rx) = mpsc::channel(buffer.max(8));

        let mic = self.mic;
        let loopback = self.loopback;

        // Mic task
        let tx_mic = tx.clone();
        tokio::spawn(async move {
            loop {
                match mic.read_chunk().await {
                    Some(samples) => {
                        let sr = mic.sample_rate();
                        let frame = TaggedFrame::new(AudioChannel::Mic, samples, sr);
                        if tx_mic.send(frame).await.is_err() {
                            debug!("DualTrackMixer: mic receiver dropped, stopping mic task");
                            break;
                        }
                    }
                    None => {
                        debug!("DualTrackMixer: mic source exhausted");
                        break;
                    }
                }
            }
        });

        // Loopback task
        let tx_lb = tx;
        tokio::spawn(async move {
            loop {
                match loopback.read_chunk().await {
                    Some(samples) => {
                        let sr = loopback.sample_rate();
                        let frame = TaggedFrame::new(AudioChannel::Loopback, samples, sr);
                        if tx_lb.send(frame).await.is_err() {
                            debug!("DualTrackMixer: loopback receiver dropped, stopping loopback task");
                            break;
                        }
                    }
                    None => {
                        debug!("DualTrackMixer: loopback source exhausted");
                        break;
                    }
                }
            }
        });

        rx
    }

    /// Convenience: create a mixer with only a mic source (loopback = silent stub).
    ///
    /// Useful when loopback capture is unavailable (e.g. macOS without
    /// ScreenCaptureKit permission) — the mixer still produces frames, just
    /// only from the mic channel.
    pub fn mic_only(mic: Arc<dyn AudioCaptureSource>) -> Self {
        let sr = mic.sample_rate();
        let silent = Arc::new(SilentSource::new(sr));
        Self::new(mic, silent)
    }
}

// ---------------------------------------------------------------------------
// Internal: always-silent loopback stub
// ---------------------------------------------------------------------------

struct SilentSource {
    sample_rate: u32,
}

impl SilentSource {
    fn new(sample_rate: u32) -> Self {
        Self { sample_rate }
    }
}

#[async_trait::async_trait]
impl AudioCaptureSource for SilentSource {
    async fn read_chunk(&self) -> Option<Vec<f32>> {
        // Never produce frames — the mixer will keep running on mic alone.
        // Sleep briefly to avoid busy-looping.
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        // Return an empty slice so we don't terminate the loopback task
        // (returning None would drop that half of the sender).
        Some(vec![])
    }

    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    fn channels(&self) -> u16 {
        1
    }

    fn label(&self) -> &str {
        "silent_loopback"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capture::PcmReplaySource;

    #[tokio::test]
    async fn mixer_interleaves_both_channels() {
        let mic_pcm = vec![0.1f32; 32];
        let lb_pcm = vec![0.2f32; 32];

        let mic = Arc::new(PcmReplaySource::new("mic", 16_000, mic_pcm, 16));
        let lb = Arc::new(PcmReplaySource::new("loopback", 16_000, lb_pcm, 16));

        let mixer = DualTrackMixer::new(mic, lb);
        let mut rx = mixer.into_stream(16);

        let mut got_mic = false;
        let mut got_loopback = false;
        while let Some(frame) = rx.recv().await {
            match frame.channel {
                AudioChannel::Mic => got_mic = true,
                AudioChannel::Loopback => got_loopback = true,
            }
        }
        assert!(got_mic, "expected mic frames");
        assert!(got_loopback, "expected loopback frames");
    }

    #[tokio::test]
    async fn mic_only_mixer_produces_mic_frames() {
        let mic_pcm = vec![0.5f32; 64];
        let mic = Arc::new(PcmReplaySource::new("mic", 16_000, mic_pcm, 32));
        let mixer = DualTrackMixer::mic_only(mic);
        let mut rx = mixer.into_stream(8);

        let mut count = 0usize;
        while let Some(frame) = rx.recv().await {
            assert_eq!(frame.channel, AudioChannel::Mic);
            count += 1;
            if count >= 2 {
                break;
            }
        }
        assert_eq!(count, 2);
    }
}
