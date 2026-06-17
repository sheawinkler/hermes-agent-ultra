//! Per-turn prefix-shape diagnostics for prompt caching.
//!
//! Ported from Reasonix `internal/agent/cache_shape.go`.  Captures a
//! deterministic fingerprint of the system prompt and tool schemas at the
//! start of each turn, then compares against the previous turn to explain
//! cache misses.

use hermes_core::{ToolSchema, UsageStats};
use sha2::{Digest, Sha256};
use tracing::debug;

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
            let json_len = serde_json::to_string(s)
                .map(|j| j.len())
                .unwrap_or(0);
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
    let (cache_hit_tokens, cache_miss_tokens) = usage
        .map(|u| (u.cache_read_tokens, u.cache_write_tokens))
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
        session_hit: 0,   // filled by caller from AgentSharedState
        session_miss: 0,  // filled by caller from AgentSharedState
    }
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
    use hermes_core::{tool_schema, JsonSchema};

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
        let a = capture_shape(
            "system",
            &[sample_schema("tool_a", "A")],
            0,
        );
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
        let a = capture_shape(
            "s",
            &[sample_schema("b", "B"), sample_schema("a", "A")],
            0,
        );
        let b = capture_shape(
            "s",
            &[sample_schema("a", "A"), sample_schema("b", "B")],
            0,
        );
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
        assert!(diag.prefix_change_reasons.contains(&"log_rewrite".to_string()));
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
}
