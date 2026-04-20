//! Sentence-boundary aware buffer used by the streaming TTS pipeline.
//!
//! Ported from the inline logic in Python's `stream_tts_to_speaker`:
//!
//! * Accumulate streamed text deltas into a `sentence_buf`.
//! * Split on a sentence-boundary regex equivalent to
//!   `(?<=[.!?])(?:\s|\n)|(?:\n\n)`.
//! * If the current sentence is shorter than `min_sentence_len`, merge it
//!   into the *next* sentence (avoids single-word audio clips).
//! * If `push` hasn't produced a full sentence for a while and the buffer
//!   grew past `long_flush_len`, [`force_flush`](SentenceBuffer::force_flush)
//!   can be called to emit the whole buffer as one pseudo-sentence.
//! * A small dedup cache drops sentences that repeat verbatim (LLM loops).
//!
//! The regex and the specific thresholds match Python exactly.

use std::sync::OnceLock;

use regex::Regex;

fn sentence_boundary_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| {
        // Rust's `regex` crate does not support lookbehind, so we port
        // Python's `(?<=[.!?])(?:\s|\n)|(?:\n\n)` by matching the
        // punctuation as part of the alternative and advancing the cut
        // point past the whitespace. Branches (in priority order):
        //   1. paragraph break `\n\n`
        //   2. `.!?` followed by a space or newline
        Regex::new(r"\n\n|[.!?]\s").expect("sentence boundary regex")
    })
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct SentenceBufferConfig {
    /// Minimum character length (after trim) for a sentence to be emitted.
    /// Anything shorter is merged into the next sentence. Python uses 20.
    pub min_sentence_len: usize,
    /// Force-flush threshold: if [`force_flush`](SentenceBuffer::force_flush)
    /// is called and the buffer is larger than this, emit it regardless of
    /// boundary state. Python uses 100.
    pub long_flush_len: usize,
    /// Maximum number of spoken sentences to remember for dedup. Python
    /// uses an unbounded list; we cap at 64 to avoid runaway memory on long
    /// conversations.
    pub dedup_window: usize,
}

impl Default for SentenceBufferConfig {
    fn default() -> Self {
        Self {
            min_sentence_len: 20,
            long_flush_len: 100,
            dedup_window: 64,
        }
    }
}

// ---------------------------------------------------------------------------
// Buffer
// ---------------------------------------------------------------------------

/// Incremental sentence splitter with dedup.
#[derive(Debug)]
pub struct SentenceBuffer {
    buf: String,
    spoken: Vec<String>,
    config: SentenceBufferConfig,
}

impl SentenceBuffer {
    pub fn new(config: SentenceBufferConfig) -> Self {
        Self {
            buf: String::new(),
            spoken: Vec::new(),
            config,
        }
    }

    /// Append a delta and return any completed sentences in order.
    ///
    /// Sentences shorter than `min_sentence_len` (after trimming) are merged
    /// back into the buffer and will be concatenated with the next sentence.
    /// Duplicate sentences (case-insensitive, ignoring trailing `.!,`) are
    /// dropped.
    pub fn push(&mut self, delta: &str) -> Vec<String> {
        self.buf.push_str(delta);
        self.extract_sentences()
    }

    /// Drain any remaining buffered text as a final pseudo-sentence. Called
    /// at end-of-stream.
    pub fn finish(&mut self) -> Option<String> {
        let leftover = std::mem::take(&mut self.buf);
        let trimmed = leftover.trim();
        if trimmed.is_empty() {
            None
        } else {
            let owned = trimmed.to_string();
            if self.admit(&owned) {
                Some(owned)
            } else {
                None
            }
        }
    }

    /// Emit the buffer right now if it has exceeded the force-flush length.
    /// Returns `None` if the buffer is shorter than `long_flush_len`.
    ///
    /// Unlike [`finish`](Self::finish), the buffer is only flushed when big
    /// enough — this mirrors Python's timeout-based flush in the queue-read
    /// loop.
    pub fn force_flush(&mut self) -> Option<String> {
        if self.buf.len() < self.config.long_flush_len {
            return None;
        }
        let taken = std::mem::take(&mut self.buf);
        let trimmed = taken.trim();
        if trimmed.is_empty() {
            return None;
        }
        let owned = trimmed.to_string();
        if self.admit(&owned) {
            Some(owned)
        } else {
            None
        }
    }

    /// Raw access to the internal buffer (useful for diagnostics and tests).
    pub fn pending_len(&self) -> usize {
        self.buf.len()
    }

    // -- internals --------------------------------------------------------

    fn extract_sentences(&mut self) -> Vec<String> {
        let mut out = Vec::new();
        loop {
            // Find the *smallest* prefix ending at a sentence boundary such
            // that the trimmed prefix reaches `min_sentence_len`. This
            // implements the Python comment "Merge short fragments into the
            // next sentence" robustly — Python's implementation had an
            // actual bug where a short leading fragment would get stuck in
            // the buffer forever; walking the boundary cursor forward fixes
            // that while preserving the intended semantics.
            let mut end = 0usize;
            let mut found_long_enough = false;
            loop {
                let hay = &self.buf[end..];
                let Some(m) = sentence_boundary_re().find(hay) else {
                    break;
                };
                end += m.end();
                if self.buf[..end].trim().len() >= self.config.min_sentence_len {
                    found_long_enough = true;
                    break;
                }
            }
            if !found_long_enough {
                break;
            }
            let candidate = self.buf[..end].to_string();
            self.buf.drain(..end);
            if self.admit(&candidate) {
                out.push(candidate);
            }
        }
        out
    }

    /// Dedup check + record. Returns true if the sentence is new and should
    /// be emitted.
    fn admit(&mut self, sentence: &str) -> bool {
        let cleaned = super::sanitizer::strip_markdown_for_tts(sentence);
        if cleaned.is_empty() {
            return false;
        }
        let key = normalise_for_dedup(&cleaned);
        if self
            .spoken
            .iter()
            .any(|prev| normalise_for_dedup(prev) == key)
        {
            return false;
        }
        self.spoken.push(cleaned);
        if self.spoken.len() > self.config.dedup_window {
            let excess = self.spoken.len() - self.config.dedup_window;
            self.spoken.drain(..excess);
        }
        true
    }
}

/// Python: `cleaned.lower().rstrip(".!,")` is used as the dedup key.
fn normalise_for_dedup(s: &str) -> String {
    let lower = s.to_lowercase();
    lower
        .trim_end_matches(|c: char| c == '.' || c == '!' || c == ',')
        .to_string()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn default_buffer() -> SentenceBuffer {
        SentenceBuffer::new(SentenceBufferConfig::default())
    }

    #[test]
    fn emits_sentence_on_period_space() {
        let mut b = default_buffer();
        let out = b.push("This is a sufficiently long sentence. And then another one? Yes.");
        // First two sentences are above min length; the trailing "Yes." has
        // no whitespace after the period, so it stays buffered.
        assert_eq!(out.len(), 2);
        assert!(out[0].starts_with("This is a sufficiently long"));
        assert!(out[1].starts_with("And then another one"));
    }

    #[test]
    fn short_fragment_merges_into_next() {
        let mut b = default_buffer();
        // "Ok." is too short; per the "merge into next sentence" semantics
        // it should be concatenated with the following sentence before the
        // boundary fires.
        let mut got = b.push("Ok. ");
        assert!(got.is_empty());
        got.extend(
            b.push("But this follow-up sentence is definitely long enough to speak aloud. "),
        );
        assert_eq!(got.len(), 1);
        let s = &got[0];
        assert!(s.starts_with("Ok."), "got {:?}", s);
        assert!(s.contains("follow-up sentence"));
    }

    #[test]
    fn paragraph_break_counts_as_boundary() {
        let mut b = default_buffer();
        let out = b.push("A fairly medium sized paragraph\n\nNext paragraph also reasonably long.");
        assert_eq!(out.len(), 1);
        assert!(out[0].trim_end().ends_with("paragraph"));
    }

    #[test]
    fn dedup_drops_repeated_sentence() {
        let mut b = default_buffer();
        let out1 = b.push("The same fairly lengthy utterance. ");
        let out2 = b.push("The same fairly lengthy utterance. ");
        assert_eq!(out1.len(), 1);
        assert!(out2.is_empty(), "dedup should suppress repeat");
    }

    #[test]
    fn dedup_ignores_case_and_trailing_punct() {
        let mut b = default_buffer();
        let _ = b.push("Hello world, my friend. ");
        let out = b.push("HELLO WORLD, MY FRIEND! ");
        assert!(out.is_empty(), "case and punct variants should still dedup");
    }

    #[test]
    fn force_flush_emits_long_buffered_text() {
        let mut cfg = SentenceBufferConfig::default();
        cfg.long_flush_len = 10;
        let mut b = SentenceBuffer::new(cfg);
        let _ = b.push("no sentence boundary here yet"); // 30 chars
        let flushed = b.force_flush().expect("should flush when > long_flush_len");
        assert!(flushed.contains("no sentence boundary"));
        assert_eq!(b.pending_len(), 0);
    }

    #[test]
    fn force_flush_does_nothing_when_short() {
        let mut b = default_buffer();
        let _ = b.push("tiny");
        assert!(b.force_flush().is_none());
    }

    #[test]
    fn finish_emits_trailing_text() {
        let mut b = default_buffer();
        let _ = b.push("trailing sentence no newline");
        let final_out = b.finish().expect("finish flushes remainder");
        assert!(final_out.contains("trailing sentence"));
    }

    #[test]
    fn finish_returns_none_when_buffer_empty() {
        let mut b = default_buffer();
        assert!(b.finish().is_none());
    }

    #[test]
    fn markdown_in_sentence_is_stripped_for_dedup_key_only() {
        // The emitted sentence should still contain its raw markdown — the
        // caller decides whether to run strip_markdown_for_tts again before
        // passing to a TTS provider. But dedup normalises so that
        // `**hello** world` and `hello world` are considered duplicates.
        let mut b = default_buffer();
        let out1 = b.push("This **is** a reasonably long sentence. ");
        assert_eq!(out1.len(), 1);
        assert!(out1[0].contains("**"));
        let out2 = b.push("This is a reasonably long sentence. ");
        assert!(out2.is_empty(), "markdown-normalised dedup should fire");
    }
}
