//! Context engine — pluggable context compression for long conversations.
//!
//! Provides a trait-based interface for compressing conversation context when
//! it approaches the model's context window limit.

use async_trait::async_trait;
use serde_json::Value;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::Duration;

pub use crate::model_metadata::IMAGE_TOKEN_ESTIMATE;
use crate::model_metadata::estimate_tokens_rough;

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

#[derive(Debug, thiserror::Error)]
pub enum ContextError {
    #[error("compression failed: {0}")]
    CompressionFailed(String),

    #[error("context too small to compress ({0} messages)")]
    TooSmall(usize),

    #[error("token estimation error: {0}")]
    TokenEstimation(String),
}

// ---------------------------------------------------------------------------
// ContextEngine trait
// ---------------------------------------------------------------------------

/// Pluggable context compression strategy.
#[async_trait]
pub trait ContextEngine: Send + Sync {
    /// Compress messages to fit within `target_tokens`.
    ///
    /// Returns the compressed messages.  The implementation decides
    /// how to condense — summarization, truncation, importance-based
    /// filtering, or a combination.
    async fn compress(
        &self,
        messages: &[Value],
        target_tokens: u64,
    ) -> Result<Vec<Value>, ContextError>;

    /// Estimate the total token count for a list of messages.
    fn estimate_tokens(&self, messages: &[Value]) -> u64 {
        messages
            .iter()
            .map(|m| {
                let content = m.get("content").and_then(|c| c.as_str()).unwrap_or("");
                estimate_tokens_rough(content)
            })
            .sum()
    }

    /// Name for logging/diagnostics.
    fn name(&self) -> &str;
}

// ---------------------------------------------------------------------------
// DefaultContextEngine — summarization-based compression
// ---------------------------------------------------------------------------

/// Default context engine that compresses by removing older messages
/// and replacing them with a structured summary marker.
pub struct DefaultContextEngine {
    /// Fraction of messages to keep (from the end).
    pub keep_ratio: f64,
    /// Whether to attempt LLM summary generation via optional HTTP endpoint.
    ///
    /// If enabled, the engine reads `HERMES_CONTEXT_SUMMARY_URL` and sends
    /// removed messages to that endpoint. On any failure it falls back to
    /// deterministic heuristic summarization.
    pub use_llm_summary: bool,
    /// Consecutive compaction count where post-compaction tokens still
    /// exceed the target window. When this reaches 2, auto-compaction
    /// is paused to prevent a stuck compaction loop that repeatedly
    /// collapses context without relief and destroys the cache prefix.
    pub consecutive_compacts: AtomicU32,
    /// Whether auto-compaction is currently paused due to stuck detection.
    /// Resets to false when a subsequent compression succeeds.
    pub compact_stuck: AtomicBool,
    /// Token estimation ratio. Default 0.25 = ~4 chars/token.
    /// CJK-heavy text may benefit from 0.5–1.0.
    /// Ported from Reasonix compact.go tokPerChar.
    pub tokens_per_char: f64,
}

impl DefaultContextEngine {
    pub fn new() -> Self {
        Self {
            keep_ratio: 0.33,
            use_llm_summary: false,
            consecutive_compacts: AtomicU32::new(0),
            compact_stuck: AtomicBool::new(false),
            tokens_per_char: 0.25,
        }
    }

    pub fn with_keep_ratio(mut self, ratio: f64) -> Self {
        self.keep_ratio = ratio.clamp(0.1, 0.9);
        self
    }

    pub fn with_tokens_per_char(mut self, ratio: f64) -> Self {
        self.tokens_per_char = ratio.clamp(0.05, 2.0);
        self
    }

    async fn maybe_generate_summary(
        &self,
        removed_messages: &[Value],
        removed_count: usize,
        removed_tokens: u64,
        keep_count: usize,
    ) -> String {
        if self.use_llm_summary {
            if let Some(summary) = self
                .llm_summary_via_endpoint(removed_messages, removed_count, removed_tokens)
                .await
            {
                return summary;
            }
        }

        let heuristic = self.heuristic_summary(removed_messages, removed_count, removed_tokens, keep_count);
        if heuristic.trim().is_empty() {
            // Final fallback: deterministic marker so compaction always
            // frees context even when the summarizer is unreachable.
            // Ported from Reasonix compact.go mechanicalFoldDigest.
            return mechanical_fold_digest(removed_count);
        }
        heuristic
    }

    async fn llm_summary_via_endpoint(
        &self,
        removed_messages: &[Value],
        removed_count: usize,
        removed_tokens: u64,
    ) -> Option<String> {
        let endpoint = std::env::var("HERMES_CONTEXT_SUMMARY_URL").ok()?;
        if endpoint.trim().is_empty() {
            return None;
        }

        let timeout_secs = std::env::var("HERMES_CONTEXT_SUMMARY_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(8)
            .clamp(1, 60);
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(timeout_secs))
            .build()
            .ok()?;
        let payload = serde_json::json!({
            "messages": removed_messages,
            "removed_count": removed_count,
            "removed_tokens": removed_tokens,
            "format": "concise-bullets"
        });
        let response = client.post(endpoint).json(&payload).send().await.ok()?;
        if !response.status().is_success() {
            return None;
        }
        let value = response.json::<Value>().await.ok()?;
        let summary = value.get("summary").and_then(|v| v.as_str())?;
        let summary = summary.trim();
        if summary.is_empty() {
            None
        } else {
            Some(summary.to_string())
        }
    }

    fn heuristic_summary(
        &self,
        removed_messages: &[Value],
        removed_count: usize,
        removed_tokens: u64,
        keep_count: usize,
    ) -> String {
        const MAX_SECTION_ITEMS: usize = 4;
        const MAX_ITEM_CHARS: usize = 180;
        const MAX_SUMMARY_CHARS: usize = 1700;

        let mut goals: Vec<String> = Vec::new();
        let mut decisions: Vec<String> = Vec::new();
        let mut tool_outcomes: Vec<String> = Vec::new();

        for msg in removed_messages.iter().rev() {
            let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
            let text = Self::extract_message_text(msg);
            if text.is_empty() {
                continue;
            }
            let concise = Self::truncate_sentence(&text, MAX_ITEM_CHARS);
            if concise.is_empty() {
                continue;
            }

            match role {
                "user" if goals.len() < MAX_SECTION_ITEMS => {
                    if !goals.contains(&concise) {
                        goals.push(concise);
                    }
                }
                "assistant" if decisions.len() < MAX_SECTION_ITEMS => {
                    if !decisions.contains(&concise) {
                        decisions.push(concise);
                    }
                }
                "tool" if tool_outcomes.len() < MAX_SECTION_ITEMS => {
                    if !tool_outcomes.contains(&concise) {
                        tool_outcomes.push(concise);
                    }
                }
                _ => {}
            }
        }

        goals.reverse();
        decisions.reverse();
        tool_outcomes.reverse();

        let mut lines: Vec<String> = vec![format!(
            "[Context compressed: {} earlier messages (~{} tokens) summarized. {} messages retained.]",
            removed_count, removed_tokens, keep_count
        )];

        if !goals.is_empty() {
            lines.push("User goals:".to_string());
            for g in &goals {
                lines.push(format!("- {g}"));
            }
        }
        if !decisions.is_empty() {
            lines.push("Assistant trajectory:".to_string());
            for d in &decisions {
                lines.push(format!("- {d}"));
            }
        }
        if !tool_outcomes.is_empty() {
            lines.push("Tool outcomes:".to_string());
            for t in &tool_outcomes {
                lines.push(format!("- {t}"));
            }
        }

        let mut out = lines.join("\n");
        if out.chars().count() > MAX_SUMMARY_CHARS {
            out = out.chars().take(MAX_SUMMARY_CHARS).collect::<String>() + "...";
        }
        out
    }

    fn extract_message_text(msg: &Value) -> String {
        if let Some(s) = msg.get("content").and_then(|c| c.as_str()) {
            return Self::normalize_whitespace(s);
        }
        if let Some(arr) = msg.get("content").and_then(|c| c.as_array()) {
            let text_parts: Vec<String> = arr
                .iter()
                .filter_map(|block| block.get("text").and_then(|t| t.as_str()))
                .map(Self::normalize_whitespace)
                .filter(|s| !s.is_empty())
                .collect();
            return text_parts.join(" ");
        }
        String::new()
    }

    fn normalize_whitespace(input: &str) -> String {
        input.split_whitespace().collect::<Vec<_>>().join(" ")
    }

    fn truncate_sentence(input: &str, max_chars: usize) -> String {
        if input.is_empty() {
            return String::new();
        }
        let mut clipped = input.to_string();
        if let Some((idx, _)) = input
            .char_indices()
            .find(|(_, c)| matches!(c, '.' | '!' | '?' | '\n'))
        {
            clipped = input[..=idx].to_string();
        }
        if clipped.chars().count() <= max_chars {
            return clipped;
        }
        clipped.chars().take(max_chars).collect::<String>() + "..."
    }
}

impl Default for DefaultContextEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ContextEngine for DefaultContextEngine {
    /// Override: use per-instance tokens_per_char ratio instead of the
    /// hardcoded estimate_tokens_rough default.  CJK-heavy conversations
    /// can configure a higher ratio for more accurate token budgeting.
    fn estimate_tokens(&self, messages: &[Value]) -> u64 {
        messages
            .iter()
            .map(|m| {
                let content = m.get("content").and_then(|c| c.as_str()).unwrap_or("");
                (content.len() as f64 * self.tokens_per_char) as u64
            })
            .sum()
    }

    async fn compress(
        &self,
        messages: &[Value],
        target_tokens: u64,
    ) -> Result<Vec<Value>, ContextError> {
        if messages.len() <= 2 {
            return Err(ContextError::TooSmall(messages.len()));
        }

        // Stuck guard: if auto-compaction has been paused because repeated
        // compressions failed to bring the context under the target window,
        // return the messages as-is rather than repeatedly collapsing the
        // prefix (which destroys the cache for zero benefit).
        if self.compact_stuck.load(Ordering::Relaxed) {
            tracing::warn!(
                "Compaction stuck — window too small, skipping auto-compaction to preserve cache prefix"
            );
            return Ok(messages.to_vec());
        }

        let current_tokens = self.estimate_tokens(messages);
        if current_tokens <= target_tokens {
            return Ok(messages.to_vec());
        }

        // Stage 1: Prune stale tool results (free — no API call).
        // Replace large old tool outputs with placeholders before
        // considering summarization.  Ported from Reasonix prune.go.
        let mut messages: Vec<Value> = messages.to_vec();
        let pinned = pinned_prefix_len(&messages, target_tokens);
        let initial_keep = std::cmp::max(
            std::cmp::max(2, (messages.len() as f64 * self.keep_ratio) as usize),
            pinned,
        );
        let initial_remove_end = messages.len() - initial_keep + pinned;
        let (pruned_results, pruned_saved) =
            prune_stale_tool_results(&mut messages, pinned, initial_remove_end);

        // Re-estimate after pruning; if pruning alone brought us under
        // target, skip the summarizer entirely.
        let current_tokens = self.estimate_tokens(&messages);
        if current_tokens <= target_tokens {
            if pruned_results > 0 {
                tracing::info!(
                    pruned_results,
                    pruned_saved,
                    "Pruning alone freed enough context; summarizer skipped"
                );
            }
            return Ok(messages);
        }

        // Count leading compression summaries + first user turn that must
        // never enter the removed region (Reasonix pinnedPrefixLen).
        // A later fold that re-summarizes an earlier digest would silently
        // drop user facts and destroy the byte-stable cache prefix.
        // The first user turn carries the user's original goal/constraints;
        // keeping it verbatim ensures those facts survive all compactions.
        let pinned = pinned_prefix_len(&messages, target_tokens);

        // Keep the last `keep_ratio` fraction of messages, but never
        // fewer than `pinned` (protects existing digests).
        let keep_count = std::cmp::max(
            std::cmp::max(2, (messages.len() as f64 * self.keep_ratio) as usize),
            pinned,
        );
        let removed_count = messages.len() - keep_count;

        // Count tokens in removed messages for the summary.
        // Skip pinned digests (they must never be re-summarized).
        let remove_start = pinned;
        let remove_end = removed_count + pinned;
        if remove_end <= remove_start || remove_end > messages.len() {
            // Nothing left to remove after protecting digests.
            return Ok(messages);
        }

        // Fold economics: skip compaction when the foldable region is too
        // small to justify the summarizer API call (≈400 tokens minimum).
        // Ported from Reasonix compact.go foldEconomics.
        const MIN_FOLD_TOKENS: u64 = 400;
        let foldable_tokens: u64 = messages[remove_start..remove_end]
            .iter()
            .map(|m| {
                let content = m.get("content").and_then(|c| c.as_str()).unwrap_or("");
                estimate_tokens_rough(content)
            })
            .sum();
        if foldable_tokens < MIN_FOLD_TOKENS {
            return Ok(messages);
        }

        // Partition the region: keep small user turns and prior digests
        // verbatim, fold the rest into a summary.
        // Ported from Reasonix compact.go partitionFold.
        let (kept, fold) = partition_fold(&messages[remove_start..remove_end], target_tokens);

        // If nothing to fold (all user turns were small enough to keep),
        // no summarization needed.
        if fold.is_empty() {
            return Ok(messages);
        }

        // Recalculate fold economics on just the foldable part.
        let fold_tokens: u64 = fold
            .iter()
            .map(|m| {
                let content = m.get("content").and_then(|c| c.as_str()).unwrap_or("");
                estimate_tokens_rough(content)
            })
            .sum();
        if fold_tokens < MIN_FOLD_TOKENS {
            return Ok(messages);
        }

        let removed_tokens: u64 = fold_tokens;

        let summary = self
            .maybe_generate_summary(
                &fold,
                fold.len(),
                removed_tokens,
                keep_count,
            )
            .await;

        // Build result: pinned + kept (verbatim user turns) + summary + tail.
        let mut result = Vec::with_capacity(pinned + kept.len() + 1 + (messages.len() - remove_end));
        // Preserve pinned compression-summary messages verbatim.
        result.extend_from_slice(&messages[..pinned]);
        // Preserve small user turns and prior digests from the fold region.
        result.extend(kept);
        result.push(serde_json::json!({
            "role": "user",
            "content": format!("<compression-summary>\n{}\n</compression-summary>", summary),
        }));
        result.extend_from_slice(&messages[remove_end..]);

        // If still over target, progressively remove more
        let new_tokens = self.estimate_tokens(&result);
        if new_tokens > target_tokens && result.len() > 3 {
            let excess_ratio = new_tokens as f64 / target_tokens as f64;
            let additional_remove =
                ((result.len() - 2) as f64 * (1.0 - 1.0 / excess_ratio)) as usize;
            if additional_remove > 0 && additional_remove < result.len() - 1 {
                let second_summary = format!(
                    "[Additional compression: {} more messages removed to fit context window.]",
                    additional_remove,
                );
                result[0] = serde_json::json!({
                    "role": "user",
                    "content": format!("<compression-summary>\n{}\n{}\n</compression-summary>", summary, second_summary),
                });
                result.drain(1..=additional_remove);
            }
        }

        // Stuck detection: if the result is still over the target window,
        // the context is too large to be compressed below the limit.
        // After two consecutive failed attempts, pause auto-compaction
        // to avoid an infinite collapse loop that destroys the cache prefix.
        let final_tokens = self.estimate_tokens(&result);
        if final_tokens > target_tokens {
            let prev = self.consecutive_compacts.fetch_add(1, Ordering::Relaxed);
            if prev >= 1 {
                self.compact_stuck.store(true, Ordering::Relaxed);
                tracing::warn!(
                    consecutive_failures = prev + 1,
                    final_tokens,
                    target_tokens,
                    "Compaction stuck: {} consecutive compactions failed to meet target; pausing auto-compaction",
                    prev + 1,
                );
            }
        } else {
            self.consecutive_compacts.store(0, Ordering::Relaxed);
            self.compact_stuck.store(false, Ordering::Relaxed);
        }

        Ok(result)
    }

    fn name(&self) -> &str {
        "default"
    }
}

/// Returns `true` when a message is a compaction summary produced by a
/// previous call to [`DefaultContextEngine::compress`].
///
/// These summaries are identified by the `<compression-summary>` XML tag
/// prefix in their content and a `user` role (matching Reasonix's
/// `isCompactionSummary` convention).  Once a fact reaches a digest it must
/// never be re-summarized — that would silently drop user-stated facts and
/// destroy the byte-stable cache prefix across compaction boundaries.
fn is_compression_summary(msg: &Value) -> bool {
    msg.get("role").and_then(|r| r.as_str()) == Some("user")
        && msg
            .get("content")
            .and_then(|c| c.as_str())
            .is_some_and(|c| c.trim_start().starts_with("<compression-summary>"))
}

// ---------------------------------------------------------------------------
// Prune stale tool results — free context reduction (no API call)
// ---------------------------------------------------------------------------

/// Minimum tool-result size (in bytes) to be eligible for pruning.
/// Ported from Reasonix prune.go `minPruneBytes`.
const MIN_PRUNE_BYTES: usize = 1024;

/// Marker prefix for pruned tool results, matching Reasonix convention.
const PRUNED_MARKER: &str = "[elided tool result — ";

/// Prune stale tool results in the compaction region, replacing large old
/// tool outputs with compact placeholders.  This is the free Stage 1 of
/// compaction: no summarizer API call, no message dropped — tool_call/result
/// pairing and assistant content are untouched.
///
/// Ported from Reasonix `prune.go PruneStaleToolResults`.  Returns the number
/// of results pruned and characters saved.
fn prune_stale_tool_results(
    messages: &mut [Value],
    region_start: usize,
    region_end: usize,
) -> (usize, usize) {
    let mut results = 0usize;
    let mut saved_chars = 0usize;

    for i in region_start..region_end.min(messages.len()) {
        let msg = &mut messages[i];
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role != "tool" {
            continue;
        }
        let content = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
        if content.len() < MIN_PRUNE_BYTES {
            continue;
        }
        // Skip already-pruned results (idempotent).
        if content.starts_with(PRUNED_MARKER) {
            continue;
        }
        // Skip error messages — they carry diagnostic value.
        let trimmed = content.trim().to_ascii_lowercase();
        if trimmed.starts_with("error:") || trimmed.starts_with("blocked:") {
            continue;
        }
        let tool_name = msg
            .get("name")
            .and_then(|n| n.as_str())
            .unwrap_or("unknown");
        let placeholder = format!(
            "{}{}, {} bytes dropped to save context; re-run the tool if the data is needed again]",
            PRUNED_MARKER,
            tool_name,
            content.len()
        );
        saved_chars += content.len() - placeholder.len();
        if let Some(c) = msg.get_mut("content") {
            *c = Value::String(placeholder);
        }
        results += 1;
    }

    if results > 0 {
        tracing::info!(
            results,
            saved_chars,
            "Pruned stale tool results before compaction (free context reduction)"
        );
    }
    (results, saved_chars)
}

// ---------------------------------------------------------------------------
// Partition fold — keep small user turns verbatim, fold the rest
// ---------------------------------------------------------------------------

/// Maximum token count for a user turn to be kept verbatim in the fold.
/// Larger turns (pasted content) stay foldable so the kept-verbatim floor
/// never starves the window.  Ported from Reasonix `maxPinnedFirstUserTokens`.
const MAX_PINNABLE_USER_TOKENS: u64 = 1500;

/// Ceiling on pinning a user turn as a fraction of the target window.
/// Ported from Reasonix `pinnedFirstUserWindowFrac`.
const PINNABLE_USER_WINDOW_FRAC: f64 = 0.15;

/// Partition a compaction region into what is kept verbatim and what folds
/// into the summary.
///
/// **Kept verbatim**: small user turns (a fact the user stated is never
/// summarized away) and prior compression summaries (a later fold never
/// re-summarizes an earlier digest, preventing drift that silently drops
/// user-stated facts after the second compaction).
///
/// **Folded**: everything else (assistant messages, tool results, large
/// user turns).
///
/// Ported from Reasonix `compact.go partitionFold`.
fn partition_fold(region: &[Value], target_tokens: u64) -> (Vec<Value>, Vec<Value>) {
    let max_tok = MAX_PINNABLE_USER_TOKENS
        .min((target_tokens as f64 * PINNABLE_USER_WINDOW_FRAC) as u64);
    let mut kept = Vec::new();
    let mut fold = Vec::new();
    for m in region {
        let role = m.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if is_compression_summary(m) {
            kept.push(m.clone());
        } else if role == "user" {
            let content = m.get("content").and_then(|c| c.as_str()).unwrap_or("");
            if estimate_tokens_rough(content) <= max_tok {
                kept.push(m.clone());
            } else {
                fold.push(m.clone());
            }
        } else {
            fold.push(m.clone());
        }
    }
    (kept, fold)
}

/// Deterministic stand-in used when the summarizer is unreachable or
/// produces empty output.  The foldable region is already accounted for
/// (archived in Reasonix), so the digest just notes the gap and points
/// the model at the user for anything it needs from before this point.
///
/// Ported from Reasonix `compact.go mechanicalFoldDigest`.
fn mechanical_fold_digest(n: usize) -> String {
    format!(
        "{n} earlier message(s) were folded here to free context, but the automatic summary was unavailable. \
         Ask the user if you need details from before this point."
    )
}

/// Compute the leading prefix length that must never be compacted.
///
/// This mirrors Reasonix `pinnedPrefixLen()`:
///   1. system prompt (index 0) — always pinned
///   2. first user turn — pinned if ≤ 1500 tokens AND ≤ 15% of target window
///   3. prior compression summaries — pinned to prevent re-summarization
fn pinned_prefix_len(messages: &[Value], target_tokens: u64) -> usize {
    let mut i = 0;
    let n = messages.len();

    // 1. System prompt is always first, always pinned.
    if i < n && messages[i].get("role").and_then(|r| r.as_str()) == Some("system") {
        i += 1;
    }

    // 2. First user turn — pin verbatim if it fits within budget.
    const MAX_PINNED_FIRST_USER_TOKENS: u64 = 1500;
    const PINNED_FIRST_USER_WINDOW_FRAC: f64 = 0.15;
    let max_tok = MAX_PINNED_FIRST_USER_TOKENS
        .min((target_tokens as f64 * PINNED_FIRST_USER_WINDOW_FRAC) as u64);
    if i < n
        && messages[i].get("role").and_then(|r| r.as_str()) == Some("user")
        && !is_compression_summary(&messages[i])
    {
        let content = messages[i]
            .get("content")
            .and_then(|c| c.as_str())
            .unwrap_or("");
        if estimate_tokens_rough(content) <= max_tok {
            i += 1;
        }
    }

    // 3. Prior compression summaries — identified by the
    //    <compression-summary> marker, always pinned.
    while i < n && is_compression_summary(&messages[i]) {
        i += 1;
    }

    i
}

// ---------------------------------------------------------------------------
// ImportanceBasedEngine — token budget with message scoring
// ---------------------------------------------------------------------------

/// Context engine that assigns importance scores to messages and
/// drops the least important ones to fit within the token budget.
pub struct ImportanceBasedEngine {
    /// System messages always have this score.
    pub system_importance: f64,
    /// Recent user messages get boosted importance.
    pub recency_weight: f64,
    /// Tool results get this base importance.
    pub tool_result_importance: f64,
}

impl ImportanceBasedEngine {
    pub fn new() -> Self {
        Self {
            system_importance: 1.0,
            recency_weight: 0.3,
            tool_result_importance: 0.5,
        }
    }

    fn score_message(&self, msg: &Value, index: usize, total: usize) -> f64 {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        let recency = (index as f64) / (total as f64).max(1.0);

        match role {
            "system" => self.system_importance,
            "tool" => self.tool_result_importance + recency * self.recency_weight,
            "assistant" => 0.6 + recency * self.recency_weight,
            "user" => 0.7 + recency * self.recency_weight,
            _ => 0.3 + recency * self.recency_weight,
        }
    }
}

impl Default for ImportanceBasedEngine {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ContextEngine for ImportanceBasedEngine {
    async fn compress(
        &self,
        messages: &[Value],
        target_tokens: u64,
    ) -> Result<Vec<Value>, ContextError> {
        if messages.len() <= 2 {
            return Err(ContextError::TooSmall(messages.len()));
        }

        let total = messages.len();

        // Score each message
        let mut scored: Vec<(usize, f64, u64)> = messages
            .iter()
            .enumerate()
            .map(|(i, m)| {
                let score = self.score_message(m, i, total);
                let tokens =
                    estimate_tokens_rough(m.get("content").and_then(|c| c.as_str()).unwrap_or(""));
                (i, score, tokens)
            })
            .collect();

        // Sort by importance (descending), keep adding until budget filled
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let mut budget = target_tokens;
        let mut keep_indices: Vec<usize> = Vec::new();

        for (idx, _score, tokens) in &scored {
            if *tokens <= budget {
                keep_indices.push(*idx);
                budget -= tokens;
            }
        }

        // Restore original order
        keep_indices.sort();

        if keep_indices.is_empty() {
            return Err(ContextError::CompressionFailed(
                "Could not fit any messages within target tokens".into(),
            ));
        }

        let dropped = total - keep_indices.len();
        let mut result = Vec::with_capacity(keep_indices.len() + 1);

        if dropped > 0 {
            result.push(serde_json::json!({
                "role": "system",
                "content": format!(
                    "[Context compressed: {} of {} messages retained (dropped {} low-priority messages).]",
                    keep_indices.len(), total, dropped,
                ),
            }));
        }

        for &idx in &keep_indices {
            result.push(messages[idx].clone());
        }

        Ok(result)
    }

    fn name(&self) -> &str {
        "importance"
    }
}

// ---------------------------------------------------------------------------
// Token counting helpers
// ---------------------------------------------------------------------------

fn is_image_content_block(block: &Value) -> bool {
    matches!(
        block.get("type").and_then(|t| t.as_str()),
        Some("image") | Some("image_url") | Some("input_image")
    ) || block.get("image_url").is_some()
}

/// Count tokens for content that may be a string or an array of content blocks.
pub fn count_content_tokens(content: &Value) -> u64 {
    if let Some(s) = content.as_str() {
        return estimate_tokens_rough(s);
    }
    if let Some(arr) = content.as_array() {
        return arr
            .iter()
            .map(|block| {
                if is_image_content_block(block) {
                    return IMAGE_TOKEN_ESTIMATE;
                }
                if let Some(s) = block.as_str() {
                    return estimate_tokens_rough(s);
                }
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    estimate_tokens_rough(text)
                } else {
                    estimate_tokens_rough(&block.to_string())
                }
            })
            .sum();
    }
    0
}

/// Estimate total tokens for a full message including role overhead.
pub fn estimate_message_tokens(msg: &Value) -> u64 {
    let role_overhead: u64 = 4; // ~4 tokens for role metadata
    let content_tokens = msg.get("content").map(count_content_tokens).unwrap_or(0);
    let tool_calls_tokens = msg
        .get("tool_calls")
        .and_then(|tc| tc.as_array())
        .map(|calls| {
            calls
                .iter()
                .map(|c| estimate_tokens_rough(&c.to_string()))
                .sum::<u64>()
        })
        .unwrap_or(0);

    role_overhead + content_tokens + tool_calls_tokens
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn make_messages(count: usize) -> Vec<Value> {
        (0..count)
            .map(|i| {
                json!({
                    "role": if i % 2 == 0 { "user" } else { "assistant" },
                    "content": format!("Message {} with some content to make it longer for token estimation purposes and testing", i),
                })
            })
            .collect()
    }

    #[tokio::test]
    async fn test_default_engine_compress() {
        let engine = DefaultContextEngine::new();
        // partition_fold keeps small user turns verbatim; only assistant
        // messages fold.  With 80 messages, ~27 assistant messages land in
        // the fold region at ~20 tokens each = ~540 tokens → passes the
        // 400-token foldEconomics threshold.
        let messages = make_messages(80);
        let result = engine.compress(&messages, 200).await.unwrap();
        assert!(result.len() < 80, "compressed result should be smaller");
        // The compression summary should appear somewhere in the result
        // (after pinned prefix and kept user turns).
        assert!(
            result.iter().any(|m| {
                m.get("content")
                    .and_then(|c| c.as_str())
                    .is_some_and(|c| c.contains("compression-summary"))
            }),
            "a compression-summary should be present in the result"
        );
    }

    #[tokio::test]
    async fn test_partition_fold_preserves_small_user_turns() {
        // partition_fold should keep small user turns verbatim and only
        // fold assistant/tool messages into the summary.
        let messages = make_messages(80);
        let pinned = pinned_prefix_len(&messages, 200);
        let keep_count = std::cmp::max(
            std::cmp::max(2, (80.0 * 0.33) as usize),
            pinned,
        );
        let remove_end = 80 - keep_count + pinned;
        let (kept, fold) = partition_fold(&messages[pinned..remove_end], 200);
        // All kept messages should be user turns (small enough to pin).
        assert!(!kept.is_empty(), "some user turns should be kept verbatim");
        assert!(kept.iter().all(|m| {
            m.get("role").and_then(|r| r.as_str()) == Some("user")
        }), "only user turns should be kept verbatim");
        // All folded messages should be assistant turns.
        assert!(!fold.is_empty(), "some assistant messages should be folded");
        assert!(fold.iter().all(|m| {
            m.get("role").and_then(|r| r.as_str()) == Some("assistant")
        }), "only assistant messages should be folded");
    }

    #[tokio::test]
    async fn test_default_engine_no_compress_needed() {
        let engine = DefaultContextEngine::new();
        let messages = make_messages(3);
        let result = engine.compress(&messages, 10_000).await.unwrap();
        assert_eq!(result.len(), 3); // No compression needed
    }

    #[tokio::test]
    async fn test_default_engine_too_small() {
        let engine = DefaultContextEngine::new();
        let messages = make_messages(2);
        assert!(engine.compress(&messages, 10).await.is_err());
    }

    #[tokio::test]
    async fn test_importance_engine() {
        let engine = ImportanceBasedEngine::new();
        let messages = make_messages(20);
        let result = engine.compress(&messages, 200).await.unwrap();
        assert!(result.len() < 20);
    }

    #[test]
    fn test_count_content_tokens() {
        assert!(count_content_tokens(&json!("hello world")) > 0);
        assert!(
            count_content_tokens(&json!([
                {"type": "text", "text": "hello"},
                {"type": "text", "text": "world"},
            ])) > 0
        );
    }

    #[test]
    fn image_blocks_charge_fixed_budget_without_counting_base64_payload() {
        let huge_data = format!("data:image/png;base64,{}", "a".repeat(100_000));
        let content = json!([
            {"type": "text", "text": "look at this"},
            {"type": "image_url", "image_url": {"url": huge_data}},
            {"type": "input_image", "image_url": "https://example.com/a.png"},
            {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "b".repeat(100_000)}}
        ]);
        let text_only = estimate_tokens_rough("look at this");
        assert_eq!(
            count_content_tokens(&content),
            text_only + IMAGE_TOKEN_ESTIMATE * 3
        );
    }

    #[test]
    fn mixed_content_blocks_count_text_and_bare_strings() {
        let content = json!([
            {"type": "text", "text": "hello"},
            "world",
            {"type": "tool_result", "value": "ok"}
        ]);
        let estimate = count_content_tokens(&content);
        assert!(estimate >= estimate_tokens_rough("hello") + estimate_tokens_rough("world"));
    }

    #[test]
    fn test_estimate_message_tokens() {
        let msg = json!({"role": "user", "content": "hello world"});
        assert!(estimate_message_tokens(&msg) > 0);
    }
}
