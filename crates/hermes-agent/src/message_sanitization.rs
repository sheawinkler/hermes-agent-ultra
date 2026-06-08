//! Python `run_agent.py`,`run_conversation.py` alignment helpers (budget pressure, history hygiene).
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
//! 11. **`run_conversation`**: B/E via `conversation_loop` (`prepare_turn` / `finalize_turn`); C–D via `run_prepared` / `run_stream_prepared`.
//! 12. **Session end / API hooks**: `on_session_end` at turn exit; `pre_api_request` before each LLM HTTP call (`tests/run_conversation_hooks.rs`).
//! 13. **Steer**: pre-API drain into last tool result (`steer.rs`); `pending_steer` on `ConversationResult`.

use std::borrow::Cow;

use regex::Regex;
use serde_json::Value;

use hermes_core::{LlmResponse, Message, MessageRole, ToolCall, ToolResult};

/// Strip persisted/ephemeral system prompts from gateway history before `run_conversation`.
///
/// Python `gateway` paths use `_filter_history` so `agent_history` is only user/assistant/tool
/// turns. Without this, each turn prepends `stored_system_prompt` **and** replays old `system`
/// rows from SQLite, then `leading_system_prompt_for_persist` concatenates them → exponential growth.
pub fn strip_system_messages_from_history(messages: &[Message]) -> Vec<Message> {
    messages
        .iter()
        .filter(|m| m.role != MessageRole::System)
        .cloned()
        .collect()
}

/// Synthetic user nudge after a Codex intermediate ack (Python `run_agent.py`).
pub const CODEX_CONTINUE_USER_MESSAGE: &str = "[System: Continue now. Execute the required tool calls and only send your final answer after completing the task.]";

/// Nudge when the user asked for `clarify` but the model replied with prose only.
pub const CLARIFY_TOOL_RETRY_USER_MESSAGE: &str = "[System: The user asked for the `clarify` tool. Invoke `clarify` now with `question` and up to 4 `choices`. Do not end with an introduction only.]";

pub const CLARIFY_TOOL_RETRY_MAX: u32 = 2;

/// True when the inbound task text explicitly mentions the clarify tool.
pub fn user_message_requests_clarify_tool(task_hint: &str) -> bool {
    let lower = task_hint.to_ascii_lowercase();
    lower.contains("clarify") || task_hint.contains("澄清")
}

/// Retry when clarify is available, the user asked for it, and the model ended without tool calls.
pub fn clarify_tool_invocation_requires_retry(
    task_hint: &str,
    clarify_available: bool,
    retry_count: u32,
) -> bool {
    clarify_available
        && retry_count < CLARIFY_TOOL_RETRY_MAX
        && user_message_requests_clarify_tool(task_hint)
}

/// Re-export for continuation prompt branching (defined on [`hermes_core::LlmResponse`]).
pub use hermes_core::PARTIAL_STREAM_STUB_ID;

/// Heuristic: does visible assistant text look intentionally finished?
///
/// Python `AIAgent._has_natural_response_ending` (`run_agent.py`).
pub fn has_natural_response_ending(content: &str) -> bool {
    if content.is_empty() {
        return false;
    }
    let stripped = content.trim_end();
    if stripped.is_empty() {
        return false;
    }
    if stripped.ends_with("```") {
        return true;
    }
    if stripped.ends_with('^') {
        return true;
    }
    let Some(last) = stripped.chars().next_back() else {
        return false;
    };
    if matches!(
        last,
        '.' | '!'
            | '?'
            | ':'
            | ')'
            | '"'
            | '\''
            | ']'
            | '}'
            | '。'
            | '！'
            | '？'
            | '：'
            | '）'
            | '】'
            | '」'
            | '』'
            | '》'
            | '^'
    ) {
        return true;
    }
    if (last as u32) >= 0x1F300 {
        return true;
    }
    false
}

/// Continuation user message after `finish_reason=length` (Python `conversation_loop._get_continuation_prompt`).
pub fn get_continuation_prompt(is_partial_stub: bool, dropped_tools: Option<&[String]>) -> String {
    if is_partial_stub {
        if let Some(tools) = dropped_tools.filter(|t| !t.is_empty()) {
            let tool_list = tools
                .iter()
                .take(3)
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            return format!(
                "[System: Your previous tool call ({tool_list}) was too large and \
the stream timed out before it could be delivered. Do NOT retry \
the same tool call with the same large content. Instead, break the \
content into multiple smaller tool calls (e.g. use multiple patch calls \
or write smaller files). Each tool call's arguments must be under ~8K \
tokens to avoid stream timeouts.]"
            );
        }
        return "[System: The previous response was cut off by a network error mid-stream. \
Continue exactly where you left off. Do not restart or repeat prior text. \
Finish the answer directly.]"
            .to_string();
    }
    "[System: Your previous response was truncated by the output length limit. \
Continue exactly where you left off. Do not restart or repeat prior text. \
Finish the answer directly.]"
        .to_string()
}

pub fn continuation_prompt_for_response(response: &hermes_core::LlmResponse) -> String {
    let is_partial = response.response_id.as_deref() == Some(PARTIAL_STREAM_STUB_ID);
    get_continuation_prompt(is_partial, response.dropped_tool_names.as_deref())
}

/// User-visible warning when a stream dies mid tool-call (Python `chat_completion_helpers`).
pub fn format_partial_stream_tool_call_warning(dropped_tools: &[String]) -> String {
    let mut name_str = dropped_tools
        .iter()
        .take(3)
        .map(String::as_str)
        .collect::<Vec<_>>()
        .join(", ");
    if dropped_tools.len() > 3 {
        name_str.push_str(&format!(", +{} more", dropped_tools.len() - 3));
    }
    format!(
        "\n\n⚠ Stream stalled mid tool-call ({name_str}); the action was not executed. \
Ask me to retry if you want to continue."
    )
}

pub fn partial_stream_tool_calls_in_flight(tool_calls: &[ToolCall]) -> bool {
    tool_calls.iter().any(|tc| {
        !tc.id.is_empty()
            || !tc.function.name.is_empty()
            || !tc.function.arguments.trim().is_empty()
    })
}

pub fn partial_stream_dropped_tool_names(tool_calls: &[ToolCall]) -> Vec<String> {
    tool_calls
        .iter()
        .filter(|tc| !tc.function.name.is_empty())
        .map(|tc| tc.function.name.clone())
        .collect()
}

/// Build a partial-stream stub so the agent loop can continue after network failure.
///
/// Python `interruptible_streaming_api_call` partial stub (`chat_completion_helpers.py`).
pub fn build_partial_stream_stub_response(
    model: impl Into<String>,
    content: impl Into<String>,
    dropped_tool_names: Option<Vec<String>>,
) -> LlmResponse {
    LlmResponse {
        message: Message::assistant(content),
        usage: None,
        model: model.into(),
        finish_reason: Some("length".to_string()),
        response_id: Some(PARTIAL_STREAM_STUB_ID.to_string()),
        dropped_tool_names,
        rate_limit_headers: None,
    }
}

/// Minimum runtime context for reliable Hermes tool use (Python `MINIMUM_CONTEXT_LENGTH`).
pub const MINIMUM_CONTEXT_LENGTH: u32 = 64_000;

/// Return a user-facing error when Ollama is loaded with too little context.
///
/// Python `conversation_loop._ollama_context_limit_error`.
pub fn ollama_context_limit_error(
    ollama_num_ctx: Option<u32>,
    has_tools: bool,
    request_tokens: u32,
    model: &str,
    provider: &str,
    base_url: &str,
    tool_count: usize,
    session_id: Option<&str>,
) -> Option<String> {
    if !has_tools {
        return None;
    }
    let runtime_ctx = ollama_num_ctx?;
    if runtime_ctx == 0 || runtime_ctx >= MINIMUM_CONTEXT_LENGTH {
        return None;
    }
    tracing::warn!(
        model = %model,
        provider = %provider,
        base_url = %base_url,
        runtime_context = runtime_ctx,
        minimum_context = MINIMUM_CONTEXT_LENGTH,
        estimated_request_tokens = request_tokens,
        tool_count = tool_count,
        session = session_id.unwrap_or("none"),
        "Ollama runtime context too small for Hermes tool use"
    );
    Some(format!(
        "Ollama loaded `{model}` with only {runtime_ctx} tokens of runtime \
         context, but Hermes needs at least {min_ctx} tokens for reliable tool use.\n\n\
         Increase the Ollama context for this model and restart/reload the model before trying again. \
         A known-good starting point is 65,536 tokens. In Hermes config, set `model.ollama_num_ctx: 65536` \
         (and `model.context_length: 65536` if you also override the displayed model context). \
         If you manage the model through an Ollama Modelfile, set `PARAMETER num_ctx 65536` there instead.",
        min_ctx = MINIMUM_CONTEXT_LENGTH
    ))
}

/// Detect the narrow backend family affected by Ollama/GLM stop misreports.
///
/// Python `AIAgent._is_ollama_glm_backend` (`run_agent.py`).
pub fn is_ollama_glm_backend(model: &str, provider: &str, base_url: &str) -> bool {
    let model_lower = model.to_ascii_lowercase();
    let provider_lower = provider.to_ascii_lowercase();
    let base_url_lower = base_url.to_ascii_lowercase();
    if !model_lower.contains("glm") && provider_lower != "zai" {
        return false;
    }
    if base_url_lower.contains("ollama") || base_url_lower.contains(":11434") {
        return true;
    }
    hermes_intelligence::is_local_endpoint(base_url)
}

/// Detect conservative stop→length misreports for Ollama-hosted GLM models.
///
/// Python `AIAgent._should_treat_stop_as_truncated` (`run_agent.py`).
pub fn should_treat_stop_as_truncated(
    finish_reason: Option<&str>,
    assistant_content: Option<&str>,
    history_includes_tool: bool,
    api_mode: &str,
    model: &str,
    provider: &str,
    base_url: Option<&str>,
) -> bool {
    if finish_reason != Some("stop") || api_mode != "chat_completions" {
        return false;
    }
    let Some(base_url) = base_url.filter(|u| !u.is_empty()) else {
        return false;
    };
    if !is_ollama_glm_backend(model, provider, base_url) {
        return false;
    }
    if !history_includes_tool {
        return false;
    }
    let Some(content) = assistant_content.filter(|c| !c.is_empty()) else {
        return false;
    };
    let stripped = strip_think_blocks_for_ack(content);
    let visible_text = stripped.trim();
    if visible_text.is_empty() || visible_text.len() < 20 {
        return false;
    }
    if !visible_text.contains(char::is_whitespace) {
        return false;
    }
    !has_natural_response_ending(visible_text)
}

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
    static ref CODEX_FUTURE_ACK_ZH_RE: Regex = Regex::new(
        r"(我会|我将|我先|我来|我去|接下来|现在(开始)?|马上|正在|先去|先检查|先看|先更新)"
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
    if !CODEX_FUTURE_ACK_RE.is_match(&assistant_text)
        && !CODEX_FUTURE_ACK_ZH_RE.is_match(&assistant_text)
    {
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
        "检查",
        "看",
        "查看",
        "分析",
        "排查",
        "检索",
        "搜索",
        "运行",
        "执行",
        "修复",
        "更新",
        "处理",
        "整理",
        "汇总",
        "总结",
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
        "目录",
        "当前目录",
        "仓库",
        "代码库",
        "项目",
        "文件",
        "路径",
    ];
    let user_text = user_message.trim().to_lowercase();
    let user_targets_workspace = workspace_markers.iter().any(|m| user_text.contains(*m))
        || user_text.contains("~/")
        || user_text.contains('/');
    let assistant_mentions_action = action_markers.iter().any(|m| assistant_text.contains(*m));
    let assistant_targets_workspace = workspace_markers
        .iter()
        .any(|m| assistant_text.contains(*m));
    if user_message_requests_clarify_tool(user_message)
        && (assistant_text.contains("clarify") || assistant_text.contains('问'))
    {
        return true;
    }
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

    #[test]
    fn codex_ack_detection_matches_chinese_intermediate_ack() {
        let user = "请检查当前目录并更新todo";
        let assistant = "找到了两个相关的 todo项，现在更新它们：";
        assert!(looks_like_codex_intermediate_ack(user, assistant, false));
    }

    #[test]
    fn codex_ack_detection_rejects_chinese_final_response_style() {
        let user = "请检查当前目录并更新todo";
        let assistant = "已完成更新。两个 todo 项都已处理完毕。";
        assert!(!looks_like_codex_intermediate_ack(user, assistant, false));
    }

    #[test]
    fn clarify_user_request_detected_from_message() {
        let task = "调用 clarify 给一些买电脑的选型";
        assert!(user_message_requests_clarify_tool(task));
        assert!(clarify_tool_invocation_requires_retry(task, true, 0));
        assert!(!clarify_tool_invocation_requires_retry(task, true, CLARIFY_TOOL_RETRY_MAX));
    }

    #[test]
    fn clarify_retry_uses_task_hint_not_synthetic_continue_user() {
        let task = "调用 clarify 给一些买电脑的选型";
        assert!(clarify_tool_invocation_requires_retry(task, true, 1));
    }

    #[test]
    fn codex_ack_detection_matches_clarify_deferral() {
        let user = "调用 clarify 给一些买电脑的选型";
        let assistant = "好的！我先用 clarify 问几个关键问题：";
        assert!(looks_like_codex_intermediate_ack(user, assistant, false));
    }

    #[test]
    fn has_natural_response_ending_detects_punctuation_and_code_fence() {
        assert!(has_natural_response_ending("All done."));
        assert!(has_natural_response_ending("完成了。"));
        assert!(has_natural_response_ending("```rust\nfn main() {}\n```"));
        assert!(!has_natural_response_ending("Still writing"));
        assert!(!has_natural_response_ending(""));
    }

    #[test]
    fn length_continuation_prompt_branching_matches_python() {
        assert!(get_continuation_prompt(true, None).contains("network error mid-stream"));
        assert!(!get_continuation_prompt(true, None).contains("output length limit"));

        let real = get_continuation_prompt(false, None);
        assert!(real.contains("output length limit"));
        assert!(!real.contains("network error"));

        let dropped = get_continuation_prompt(true, Some(&["write_file".to_string()]));
        assert!(dropped.contains("too large"));
        assert!(dropped.contains("write_file"));
        assert!(!dropped.contains("network error"));
        assert!(!dropped.contains("output length limit"));
    }

    #[test]
    fn should_treat_stop_as_truncated_requires_ollama_glm_and_unnatural_end() {
        assert!(should_treat_stop_as_truncated(
            Some("stop"),
            Some("partial answer without ending"),
            true,
            "chat_completions",
            "glm-4",
            "openai",
            Some("http://127.0.0.1:11434/v1"),
        ));
        assert!(!should_treat_stop_as_truncated(
            Some("stop"),
            Some("partial answer without ending"),
            true,
            "chat_completions",
            "glm-4",
            "openai",
            Some("https://api.openai.com/v1"),
        ));
        assert!(!should_treat_stop_as_truncated(
            Some("stop"),
            Some("This is a complete answer."),
            true,
            "chat_completions",
            "glm-4",
            "openai",
            Some("http://127.0.0.1:11434/v1"),
        ));
    }

    #[test]
    fn partial_stream_stub_response_matches_python_contract() {
        let resp = build_partial_stream_stub_response("test/model", "The first half of ", None);
        assert_eq!(resp.response_id.as_deref(), Some(PARTIAL_STREAM_STUB_ID));
        assert_eq!(resp.finish_reason.as_deref(), Some("length"));
        assert!(
            resp.message
                .tool_calls
                .as_ref()
                .map_or(true, |v| v.is_empty())
        );
        assert_eq!(
            continuation_prompt_for_response(&resp),
            get_continuation_prompt(true, None)
        );

        let dropped = build_partial_stream_stub_response(
            "test/model",
            "Let me write the audit: ",
            Some(vec!["write_file".to_string()]),
        );
        assert_eq!(
            dropped.dropped_tool_names.as_deref(),
            Some(["write_file".to_string()].as_slice())
        );
        let prompt = continuation_prompt_for_response(&dropped);
        assert!(prompt.contains("too large"));
        assert!(prompt.contains("write_file"));
    }

    #[test]
    fn partial_stream_tool_call_warning_matches_python() {
        let warn = format_partial_stream_tool_call_warning(&["write_file".to_string()]);
        assert!(warn.contains("Stream stalled mid tool-call"));
        assert!(warn.contains("write_file"));
        assert!(warn.contains("not executed"));
    }
}
