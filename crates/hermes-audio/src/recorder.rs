//! `MeetingRecorder` — VAD-segmented real-time meeting recorder.
//!
//! Reads from a `DualTrackMixer`, segments audio via VAD, and calls an async
//! STT callback for each speech segment.  The caller receives incremental
//! transcript updates via an `mpsc` channel, enabling live caption display.
//!
//! # Architecture
//!
//! ```text
//! DualTrackMixer ──(TaggedFrame)──► MeetingRecorder
//!                                       │
//!                        per-channel VAD (EnergyVad / SileroVad)
//!                                       │
//!                             speech segment detected
//!                                       │
//!                    async SttCallback (background task)
//!                                       │
//!                         tx.send(TranscriptSegment)
//! ```
//!
//! Call `MeetingRecorder::record()` to start.  It runs until the mixer
//! channel closes (both sources exhausted) or `stop()` is called.

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::frame::{AudioChannel, TaggedFrame};
use crate::vad::{create_vad, VadBackend, VadConfig};

// ---------------------------------------------------------------------------
// Output type
// ---------------------------------------------------------------------------

/// One recognized speech segment from the meeting.
#[derive(Debug, Clone)]
pub struct TranscriptSegment {
    /// "Speaker A" (mic) or "Speaker B" (loopback).
    pub speaker: String,
    pub text: String,
    /// Approximate recording time in seconds from start (best effort).
    pub offset_s: f32,
}

// ---------------------------------------------------------------------------
// STT callback trait
// ---------------------------------------------------------------------------

/// Async callback that converts a PCM buffer into transcript text.
///
/// Implementors typically wrap `SttEngine::transcribe_file` (via a temp WAV)
/// or a WebSocket streaming client.
#[async_trait::async_trait]
pub trait SttCallback: Send + Sync + 'static {
    async fn transcribe(&self, channel: AudioChannel, pcm: Vec<f32>, sample_rate: u32)
        -> Option<String>;
}

// ---------------------------------------------------------------------------
// Per-channel state
// ---------------------------------------------------------------------------

struct ChannelState {
    vad: Box<dyn VadBackend>,
    buffer: Vec<f32>,
    recording: bool,
    silence_start: Option<std::time::Instant>,
}

impl ChannelState {
    fn new(vad_cfg: VadConfig) -> Self {
        Self {
            vad: create_vad(vad_cfg),
            buffer: Vec::new(),
            recording: false,
            silence_start: None,
        }
    }
}

// ---------------------------------------------------------------------------
// MeetingRecorder
// ---------------------------------------------------------------------------

/// Drives a `DualTrackMixer` stream through per-channel VAD and emits
/// `TranscriptSegment` values whenever speech ends.
pub struct MeetingRecorder {
    vad_config: VadConfig,
    stt: Arc<dyn SttCallback>,
    /// Maximum recording length per segment (prevents runaway buffers).
    max_segment_secs: f32,
    stop_flag: Arc<std::sync::atomic::AtomicBool>,
}

impl MeetingRecorder {
    pub fn new(vad_config: VadConfig, stt: Arc<dyn SttCallback>) -> Self {
        Self {
            vad_config,
            stt,
            max_segment_secs: 60.0,
            stop_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Request graceful shutdown.
    pub fn stop(&self) {
        self.stop_flag
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }

    /// Start recording.  Returns a receiver that yields `TranscriptSegment`
    /// values and a `JoinHandle` for the background task.
    ///
    /// `frames_rx`: output of `DualTrackMixer::into_stream()`.
    pub fn record(
        &self,
        mut frames_rx: mpsc::Receiver<TaggedFrame>,
    ) -> (mpsc::Receiver<TranscriptSegment>, tokio::task::JoinHandle<()>) {
        let (seg_tx, seg_rx) = mpsc::channel::<TranscriptSegment>(64);
        let vad_cfg = self.vad_config.clone();
        let stt = Arc::clone(&self.stt);
        let max_secs = self.max_segment_secs;
        let stop = Arc::clone(&self.stop_flag);

        let handle = tokio::spawn(async move {
            let mut channels: HashMap<AudioChannel, ChannelState> = HashMap::new();
            let start = std::time::Instant::now();

            while let Some(frame) = frames_rx.recv().await {
                if stop.load(std::sync::atomic::Ordering::Relaxed) {
                    debug!("MeetingRecorder: stop requested");
                    break;
                }
                if frame.samples.is_empty() {
                    continue;
                }

                let elapsed_s = start.elapsed().as_secs_f32();
                let ch = frame.channel;
                let sample_rate = frame.sample_rate;

                let state = channels
                    .entry(ch)
                    .or_insert_with(|| ChannelState::new(vad_cfg.clone()));

                let is_speech = state.vad.process_frame(&frame.samples);

                if is_speech {
                    state.recording = true;
                    state.silence_start = None;
                    state.buffer.extend_from_slice(&frame.samples);

                    // Safety cap: flush if segment grows too long
                    let seg_secs =
                        state.buffer.len() as f32 / sample_rate as f32;
                    if seg_secs >= max_secs {
                        debug!("MeetingRecorder: max_segment_secs reached on {ch:?}, flushing");
                        let pcm = std::mem::take(&mut state.buffer);
                        state.recording = false;
                        state.vad.reset();
                        let tx = seg_tx.clone();
                        let stt2 = Arc::clone(&stt);
                        let offset = elapsed_s;
                        tokio::spawn(async move {
                            if let Some(text) = stt2.transcribe(ch, pcm, sample_rate).await {
                                let _ = tx.send(TranscriptSegment {
                                    speaker: ch.speaker_label().to_string(),
                                    text,
                                    offset_s: offset,
                                }).await;
                            }
                        });
                    }
                } else if state.recording {
                    // Track silence duration
                    let now = std::time::Instant::now();
                    if state.silence_start.is_none() {
                        state.silence_start = Some(now);
                    }
                    let silence_ms = state
                        .silence_start
                        .map(|t| t.elapsed().as_millis() as u64)
                        .unwrap_or(0);

                    if silence_ms >= vad_cfg.silence_timeout_ms {
                        // Speech ended — flush buffer
                        let pcm = std::mem::take(&mut state.buffer);
                        state.recording = false;
                        state.silence_start = None;

                        if pcm.len() > sample_rate as usize / 4 {
                            // at least 250ms of audio
                            let tx = seg_tx.clone();
                            let stt2 = Arc::clone(&stt);
                            let offset = elapsed_s;
                            tokio::spawn(async move {
                                if let Some(text) =
                                    stt2.transcribe(ch, pcm, sample_rate).await
                                {
                                    let _ = tx.send(TranscriptSegment {
                                        speaker: ch.speaker_label().to_string(),
                                        text,
                                        offset_s: offset,
                                    }).await;
                                }
                            });
                        }
                    }
                }
            }

            // Flush remaining buffers on clean exit
            for (ch, mut state) in channels {
                if !state.buffer.is_empty() {
                    let sample_rate = 16_000u32;
                    let pcm = std::mem::take(&mut state.buffer);
                    let tx = seg_tx.clone();
                    let stt2 = Arc::clone(&stt);
                    tokio::spawn(async move {
                        if let Some(text) = stt2.transcribe(ch, pcm, sample_rate).await {
                            let _ = tx.send(TranscriptSegment {
                                speaker: ch.speaker_label().to_string(),
                                text,
                                offset_s: 0.0,
                            }).await;
                        }
                    });
                }
            }

            info!("MeetingRecorder: stream ended");
        });

        (seg_rx, handle)
    }
}

// ---------------------------------------------------------------------------
// PCM → WAV helper (for SttCallback implementations)
// ---------------------------------------------------------------------------

/// Encode mono f32 PCM as a minimal WAV byte vector (16-bit LE, 1 channel).
pub fn pcm_to_wav(samples: &[f32], sample_rate: u32) -> Vec<u8> {
    let pcm_i16: Vec<i16> = samples
        .iter()
        .map(|s| (s.clamp(-1.0, 1.0) * i16::MAX as f32) as i16)
        .collect();
    let data_bytes: Vec<u8> = pcm_i16
        .iter()
        .flat_map(|s| s.to_le_bytes())
        .collect();
    let data_len = data_bytes.len() as u32;
    let channels: u16 = 1;
    let bits: u16 = 16;
    let byte_rate = sample_rate * channels as u32 * bits as u32 / 8;

    let mut wav = Vec::with_capacity(44 + data_bytes.len());
    wav.extend_from_slice(b"RIFF");
    wav.extend_from_slice(&(36 + data_len).to_le_bytes());
    wav.extend_from_slice(b"WAVE");
    wav.extend_from_slice(b"fmt ");
    wav.extend_from_slice(&16u32.to_le_bytes());
    wav.extend_from_slice(&1u16.to_le_bytes()); // PCM
    wav.extend_from_slice(&channels.to_le_bytes());
    wav.extend_from_slice(&sample_rate.to_le_bytes());
    wav.extend_from_slice(&byte_rate.to_le_bytes());
    wav.extend_from_slice(&(channels * bits / 8).to_le_bytes());
    wav.extend_from_slice(&bits.to_le_bytes());
    wav.extend_from_slice(b"data");
    wav.extend_from_slice(&data_len.to_le_bytes());
    wav.extend_from_slice(&data_bytes);
    wav
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pcm_to_wav_produces_valid_header() {
        let samples = vec![0.0f32; 160];
        let wav = pcm_to_wav(&samples, 16_000);
        assert_eq!(&wav[0..4], b"RIFF");
        assert_eq!(&wav[8..12], b"WAVE");
        assert_eq!(&wav[12..16], b"fmt ");
    }

    struct NullStt;
    #[async_trait::async_trait]
    impl SttCallback for NullStt {
        async fn transcribe(
            &self,
            _ch: AudioChannel,
            _pcm: Vec<f32>,
            _sr: u32,
        ) -> Option<String> {
            Some("test".to_string())
        }
    }

    #[tokio::test]
    async fn recorder_emits_segment_from_loud_frames() {
        use crate::capture::PcmReplaySource;
        use crate::frame::AudioChannel;
        use crate::mixer::DualTrackMixer;
        use std::sync::Arc;

        // 2s of loud audio at 16kHz → should trigger speech → segment
        let loud = vec![0.8f32; 16_000 * 2];
        let silent = vec![0.0f32; 16_000];
        let mic = Arc::new(PcmReplaySource::new("mic", 16_000, loud, 512));
        let lb = Arc::new(PcmReplaySource::new("loopback", 16_000, silent, 512));

        let vad_cfg = VadConfig {
            threshold: 0.01,
            min_speech_frames: 2,
            silence_timeout_ms: 100,
            frame_size: 512,
            max_zcr: 1.0,
        };

        let mixer = DualTrackMixer::new(mic, lb);
        let frames_rx = mixer.into_stream(64);
        let recorder = MeetingRecorder::new(vad_cfg, Arc::new(NullStt));
        let (mut seg_rx, _handle) = recorder.record(frames_rx);

        // Wait up to 2s for at least one segment
        let timeout = tokio::time::timeout(Duration::from_secs(4), seg_rx.recv());
        let seg = timeout.await;
        assert!(
            seg.is_ok() && seg.unwrap().is_some(),
            "expected at least one transcript segment"
        );
    }
}
