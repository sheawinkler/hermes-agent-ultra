//! `TaggedFrame` — a PCM chunk annotated with its source channel.
//!
//! The dual-track meeting recorder produces `TaggedFrame` values so that
//! downstream components (STT, diarization) know whether audio came from the
//! local microphone or the loopback (remote participants).

use serde::{Deserialize, Serialize};

/// Identifies which physical audio channel a frame originated from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AudioChannel {
    /// Local microphone input (the current speaker).
    Mic,
    /// System audio loopback (remote participants via speaker output).
    Loopback,
}

impl AudioChannel {
    /// Returns a short human-readable label suitable for transcripts.
    pub fn label(&self) -> &'static str {
        match self {
            AudioChannel::Mic => "mic",
            AudioChannel::Loopback => "loopback",
        }
    }

    /// Speaker designation used in meeting transcripts.
    /// Convention: mic = "Speaker A" (self), loopback = "Speaker B" (remote).
    pub fn speaker_label(&self) -> &'static str {
        match self {
            AudioChannel::Mic => "Speaker A",
            AudioChannel::Loopback => "Speaker B",
        }
    }
}

impl std::fmt::Display for AudioChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// A PCM audio chunk tagged with its originating channel.
#[derive(Debug, Clone)]
pub struct TaggedFrame {
    /// Which audio channel this frame came from.
    pub channel: AudioChannel,
    /// Mono f32 PCM samples normalized to [-1, 1].
    pub samples: Vec<f32>,
    /// Sample rate in Hz.
    pub sample_rate: u32,
}

impl TaggedFrame {
    pub fn new(channel: AudioChannel, samples: Vec<f32>, sample_rate: u32) -> Self {
        Self { channel, samples, sample_rate }
    }

    pub fn is_mic(&self) -> bool {
        self.channel == AudioChannel::Mic
    }

    pub fn is_loopback(&self) -> bool {
        self.channel == AudioChannel::Loopback
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn speaker_labels() {
        assert_eq!(AudioChannel::Mic.speaker_label(), "Speaker A");
        assert_eq!(AudioChannel::Loopback.speaker_label(), "Speaker B");
    }

    #[test]
    fn tagged_frame_channel_checks() {
        let f = TaggedFrame::new(AudioChannel::Mic, vec![0.0; 16], 16_000);
        assert!(f.is_mic());
        assert!(!f.is_loopback());
    }
}
