//! hermes-audio — shared audio capture abstractions.
//!
//! This crate provides the `AudioCaptureSource` trait and common types used by
//! both the real-time voice-mode pipeline (`voice_mode.rs`) and the meeting
//! recorder (`meeting_notes.rs`).
//!
//! # Architecture
//!
//! ```text
//!   MicSource          LoopbackSource
//!       └──────┬────────────┘
//!           DualTrackMixer (TaggedFrame)
//!                 │
//!            VAD / STT pipeline
//! ```
//!
//! `voice_mode.rs` uses `MicSource` directly (single-track, real-time dialogue).
//! `meeting_notes.rs` uses `DualTrackMixer` (two-track, meeting recording).

pub mod capture;
pub mod frame;
pub mod loopback;
pub mod mixer;
pub mod recorder;
pub mod vad;

pub use capture::AudioCaptureSource;
pub use frame::{AudioChannel, TaggedFrame};
pub use loopback::LoopbackSource;
pub use mixer::DualTrackMixer;
pub use recorder::{pcm_to_wav, MeetingRecorder, SttCallback, TranscriptSegment};
pub use vad::{create_vad, EnergyVad, VadBackend, VadConfig};
