//! Per-turn prefix-shape diagnostics for prompt caching.
//!
//! Ported from Reasonix `internal/agent/cache_shape.go`.  Captures a
//! deterministic fingerprint of the system prompt and tool schemas at the
//! start of each turn, then compares against the previous turn to explain
//! cache misses.

use hermes_core::{MessageRole, ToolSchema, UsageStats};
use sha2::{Digest, Sha256};
use tracing::{debug, info};

// ---------------------------------------------------------------------------
// PrefixShape
// ---------------------------------------------------------------------------

/// A deterministic fingerprint of the cacheable prefix (system prompt +
/// tool schemas) captured at the start of a turn.
#[derive(Debug, Clone, PartialEq)]
pub struct PrefixShape {
    /// SHA-256 of the merged system prompt text.
    pub system_hash: String,
    /// SHA-256 of the sorted, normalized tool-schema JSON.
    pub tools_hash: String,
    /// SHA-256 of system_hash + tools_hash (the full prefix sent to the provider).
    pub prefix_hash: String,
    /// Compaction / log-rewrite version from the session.  Increments on
    /// each compaction, which resets the cache.
    pub log_rewrite_version: u32,
    /// Rough token estimate for the tool schema block.
    pub tool_schema_tokens: u64,
}

// ---------------------------------------------------------------------------
// CacheDiagnostics
// ---------------------------------------------------------------------------

/// Result of comparing the current turn's [`PrefixShape`] against the
/// previous turn's shape, explaining any cache-miss delta reported in the
/// provider's usage response.
#[derive(Debug, Clone)]
pub struct CacheDiagnostics {
    /// Hex-encoded SHA-256 of the current prefix (system + tools).
    pub prefix_hash: String,
    /// True when the prefix hash differs from the previous turn.
    pub prefix_changed: bool,
    /// Which components caused the prefix change (e.g. "system", "tools", "log_rewrite").
    pub prefix_change_reasons: Vec<String>,
    pub system_hash: String,
    pub tools_hash: String,
    pub log_rewrite_version: u32,
    pub tool_schema_tokens: u64,
    /// Provider-reported cache-miss input tokens for this turn (0 if usage is None).
    pub cache_miss_tokens: u64,
    /// Provider-reported cache-hit input tokens for this turn (0 if usage is None).
    pub cache_hit_tokens: u64,
    /// Cumulative session-level cache-hit tokens.
    pub session_hit: u64,
    /// Cumulative session-level cache-miss tokens.
    pub session_miss: u64,
}

// ---------------------------------------------------------------------------
// capture_shape
// ---------------------------------------------------------------------------

/// Builds a [`PrefixShape`] from the given system-prompt text, resolved tool
/// schemas, and the current compaction version.
pub fn capture_shape(
    system_prompt: &str,
    schemas: &[ToolSchema],
    rewrite_version: u32,
) -> PrefixShape {
    let tools_hash = hash_normalized_tool_schemas(schemas);
    let system_hash = hash_string(system_prompt);
    let combined = format!("{system_hash}{tools_hash}");
    let prefix_hash = hash_string(&combined);

    // Rough token estimate: 1 token ≈ 4 chars for most JSON schema blocks.
    let tool_schema_tokens = schemas
        .iter()
        .map(|s| {
            let json_len = serde_json::to_string(s).map(|j| j.len()).unwrap_or(0);
            (json_len as f64 * 0.25) as u64
        })
        .sum();

    PrefixShape {
        system_hash,
        tools_hash,
        prefix_hash,
        log_rewrite_version: rewrite_version,
        tool_schema_tokens,
    }
}

// ---------------------------------------------------------------------------
// compare_shape
// ---------------------------------------------------------------------------

/// Compares the current [`PrefixShape`] against `prev`, attaching provider
/// usage counters from `usage` (when available).
pub fn compare_shape(
    prev: &PrefixShape,
    cur: &PrefixShape,
    usage: Option<&UsageStats>,
) -> CacheDiagnostics {
    let mut reasons: Vec<String> = Vec::new();
    if cur.system_hash != prev.system_hash {
        reasons.push("system".to_string());
    }
    if cur.tools_hash != prev.tools_hash {
        reasons.push("tools".to_string());
    }
    if cur.log_rewrite_version != prev.log_rewrite_version {
        reasons.push("log_rewrite".to_string());
    }

    let prefix_changed = !reasons.is_empty();
    // For DeepSeek (no cache-write phase), miss tokens live in `input_tokens`.
    // For Anthropic, miss = `cache_write_tokens` (cache creation) + `input_tokens`
    // (truly uncached).  Summing both correctly covers every provider.
    let (cache_hit_tokens, cache_miss_tokens) = usage
        .map(|u| (u.cache_read_tokens, u.cache_write_tokens + u.input_tokens))
        .unwrap_or((0, 0));

    if prefix_changed {
        debug!(
            reasons = ?reasons,
            prev_hash = %prev.prefix_hash,
            cur_hash = %cur.prefix_hash,
            "Cache prefix changed"
        );
    }

    CacheDiagnostics {
        prefix_hash: cur.prefix_hash.clone(),
        prefix_changed,
        prefix_change_reasons: reasons,
        system_hash: cur.system_hash.clone(),
        tools_hash: cur.tools_hash.clone(),
        log_rewrite_version: cur.log_rewrite_version,
        tool_schema_tokens: cur.tool_schema_tokens,
        cache_miss_tokens,
        cache_hit_tokens,
        session_hit: 0,  // filled by caller from AgentSharedState
        session_miss: 0, // filled by caller from AgentSharedState
    }
}

// ---------------------------------------------------------------------------
// trace_turn — convenience function for production wiring
// ---------------------------------------------------------------------------

/// Capture the current prefix shape from context messages + tool schemas,
/// compare against the previous turn's shape (stored in `prev_shape`),
/// log a structured diagnostic line, and return the new shape + cumulative
/// session counters.
///
/// Call this right after receiving an LLM response, passing the usage stats
/// from the response. The `log_rewrite_version` should come from the
/// compression module (increments on each compaction).
pub fn trace_turn(
    messages: &[hermes_core::types::Message],
    tool_schemas: &[ToolSchema],
    log_rewrite_version: u32,
    usage: Option<&UsageStats>,
    prev_shape: Option<&PrefixShape>,
    session_hit: u64,
    session_miss: u64,
) -> (PrefixShape, CacheDiagnostics) {
    // Extract system prompt from messages (first system message).
    let system_prompt: String = messages
        .iter()
        .find(|m| m.role == MessageRole::System)
        .and_then(|m| m.content.as_deref())
        .unwrap_or("")
        .to_string();

    let cur_shape = capture_shape(&system_prompt, tool_schemas, log_rewrite_version);
    let mut diag = if let Some(prev) = prev_shape {
        compare_shape(prev, &cur_shape, usage)
    } else {
        // First turn — no previous shape to compare.
        let (hit, miss) = usage
            .map(|u| (u.cache_read_tokens, u.cache_write_tokens + u.input_tokens))
            .unwrap_or((0, 0));
        CacheDiagnostics {
            prefix_hash: cur_shape.prefix_hash.clone(),
            prefix_changed: false,
            prefix_change_reasons: vec![],
            system_hash: cur_shape.system_hash.clone(),
            tools_hash: cur_shape.tools_hash.clone(),
            log_rewrite_version: cur_shape.log_rewrite_version,
            tool_schema_tokens: cur_shape.tool_schema_tokens,
            cache_miss_tokens: miss,
            cache_hit_tokens: hit,
            session_hit,
            session_miss,
        }
    };

    // Update cumulative session counters.
    diag.session_hit = session_hit + diag.cache_hit_tokens;
    diag.session_miss = session_miss + diag.cache_miss_tokens;

    // Structured log line for diagnostics.
    let total = diag.session_hit + diag.session_miss;
    let session_rate = if total > 0 {
        diag.session_hit * 100 / total
    } else {
        0
    };
    let turn_total = diag.cache_hit_tokens + diag.cache_miss_tokens;
    let turn_rate = if turn_total > 0 {
        diag.cache_hit_tokens * 100 / turn_total
    } else {
        0
    };

    // Full token breakdown from provider usage.
    let u = usage.cloned().unwrap_or_default();
    let short_hash = &diag.prefix_hash[..8.min(diag.prefix_hash.len())];

    // Human-readable explanation for cache miss.
    let miss_explanation = if diag.prefix_changed {
        let reasons: Vec<&str> = diag
            .prefix_change_reasons
            .iter()
            .map(|s| match s.as_str() {
                "system" => "system prompt changed",
                "tools" => "tool set changed (added/removed/modified)",
                "log_rewrite" => "context compaction rewrote message history",
                _ => s.as_str(),
            })
            .collect();
        format!("prefix changed: {}", reasons.join("; "))
    } else if turn_total == 0 {
        "no usage data".to_string()
    } else if diag.cache_miss_tokens > 0 {
        "prefix stable — miss is from incremental content (new user msg / tool result)".to_string()
    } else {
        "full hit".to_string()
    };

    info!(
        target: "cache_diag",
        // Per-turn token composition
        turn_hit = diag.cache_hit_tokens,
        turn_miss = diag.cache_miss_tokens,
        turn_rate = %format!("{}%", turn_rate),
        input_tokens = u.input_tokens,
        output_tokens = u.output_tokens,
        cache_read = u.cache_read_tokens,
        cache_write = u.cache_write_tokens,
        reasoning = u.reasoning_tokens,
        completion = u.completion_tokens,
        prompt_tokens = u.prompt_tokens,
        total_tokens = u.total_tokens,
        // Session cumulative
        session_hit = diag.session_hit,
        session_miss = diag.session_miss,
        session_rate = %format!("{}%", session_rate),
        // Prefix diagnostics
        prefix_changed = diag.prefix_changed,
        reasons = ?diag.prefix_change_reasons,
        miss_explanation = %miss_explanation,
        tool_schema_tokens = diag.tool_schema_tokens,
        prefix_hash = %short_hash,
        "cache_diag: hit={} miss={} rate={}% | input={} output={} cache_read={} cache_write={} reasoning={} | session rate={}% | {}",
        diag.cache_hit_tokens,
        diag.cache_miss_tokens,
        turn_rate,
        u.input_tokens,
        u.output_tokens,
        u.cache_read_tokens,
        u.cache_write_tokens,
        u.reasoning_tokens,
        session_rate,
        miss_explanation,
    );

    (cur_shape, diag)
}

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

fn hash_string(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let result = hasher.finalize();
    result.iter().map(|b| format!("{:02x}", b)).collect()
}

/// Hash tool schemas in a deterministic order so the same set always
/// produces the same fingerprint regardless of insertion order.
fn hash_normalized_tool_schemas(schemas: &[ToolSchema]) -> String {
    let mut canon = schemas.to_vec();
    canon.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.description.cmp(&b.description))
            .then_with(|| {
                let a_params = serde_json::to_string(&a.parameters).unwrap_or_default();
                let b_params = serde_json::to_string(&b.parameters).unwrap_or_default();
                a_params.cmp(&b_params)
            })
    });
    let json = serde_json::to_string(&canon).unwrap_or_default();
    hash_string(&json)
}

// ---------------------------------------------------------------------------
// tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::{JsonSchema, tool_schema};

    fn sample_schema(name: &str, desc: &str) -> ToolSchema {
        tool_schema(name, desc, JsonSchema::new("object"))
    }

    #[test]
    fn same_system_and_tools_gives_same_hash() {
        let schemas = vec![
            sample_schema("read_file", "Read a file from disk."),
            sample_schema("write_file", "Write a file to disk."),
        ];
        let a = capture_shape("You are a helpful assistant.", &schemas, 0);
        let b = capture_shape("You are a helpful assistant.", &schemas, 0);
        assert_eq!(a.prefix_hash, b.prefix_hash);
        assert_eq!(a.system_hash, b.system_hash);
        assert_eq!(a.tools_hash, b.tools_hash);
    }

    #[test]
    fn changed_system_gives_different_prefix() {
        let schemas = vec![sample_schema("tool_a", "A tool")];
        let a = capture_shape("System v1", &schemas, 0);
        let b = capture_shape("System v2", &schemas, 0);
        assert_ne!(a.prefix_hash, b.prefix_hash);
        assert_ne!(a.system_hash, b.system_hash);
        assert_eq!(a.tools_hash, b.tools_hash);
    }

    #[test]
    fn changed_tools_gives_different_prefix() {
        let a = capture_shape("system", &[sample_schema("tool_a", "A")], 0);
        let b = capture_shape(
            "system",
            &[sample_schema("tool_a", "A"), sample_schema("tool_b", "B")],
            0,
        );
        assert_ne!(a.prefix_hash, b.prefix_hash);
        assert_ne!(a.tools_hash, b.tools_hash);
        assert_eq!(a.system_hash, b.system_hash);
    }

    #[test]
    fn schema_order_independent() {
        let a = capture_shape("s", &[sample_schema("b", "B"), sample_schema("a", "A")], 0);
        let b = capture_shape("s", &[sample_schema("a", "A"), sample_schema("b", "B")], 0);
        assert_eq!(a.tools_hash, b.tools_hash);
        assert_eq!(a.prefix_hash, b.prefix_hash);
    }

    #[test]
    fn rewrite_version_triggers_change_reason() {
        let schemas = vec![sample_schema("t", "T")];
        let prev = capture_shape("system", &schemas, 0);
        let cur = capture_shape("system", &schemas, 1);
        let diag = compare_shape(&prev, &cur, None);
        assert!(diag.prefix_changed);
        assert!(
            diag.prefix_change_reasons
                .contains(&"log_rewrite".to_string())
        );
    }

    #[test]
    fn usage_buckets_flow_to_diagnostics() {
        let schemas = vec![sample_schema("t", "T")];
        let shape = capture_shape("system", &schemas, 0);
        let usage = UsageStats {
            cache_read_tokens: 100,
            cache_write_tokens: 50,
            ..Default::default()
        };
        let diag = compare_shape(&shape, &shape, Some(&usage));
        assert!(!diag.prefix_changed);
        assert_eq!(diag.cache_hit_tokens, 100);
        assert_eq!(diag.cache_miss_tokens, 50);
    }

    #[test]
    fn deepseek_miss_tokens_mapped_from_input_tokens() {
        // DeepSeek has no cache_write phase: cache_write_tokens = 0,
        // and miss tokens are in input_tokens.  The diagnostics must
        // report the correct miss count, not 0.
        let schemas = vec![sample_schema("t", "T")];
        let shape = capture_shape("system", &schemas, 0);
        let usage = UsageStats {
            cache_read_tokens: 3500,   // prompt_cache_hit_tokens
            cache_write_tokens: 0,     // DeepSeek: no write phase
            input_tokens: 1500,        // prompt_cache_miss_tokens
            ..Default::default()
        };
        let diag = compare_shape(&shape, &shape, Some(&usage));
        assert_eq!(diag.cache_hit_tokens, 3500);
        assert_eq!(diag.cache_miss_tokens, 1500); // NOT 0
    }
}
