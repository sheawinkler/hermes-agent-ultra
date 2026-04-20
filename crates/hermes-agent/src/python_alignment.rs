//! Python `run_agent.py` alignment helpers (budget pressure, history hygiene).
//!
//! # Phase A — contract scenarios (mirrors Python tests)
//!
//! 1. **New session**: empty `stored_system_prompt`, fresh `build_system_prompt`, `on_session_start` may fire.
//! 2. **Continue session**: `stored_system_prompt` from SQLite matches prior turn — stable Anthropic prefix cache.
//! 3. **Budget caution (70%)**: `_get_budget_warning` returns `[BUDGET: ...]` injected into last tool JSON as `_budget_warning`.
//! 4. **Budget warning (90%)**: `[BUDGET WARNING: ...]` same injection path.
//! 5. **History replay**: `_strip_budget_warnings_from_history` removes `_budget_warning` / `[BUDGET` tails from tool messages.
//! 6. **Preflight compress**: when context chars exceed threshold before first LLM, compress once (see `AgentLoop`).
//! 7. **Empty LLM reply**: retry up to N times without appending assistant (consumes turn budget like Python).
//! 8. **Streaming**: deltas forwarded; interrupt checked during stream collection.
//! 9. **Hooks**: `pre_llm_call` / `post_llm_call` / tool hooks / `on_session_start` for new sessions.
//! 10. **Result**: `AgentResult` carries `session_cost_usd` / `interrupted` when applicable.

use std::borrow::Cow;

use regex::Regex;
use serde_json::Value;

use hermes_core::{Message, MessageRole, ToolResult};

/// Synthetic user nudge after a Codex intermediate ack (Python `run_agent.py`).
pub const CODEX_CONTINUE_USER_MESSAGE: &str = "[System: Continue now. Execute the required tool calls and only send your final answer after completing the task.]";

lazy_static::lazy_static! {
    /// Mirrors Python `_BUDGET_WARNING_RE` in `run_agent.py`.
    static ref BUDGET_WARNING_TEXT_RE: Regex = Regex::new(
        r"(?s)\[\s*BUDGET(?:\s+WARNING)?:\s*Iteration\s+\d+/\d+\..*?\]"
    )
    .expect("valid regex");
    static ref CODEX_FUTURE_ACK_RE: Regex = Regex::new(
        r"(?i)\b(i[''']ll|i will|let me|i can do that|i can help with that)\b"
    )
    .expect("valid regex");
    static ref ACK_RED: Regex =
        Regex::new(r"(?s)<redacted_thinking>.*?</redacted_thinking>").expect("regex");
    static ref ACK_THINK: Regex = Regex::new(r"(?is)<thinking>.*?</thinking>").expect("regex");
    static ref ACK_REASON: Regex = Regex::new(r"(?s)<reasoning>.*?</reasoning>").expect("regex");
}

/// Strip turn-scoped budget pressure markers from tool results (Python `_strip_budget_warnings_from_history`).
pub fn strip_budget_warnings_from_messages(messages: &mut [Message]) {
    for msg in messages.iter_mut() {
        if msg.role != MessageRole::Tool {
            continue;
        }
        let Some(content) = msg.content.as_mut() else {
            continue;
        };
        if !content.contains("_budget_warning") && !content.contains("[BUDGET") {
            continue;
        }
        if let Ok(mut parsed) = serde_json::from_str::<serde_json::Map<String, Value>>(content) {
            if parsed.remove("_budget_warning").is_some() {
                *content = serde_json::to_string(&parsed).unwrap_or_else(|_| content.clone());
                continue;
            }
        }
        let cleaned = BUDGET_WARNING_TEXT_RE
            .replace_all(content, "")
            .trim()
            .to_string();
        if cleaned != *content {
            *content = cleaned;
        }
    }
}

/// Two-tier budget pressure string matching Python `_get_budget_warning(api_call_count)`.
pub fn budget_pressure_text(
    api_call_count: u32,
    max_iterations: u32,
    caution_threshold: f64,
    warning_threshold: f64,
    enabled: bool,
) -> Option<Cow<'static, str>> {
    if !enabled || max_iterations == 0 {
        return None;
    }
    let progress = api_call_count as f64 / max_iterations as f64;
    let remaining = max_iterations.saturating_sub(api_call_count);
    if progress >= warning_threshold {
        return Some(Cow::Owned(format!(
            "[BUDGET WARNING: Iteration {api_call_count}/{max_iterations}. \
             Only {remaining} iteration(s) left. \
             Provide your final response NOW. No more tool calls unless absolutely critical.]"
        )));
    }
    if progress >= caution_threshold {
        return Some(Cow::Owned(format!(
            "[BUDGET: Iteration {api_call_count}/{max_iterations}. \
             {remaining} iterations left. Start consolidating your work.]"
        )));
    }
    None
}

/// Inject budget pressure into the last tool result (Python tool-loop tail).
pub fn inject_budget_pressure_into_last_tool_result(
    results: &mut [ToolResult],
    warning: Option<&str>,
) {
    let Some(w) = warning else {
        return;
    };
    if w.is_empty() {
        return;
    }
    let Some(last) = results.last_mut() else {
        return;
    };
    if let Ok(val) = serde_json::from_str::<Value>(&last.content) {
        if let Value::Object(mut map) = val {
            map.insert("_budget_warning".to_string(), Value::String(w.to_string()));
            if let Ok(s) = serde_json::to_string(&map) {
                last.content = s;
                return;
            }
        }
    }
    last.content = format!("{}\n\n{}", last.content, w);
}

fn is_utf16_surrogate_scalar(c: char) -> bool {
    matches!(c as u32, 0xD800..=0xDFFF)
}

/// Replace lone surrogate code points with U+FFFD (Python `_sanitize_surrogates`).
/// Strip common reasoning / thinking wrappers for Codex ack heuristics (subset of Python `_strip_think_blocks`).
pub fn strip_think_blocks_for_ack(content: &str) -> String {
    let mut c = content.to_string();
    c = ACK_RED.replace_all(&c, "").to_string();
    c = ACK_THINK.replace_all(&c, "").to_string();
    c = ACK_REASON.replace_all(&c, "").to_string();
    c
}

/// Detect a planning/ack assistant reply that should get a continuation nudge (Codex Responses API).
pub fn looks_like_codex_intermediate_ack(
    user_message: &str,
    assistant_content: &str,
    history_includes_tool_messages: bool,
) -> bool {
    if history_includes_tool_messages {
        return false;
    }
    let assistant_text = strip_think_blocks_for_ack(assistant_content);
    let assistant_text = assistant_text.trim().to_lowercase();
    if assistant_text.is_empty() || assistant_text.len() > 1200 {
        return false;
    }
    if !CODEX_FUTURE_ACK_RE.is_match(&assistant_text) {
        return false;
    }
    let action_markers: &[&str] = &[
        "look into",
        "look at",
        "inspect",
        "scan",
        "check",
        "analyz",
        "review",
        "explore",
        "read",
        "open",
        "run",
        "test",
        "fix",
        "debug",
        "search",
        "find",
        "walkthrough",
        "report back",
        "summarize",
    ];
    let workspace_markers: &[&str] = &[
        "directory",
        "current directory",
        "current dir",
        "cwd",
        "repo",
        "repository",
        "codebase",
        "project",
        "folder",
        "filesystem",
        "file tree",
        "files",
        "path",
    ];
    let user_text = user_message.trim().to_lowercase();
    let user_targets_workspace = workspace_markers.iter().any(|m| user_text.contains(*m))
        || user_text.contains("~/")
        || user_text.contains('/');
    let assistant_mentions_action = action_markers.iter().any(|m| assistant_text.contains(*m));
    let assistant_targets_workspace = workspace_markers
        .iter()
        .any(|m| assistant_text.contains(*m));
    (user_targets_workspace || assistant_targets_workspace) && assistant_mentions_action
}

pub fn sanitize_surrogates(s: &str) -> Cow<'_, str> {
    if !s.chars().any(is_utf16_surrogate_scalar) {
        return Cow::Borrowed(s);
    }
    let t: String = s
        .chars()
        .map(|c| {
            if is_utf16_surrogate_scalar(c) {
                '\u{FFFD}'
            } else {
                c
            }
        })
        .collect();
    Cow::Owned(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::Message;

    #[test]
    fn strip_json_budget_warning_key() {
        let mut messages = vec![Message {
            role: MessageRole::Tool,
            content: Some(r#"{"ok":true,"_budget_warning":"[BUDGET: x]"}"#.to_string()),
            tool_calls: None,
            tool_call_id: Some("1".into()),
            name: None,
            reasoning_content: None,
            cache_control: None,
        }];
        strip_budget_warnings_from_messages(&mut messages);
        let parsed: serde_json::Value =
            serde_json::from_str(messages[0].content.as_ref().unwrap()).unwrap();
        assert!(!parsed.as_object().unwrap().contains_key("_budget_warning"));
    }

    #[test]
    fn budget_pressure_tiers_match_python_ratios() {
        let max = 60u32;
        // 41/60 ≈ 0.683 — below 0.7 → none
        assert!(budget_pressure_text(41, max, 0.7, 0.9, true).is_none());
        // 42/60 = 0.7 — caution
        let c = budget_pressure_text(42, max, 0.7, 0.9, true).unwrap();
        assert!(c.contains("[BUDGET:") && !c.contains("BUDGET WARNING"));
        // 54/60 = 0.9 — warning
        let w = budget_pressure_text(54, max, 0.7, 0.9, true).unwrap();
        assert!(w.contains("BUDGET WARNING"));
    }

    #[test]
    fn inject_budget_into_json_tool_result() {
        let mut results = vec![ToolResult::ok("tc1", r#"{"a":1}"#)];
        inject_budget_pressure_into_last_tool_result(
            &mut results,
            Some("[BUDGET: Iteration 1/10. 9 iterations left.]"),
        );
        let v: serde_json::Value = serde_json::from_str(&results[0].content).unwrap();
        assert!(v["_budget_warning"].as_str().unwrap().contains("BUDGET"));
    }
}
