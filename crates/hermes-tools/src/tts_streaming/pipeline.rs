//! Streaming TTS pipeline.
//!
//! Glues together:
//!
//! ```text
//!   LLM deltas (strings)
//!       │
//!       ▼
//! IncrementalThinkStripper      (hide <think>...</think>)
//!       │
//!       ▼
//!   SentenceBuffer              (collect, split, dedup)
//!       │  (emits whole sentences)
//!       ▼
//!     SentenceSink              (user-supplied: display, audio, tests)
//! ```
//!
//! The pipeline itself is synchronous with respect to text; audio streaming
//! is delegated to a [`PcmSink`] that callers optionally plug in. A
//! [`VecSentenceSink`] is provided for tests and non-audio contexts.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;

use hermes_core::ToolError;

use super::elevenlabs_stream::{ElevenLabsStreamConfig, PcmChunkStream, StreamingTtsBackend};
use super::sanitizer::{strip_markdown_for_tts, IncrementalThinkStripper};
use super::sentence_buffer::{SentenceBuffer, SentenceBufferConfig};

// ---------------------------------------------------------------------------
// Delta / sink types
// ---------------------------------------------------------------------------

/// A single piece of incoming text from the LLM. The sentinel `End` signals
/// end-of-stream (equivalent to Python's `None` queue sentinel).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TextDelta {
    Chunk(String),
    End,
}

impl TextDelta {
    pub fn chunk(s: impl Into<String>) -> Self {
        Self::Chunk(s.into())
    }
}

/// Consumer invoked once per completed sentence.
///
/// Implementors typically do at least two things:
///   1. Render the sentence for the user (print, display callback, ...).
///   2. Optionally trigger TTS synthesis (non-streaming file output, or the
///      streaming PCM path via [`PcmSink`]).
#[async_trait]
pub trait SentenceSink: Send + Sync {
    async fn on_sentence(&self, sentence: &str) -> Result<(), ToolError>;
    /// Called exactly once when the pipeline finishes (success or abort).
    async fn on_finish(&self, _stats: &PipelineStats) -> Result<(), ToolError> {
        Ok(())
    }
}

/// Consumer invoked with PCM chunks produced by the ElevenLabs streaming
/// endpoint. Kept separate from `SentenceSink` so tests can verify text
/// behaviour without plumbing an audio backend.
#[async_trait]
pub trait PcmSink: Send + Sync {
    async fn on_chunk(&self, sentence: &str, chunk: Bytes) -> Result<(), ToolError>;
    async fn on_sentence_end(&self, _sentence: &str) -> Result<(), ToolError> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Stats
// ---------------------------------------------------------------------------

/// Per-run counters surfaced on completion.
#[derive(Debug, Default, Clone)]
pub struct PipelineStats {
    pub sentences_emitted: usize,
    pub sentences_deduped_or_dropped: usize,
    pub pcm_chunks_forwarded: usize,
    pub pcm_bytes_forwarded: usize,
    pub aborted: bool,
}

#[derive(Debug, Clone)]
pub struct PipelineOutcome {
    pub stats: PipelineStats,
}

// ---------------------------------------------------------------------------
// Helper: sentence sink that records into a Vec (test + diagnostic use)
// ---------------------------------------------------------------------------

#[derive(Default)]
pub struct VecSentenceSink {
    inner: Mutex<Vec<String>>,
}

impl VecSentenceSink {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn take(&self) -> Vec<String> {
        std::mem::take(&mut self.inner.lock().unwrap())
    }
    pub fn snapshot(&self) -> Vec<String> {
        self.inner.lock().unwrap().clone()
    }
}

#[async_trait]
impl SentenceSink for VecSentenceSink {
    async fn on_sentence(&self, sentence: &str) -> Result<(), ToolError> {
        self.inner.lock().unwrap().push(sentence.to_string());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Pipeline
// ---------------------------------------------------------------------------

/// Build-and-run harness for a single TTS streaming session.
///
/// Use [`TtsStreamingPipeline::builder`] to configure sinks, then call
/// [`feed`](Self::feed) with each [`TextDelta`] and finally wait for
/// [`finish`](Self::finish).
pub struct TtsStreamingPipeline {
    sentence_sink: Arc<dyn SentenceSink>,
    pcm_sink: Option<Arc<dyn PcmSink>>,
    streaming_backend: Option<Arc<dyn StreamingTtsBackend>>,
    stream_config: ElevenLabsStreamConfig,
    sanitizer: Mutex<IncrementalThinkStripper>,
    buffer: Mutex<SentenceBuffer>,
    stats: Mutex<PipelineStats>,
    stop: Arc<AtomicBool>,
    strip_markdown_before_audio: bool,
}

impl TtsStreamingPipeline {
    pub fn builder() -> TtsStreamingPipelineBuilder {
        TtsStreamingPipelineBuilder::new()
    }

    /// Cooperative abort signal. Callers flip this (e.g. on user interrupt)
    /// and the pipeline stops forwarding audio chunks. In-flight sentences
    /// are still emitted to the [`SentenceSink`] — Python's behaviour is to
    /// skip only the audio, keeping the text record coherent.
    pub fn stop_handle(&self) -> Arc<AtomicBool> {
        self.stop.clone()
    }

    /// Push a single delta through the pipeline. Completed sentences are
    /// forwarded to the configured sinks before this method returns.
    pub async fn feed(&self, delta: TextDelta) -> Result<(), ToolError> {
        if self.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        match delta {
            TextDelta::Chunk(s) => {
                // (1) Strip <think> blocks across delta boundaries.
                let visible = {
                    let mut guard = self.sanitizer.lock().unwrap();
                    guard.push(&s)
                };
                if visible.is_empty() {
                    return Ok(());
                }
                // (2) Feed into the sentence buffer and emit completed ones.
                let sentences = {
                    let mut buf = self.buffer.lock().unwrap();
                    buf.push(&visible)
                };
                for sentence in sentences {
                    self.emit(&sentence).await?;
                }
            }
            TextDelta::End => {
                self.drain_end().await?;
            }
        }
        Ok(())
    }

    /// Attempt to force-flush the buffer if it has grown past the configured
    /// threshold. Mirrors Python's timeout-triggered flush in the queue
    /// reader. Callers can invoke this periodically (e.g. on a 500ms timer)
    /// when deltas are sparse.
    pub async fn poll_force_flush(&self) -> Result<(), ToolError> {
        if self.stop.load(Ordering::SeqCst) {
            return Ok(());
        }
        let flushed = {
            let mut buf = self.buffer.lock().unwrap();
            buf.force_flush()
        };
        if let Some(sentence) = flushed {
            self.emit(&sentence).await?;
        }
        Ok(())
    }

    /// End-of-stream: drain any pending sanitizer state and remaining
    /// buffered text, then notify sinks.
    pub async fn finish(&self) -> Result<PipelineOutcome, ToolError> {
        self.drain_end().await?;
        let stats = self.stats.lock().unwrap().clone();
        self.sentence_sink.on_finish(&stats).await?;
        Ok(PipelineOutcome { stats })
    }

    async fn drain_end(&self) -> Result<(), ToolError> {
        // Flush sanitizer remainder (dropped per Python semantics if inside
        // an unclosed `<think>` block).
        let tail = {
            let mut guard = self.sanitizer.lock().unwrap();
            guard.flush()
        };
        if !tail.is_empty() {
            let sentences = {
                let mut buf = self.buffer.lock().unwrap();
                buf.push(&tail)
            };
            for sentence in sentences {
                self.emit(&sentence).await?;
            }
        }
        // Then flush any remaining buffered text.
        let leftover = {
            let mut buf = self.buffer.lock().unwrap();
            buf.finish()
        };
        if let Some(sentence) = leftover {
            self.emit(&sentence).await?;
        }
        Ok(())
    }

    async fn emit(&self, sentence: &str) -> Result<(), ToolError> {
        if self.stop.load(Ordering::SeqCst) {
            self.stats.lock().unwrap().aborted = true;
            return Ok(());
        }
        self.stats.lock().unwrap().sentences_emitted += 1;
        self.sentence_sink.on_sentence(sentence).await?;
        if let (Some(backend), Some(pcm_sink)) =
            (self.streaming_backend.as_ref(), self.pcm_sink.as_ref())
        {
            self.forward_audio(backend.clone(), pcm_sink.clone(), sentence)
                .await?;
        }
        Ok(())
    }

    async fn forward_audio(
        &self,
        backend: Arc<dyn StreamingTtsBackend>,
        sink: Arc<dyn PcmSink>,
        sentence: &str,
    ) -> Result<(), ToolError> {
        // Apply the same markdown strip Python did before calling the TTS
        // API, so fences and URLs don't end up in the synthesised audio.
        let tts_text = if self.strip_markdown_before_audio {
            strip_markdown_for_tts(sentence)
        } else {
            sentence.to_string()
        };
        if tts_text.is_empty() {
            return Ok(());
        }
        let mut stream: PcmChunkStream = backend
            .stream_sentence(&tts_text, &self.stream_config)
            .await?;
        while let Some(chunk) = stream.next().await {
            if self.stop.load(Ordering::SeqCst) {
                self.stats.lock().unwrap().aborted = true;
                break;
            }
            let bytes = chunk?;
            let len = bytes.len();
            sink.on_chunk(sentence, bytes).await?;
            let mut stats = self.stats.lock().unwrap();
            stats.pcm_chunks_forwarded += 1;
            stats.pcm_bytes_forwarded += len;
        }
        sink.on_sentence_end(sentence).await?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

pub struct TtsStreamingPipelineBuilder {
    sentence_sink: Option<Arc<dyn SentenceSink>>,
    pcm_sink: Option<Arc<dyn PcmSink>>,
    streaming_backend: Option<Arc<dyn StreamingTtsBackend>>,
    stream_config: ElevenLabsStreamConfig,
    buffer_config: SentenceBufferConfig,
    strip_markdown_before_audio: bool,
}

impl TtsStreamingPipelineBuilder {
    pub fn new() -> Self {
        Self {
            sentence_sink: None,
            pcm_sink: None,
            streaming_backend: None,
            stream_config: ElevenLabsStreamConfig::default(),
            buffer_config: SentenceBufferConfig::default(),
            strip_markdown_before_audio: true,
        }
    }

    pub fn with_sentence_sink(mut self, sink: Arc<dyn SentenceSink>) -> Self {
        self.sentence_sink = Some(sink);
        self
    }

    pub fn with_pcm_sink(mut self, sink: Arc<dyn PcmSink>) -> Self {
        self.pcm_sink = Some(sink);
        self
    }

    pub fn with_streaming_backend(mut self, backend: Arc<dyn StreamingTtsBackend>) -> Self {
        self.streaming_backend = Some(backend);
        self
    }

    pub fn with_stream_config(mut self, cfg: ElevenLabsStreamConfig) -> Self {
        self.stream_config = cfg;
        self
    }

    pub fn with_buffer_config(mut self, cfg: SentenceBufferConfig) -> Self {
        self.buffer_config = cfg;
        self
    }

    pub fn strip_markdown_before_audio(mut self, yes: bool) -> Self {
        self.strip_markdown_before_audio = yes;
        self
    }

    pub fn build(self) -> Result<TtsStreamingPipeline, ToolError> {
        let sentence_sink = self.sentence_sink.ok_or_else(|| {
            ToolError::InvalidParams("TtsStreamingPipeline requires a sentence sink".into())
        })?;
        Ok(TtsStreamingPipeline {
            sentence_sink,
            pcm_sink: self.pcm_sink,
            streaming_backend: self.streaming_backend,
            stream_config: self.stream_config,
            sanitizer: Mutex::new(IncrementalThinkStripper::new()),
            buffer: Mutex::new(SentenceBuffer::new(self.buffer_config)),
            stats: Mutex::new(PipelineStats::default()),
            stop: Arc::new(AtomicBool::new(false)),
            strip_markdown_before_audio: self.strip_markdown_before_audio,
        })
    }
}

impl Default for TtsStreamingPipelineBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tts_streaming::elevenlabs_stream::tests::FakeStreamingBackend;

    fn feed_text(p: &TtsStreamingPipeline, chunks: &[&str]) {
        let rt = tokio::runtime::Runtime::new().unwrap();
        rt.block_on(async {
            for c in chunks {
                p.feed(TextDelta::chunk(*c)).await.unwrap();
            }
            p.feed(TextDelta::End).await.unwrap();
            p.finish().await.unwrap();
        });
    }

    #[test]
    fn end_to_end_text_only() {
        let sink = Arc::new(VecSentenceSink::new());
        let pipeline = TtsStreamingPipeline::builder()
            .with_sentence_sink(sink.clone() as Arc<dyn SentenceSink>)
            .build()
            .unwrap();
        feed_text(
            &pipeline,
            &[
                "This is a fairly long opening sentence. ",
                "Now here comes the second cool sentence. ",
            ],
        );
        let got = sink.snapshot();
        assert_eq!(got.len(), 2);
        assert!(got[0].contains("opening sentence"));
        assert!(got[1].contains("second cool"));
    }

    #[test]
    fn think_block_is_stripped_end_to_end() {
        let sink = Arc::new(VecSentenceSink::new());
        let pipeline = TtsStreamingPipeline::builder()
            .with_sentence_sink(sink.clone() as Arc<dyn SentenceSink>)
            .build()
            .unwrap();
        feed_text(
            &pipeline,
            &[
                "Hello there friend. ",
                "<think>this is hidden from the user</think> ",
                "Here is another spoken aloud sentence. ",
            ],
        );
        let got = sink.snapshot();
        let joined = got.join(" | ");
        assert!(
            !joined.contains("hidden"),
            "think content leaked: {joined:?}"
        );
        assert!(joined.contains("Hello there friend"));
        assert!(joined.contains("another spoken aloud"));
    }

    #[test]
    fn split_think_block_across_deltas() {
        let sink = Arc::new(VecSentenceSink::new());
        let pipeline = TtsStreamingPipeline::builder()
            .with_sentence_sink(sink.clone() as Arc<dyn SentenceSink>)
            .build()
            .unwrap();
        feed_text(
            &pipeline,
            &[
                "first half sentence is long enough. ",
                "before <thi",
                "nk>hidden stuff</thi",
                "nk> after text that is sufficiently long. ",
            ],
        );
        let got = sink.snapshot();
        let joined = got.join(" | ");
        assert!(!joined.contains("hidden stuff"));
        assert!(joined.contains("before  after text"));
    }

    #[test]
    fn dedup_runs_across_the_whole_pipeline() {
        let sink = Arc::new(VecSentenceSink::new());
        let pipeline = TtsStreamingPipeline::builder()
            .with_sentence_sink(sink.clone() as Arc<dyn SentenceSink>)
            .build()
            .unwrap();
        feed_text(
            &pipeline,
            &[
                "Repeated line that is rather long. ",
                "Repeated line that is rather long. ",
                "A different fairly long sentence lives here. ",
            ],
        );
        let got = sink.snapshot();
        assert_eq!(got.len(), 2);
        assert!(got[0].contains("Repeated line"));
        assert!(got[1].contains("different fairly long"));
    }

    #[test]
    fn finish_flushes_trailing_text_without_boundary() {
        let sink = Arc::new(VecSentenceSink::new());
        let pipeline = TtsStreamingPipeline::builder()
            .with_sentence_sink(sink.clone() as Arc<dyn SentenceSink>)
            .build()
            .unwrap();
        feed_text(&pipeline, &["A trailing sentence with no period at end"]);
        let got = sink.snapshot();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0], "A trailing sentence with no period at end");
    }

    // ---------- PCM sink integration -----------------------------------

    struct RecordingPcmSink {
        chunks: Mutex<Vec<(String, Bytes)>>,
        sentence_ends: Mutex<Vec<String>>,
    }

    impl RecordingPcmSink {
        fn new() -> Self {
            Self {
                chunks: Mutex::new(Vec::new()),
                sentence_ends: Mutex::new(Vec::new()),
            }
        }
    }

    #[async_trait]
    impl PcmSink for RecordingPcmSink {
        async fn on_chunk(&self, sentence: &str, chunk: Bytes) -> Result<(), ToolError> {
            self.chunks
                .lock()
                .unwrap()
                .push((sentence.to_string(), chunk));
            Ok(())
        }
        async fn on_sentence_end(&self, sentence: &str) -> Result<(), ToolError> {
            self.sentence_ends
                .lock()
                .unwrap()
                .push(sentence.to_string());
            Ok(())
        }
    }

    #[tokio::test]
    async fn pcm_sink_receives_chunks_per_sentence() {
        let sink = Arc::new(VecSentenceSink::new());
        let pcm = Arc::new(RecordingPcmSink::new());
        let backend = Arc::new(FakeStreamingBackend::with_chunks(vec![
            vec![
                Bytes::from_static(&[1u8; 8]),
                Bytes::from_static(&[2u8; 16]),
            ],
            vec![Bytes::from_static(&[3u8; 4])],
        ]));
        let pipeline = TtsStreamingPipeline::builder()
            .with_sentence_sink(sink.clone() as Arc<dyn SentenceSink>)
            .with_pcm_sink(pcm.clone() as Arc<dyn PcmSink>)
            .with_streaming_backend(backend.clone() as Arc<dyn StreamingTtsBackend>)
            .build()
            .unwrap();

        pipeline
            .feed(TextDelta::chunk(
                "First long enough sentence for the pipeline. ",
            ))
            .await
            .unwrap();
        pipeline
            .feed(TextDelta::chunk("Second reasonably long sentence here. "))
            .await
            .unwrap();
        pipeline.feed(TextDelta::End).await.unwrap();
        let outcome = pipeline.finish().await.unwrap();

        assert_eq!(outcome.stats.sentences_emitted, 2);
        let chunks = pcm.chunks.lock().unwrap();
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].1.len(), 8);
        assert_eq!(chunks[1].1.len(), 16);
        assert_eq!(chunks[2].1.len(), 4);
        let ends = pcm.sentence_ends.lock().unwrap();
        assert_eq!(ends.len(), 2);
    }

    #[tokio::test]
    async fn stop_handle_aborts_audio_forwarding() {
        let sink = Arc::new(VecSentenceSink::new());
        let pcm = Arc::new(RecordingPcmSink::new());
        let backend = Arc::new(FakeStreamingBackend::with_chunks(vec![vec![
            Bytes::from_static(&[0u8; 4]),
            Bytes::from_static(&[0u8; 4]),
        ]]));
        let pipeline = TtsStreamingPipeline::builder()
            .with_sentence_sink(sink.clone() as Arc<dyn SentenceSink>)
            .with_pcm_sink(pcm.clone() as Arc<dyn PcmSink>)
            .with_streaming_backend(backend.clone() as Arc<dyn StreamingTtsBackend>)
            .build()
            .unwrap();
        let stop = pipeline.stop_handle();
        stop.store(true, Ordering::SeqCst);

        pipeline
            .feed(TextDelta::chunk("Long enough blocked sentence here. "))
            .await
            .unwrap();
        pipeline.feed(TextDelta::End).await.unwrap();
        let outcome = pipeline.finish().await.unwrap();

        // The stop flag was set before any feed; no sentences should have
        // been emitted at all.
        assert_eq!(outcome.stats.sentences_emitted, 0);
        assert!(pcm.chunks.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn force_flush_emits_stalled_buffer() {
        let sink = Arc::new(VecSentenceSink::new());
        let cfg = SentenceBufferConfig {
            min_sentence_len: 20,
            long_flush_len: 10,
            dedup_window: 16,
        };
        let pipeline = TtsStreamingPipeline::builder()
            .with_sentence_sink(sink.clone() as Arc<dyn SentenceSink>)
            .with_buffer_config(cfg)
            .build()
            .unwrap();
        pipeline
            .feed(TextDelta::chunk("a trickle of text without any boundary"))
            .await
            .unwrap();
        pipeline.poll_force_flush().await.unwrap();
        pipeline.finish().await.unwrap();
        let got = sink.snapshot();
        assert_eq!(got.len(), 1);
        assert!(got[0].contains("trickle of text"));
    }
}
