//! TTS streaming pipeline ported from Python `tools/tts_tool.py`.
//!
//! This module owns the zero-Python portions of the Python TTS stack:
//!
//! * [`sanitizer`]      — Markdown stripping + incremental `<think>` filtering.
//! * [`sentence_buffer`] — Sentence-boundary aware buffer with dedup + forced flush.
//! * [`minimax`]        — MiniMax `t2a_v2` HTTP backend.
//! * [`elevenlabs_stream`] — ElevenLabs PCM streaming endpoint (chunked bytes).
//! * [`pipeline`]       — Glue layer: text deltas → sentences → [optional] audio sink.
//!
//! Explicitly *not* ported:
//!
//! * Microsoft Edge TTS (Python CLI dependency; rejected in earlier sprints).
//! * NeuTTS (local Python inference process).
//! * `sounddevice` / PortAudio playback — kept out of the core crate to avoid
//!   pulling a platform-specific C library into every binary. The gateway or
//!   CLI wraps a concrete [`PcmSink`](pipeline::PcmSink) when it actually
//!   wants real-time playback.
//! * ffmpeg-based Opus conversion — not required by the streaming path and
//!   can be done separately if the Telegram gateway demands it.

pub mod elevenlabs_stream;
pub mod minimax;
pub mod pipeline;
pub mod sanitizer;
pub mod sentence_buffer;

pub use elevenlabs_stream::{ElevenLabsStreamConfig, ElevenLabsStreamingClient};
pub use minimax::{MiniMaxTtsBackend, MiniMaxVoiceSettings};
pub use pipeline::{
    PcmSink, PipelineOutcome, PipelineStats, SentenceSink, TextDelta, TtsStreamingPipeline,
    VecSentenceSink,
};
pub use sanitizer::{strip_markdown_for_tts, IncrementalThinkStripper};
pub use sentence_buffer::{SentenceBuffer, SentenceBufferConfig};
