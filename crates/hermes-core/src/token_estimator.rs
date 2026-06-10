//! Token estimation with a pluggable strategy.
//!
//! # Design
//!
//! A [`TokenEstimator`] trait abstracts how many tokens a piece of text
//! occupies.   The default [`CharBasedEstimator`] uses a content-aware heuristic
//! that separates ASCII code from CJK / Unicode text — a single ratio (4×)
//! undercounts CJK-heavy content by ~2× and overcounts pure code by ~30 %.
//!
//! Callers that need exact counts can swap in a BPE-based estimator
//! (e.g. wrapping `tiktoken-rs`) without changing any calling code.
//!
//! # Example
//!
//! ```rust
//! use hermes_core::token_estimator::{TokenEstimator, CharBasedEstimator};
//!
//! let est = CharBasedEstimator;
//! assert!(est.estimate("hello world") > 0);
//! assert!(est.estimate("") == 0);
//! let code = "fn main() { println!(\"hi\"); }";
//! assert!(est.estimate(code) < code.chars().count() as u64);
//! ```

/// Pluggable token-counting strategy.
pub trait TokenEstimator: Send + Sync {
    /// Estimate the number of tokens `text` consumes.
    fn estimate(&self, text: &str) -> u64;

    /// Estimate tokens for a full message (content + metadata overhead).
    fn estimate_message(&self, content: Option<&str>, overhead: u64) -> u64 {
        let content = content.unwrap_or("");
        self.estimate(content) + overhead
    }
}

// ---------------------------------------------------------------------------
// CharBasedEstimator
// ---------------------------------------------------------------------------

/// Content-aware character-based estimator.
///
/// Separates ASCII bytes from non-ASCII (CJK, emoji, etc.) and applies
/// different ratios:
///
/// | Content type  | Chars per token |
/// |---------------|-----------------|
/// | ASCII         | 4.0             |
/// | Non-ASCII     | 1.5             |
/// | Mixed         | Weighted blend  |
///
/// This is still approximate (±30 %) but significantly better than a flat 4×
/// ratio, especially for CJK-heavy agent conversations.
pub struct CharBasedEstimator;

impl TokenEstimator for CharBasedEstimator {
    fn estimate(&self, text: &str) -> u64 {
        if text.is_empty() {
            return 0;
        }

        let mut ascii_bytes = 0usize;
        let mut non_ascii_chars = 0usize;

        for ch in text.chars() {
            if ch.is_ascii() {
                ascii_bytes += 1;
            } else {
                non_ascii_chars += 1;
            }
        }

        // ASCII: ~4 chars/token, non-ASCII: ~1.5 chars/token
        let ascii_tokens = ascii_bytes.div_ceil(4);
        let non_ascii_tokens = ((non_ascii_chars as u64) * 2 + 2) / 3; // ceil(n * 2/3)

        (ascii_tokens as u64) + non_ascii_tokens
    }
}

// ---------------------------------------------------------------------------
// Default instance (used when no other strategy is configured)
// ---------------------------------------------------------------------------

/// Convenience constant: the default [`CharBasedEstimator`].
pub const DEFAULT_TOKEN_ESTIMATOR: CharBasedEstimator = CharBasedEstimator;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convenience: call the default estimator on a string.
pub fn estimate(text: &str) -> u64 {
    DEFAULT_TOKEN_ESTIMATOR.estimate(text)
}

/// Convenience: estimate tokens for a serializable value (JSON round-trip).
pub fn estimate_json(value: &serde_json::Value) -> u64 {
    match value {
        serde_json::Value::String(s) => estimate(s),
        other => estimate(&other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_is_zero() {
        assert_eq!(estimate(""), 0);
    }

    #[test]
    fn ascii_four_per_token() {
        let est = CharBasedEstimator;
        // ~4 chars per token for ASCII
        assert_eq!(est.estimate("abcd"), 1); // 4 ascii chars = 1 token
        assert_eq!(est.estimate("abcdefgh"), 2); // 8 chars = 2 tokens
    }

    #[test]
    fn cjk_fewer_chars_per_token() {
        let est = CharBasedEstimator;
        // CJK chars are denser → more tokens per char
        let cjk = "你好世界";
        let tokens = est.estimate(cjk);
        assert!(tokens >= 2, "CJK text should produce more than 1 token"); // 4 CJK chars * 2/3 = 2.67 → 3
        assert!(tokens <= 6);
    }

    #[test]
    fn mixed_content_blend() {
        let est = CharBasedEstimator;
        let mixed = "hello你好world世界";
        let tokens = est.estimate(mixed);
        // 10 ASCII chars = 3 tokens, 4 CJK = ceil(4*2/3) = 3 tokens
        assert_eq!(tokens, 6, "expected 6, got {tokens}");
    }

    #[test]
    fn estimate_message_with_overhead() {
        let est = CharBasedEstimator;
        // "hi" = 2 ASCII chars = 1 token, + 10 overhead = 11
        assert_eq!(est.estimate_message(Some("hi"), 10), 11);
    }
}
