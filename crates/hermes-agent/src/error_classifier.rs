//! Structured API error classification (parity with `agent/error_classifier.py`).
//!
//! Uses a single Aho-Corasick DFA to match all failover-relevant patterns
//! in one pass over the lowercased error string — eliminating ~50+ redundant
//! `str::contains` scans per invocation.

use std::sync::LazyLock;

use aho_corasick::AhoCorasick;

use hermes_core::Message;

use crate::context::ContextManager;

/// Recovery-oriented error reasons used by the retry loop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailoverReason {
    Auth,
    Billing,
    RateLimit,
    ThinkingSignature,
    ImageTooLarge,
    ProviderPolicyBlocked,
    LlamaCppGrammarPattern,
    OAuthLongContextBetaForbidden,
    InvalidEncryptedReplay,
    Unknown,
}

// ---------------------------------------------------------------------------
// Single-pass Aho-Corasick automaton
// ---------------------------------------------------------------------------
// Pattern indices are grouped into semantic ranges so that one `find_iter`
// call populates a 128-bit bitmap, and each classification check is O(1)
// bit-test rather than O(K·N) substring-search.
//
// Groups:
//   0..20  — Billing patterns
//  20..26  — Rate-limit patterns
//  26..33  — Image-too-large patterns
//  33..37  — LlamaCpp grammar patterns
//  37..43  — Encrypted-replay patterns
//  43..46  — Provider-policy-blocked patterns
//  46      — Billing status code "402"
//  47..51  — Auth tokens ("401", "403", "unauthorized", "authentication")
//  51      — OAuth long-context-beta sentence
//  52..54  — Thinking-signature sub-strings ("400", "signature", "thinking")
// ---------------------------------------------------------------------------

static FAILOVER_AC: LazyLock<AhoCorasick> = LazyLock::new(|| {
    AhoCorasick::builder()
        .build(&[
            //  0 .. 19  — BILLING_PATTERNS (20 items)
            "insufficient credits",
            "insufficient_quota",
            "insufficient balance",
            "credit balance",
            "credits exhausted",
            "credits have been exhausted",
            "no usable credits",
            "top up your credits",
            "payment required",
            "billing hard limit",
            "exceeded your current quota",
            "account is deactivated",
            "plan does not include",
            "out of funds",
            "run out of funds",
            "balance_depleted",
            "model_not_supported_on_free_tier",
            "not available on the free tier",
            "key limit exceeded",
            "spending limit",
            // 20 .. 25  — RATE_LIMIT_PATTERNS (6 items)
            "rate limit",
            "rate_limit",
            "too many requests",
            "429",
            "throttled",
            "resource_exhausted",
            // 26 .. 32  — IMAGE_TOO_LARGE_PATTERNS (7 items)
            "image exceeds",
            "image too large",
            "image_too_large",
            "image size exceeds",
            "exceeds 5 mb maximum",
            "exceeds 5mb",
            "maximum: 6291456",
            // 33 .. 36  — LLAMA_CPP_GRAMMAR_PATTERNS (4 items)
            "error parsing grammar",
            "unable to generate parser",
            "json-schema-to-grammar",
            "parse: error parsing grammar",
            // 37 .. 42  — ENCRYPTED_REPLAY_PATTERNS (6 items)
            "encrypted content",
            "encrypted_content",
            "failed to decrypt",
            "invalid encrypted",
            "cannot replay encrypted",
            "reasoning.encrypted_content",
            // 43 .. 45  — ProviderPolicyBlocked (3 items)
            "guardrail restrictions",
            "matching your data policy",
            "no endpoints available matching",
            // 46  — Billing status code
            "402",
            // 47 .. 50  — Auth tokens (4 items)
            "401",
            "403",
            "unauthorized",
            "authentication",
            // 51  — OAuth long-context-beta
            "long context beta is not yet available for this subscription",
            // 52 .. 54  — Thinking-signature sub-strings (3 items)
            "400",
            "signature",
            "thinking",
        ])
        .unwrap()
});

// ---  Group boundary constants  ---------------------------------------------

const BILLING_END: usize = 20;
const RATE_LIMIT_END: usize = 26;
const IMAGE_END: usize = 33;
const LLAMA_END: usize = 37;
const ENCRYPTED_END: usize = 43;
const POLICY_END: usize = 46;
const BILLING_CODE: usize = 46; // single index
const AUTH_END: usize = 51;
const OAUTH_LONG: usize = 51; // single index
const THINKING_400: usize = 52;
const THINKING_SIG: usize = 53;
const THINKING_TERM: usize = 54;

// ---------------------------------------------------------------------------
// Bitmap helpers
// ---------------------------------------------------------------------------

/// Build a 128-bit bitmap of which patterns matched, via one DFA pass.
fn failover_bitmap(lower: &str) -> u128 {
    let mut bitmap: u128 = 0;
    for m in FAILOVER_AC.find_iter(lower) {
        bitmap |= 1u128 << m.pattern().as_usize();
    }
    bitmap
}

/// True when *any* pattern in `[start, end)` is set in the bitmap.
fn any_bit(bitmap: u128, start: usize, end: usize) -> bool {
    let count = end.saturating_sub(start);
    if count == 0 {
        return false;
    }
    let mask = if count >= 128 {
        !0u128
    } else {
        (1u128 << count).wrapping_sub(1)
    };
    (bitmap & (mask << start)) != 0
}

/// True when a specific single-bit pattern is set.
fn has_bit(bitmap: u128, idx: usize) -> bool {
    (bitmap & (1u128 << idx)) != 0
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Classify an LLM API error string (priority-ordered).
pub fn classify_failover_reason(err: &str) -> FailoverReason {
    classify_failover_reason_with_provider(err, "")
}

pub fn classify_failover_reason_with_provider(err: &str, provider: &str) -> FailoverReason {
    // 前置条件：错误字符串不能为空
    debug_assert!(!err.is_empty(), "classify: empty error string");
    debug_assert!(
        provider.is_empty() || !provider.trim().is_empty(),
        "classify: provider must not be whitespace-only"
    );

    let lower = err.to_ascii_lowercase();
    let bitmap = failover_bitmap(&lower);

    // --- Priority 1: Thinking signature (all three sub-strings required) ---
    if has_bit(bitmap, THINKING_400)
        && has_bit(bitmap, THINKING_SIG)
        && has_bit(bitmap, THINKING_TERM)
    {
        return FailoverReason::ThinkingSignature;
    }

    // --- Priority 2: Encrypted replay ---
    if any_bit(bitmap, 37, ENCRYPTED_END) {
        return FailoverReason::InvalidEncryptedReplay;
    }

    // --- Priority 3: Image too large ---
    if any_bit(bitmap, 26, IMAGE_END) {
        return FailoverReason::ImageTooLarge;
    }

    // --- Priority 4: LlamaCpp grammar (requires "400" + grammar pattern) ---
    if has_bit(bitmap, THINKING_400) && any_bit(bitmap, 33, LLAMA_END) {
        return FailoverReason::LlamaCppGrammarPattern;
    }

    // --- Priority 5: OAuth long-context-beta (provider must be anthropic) ---
    if provider.to_ascii_lowercase().contains("anthropic") && has_bit(bitmap, OAUTH_LONG) {
        return FailoverReason::OAuthLongContextBetaForbidden;
    }

    // --- Priority 6: Provider policy blocked ---
    if any_bit(bitmap, 43, POLICY_END) {
        return FailoverReason::ProviderPolicyBlocked;
    }

    // --- Priority 7: Billing (status code 402 OR billing patterns) ---
    if has_bit(bitmap, BILLING_CODE) || any_bit(bitmap, 0, BILLING_END) {
        return FailoverReason::Billing;
    }

    // --- Priority 8: Rate limit ---
    if any_bit(bitmap, 20, RATE_LIMIT_END) {
        return FailoverReason::RateLimit;
    }

    // --- Priority 9: Auth ---
    if any_bit(bitmap, 47, AUTH_END) {
        return FailoverReason::Auth;
    }

    FailoverReason::Unknown
}

// ---------------------------------------------------------------------------
// Legacy individual checks (kept for external callers; all delegate to the
// same bitmap internally or use trivial `contains`).
// ---------------------------------------------------------------------------

pub fn is_thinking_signature_error(lower: &str) -> bool {
    let bitmap = failover_bitmap(lower);
    has_bit(bitmap, THINKING_400) && has_bit(bitmap, THINKING_SIG) && has_bit(bitmap, THINKING_TERM)
}

pub fn is_image_too_large_error(lower: &str) -> bool {
    let bitmap = failover_bitmap(lower);
    any_bit(bitmap, 26, IMAGE_END)
}

pub fn is_llama_cpp_grammar_error(lower: &str) -> bool {
    let bitmap = failover_bitmap(lower);
    has_bit(bitmap, THINKING_400) && any_bit(bitmap, 33, LLAMA_END)
}

pub fn is_invalid_encrypted_replay_error(lower: &str) -> bool {
    let bitmap = failover_bitmap(lower);
    any_bit(bitmap, 37, ENCRYPTED_END)
}

pub fn is_provider_policy_blocked(lower: &str) -> bool {
    let bitmap = failover_bitmap(lower);
    any_bit(bitmap, 43, POLICY_END)
}

/// Strip thinking/reasoning blocks so the next retry sends no signed thinking content.
pub fn strip_thinking_blocks_from_context(ctx: &mut ContextManager) {
    strip_thinking_blocks_from_messages(ctx.get_messages_mut());
}

pub fn strip_thinking_blocks_from_messages(messages: &mut [Message]) {
    for msg in messages {
        msg.reasoning_content = None;
    }
}

/// Remove provider encrypted-replay blobs from assistant messages (Codex / Responses API).
pub fn strip_invalid_encrypted_replay_from_context(ctx: &mut ContextManager) -> bool {
    strip_invalid_encrypted_replay_from_messages(ctx.get_messages_mut())
}

pub fn strip_invalid_encrypted_replay_from_messages(messages: &mut [Message]) -> bool {
    let mut changed = false;
    for msg in messages.iter_mut() {
        if let Some(ref mut content) = msg.content {
            if content.contains("encrypted_content") || content.contains("gAAAA") {
                *content = "[Encrypted reasoning replay removed for retry.]".to_string();
                changed = true;
            }
        }
        if msg.reasoning_content.is_some() {
            msg.reasoning_content = None;
            changed = true;
        }
    }
    changed
}

/// Best-effort image shrink: strip/replace multimodal image payloads for retry.
pub fn shrink_oversized_images_in_context(ctx: &mut ContextManager) -> bool {
    let mut messages = ctx.get_messages().to_vec();
    let before = messages
        .iter()
        .filter(|m| m.content.as_ref().is_some_and(|c| c.contains("data:image")))
        .count();
    crate::vision_message_prepare::strip_images_for_non_vision_model_in_place(&mut messages);
    let after = messages
        .iter()
        .filter(|m| m.content.as_ref().is_some_and(|c| c.contains("data:image")))
        .count();
    if before == after {
        return false;
    }
    *ctx.get_messages_mut() = messages;
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn image_too_large_not_context_overflow() {
        let err = "messages.0.content.1.image.source.base64: image exceeds 5 MB maximum";
        assert_eq!(classify_failover_reason(err), FailoverReason::ImageTooLarge);
    }

    #[test]
    fn llama_cpp_grammar_requires_400() {
        let err = "HTTP 400: parse: error parsing grammar: unknown escape";
        assert_eq!(
            classify_failover_reason(err),
            FailoverReason::LlamaCppGrammarPattern
        );
        let err500 = "error parsing grammar";
        assert_ne!(
            classify_failover_reason(err500),
            FailoverReason::LlamaCppGrammarPattern
        );
    }

    #[test]
    fn provider_policy_blocked() {
        let err = "No endpoints available matching your guardrail restrictions and data policy";
        assert_eq!(
            classify_failover_reason(err),
            FailoverReason::ProviderPolicyBlocked
        );
    }

    #[test]
    fn oauth_1m_beta_forbidden() {
        let err = "The long context beta is not yet available for this subscription.";
        assert_eq!(
            classify_failover_reason_with_provider(err, "anthropic"),
            FailoverReason::OAuthLongContextBetaForbidden
        );
    }

    #[test]
    fn encrypted_replay_detected() {
        assert_eq!(
            classify_failover_reason("failed to decrypt encrypted_content replay"),
            FailoverReason::InvalidEncryptedReplay
        );
    }
}
