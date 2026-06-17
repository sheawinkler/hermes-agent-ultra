//! Runtime helpers ported from Python `agent/agent_runtime_helpers.py` and `run_agent.py`.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use hermes_core::{Message, MessageRole, ToolCall};
use lazy_static::lazy_static;
use regex::Regex;
use serde_json::{Value, json};
use tracing::{debug, warn};

use crate::credential_pool::CredentialPool;
use crate::error_classifier::FailoverReason;
use crate::message_sanitization;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

/// Marker prepended when assistant `tool_calls[].function.arguments` JSON was corrupted.
pub const TOOL_CALL_ARGUMENTS_CORRUPTION_MARKER: &str = concat!(
    "[hermes-agent: tool call arguments were corrupted in this session and ",
    "have been dropped to keep the conversation alive. See issue #15236.]"
);

/// Roles accepted on the wire by OpenAI-compatible APIs (Python `AIAgent._VALID_API_ROLES`).
pub const VALID_API_ROLES: &[&str] = &[
    "system",
    "user",
    "assistant",
    "tool",
    "function",
    "developer",
];

const STUB_TOOL_RESULT_CONTENT: &str = "[Result unavailable — see context summary above]";

const TRAJECTORY_SYSTEM_TEMPLATE: &str = "\
You are a function calling AI model. You are provided with function signatures within <tools> </tools> XML tags. \
You may call one or more functions to assist with the user query. If available tools are not relevant in assisting \
with user query, just respond in natural conversational language. Don't make assumptions about what values to plug \
into functions. After calling & executing the functions, you will be provided with function results within \
<tool_response> </tool_response> XML tags. Here are the available tools:\n\
<tools>\n{tools}\n</tools>\n\
For each function call return a JSON object, with the following pydantic model json schema for each:\n\
{'title': 'FunctionCall', 'type': 'object', 'properties': {'name': {'title': 'Name', 'type': 'string'}, \
'arguments': {'title': 'Arguments', 'type': 'object'}}, 'required': ['name', 'arguments']}\n\
Each function call should be enclosed within <tool_call> </tool_call> XML tags.\n\
Example:\n<tool_call>\n{'name': <function-name>,'arguments': <args-dict>}\n</tool_call>";

lazy_static! {
    static ref STRIP_CLOSED_RED: Regex =
        Regex::new(r"(?is)<think>.*?</think>").unwrap();
    static ref STRIP_CLOSED_THINK: Regex =
        Regex::new(r"(?is)<thinking>.*?</thinking>").unwrap();
    static ref STRIP_CLOSED_REASON: Regex =
        Regex::new(r"(?is)<reasoning>.*?</reasoning>").unwrap();
    static ref STRIP_CLOSED_SCRATCH: Regex =
        Regex::new(r"(?s)<REASONING_SCRATCHPAD>.*?</REASONING_SCRATCHPAD>").unwrap();
    static ref STRIP_CLOSED_THOUGHT: Regex =
        Regex::new(r"(?is)<thought>.*?</thought>").unwrap();
    static ref STRIP_UNTERM_REASON: Regex = Regex::new(
        r"(?is)(?:^|\n)[ \t]*<(?:think|thinking|reasoning|thought|REASONING_SCRATCHPAD)\b[^>]*>.*$"
    )
    .unwrap();
    static ref STRIP_ORPHAN_TAGS: Regex = Regex::new(
        r"(?i)</?(?:think|thinking|reasoning|thought|REASONING_SCRATCHPAD)>\s*"
    )
    .unwrap();
    static ref STRIP_TOOL_CLOSERS: Regex = Regex::new(
        r"(?i)</(?:tool_call|tool_calls|tool_result|function_call|function_calls|function)>\s*"
    )
    .unwrap();
    static ref STRIP_FUNCTION_NAMED: Regex = Regex::new(
        r"(?is)<function\b[^>]*\bname\s*=[^>]*>.*?</function>"
    )
    .unwrap();
    static ref INLINE_REASONING_PATTERNS: Vec<Regex> = vec![
        Regex::new(r"(?is)<think>(.*?)</think>").unwrap(),
        Regex::new(r"(?is)<thinking>(.*?)</thinking>").unwrap(),
        Regex::new(r"(?is)<thought>(.*?)</thought>").unwrap(),
        Regex::new(r"(?is)<reasoning>(.*?)</reasoning>").unwrap(),
        Regex::new(r"(?s)<REASONING_SCRATCHPAD>(.*?)</REASONING_SCRATCHPAD>").unwrap(),
    ];
    static ref EXTRACT_RESET_DELAY: Regex =
        Regex::new(r#"(?i)quotaResetDelay[:\s"]+(\d+(?:\.\d+)?)(ms|s)"#).unwrap();
    static ref EXTRACT_RESETS_IN: Regex = Regex::new(
        r"(?i)resets?\s+in\s+(?:(\d+(?:\.\d+)?)\s*(?:h|hr|hrs|hour|hours)\b\s*)?(?:(\d+(?:\.\d+)?)\s*(?:m|min|mins|minute|minutes)\b\s*)?(?:(\d+(?:\.\d+)?)\s*(?:s|sec|secs|second|seconds)\b)?"
    )
    .unwrap();
    static ref EXTRACT_RETRY_AFTER: Regex =
        Regex::new(r"(?i)retry\s+(?:after\s+)?(\d+(?:\.\d+)?)\s*(?:sec|secs|seconds|s\b)")
            .unwrap();
}

fn tool_call_xml_regex(name: &str) -> Regex {
    Regex::new(&format!(r"(?is)<{name}\b[^>]*>.*?</{name}>")).unwrap()
}

// ---------------------------------------------------------------------------
// Trajectory conversion
// ---------------------------------------------------------------------------

fn convert_scratchpad_to_think(content: &str) -> String {
    if !content.contains("<REASONING_SCRATCHPAD>") {
        return content.to_string();
    }
    content
        .replace("<REASONING_SCRATCHPAD>", "<think>")
        .replace("</REASONING_SCRATCHPAD>", "</think>")
}

/// Convert internal messages to ShareGPT-style trajectory training format.
pub fn convert_to_trajectory_format(
    messages: &[Message],
    user_query: &str,
    _completed: bool,
    tools_xml_fragment: &str,
) -> Vec<Value> {
    let mut trajectory = Vec::new();
    trajectory.push(json!({
        "from": "system",
        "value": TRAJECTORY_SYSTEM_TEMPLATE.replace("{tools}", tools_xml_fragment),
    }));
    trajectory.push(json!({ "from": "human", "value": user_query }));

    let mut i = 1usize;
    while i < messages.len() {
        let msg = &messages[i];
        match msg.role {
            MessageRole::Assistant => {
                if let Some(tool_calls) = msg.tool_calls.as_ref().filter(|t| !t.is_empty()) {
                    let mut content = String::new();
                    if let Some(r) = msg
                        .reasoning_content
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                    {
                        content.push_str(&format!("<think>\n{r}\n</think>\n"));
                    }
                    if let Some(c) = msg.content.as_deref().filter(|s| !s.trim().is_empty()) {
                        content.push_str(&convert_scratchpad_to_think(c));
                        content.push('\n');
                    }
                    for tc in tool_calls {
                        let arguments: Value = serde_json::from_str(&tc.function.arguments)
                            .unwrap_or_else(|_| {
                                warn!(
                                    "Unexpected invalid JSON in trajectory conversion: {}",
                                    &tc.function.arguments[..tc.function.arguments.len().min(100)]
                                );
                                json!({})
                            });
                        let tool_call_json = json!({
                            "name": tc.function.name,
                            "arguments": arguments,
                        });
                        content.push_str(&format!(
                            "<tool_call>\n{}\n</tool_call>\n",
                            serde_json::to_string(&tool_call_json).unwrap_or_default()
                        ));
                    }
                    if !content.contains("<think>") {
                        content.insert_str(0, "<think>\n</think>\n");
                    }
                    trajectory.push(json!({ "from": "gpt", "value": content.trim_end() }));

                    let mut tool_responses = Vec::new();
                    let mut j = i + 1;
                    while j < messages.len() && messages[j].role == MessageRole::Tool {
                        let tool_msg = &messages[j];
                        let tool_content: Value = tool_msg
                            .content
                            .as_deref()
                            .map(|s| {
                                let t = s.trim();
                                if (t.starts_with('{') || t.starts_with('['))
                                    && serde_json::from_str::<Value>(t).is_ok()
                                {
                                    serde_json::from_str(t).unwrap_or(Value::String(s.to_string()))
                                } else {
                                    Value::String(s.to_string())
                                }
                            })
                            .unwrap_or(Value::Null);
                        let tool_index = tool_responses.len();
                        let tool_name = tool_calls
                            .get(tool_index)
                            .map(|tc| tc.function.name.as_str())
                            .unwrap_or("unknown");
                        let block = format!(
                            "<tool_response>\n{}\n</tool_response>",
                            serde_json::to_string(&json!({
                                "tool_call_id": tool_msg.tool_call_id.as_deref().unwrap_or(""),
                                "name": tool_name,
                                "content": tool_content,
                            }))
                            .unwrap_or_default()
                        );
                        tool_responses.push(block);
                        j += 1;
                    }
                    if !tool_responses.is_empty() {
                        trajectory.push(json!({
                            "from": "tool",
                            "value": tool_responses.join("\n"),
                        }));
                        i = j.saturating_sub(1);
                    }
                } else {
                    let mut content = String::new();
                    if let Some(r) = msg
                        .reasoning_content
                        .as_deref()
                        .filter(|s| !s.trim().is_empty())
                    {
                        content.push_str(&format!("<think>\n{r}\n</think>\n"));
                    }
                    let raw = msg.content.as_deref().unwrap_or("");
                    content.push_str(&convert_scratchpad_to_think(raw));
                    if !content.contains("<think>") {
                        content.insert_str(0, "<think>\n</think>\n");
                    }
                    trajectory.push(json!({ "from": "gpt", "value": content.trim() }));
                }
            }
            MessageRole::User => {
                if let Some(c) = &msg.content {
                    trajectory.push(json!({ "from": "human", "value": c }));
                }
            }
            _ => {}
        }
        i += 1;
    }
    trajectory
}

// ---------------------------------------------------------------------------
// Tool-call argument sanitization
// ---------------------------------------------------------------------------

pub fn sanitize_tool_call_arguments(messages: &mut Vec<Message>, session_id: Option<&str>) -> u32 {
    let mut repaired = 0u32;
    let mut message_index = 0usize;
    while message_index < messages.len() {
        let msg = &messages[message_index];
        if msg.role != MessageRole::Assistant {
            message_index += 1;
            continue;
        }
        let Some(tool_calls) = msg.tool_calls.clone() else {
            message_index += 1;
            continue;
        };
        if tool_calls.is_empty() {
            message_index += 1;
            continue;
        }

        let mut insert_at = message_index + 1;
        for (tc_idx, mut tc) in tool_calls.into_iter().enumerate() {
            let arguments = tc.function.arguments.clone();
            if arguments.is_empty() {
                tc.function.arguments = "{}".to_string();
                if let Some(tcs) = messages[message_index].tool_calls.as_mut() {
                    tcs[tc_idx].function.arguments = "{}".to_string();
                }
                continue;
            }
            if arguments.trim().is_empty() {
                tc.function.arguments = "{}".to_string();
                if let Some(tcs) = messages[message_index].tool_calls.as_mut() {
                    tcs[tc_idx].function.arguments = "{}".to_string();
                }
                continue;
            }
            if serde_json::from_str::<Value>(&arguments).is_ok() {
                continue;
            }

            let tool_call_id = tc.id.clone();
            let function_name = tc.function.name.clone();
            let preview: String = arguments.chars().take(80).collect();
            warn!(
                session = session_id.unwrap_or("-"),
                message_index,
                tool_call_id = %tool_call_id,
                function = %function_name,
                preview = %preview,
                "Corrupted tool_call arguments repaired before request"
            );
            tc.function.arguments = "{}".to_string();
            if let Some(tcs) = messages[message_index].tool_calls.as_mut() {
                tcs[tc_idx].function.arguments = "{}".to_string();
            }

            let mut existing_tool_idx = None;
            let mut scan_index = message_index + 1;
            while scan_index < messages.len() {
                let candidate = &messages[scan_index];
                if candidate.role != MessageRole::Tool {
                    break;
                }
                if candidate.tool_call_id.as_deref() == Some(tool_call_id.as_str()) {
                    existing_tool_idx = Some(scan_index);
                    break;
                }
                scan_index += 1;
            }

            if let Some(idx) = existing_tool_idx {
                prepend_corruption_marker(&mut messages[idx].content);
            } else {
                messages.insert(
                    insert_at,
                    Message {
                        role: MessageRole::Tool,
                        content: Some(TOOL_CALL_ARGUMENTS_CORRUPTION_MARKER.to_string()),
                        tool_calls: None,
                        tool_call_id: Some(tool_call_id),
                        name: if function_name.is_empty() {
                            None
                        } else {
                            Some(function_name)
                        },
                        reasoning_content: None,
                        cache_control: None,
                    },
                );
                insert_at += 1;
            }
            repaired += 1;
        }
        message_index += 1;
    }
    repaired
}

fn prepend_corruption_marker(content: &mut Option<String>) {
    match content {
        Some(existing) if existing.is_empty() => {
            *existing = TOOL_CALL_ARGUMENTS_CORRUPTION_MARKER.to_string();
        }
        Some(existing) if !existing.starts_with(TOOL_CALL_ARGUMENTS_CORRUPTION_MARKER) => {
            *existing = format!("{}\n{}", TOOL_CALL_ARGUMENTS_CORRUPTION_MARKER, existing);
        }
        None => {
            *content = Some(TOOL_CALL_ARGUMENTS_CORRUPTION_MARKER.to_string());
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Message sequence repair
// ---------------------------------------------------------------------------

pub fn repair_message_sequence(messages: &mut Vec<Message>) -> u32 {
    if messages.is_empty() {
        return 0;
    }
    let mut repairs = 0u32;
    let mut known_tool_ids: HashSet<String> = HashSet::new();
    let mut filtered = Vec::with_capacity(messages.len());

    for msg in messages.iter() {
        match msg.role {
            MessageRole::Assistant => {
                known_tool_ids.clear();
                if let Some(tcs) = &msg.tool_calls {
                    for tc in tcs {
                        if !tc.id.is_empty() {
                            known_tool_ids.insert(tc.id.clone());
                        }
                    }
                }
                filtered.push(msg.clone());
            }
            MessageRole::Tool => {
                let tc_id = msg.tool_call_id.as_deref().unwrap_or("");
                if !tc_id.is_empty() && known_tool_ids.contains(tc_id) {
                    filtered.push(msg.clone());
                } else {
                    repairs += 1;
                }
            }
            MessageRole::User => {
                known_tool_ids.clear();
                filtered.push(msg.clone());
            }
            _ => filtered.push(msg.clone()),
        }
    }

    let mut merged: Vec<Message> = Vec::with_capacity(filtered.len());
    for msg in filtered {
        if let (Some(prev), MessageRole::User) = (merged.last_mut(), msg.role) {
            if prev.role == MessageRole::User {
                let prev_s = prev.content.as_deref().unwrap_or("");
                let new_s = msg.content.as_deref().unwrap_or("");
                if !prev_s.is_empty() || !new_s.is_empty() {
                    let joined = match (prev_s.is_empty(), new_s.is_empty()) {
                        (true, _) => new_s.to_string(),
                        (_, true) => prev_s.to_string(),
                        _ => format!("{prev_s}\n\n{new_s}"),
                    };
                    prev.content = Some(joined);
                    repairs += 1;
                    continue;
                }
            }
        }
        merged.push(msg);
    }

    if repairs > 0 {
        *messages = merged;
    }
    repairs
}

// ---------------------------------------------------------------------------
// Think-block stripping (full Python `_strip_think_blocks` regex set)
// ---------------------------------------------------------------------------

pub fn strip_think_blocks(content: &str) -> String {
    if content.is_empty() {
        return String::new();
    }
    let mut c = content.to_string();
    c = STRIP_CLOSED_RED.replace_all(&c, "").to_string();
    c = STRIP_CLOSED_THINK.replace_all(&c, "").to_string();
    c = STRIP_CLOSED_REASON.replace_all(&c, "").to_string();
    c = STRIP_CLOSED_SCRATCH.replace_all(&c, "").to_string();
    c = STRIP_CLOSED_THOUGHT.replace_all(&c, "").to_string();
    for name in [
        "tool_call",
        "tool_calls",
        "tool_result",
        "function_call",
        "function_calls",
    ] {
        let re = tool_call_xml_regex(name);
        c = re.replace_all(&c, "").to_string();
    }
    c = STRIP_FUNCTION_NAMED.replace_all(&c, "").to_string();
    c = STRIP_UNTERM_REASON.replace_all(&c, "").to_string();
    c = STRIP_ORPHAN_TAGS.replace_all(&c, "").to_string();
    c = STRIP_TOOL_CLOSERS.replace_all(&c, "").to_string();
    c
}

// ---------------------------------------------------------------------------
// Thinking-only assistant cleanup
// ---------------------------------------------------------------------------

pub fn is_thinking_only_assistant(msg: &Message) -> bool {
    if msg.role != MessageRole::Assistant {
        return false;
    }
    if msg.tool_calls.as_ref().is_some_and(|t| !t.is_empty()) {
        return false;
    }
    if let Some(c) = msg.content.as_deref() {
        if !c.trim().is_empty() {
            return false;
        }
    }
    if msg
        .reasoning_content
        .as_deref()
        .is_some_and(|r| !r.trim().is_empty())
    {
        return true;
    }
    false
}

pub fn drop_thinking_only_and_merge_users(messages: Vec<Message>) -> Vec<Message> {
    if messages.is_empty() {
        return messages;
    }
    let original_len = messages.len();
    let kept: Vec<Message> = messages
        .into_iter()
        .filter(|m| !is_thinking_only_assistant(m))
        .collect();
    if kept.len() == original_len {
        return kept;
    }
    let dropped = original_len - kept.len();
    let mut merged: Vec<Message> = Vec::with_capacity(kept.len());
    let mut merges = 0u32;
    for m in kept {
        if let (Some(prev), MessageRole::User) = (merged.last_mut(), m.role) {
            if prev.role == MessageRole::User {
                let prev_s = prev.content.take().unwrap_or_default();
                let cur_s = m.content.unwrap_or_default();
                let sep = if !prev_s.is_empty() && !cur_s.is_empty() {
                    "\n\n"
                } else {
                    ""
                };
                prev.content = Some(format!("{prev_s}{sep}{cur_s}"));
                merges += 1;
                continue;
            }
        }
        merged.push(m);
    }
    debug!(
        dropped,
        merges, "Pre-call sanitizer: dropped thinking-only assistant turns"
    );
    merged
}

// ---------------------------------------------------------------------------
// API message sanitization
// ---------------------------------------------------------------------------

fn role_str(role: MessageRole) -> &'static str {
    match role {
        MessageRole::System => "system",
        MessageRole::User => "user",
        MessageRole::Assistant => "assistant",
        MessageRole::Tool => "tool",
    }
}

pub fn sanitize_api_messages(messages: Vec<Message>) -> Vec<Message> {
    let valid: HashSet<&str> = VALID_API_ROLES.iter().copied().collect();
    let messages: Vec<Message> = messages
        .into_iter()
        .filter(|m| {
            let ok = valid.contains(role_str(m.role));
            if !ok {
                debug!(
                    role = role_str(m.role),
                    "Pre-call sanitizer: dropping message with invalid role"
                );
            }
            ok
        })
        .collect();

    let mut surviving_call_ids: HashSet<String> = HashSet::new();
    for msg in &messages {
        if msg.role == MessageRole::Assistant {
            if let Some(tcs) = &msg.tool_calls {
                for tc in tcs {
                    if !tc.id.is_empty() {
                        surviving_call_ids.insert(tc.id.clone());
                    }
                }
            }
        }
    }

    let mut result_call_ids: HashSet<String> = HashSet::new();
    for msg in &messages {
        if msg.role == MessageRole::Tool {
            if let Some(id) = &msg.tool_call_id {
                result_call_ids.insert(id.clone());
            }
        }
    }

    let orphaned_results: HashSet<_> = result_call_ids
        .difference(&surviving_call_ids)
        .cloned()
        .collect();
    let messages: Vec<Message> = if orphaned_results.is_empty() {
        messages
    } else {
        debug!(
            count = orphaned_results.len(),
            "Pre-call sanitizer: removed orphaned tool result(s)"
        );
        messages
            .into_iter()
            .filter(|m| {
                !(m.role == MessageRole::Tool
                    && m.tool_call_id
                        .as_ref()
                        .is_some_and(|id| orphaned_results.contains(id)))
            })
            .collect()
    };

    let missing_results: HashSet<_> = surviving_call_ids
        .difference(&result_call_ids)
        .cloned()
        .collect();
    if missing_results.is_empty() {
        return messages;
    }

    let mut patched = Vec::new();
    for msg in messages {
        patched.push(msg.clone());
        if msg.role == MessageRole::Assistant {
            if let Some(tcs) = &msg.tool_calls {
                for tc in tcs {
                    if !tc.id.is_empty() && missing_results.contains(&tc.id) {
                        patched.push(Message {
                            role: MessageRole::Tool,
                            content: Some(STUB_TOOL_RESULT_CONTENT.to_string()),
                            tool_calls: None,
                            tool_call_id: Some(tc.id.clone()),
                            name: Some(tc.function.name.clone()),
                            reasoning_content: None,
                            cache_control: None,
                        });
                    }
                }
            }
        }
    }
    debug!(
        count = missing_results.len(),
        "Pre-call sanitizer: added stub tool result(s)"
    );
    patched
}

// ---------------------------------------------------------------------------
// Tool name repair
// ---------------------------------------------------------------------------

fn norm_tool_name(s: &str) -> String {
    s.to_ascii_lowercase().replace('-', "_").replace(' ', "_")
}

fn camel_to_snake(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

fn strip_tool_suffix(s: &str) -> Option<String> {
    let lc = s.to_ascii_lowercase();
    for suffix in ["_tool", "-tool", "tool"] {
        if lc.ends_with(suffix) {
            let trimmed = &s[..s.len().saturating_sub(suffix.len())];
            return Some(
                trimmed
                    .trim_end_matches(|c| c == '_' || c == '-')
                    .to_string(),
            );
        }
    }
    None
}

fn levenshtein_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }
    let mut prev: Vec<usize> = (0..=n).collect();
    let mut curr = vec![0; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

fn similarity_ratio(a: &str, b: &str) -> f64 {
    let max_len = a.len().max(b.len());
    if max_len == 0 {
        return 1.0;
    }
    1.0 - (levenshtein_distance(a, b) as f64 / max_len as f64)
}

/// Repair a mismatched tool name against the registry (Python `repair_tool_call`).
pub fn repair_tool_name(tool_name: &str, valid_names: &[String]) -> Option<String> {
    if tool_name.is_empty() || valid_names.is_empty() {
        return None;
    }
    let valid_set: HashSet<&str> = valid_names.iter().map(String::as_str).collect();
    let lowered = tool_name.to_ascii_lowercase();
    if valid_set.contains(lowered.as_str()) {
        return Some(lowered);
    }
    let normalized = norm_tool_name(tool_name);
    if valid_set.contains(normalized.as_str()) {
        return valid_names
            .iter()
            .find(|n| norm_tool_name(n) == normalized)
            .cloned();
    }

    let mut cands: HashSet<String> = HashSet::new();
    cands.insert(tool_name.to_string());
    cands.insert(lowered.clone());
    cands.insert(normalized.clone());
    cands.insert(camel_to_snake(tool_name));
    for _ in 0..2 {
        let extra: Vec<String> = cands
            .iter()
            .filter_map(|c| strip_tool_suffix(c))
            .flat_map(|stripped| {
                [
                    stripped.clone(),
                    norm_tool_name(&stripped),
                    camel_to_snake(&stripped),
                ]
            })
            .collect();
        cands.extend(extra);
    }
    for c in &cands {
        if valid_set.contains(c.as_str()) {
            return valid_names.iter().find(|n| *n == c).cloned();
        }
        let nc = norm_tool_name(c);
        if let Some(found) = valid_names.iter().find(|n| norm_tool_name(n) == nc) {
            return Some(found.clone());
        }
    }

    let mut best: Option<(f64, String)> = None;
    for name in valid_names {
        let ratio = similarity_ratio(&lowered, &name.to_ascii_lowercase());
        if ratio >= 0.7 {
            if best.as_ref().is_none_or(|(r, _)| ratio > *r) {
                best = Some((ratio, name.clone()));
            }
        }
    }
    best.map(|(_, n)| n)
}

pub fn normalize_tool_call_arguments(tc: &mut ToolCall) -> Result<(), String> {
    let trimmed = tc.function.arguments.trim();
    if trimmed.is_empty() {
        tc.function.arguments = "{}".to_string();
        return Ok(());
    }
    serde_json::from_str::<Value>(trimmed)
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ---------------------------------------------------------------------------
// Codex intermediate ack
// ---------------------------------------------------------------------------

pub fn looks_like_codex_intermediate_ack(
    user_message: &str,
    assistant_content: &str,
    messages: &[Message],
) -> bool {
    let history_includes_tool = messages.iter().any(|m| m.role == MessageRole::Tool);
    message_sanitization::looks_like_codex_intermediate_ack(
        user_message,
        assistant_content,
        history_includes_tool,
    )
}

// ---------------------------------------------------------------------------
// Prompt cache policy (includes Qwen / Alibaba family from Python)
// ---------------------------------------------------------------------------

fn base_url_hostname(base_url: &str) -> Option<String> {
    let trimmed = base_url.trim();
    if trimmed.is_empty() {
        return None;
    }
    let without_scheme = trimmed.split("://").nth(1).unwrap_or(trimmed);
    without_scheme
        .split('/')
        .next()
        .map(|host| host.split(':').next().unwrap_or(host).to_ascii_lowercase())
}

fn base_url_host_matches(base_url: &str, host: &str) -> bool {
    base_url_hostname(base_url)
        .as_deref()
        .is_some_and(|h| h == host || h.ends_with(&format!(".{host}")))
}

pub fn anthropic_prompt_cache_policy(
    provider: &str,
    base_url: &str,
    api_mode: &str,
    model: &str,
) -> (bool, bool) {
    let model_lower = model.to_ascii_lowercase();
    let provider_lower = provider.to_ascii_lowercase();
    let base_lower = base_url.to_ascii_lowercase();
    let is_claude = model_lower.contains("claude");
    let is_openrouter = base_url_host_matches(base_url, "openrouter.ai");
    let is_nous_portal = base_lower.contains("nousresearch");
    let is_anthropic_wire = api_mode == "anthropic_messages";
    let is_native_anthropic = is_anthropic_wire
        && (provider_lower == "anthropic"
            || base_url_hostname(base_url).as_deref() == Some("api.anthropic.com"));

    if is_native_anthropic {
        return (true, true);
    }
    if (is_openrouter || is_nous_portal) && is_claude {
        return (true, false);
    }
    if is_nous_portal && model_lower.contains("qwen") {
        return (true, false);
    }
    if is_anthropic_wire && is_claude {
        return (true, true);
    }
    if is_anthropic_wire {
        let is_minimax_provider = matches!(provider_lower.as_str(), "minimax" | "minimax-cn");
        let is_minimax_host = base_url_host_matches(base_url, "api.minimax.io")
            || base_url_host_matches(base_url, "api.minimaxi.com");
        if is_minimax_provider || is_minimax_host {
            return (true, true);
        }
    }
    let model_is_qwen = model_lower.contains("qwen");
    let provider_is_alibaba_family = matches!(
        provider_lower.as_str(),
        "opencode" | "opencode-zen" | "opencode-go" | "alibaba"
    );
    if provider_is_alibaba_family && model_is_qwen {
        return (true, false);
    }
    // DeepSeek: automatic server-side prefix caching via chat_completions.
    // No client-side cache_control markers needed; the provider compares
    // byte prefixes and only bills new tokens.
    let is_deepseek_host = base_url_host_matches(base_url, "api.deepseek.com");
    if provider_lower == "deepseek" || is_deepseek_host || model_lower.contains("deepseek") {
        return (true, false);
    }
    (false, false)
}

fn prompt_cache_env_flag(name: &str) -> bool {
    std::env::var(name).ok().is_some_and(|v| {
        matches!(
            v.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

/// Resolve the effective prompt-cache policy, layering a user opt-in on top of
/// the built-in [`anthropic_prompt_cache_policy`].
///
/// The built-in policy only knows a fixed set of providers (native Anthropic,
/// OpenRouter/Nous Claude+Qwen, MiniMax anthropic-wire, Alibaba/Qwen). Custom
/// or self-hosted OpenAI-compatible endpoints that *do* support prompt caching
/// are not covered, so this wrapper lets them opt in:
///
/// - `HERMES_FORCE_PROMPT_CACHING=1` force-enables the caching subsystem when
///   the built-in policy declines (any provider, including `custom:`).
/// - Layout defaults to the native Anthropic content-block layout only on the
///   `anthropic_messages` wire; otherwise the envelope (chat_completions)
///   layout is used. `HERMES_FORCE_PROMPT_CACHE_NATIVE=1` forces native layout
///   regardless of wire — only set this if the endpoint speaks the Anthropic
///   Messages schema.
///
/// Default off: sending `cache_control` markers to an endpoint that rejects
/// unknown fields can break requests, so this is strictly opt-in.
pub fn resolve_prompt_cache_policy(
    provider: &str,
    base_url: &str,
    api_mode: &str,
    model: &str,
) -> (bool, bool) {
    let (should_cache, native) = anthropic_prompt_cache_policy(provider, base_url, api_mode, model);
    if should_cache {
        return (should_cache, native);
    }
    if prompt_cache_env_flag("HERMES_FORCE_PROMPT_CACHING") {
        let force_native = prompt_cache_env_flag("HERMES_FORCE_PROMPT_CACHE_NATIVE")
            || api_mode == "anthropic_messages";
        return (true, force_native);
    }
    (false, false)
}

// ---------------------------------------------------------------------------
// Thinking-mode reasoning pad
// ---------------------------------------------------------------------------

pub fn needs_thinking_reasoning_pad(provider: &str, model: &str, base_url: &str) -> bool {
    needs_deepseek_tool_reasoning(provider, model, base_url)
        || needs_kimi_tool_reasoning(provider, base_url)
        || needs_mimo_tool_reasoning(provider, model, base_url)
}

fn needs_deepseek_tool_reasoning(provider: &str, model: &str, base_url: &str) -> bool {
    let provider = provider.to_ascii_lowercase();
    let model = model.to_ascii_lowercase();
    provider == "deepseek"
        || model.contains("deepseek")
        || base_url_host_matches(base_url, "api.deepseek.com")
}

fn needs_kimi_tool_reasoning(provider: &str, base_url: &str) -> bool {
    matches!(
        provider.to_ascii_lowercase().as_str(),
        "kimi-coding" | "kimi-coding-cn"
    ) || base_url_host_matches(base_url, "api.kimi.com")
        || base_url_host_matches(base_url, "moonshot.ai")
        || base_url_host_matches(base_url, "moonshot.cn")
}

fn needs_mimo_tool_reasoning(provider: &str, model: &str, base_url: &str) -> bool {
    let provider = provider.to_ascii_lowercase();
    let model = model.to_ascii_lowercase();
    provider == "xiaomi"
        || model.contains("mimo")
        || base_url_host_matches(base_url, "api.xiaomimimo.com")
        || base_url_host_matches(base_url, "xiaomimimo.com")
}

pub fn copy_reasoning_content_for_api(source: &Message, api_msg: &mut Message, needs_pad: bool) {
    if source.role != MessageRole::Assistant {
        return;
    }
    if let Some(existing) = source.reasoning_content.as_ref() {
        if existing.is_empty() && needs_pad {
            api_msg.reasoning_content = Some(" ".to_string());
        } else {
            api_msg.reasoning_content = Some(existing.clone());
        }
        return;
    }
    if needs_pad {
        api_msg.reasoning_content = Some(" ".to_string());
        return;
    }
    api_msg.reasoning_content = None;
}

pub fn reapply_reasoning_echo_for_provider(messages: &mut [Message], needs_pad: bool) -> u32 {
    if !needs_pad {
        return 0;
    }
    let mut padded = 0u32;
    for i in 0..messages.len() {
        if messages[i].role != MessageRole::Assistant {
            continue;
        }
        if messages[i]
            .reasoning_content
            .as_ref()
            .is_some_and(|s| !s.is_empty())
        {
            continue;
        }
        let source = messages[i].clone();
        let before = messages[i].reasoning_content.clone();
        copy_reasoning_content_for_api(&source, &mut messages[i], needs_pad);
        if messages[i].reasoning_content != before && messages[i].reasoning_content.is_some() {
            padded += 1;
        }
    }
    padded
}

// ---------------------------------------------------------------------------
// API error context
// ---------------------------------------------------------------------------

pub fn extract_api_error_context(
    error_message: &str,
    status_code: Option<u16>,
    body: Option<&Value>,
) -> Value {
    let mut context = serde_json::Map::new();
    if let Some(body) = body {
        let payload = body.get("error").filter(|e| e.is_object()).unwrap_or(body);
        if let Some(obj) = payload.as_object() {
            for key in ["code", "type", "error"] {
                if let Some(reason) = obj.get(key).and_then(|v| v.as_str()) {
                    if !reason.trim().is_empty() {
                        context.insert("reason".into(), Value::String(reason.trim().to_string()));
                        break;
                    }
                }
            }
            for key in ["message", "error_description"] {
                if let Some(msg) = obj.get(key).and_then(|v| v.as_str()) {
                    if !msg.trim().is_empty() {
                        context.insert("message".into(), Value::String(msg.trim().to_string()));
                        break;
                    }
                }
            }
            for key in ["resets_at", "reset_at"] {
                if let Some(v) = obj.get(key) {
                    if !v.is_null() && v != &Value::String(String::new()) {
                        context.insert("reset_at".into(), v.clone());
                        break;
                    }
                }
            }
            if !context.contains_key("reset_at") {
                if let Some(retry) = obj.get("retry_after") {
                    if let Some(secs) = retry.as_f64().or_else(|| retry.as_u64().map(|u| u as f64))
                    {
                        let now = SystemTime::now()
                            .duration_since(UNIX_EPOCH)
                            .map(|d| d.as_secs_f64())
                            .unwrap_or(0.0);
                        context.insert("reset_at".into(), json!(now + secs));
                    }
                }
            }
        }
    }
    if !context.contains_key("message") {
        let trimmed = error_message.trim();
        if !trimmed.is_empty() {
            let cap: String = trimmed.chars().take(500).collect();
            context.insert("message".into(), Value::String(cap));
        }
    }
    if !context.contains_key("reset_at") {
        if let Some(msg) = context.get("message").and_then(|v| v.as_str()) {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs_f64())
                .unwrap_or(0.0);
            if let Some(caps) = EXTRACT_RESET_DELAY.captures(msg) {
                let value: f64 = caps[1].parse().unwrap_or(0.0);
                let seconds = if caps[2].eq_ignore_ascii_case("ms") {
                    value / 1000.0
                } else {
                    value
                };
                context.insert("reset_at".into(), json!(now + seconds));
            } else if let Some(caps) = EXTRACT_RESETS_IN.captures(msg) {
                let hours: f64 = caps
                    .get(1)
                    .and_then(|m| m.as_str().parse().ok())
                    .unwrap_or(0.0);
                let minutes: f64 = caps
                    .get(2)
                    .and_then(|m| m.as_str().parse().ok())
                    .unwrap_or(0.0);
                let seconds: f64 = caps
                    .get(3)
                    .and_then(|m| m.as_str().parse().ok())
                    .unwrap_or(0.0);
                if hours + minutes + seconds > 0.0 {
                    context.insert(
                        "reset_at".into(),
                        json!(now + hours * 3600.0 + minutes * 60.0 + seconds),
                    );
                }
            } else if let Some(caps) = EXTRACT_RETRY_AFTER.captures(msg) {
                let seconds: f64 = caps[1].parse().unwrap_or(0.0);
                context.insert("reset_at".into(), json!(now + seconds));
            }
        }
    }
    let _ = status_code;
    Value::Object(context)
}

// ---------------------------------------------------------------------------
// Inline reasoning extraction
// ---------------------------------------------------------------------------

pub fn extract_reasoning_from_message_content(
    content: &str,
    reasoning_content: Option<&str>,
    reasoning_details: Option<&[Value]>,
) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(r) = reasoning_content.filter(|s| !s.trim().is_empty()) {
        parts.push(r.to_string());
    }
    if let Some(details) = reasoning_details {
        for detail in details {
            for key in ["summary", "thinking", "content", "text"] {
                if let Some(s) = detail.get(key).and_then(|v| v.as_str()) {
                    if !s.trim().is_empty() && !parts.iter().any(|p| p == s) {
                        parts.push(s.to_string());
                    }
                }
            }
        }
    }
    if parts.is_empty() && !content.is_empty() {
        for pattern in INLINE_REASONING_PATTERNS.iter() {
            for cap in pattern.captures_iter(content) {
                if let Some(block) = cap.get(1) {
                    let cleaned = block.as_str().trim();
                    if !cleaned.is_empty() && !parts.iter().any(|p| p == cleaned) {
                        parts.push(cleaned.to_string());
                    }
                }
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

// ---------------------------------------------------------------------------
// Debug dump
// ---------------------------------------------------------------------------

pub fn dump_api_request_debug(
    hermes_home: &Path,
    session_id: &str,
    api_mode: &str,
    base_url: &str,
    body: &Value,
    reason: &str,
    error: Option<&str>,
) -> Option<PathBuf> {
    let logs_dir = hermes_home.join("logs");
    if std::fs::create_dir_all(&logs_dir).is_err() {
        return None;
    }
    let path_suffix = if api_mode == "codex_responses" {
        "/responses"
    } else {
        "/chat/completions"
    };
    let url = format!("{}{}", base_url.trim_end_matches('/'), path_suffix);
    let timestamp = chrono::Local::now().format("%Y%m%d_%H%M%S_%f").to_string();
    let dump_file = logs_dir.join(format!("request_dump_{session_id}_{timestamp}.json"));
    let mut payload = json!({
        "timestamp": chrono::Local::now().to_rfc3339(),
        "session_id": session_id,
        "reason": reason,
        "request": {
            "method": "POST",
            "url": url,
            "headers": {
                "Authorization": "Bearer [REDACTED]",
                "Content-Type": "application/json",
            },
            "body": body,
        },
    });
    if let Some(err) = error {
        payload["error"] = json!({ "message": err });
    }
    match std::fs::write(
        &dump_file,
        serde_json::to_string_pretty(&payload).unwrap_or_default(),
    ) {
        Ok(()) => Some(dump_file),
        Err(e) => {
            warn!("Failed to dump API request debug payload: {e}");
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Credential pool recovery
// ---------------------------------------------------------------------------

fn is_entitlement_failure(error_context: &Value, status_code: Option<u16>) -> bool {
    if !matches!(status_code, Some(401) | Some(403) | None) {
        return false;
    }
    let Some(obj) = error_context.as_object() else {
        return false;
    };
    let haystack = ["message", "reason", "code", "error"]
        .iter()
        .filter_map(|k| obj.get(*k).and_then(|v| v.as_str()))
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase();
    if haystack.trim().is_empty() {
        return false;
    }
    if haystack.contains("[wke=unauthenticated:") {
        return false;
    }
    if haystack.contains("oauth2 access token could not be validated") {
        return false;
    }
    if haystack.contains("do not have an active grok subscription") {
        return true;
    }
    if haystack.contains("out of available resources") && haystack.contains("grok") {
        return true;
    }
    haystack.contains("does not have permission") && haystack.contains("grok")
}

fn usage_limit_reached(error_context: &Value) -> bool {
    let obj = match error_context.as_object() {
        Some(o) => o,
        None => return false,
    };
    let reason = obj
        .get("reason")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let message = obj
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    reason.contains("usage_limit_reached")
        || reason.contains("gousagelimit")
        || message.contains("usage limit reached")
        || message.contains("usage limit has been reached")
}

/// Attempt credential recovery via pool rotation (Python `recover_with_credential_pool`).
///
/// Returns `(recovered, has_retried_429)`.
pub fn recover_with_credential_pool(
    pool: Option<&CredentialPool>,
    provider: &str,
    _base_url: &str,
    status_code: Option<u16>,
    has_retried_429: bool,
    classified_reason: Option<FailoverReason>,
    error_context: &Value,
) -> (bool, bool) {
    let Some(pool) = pool else {
        return (false, has_retried_429);
    };

    let effective_reason = classified_reason.unwrap_or_else(|| match status_code {
        Some(402) => FailoverReason::Billing,
        Some(429) => FailoverReason::RateLimit,
        Some(401) | Some(403) => FailoverReason::Auth,
        _ => FailoverReason::Unknown,
    });

    let rotate = || {
        let duration = std::time::Duration::from_secs(60);
        pool.mark_last_issued_rate_limited_and_has_alternate(duration)
    };

    match effective_reason {
        FailoverReason::Billing => {
            if rotate() {
                let _ = pool.get_key();
                return (true, false);
            }
            (false, has_retried_429)
        }
        FailoverReason::RateLimit => {
            if usage_limit_reached(error_context) {
                if rotate() {
                    let _ = pool.get_key();
                    return (true, false);
                }
                return (false, true);
            }
            if !has_retried_429 {
                return (false, true);
            }
            if rotate() {
                let _ = pool.get_key();
                return (true, false);
            }
            (false, true)
        }
        FailoverReason::Auth => {
            let mut is_entitlement = is_entitlement_failure(error_context, status_code);
            if !is_entitlement
                && status_code == Some(403)
                && provider.eq_ignore_ascii_case("xai-oauth")
            {
                let haystack = ["message", "reason", "code", "error"]
                    .iter()
                    .filter_map(|k| error_context.get(*k).and_then(|v| v.as_str()))
                    .collect::<Vec<_>>()
                    .join(" ")
                    .to_ascii_lowercase();
                let is_xai_auth_failure = haystack.contains("[wke=unauthenticated:")
                    || haystack.contains("oauth2 access token could not be validated");
                if !is_xai_auth_failure {
                    is_entitlement = true;
                }
            }
            if is_entitlement {
                return (false, has_retried_429);
            }
            if rotate() {
                let _ = pool.get_key();
                return (true, has_retried_429);
            }
            (false, has_retried_429)
        }
        _ => (false, has_retried_429),
    }
}

// ---------------------------------------------------------------------------
// Pre-API preparation chains
// ---------------------------------------------------------------------------

/// Repair live history before persistence / next turn (tool args + sequence).
pub fn prepare_live_history_for_api(
    messages: &mut Vec<Message>,
    session_id: Option<&str>,
) -> (u32, u32) {
    let tool_repairs = sanitize_tool_call_arguments(messages, session_id);
    let seq_repairs = repair_message_sequence(messages);
    (tool_repairs, seq_repairs)
}

/// Build the wire copy sent to the provider.
pub fn prepare_wire_messages_for_api(
    messages: Vec<Message>,
    provider: &str,
    model: &str,
    base_url: &str,
) -> Vec<Message> {
    let needs_pad = needs_thinking_reasoning_pad(provider, model, base_url);
    let mut out = sanitize_api_messages(messages);
    out = drop_thinking_only_and_merge_users(out);
    // DeepSeek: automatic prefix caching means re-sending reasoning_content
    // changes the byte prefix every turn, destroying cache. The reasoning
    // is billable prompt input with no cache benefit — drop it.
    // (Reasonix agent.go#L646-L649: "re-sent reasoning is billable prompt
    //  input for no cache or coherence gain")
    let is_deepseek = provider.to_ascii_lowercase() == "deepseek"
        || model.to_ascii_lowercase().contains("deepseek")
        || base_url_host_matches(base_url, "api.deepseek.com");
    if !is_deepseek {
        reapply_reasoning_echo_for_provider(&mut out, needs_pad);
    } else {
        // Strip reasoning_content from all messages for DeepSeek.
        // Re-sending reasoning is billable prompt input with no cache
        // or coherence gain — and it changes the byte prefix, destroying
        // the service-side automatic prefix cache.
        for msg in &mut out {
            if msg.role == MessageRole::Assistant {
                msg.reasoning_content = None;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::{FunctionCall, ToolCall};

    fn tc(id: &str, name: &str, args: &str) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            function: FunctionCall {
                name: name.to_string(),
                arguments: args.to_string(),
            },
            extra_content: None,
        }
    }

    #[test]
    fn repair_merges_consecutive_user_messages() {
        let mut messages = vec![Message::user("first"), Message::user("second")];
        let repairs = repair_message_sequence(&mut messages);
        assert_eq!(repairs, 1);
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].content.as_deref(), Some("first\n\nsecond"));
    }

    #[test]
    fn repair_drops_stray_tool_message() {
        let mut messages = vec![
            Message::user("hi"),
            Message::assistant("hello"),
            Message {
                role: MessageRole::Tool,
                content: Some("stray".into()),
                tool_calls: None,
                tool_call_id: Some("orphan".into()),
                name: None,
                reasoning_content: None,
                cache_control: None,
            },
            Message::user("real"),
        ];
        let repairs = repair_message_sequence(&mut messages);
        assert!(repairs >= 1);
        assert!(messages.iter().all(|m| m.role != MessageRole::Tool));
    }

    #[test]
    fn repair_preserves_valid_tool_chain() {
        let mut messages = vec![
            Message::user("list files"),
            Message::assistant_with_tool_calls(None, vec![tc("t1", "ls", "{}")]),
            Message {
                role: MessageRole::Tool,
                content: Some("a.txt".into()),
                tool_calls: None,
                tool_call_id: Some("t1".into()),
                name: None,
                reasoning_content: None,
                cache_control: None,
            },
            Message::assistant("Found 2 files"),
            Message::user("more"),
        ];
        let original = messages.clone();
        assert_eq!(repair_message_sequence(&mut messages), 0);
        assert_eq!(messages.len(), original.len());
    }

    #[test]
    fn thinking_only_detection_and_drop_merge() {
        let thinking = Message {
            role: MessageRole::Assistant,
            content: Some(String::new()),
            tool_calls: None,
            tool_call_id: None,
            name: None,
            reasoning_content: Some("internal".into()),
            cache_control: None,
        };
        assert!(is_thinking_only_assistant(&thinking));
        assert!(!is_thinking_only_assistant(&Message::assistant("visible")));

        let msgs = vec![
            Message::user("help me with X"),
            thinking,
            Message::user("ok continue"),
        ];
        let out = drop_thinking_only_and_merge_users(msgs);
        assert_eq!(out.len(), 1);
        assert_eq!(
            out[0].content.as_deref(),
            Some("help me with X\n\nok continue")
        );
    }

    #[test]
    fn sanitize_corrupted_tool_args_inserts_marker() {
        let mut messages = vec![Message::assistant_with_tool_calls(
            Some("tooling".into()),
            vec![tc("call_1", "read_file", r#"{"path": "/tmp/foo"#)],
        )];
        let repaired = sanitize_tool_call_arguments(&mut messages, Some("session-123"));
        assert_eq!(repaired, 1);
        assert_eq!(
            messages[0].tool_calls.as_ref().unwrap()[0]
                .function
                .arguments,
            "{}"
        );
        assert_eq!(messages.len(), 2);
        assert_eq!(
            messages[1].content.as_deref(),
            Some(TOOL_CALL_ARGUMENTS_CORRUPTION_MARKER)
        );
        assert_eq!(messages[1].tool_call_id.as_deref(), Some("call_1"));
    }

    #[test]
    fn sanitize_marker_prepended_to_existing_tool_message() {
        let mut messages = vec![
            Message::assistant_with_tool_calls(
                None,
                vec![tc("call_1", "read_file", r#"{"path": "/tmp/foo"#)],
            ),
            Message {
                role: MessageRole::Tool,
                content: Some("existing tool output".into()),
                tool_calls: None,
                tool_call_id: Some("call_1".into()),
                name: None,
                reasoning_content: None,
                cache_control: None,
            },
        ];
        assert_eq!(sanitize_tool_call_arguments(&mut messages, None), 1);
        let expected = format!(
            "{}\nexisting tool output",
            TOOL_CALL_ARGUMENTS_CORRUPTION_MARKER
        );
        assert_eq!(messages[1].content.as_deref(), Some(expected.as_str()));
    }

    #[test]
    fn repair_tool_name_fuzzy_and_suffix() {
        let valid = vec![
            "todo".to_string(),
            "read_file".to_string(),
            "patch".to_string(),
        ];
        assert_eq!(
            repair_tool_name("TodoTool_tool", &valid).as_deref(),
            Some("todo")
        );
        assert_eq!(
            repair_tool_name("read-file", &valid).as_deref(),
            Some("read_file")
        );
    }

    #[test]
    fn strip_think_blocks_removes_closed_and_unterminated() {
        let input = "<think>secret</think>\nHello\n<thinking>tail";
        let out = strip_think_blocks(input);
        assert!(out.contains("Hello"));
        assert!(!out.contains("secret"));
        assert!(!out.to_ascii_lowercase().contains("<thinking>"));
    }

    #[test]
    fn anthropic_policy_qwen_opencode() {
        let (cache, native) = anthropic_prompt_cache_policy(
            "opencode-go",
            "https://opencode.ai/zen/v1",
            "chat_completions",
            "qwen3.6-plus",
        );
        assert!(cache);
        assert!(!native);
    }

    // Combined into one test: all three cases mutate the same process-global env
    // vars, so they must run sequentially rather than as parallel test threads.
    #[test]
    fn resolve_policy_force_enable_override() {
        unsafe {
            std::env::remove_var("HERMES_FORCE_PROMPT_CACHING");
            std::env::remove_var("HERMES_FORCE_PROMPT_CACHE_NATIVE");
        }
        // Off by default for an uncovered custom provider.
        let (cache, native) = resolve_prompt_cache_policy(
            "custom",
            "https://my-endpoint.example/v1",
            "chat_completions",
            "custom:MiniMax-M2.7",
        );
        assert!(!cache);
        assert!(!native);

        // Force-enabled on chat_completions wire -> envelope layout (native=false).
        unsafe {
            std::env::set_var("HERMES_FORCE_PROMPT_CACHING", "1");
        }
        let (cache, native) = resolve_prompt_cache_policy(
            "custom",
            "https://my-endpoint.example/v1",
            "chat_completions",
            "custom:MiniMax-M2.7",
        );
        assert!(cache);
        assert!(!native);

        // anthropic_messages wire -> native content-block layout.
        let (cache, native) = resolve_prompt_cache_policy(
            "custom",
            "https://my-endpoint.example/v1",
            "anthropic_messages",
            "custom:some-claude-compatible",
        );
        assert!(cache);
        assert!(native);

        unsafe {
            std::env::remove_var("HERMES_FORCE_PROMPT_CACHING");
        }
    }
}
