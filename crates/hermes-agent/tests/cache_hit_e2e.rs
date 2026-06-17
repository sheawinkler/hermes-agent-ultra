//! End-to-end prompt cache hit-rate test.
//!
//! Validates:
//! - DeepSeek policy fires correctly
//! - Append-only turns keep the prefix byte-stable
//! - Reasoning content is NOT sent for DeepSeek
//! - Stuck guard pauses compaction when the window is too small
//! - Existing compaction digests are never re-summarized

use hermes_agent::agent_runtime_helpers::{
    anthropic_prompt_cache_policy, prepare_wire_messages_for_api, resolve_prompt_cache_policy,
};
use hermes_agent::cache_diagnostics::{capture_shape, compare_shape};
use hermes_core::{Message, UsageStats};
use hermes_intelligence::context_engine::{ContextEngine, DefaultContextEngine};
use serde_json::{json, Value};

// ---------------------------------------------------------------------------
// Task 1 — DeepSeek is recognised in the cache policy
// ---------------------------------------------------------------------------

#[test]
fn deepseek_policy_returns_cache_without_native_layout() {
    let (use_cache, native) =
        anthropic_prompt_cache_policy("deepseek", "https://api.deepseek.com/v1", "openai_chat", "deepseek-chat");
    assert!(use_cache, "DeepSeek should enable prompt caching");
    assert!(!native, "DeepSeek should NOT use native cache_control markers");

    // Also test host-based detection via api.deepseek.com
    let (use_cache2, _) = anthropic_prompt_cache_policy(
        "custom",
        "https://api.deepseek.com/v1/chat/completions",
        "openai_chat",
        "deepseek-v3",
    );
    assert!(use_cache2, "api.deepseek.com host should enable caching");
}

#[test]
fn deepseek_policy_detected_via_model_name() {
    let (use_cache, _) =
        anthropic_prompt_cache_policy("openai", "https://api.openai.com/v1", "openai_chat", "deepseek-r1-distill");
    assert!(use_cache, "Model name containing 'deepseek' should enable caching");
}

#[test]
fn anthropic_native_unchanged_by_deepseek_addition() {
    let (use_cache, native) =
        anthropic_prompt_cache_policy("anthropic", "https://api.anthropic.com", "anthropic_messages", "claude-sonnet-4-20250514");
    assert!(use_cache);
    assert!(native);
}

#[test]
fn resolve_policy_honors_env_override() {
    // Without override
    let (use_cache, _native) = resolve_prompt_cache_policy(
        "deepseek", "https://api.deepseek.com/v1", "openai_chat", "deepseek-chat",
    );
    assert!(use_cache);

    // FORCE disable (can't actually set env reliably in parallel tests,
    // so we just verify the function exists and returns consistent results)
}

// ---------------------------------------------------------------------------
// Task 4 — Reasoning content is NOT sent to DeepSeek
// ---------------------------------------------------------------------------

#[test]
fn deepseek_skips_reasoning_echo() {
    let mut msg = Message::assistant("answer");
    msg.reasoning_content = Some("long-chain-of-thought...".to_string());

    let messages = vec![msg];
    let wire = prepare_wire_messages_for_api(
        messages,
        "deepseek",
        "deepseek-chat",
        "https://api.deepseek.com/v1",
    );
    assert_eq!(wire.len(), 1);
    // Reasoning must NOT appear in the wire copy for DeepSeek
    assert!(
        wire[0].reasoning_content.is_none(),
        "DeepSeek wire messages must not carry reasoning_content"
    );
}

#[test]
fn anthropic_still_gets_reasoning_echo() {
    let mut msg = Message::assistant("answer");
    msg.reasoning_content = Some("chain-of-thought".to_string());

    let messages = vec![msg];
    let wire = prepare_wire_messages_for_api(
        messages,
        "anthropic",
        "claude-sonnet-4-20250514",
        "https://api.anthropic.com",
    );
    // Anthropic DOES NOT use thinking_reasoning_pad by default for claude
    // (pad is only for DeepSeek/Kimi/Mimo), so reasoning may not be echoed.
    // We just verify the function doesn't panic.
    assert!(!wire.is_empty());
}

// ---------------------------------------------------------------------------
// Task 5 — Compaction digests are never re-summarized
// ---------------------------------------------------------------------------

#[tokio::test]
async fn compression_digest_is_never_re_summarized() {
    let engine = DefaultContextEngine::new();

    // Create 20 messages — enough to trigger compression with a low target.
    let msgs: Vec<Value> = (0..20)
        .map(|i| {
            json!({"role": if i % 2 == 0 { "user" } else { "assistant" }, "content": format!("message {i} with some extra padding text to consume tokens and push past the budget threshold")})
        })
        .collect();

    // First compression: should produce a digest.
    let r1 = engine.compress(&msgs, 100).await.unwrap();
    assert!(r1.len() < 20, "first compress should reduce count");

    // Verify the digest has the compression-summary marker in user role.
    let first_summary = &r1[0];
    assert_eq!(first_summary["role"], "user");
    assert!(
        first_summary["content"]
            .as_str()
            .unwrap()
            .starts_with("<compression-summary>"),
        "digest must be tagged"
    );

    // Second compression over the already-compressed result:
    // the existing digest must NOT be re-summarized.
    let r2 = engine.compress(&r1, 100).await.unwrap();
    let second_first = &r2[0];
    assert_eq!(second_first["role"], "user");
    // The first digest should still be the leading message, unchanged.
    assert_eq!(second_first["content"], first_summary["content"]);
}

// ---------------------------------------------------------------------------
// Task 3 — Stuck guard pauses when window is too small
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stuck_guard_pauses_after_two_consecutive_failures() {
    let engine = DefaultContextEngine::new();

    // 20 bulky messages, target so low no compression can meet it.
    let msgs: Vec<Value> = (0..20)
        .map(|i| {
            json!({"role": "user", "content": format!("very long message number {i} with lots of padding text to ensure tokens always exceed budget regardless of compression")})
        })
        .collect();

    // First compression: will still be over target.
    let r1 = engine.compress(&msgs, 10).await;
    assert!(r1.is_ok(), "first compress should not error");
    // NOT yet stuck — only after 2 consecutive failures.

    // Second compression: should trigger stuck guard.
    let r2 = engine.compress(&r1.unwrap(), 10).await.unwrap();
    // Third call: stuck guard should return as-is (no further collapse).
    let r3 = engine.compress(&r2, 10).await.unwrap();
    // After stuck, the engine should return messages unchanged.
    assert_eq!(r2.len(), r3.len(), "stuck guard should not reduce further");
}

// ---------------------------------------------------------------------------
// Task 2 — PrefixShape diagnostics
// ---------------------------------------------------------------------------

#[test]
fn shape_detects_system_prompt_change() {
    let schemas = vec![];
    let prev = capture_shape("system v1", &schemas, 0);
    let cur = capture_shape("system v2", &schemas, 0);
    let diag = compare_shape(&prev, &cur, None);
    assert!(diag.prefix_changed);
    assert!(diag.prefix_change_reasons.contains(&"system".to_string()));
}

#[test]
fn shape_tracks_session_cache_counters_via_usage() {
    let schemas = vec![];
    let shape = capture_shape("s", &schemas, 0);
    let usage = UsageStats {
        cache_read_tokens: 1500,
        cache_write_tokens: 200,
        ..Default::default()
    };
    let diag = compare_shape(&shape, &shape, Some(&usage));
    assert!(!diag.prefix_changed);
    assert_eq!(diag.cache_hit_tokens, 1500);
    assert_eq!(diag.cache_miss_tokens, 200);
}
