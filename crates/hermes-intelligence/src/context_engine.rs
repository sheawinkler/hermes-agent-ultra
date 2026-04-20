//! Context engine — pluggable context compression for long conversations.
//!
//! Provides a trait-based interface for compressing conversation context when
//! it approaches the model's context window limit.

use async_trait::async_trait;
use serde_json::Value;
use std::time::Duration;

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
}

impl DefaultContextEngine {
    pub fn new() -> Self {
        Self {
            keep_ratio: 0.33,
            use_llm_summary: false,
        }
    }

    pub fn with_keep_ratio(mut self, ratio: f64) -> Self {
        self.keep_ratio = ratio.clamp(0.1, 0.9);
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

        self.heuristic_summary(removed_messages, removed_count, removed_tokens, keep_count)
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
    async fn compress(
        &self,
        messages: &[Value],
        target_tokens: u64,
    ) -> Result<Vec<Value>, ContextError> {
        if messages.len() <= 2 {
            return Err(ContextError::TooSmall(messages.len()));
        }

        let current_tokens = self.estimate_tokens(messages);
        if current_tokens <= target_tokens {
            return Ok(messages.to_vec());
        }

        // Keep the last `keep_ratio` fraction of messages
        let keep_count = std::cmp::max(2, (messages.len() as f64 * self.keep_ratio) as usize);
        let removed_count = messages.len() - keep_count;

        // Count tokens in removed messages for the summary
        let removed_tokens: u64 = messages[..removed_count]
            .iter()
            .map(|m| {
                let content = m.get("content").and_then(|c| c.as_str()).unwrap_or("");
                estimate_tokens_rough(content)
            })
            .sum();

        let summary = self
            .maybe_generate_summary(
                &messages[..removed_count],
                removed_count,
                removed_tokens,
                keep_count,
            )
            .await;

        let mut result = Vec::with_capacity(keep_count + 1);
        result.push(serde_json::json!({
            "role": "system",
            "content": summary,
        }));
        result.extend_from_slice(&messages[removed_count..]);

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
                    "role": "system",
                    "content": format!("{}\n{}", summary, second_summary),
                });
                result.drain(1..=additional_remove);
            }
        }

        Ok(result)
    }

    fn name(&self) -> &str {
        "default"
    }
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

/// Count tokens for content that may be a string or an array of content blocks.
pub fn count_content_tokens(content: &Value) -> u64 {
    if let Some(s) = content.as_str() {
        return estimate_tokens_rough(s);
    }
    if let Some(arr) = content.as_array() {
        return arr
            .iter()
            .map(|block| {
                if let Some(text) = block.get("text").and_then(|t| t.as_str()) {
                    estimate_tokens_rough(text)
                } else {
                    // Image blocks, tool results, etc. — rough estimate
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
        let messages = make_messages(20);
        let result = engine.compress(&messages, 100).await.unwrap();
        assert!(result.len() < 20);
        assert!(result[0]
            .get("content")
            .unwrap()
            .as_str()
            .unwrap()
            .contains("compressed"));
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
    fn test_estimate_message_tokens() {
        let msg = json!({"role": "user", "content": "hello world"});
        assert!(estimate_message_tokens(&msg) > 0);
    }
}
