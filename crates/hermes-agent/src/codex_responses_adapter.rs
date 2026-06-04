//! Python `agent.codex_responses_adapter` — Responses API format conversion.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use regex::Regex;
use serde_json::{json, Map, Value};
use sha1::{Digest as Sha1Digest, Sha1};
use sha2::{Digest as Sha2Digest, Sha256};
use uuid::Uuid;

use crate::prompt_builder::DEFAULT_AGENT_IDENTITY;
use hermes_tools::tools::schema_sanitizer::strip_slash_enum;

const ACP_MULTIMODAL_PREFIX: &str = "__hermes_acp_parts_json__:";

static CROSS_ISSUER_WARN_EMITTED: AtomicBool = AtomicBool::new(false);
static TOOL_CALL_LEAK_PATTERN: OnceLock<Regex> = OnceLock::new();

const RESPONSE_MESSAGE_STATUSES: &[&str] = &["completed", "incomplete", "in_progress"];

/// Adapter errors mirroring Python `ValueError` / `RuntimeError` messages.
#[derive(Debug, Clone, thiserror::Error)]
pub enum CodexAdapterError {
    #[error("{0}")]
    ValueError(String),
    #[error("{0}")]
    RuntimeError(String),
}

impl CodexAdapterError {
    fn value_error(msg: impl Into<String>) -> Self {
        Self::ValueError(msg.into())
    }

    fn runtime_error(msg: impl Into<String>) -> Self {
        Self::RuntimeError(msg.into())
    }
}

/// Normalized assistant payload from [`normalize_codex_response`].
#[derive(Debug, Clone, PartialEq)]
pub struct CodexAssistantMessage {
    pub content: String,
    pub tool_calls: Vec<CodexToolCall>,
    pub reasoning: Option<String>,
    pub reasoning_content: Option<Value>,
    pub reasoning_details: Option<Value>,
    pub codex_reasoning_items: Option<Vec<Value>>,
    pub codex_message_items: Option<Vec<Value>>,
}

/// Tool call extracted from a Responses `function_call` item.
#[derive(Debug, Clone, PartialEq)]
pub struct CodexToolCall {
    pub id: String,
    pub call_id: String,
    pub response_item_id: String,
    pub type_: String,
    pub function: CodexFunctionCall,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CodexFunctionCall {
    pub name: String,
    pub arguments: String,
}

pub fn classify_responses_issuer(
    is_xai_responses: bool,
    is_github_responses: bool,
    is_codex_backend: bool,
    base_url: Option<&str>,
) -> String {
    if is_xai_responses {
        return "xai_responses".to_string();
    }
    if is_github_responses {
        return "github_responses".to_string();
    }
    if is_codex_backend {
        return "codex_backend".to_string();
    }
    if let Some(url) = base_url {
        return format!("other:{url}");
    }
    "other".to_string()
}

pub fn chat_content_to_responses_parts(content: &Value, role: &str) -> Vec<Value> {
    let text_type = if role == "assistant" {
        "output_text"
    } else {
        "input_text"
    };
    let Value::Array(parts) = content else {
        return Vec::new();
    };
    let mut converted = Vec::new();
    for part in parts {
        if let Some(s) = part.as_str() {
            if !s.is_empty() {
                converted.push(json!({"type": text_type, "text": s}));
            }
            continue;
        }
        let Some(obj) = part.as_object() else {
            continue;
        };
        let ptype = obj
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        if matches!(ptype.as_str(), "text" | "input_text" | "output_text") {
            if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    converted.push(json!({"type": text_type, "text": text}));
                }
            }
            continue;
        }
        if matches!(ptype.as_str(), "image_url" | "input_image") {
            let mut detail = obj.get("detail").and_then(|v| v.as_str()).map(str::to_string);
            let url = match obj.get("image_url") {
                Some(Value::Object(ir)) => {
                    detail = ir
                        .get("detail")
                        .and_then(|v| v.as_str())
                        .map(str::to_string)
                        .or(detail);
                    ir.get("url").and_then(|v| v.as_str())
                }
                Some(Value::String(s)) => Some(s.as_str()),
                _ => None,
            };
            if let Some(url) = url {
                if !url.is_empty() {
                    let mut image_part = json!({"type": "input_image", "image_url": url});
                    if let Some(d) = detail.as_deref() {
                        if !d.trim().is_empty() {
                            image_part["detail"] = json!(d.trim());
                        }
                    }
                    converted.push(image_part);
                }
            }
        }
    }
    converted
}

pub fn summarize_user_message_for_log(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    match content {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        Value::Array(parts) => summarize_multimodal_parts(parts),
        other => other.to_string(),
    }
}

fn summarize_multimodal_parts(parts: &[Value]) -> String {
    let mut text_bits = Vec::new();
    let mut image_count = 0usize;
    for part in parts {
        if let Some(s) = part.as_str() {
            if !s.is_empty() {
                text_bits.push(s.to_string());
            }
            continue;
        }
        let Some(obj) = part.as_object() else {
            continue;
        };
        let ptype = obj
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_ascii_lowercase();
        match ptype.as_str() {
            "text" | "input_text" | "output_text" => {
                if let Some(text) = obj.get("text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        text_bits.push(text.to_string());
                    }
                }
            }
            "image_url" | "input_image" => image_count += 1,
            _ => {}
        }
    }
    let mut summary = text_bits.join(" ").trim().to_string();
    if image_count > 0 {
        let note = if image_count == 1 {
            "[1 image]".to_string()
        } else {
            format!("[{image_count} images]")
        };
        summary = if summary.is_empty() {
            note
        } else {
            format!("{note} {summary}")
        };
    }
    summary
}

pub fn coerce_content_for_summarize(content: &str) -> Value {
    if let Some(payload) = content.trim().strip_prefix(ACP_MULTIMODAL_PREFIX) {
        if let Ok(parsed) = serde_json::from_str::<Value>(payload) {
            return parsed;
        }
    }
    let trimmed = content.trim();
    if trimmed.starts_with('[') {
        if let Ok(parsed) = serde_json::from_str::<Value>(trimmed) {
            if parsed.is_array() {
                return parsed;
            }
        }
    }
    Value::String(content.to_string())
}

pub fn summarize_user_message_for_log_str(content: &str) -> String {
    let value = coerce_content_for_summarize(content);
    summarize_user_message_for_log(Some(&value))
}

pub fn deterministic_call_id(fn_name: &str, arguments: &str, index: usize) -> String {
    let seed = format!("{fn_name}:{arguments}:{index}");
    let digest = Sha256::digest(seed.as_bytes());
    let hex_digest = hex::encode(digest);
    format!("call_{}", &hex_digest[..12])
}

pub fn split_responses_tool_id(raw_id: Option<&str>) -> (Option<String>, Option<String>) {
    let Some(value) = raw_id.map(str::trim).filter(|s| !s.is_empty()) else {
        return (None, None);
    };
    if let Some((call_id, response_item_id)) = value.split_once('|') {
        let call_id = call_id.trim();
        let response_item_id = response_item_id.trim();
        return (
            (!call_id.is_empty()).then(|| call_id.to_string()),
            (!response_item_id.is_empty()).then(|| response_item_id.to_string()),
        );
    }
    if value.starts_with("fc_") {
        return (None, Some(value.to_string()));
    }
    (Some(value.to_string()), None)
}

pub fn derive_responses_function_call_id(
    call_id: &str,
    response_item_id: Option<&str>,
) -> String {
    if let Some(candidate) = response_item_id.map(str::trim).filter(|s| !s.is_empty()) {
        if candidate.starts_with("fc_") {
            return candidate.to_string();
        }
    }

    let source = call_id.trim();
    if source.starts_with("fc_") {
        return source.to_string();
    }
    if source.starts_with("call_") && source.len() > "call_".len() {
        return format!("fc_{}", &source["call_".len()..]);
    }

    let sanitized: String = source
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect();
    if sanitized.starts_with("fc_") {
        return sanitized;
    }
    if sanitized.starts_with("call_") && sanitized.len() > "call_".len() {
        return format!("fc_{}", &sanitized["call_".len()..]);
    }
    if !sanitized.is_empty() {
        let truncated: String = sanitized.chars().take(48).collect();
        return format!("fc_{truncated}");
    }

    let seed = if !source.is_empty() {
        source.to_string()
    } else {
        response_item_id
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| Uuid::new_v4().simple().to_string())
    };
    let digest = Sha1::digest(seed.as_bytes());
    let hex_digest = hex::encode(digest);
    format!("fc_{}", &hex_digest[..24])
}

pub fn responses_tools(tools: Option<&[Value]>) -> Option<Vec<Value>> {
    let tools = tools?;
    let mut converted = Vec::new();
    for item in tools {
        let Some(obj) = item.as_object() else {
            continue;
        };
        let fn_obj = obj.get("function").and_then(|v| v.as_object());
        let name = fn_obj
            .and_then(|f| f.get("name"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty());
        let Some(name) = name else {
            continue;
        };
        let description = fn_obj
            .and_then(|f| f.get("description"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let parameters = fn_obj
            .and_then(|f| f.get("parameters"))
            .cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
        converted.push(json!({
            "type": "function",
            "name": name,
            "description": description,
            "strict": false,
            "parameters": parameters,
        }));
    }
    if converted.is_empty() {
        None
    } else {
        Some(converted)
    }
}

pub fn normalize_responses_message_status(value: Option<&Value>, default: &str) -> String {
    if let Some(status) = value.and_then(|v| v.as_str()) {
        let status = status
            .trim()
            .to_ascii_lowercase()
            .replace('-', "_")
            .replace(' ', "_");
        if RESPONSE_MESSAGE_STATUSES.contains(&status.as_str()) {
            return status;
        }
    }
    default.to_string()
}

pub fn chat_messages_to_responses_input(
    messages: &[Value],
    replay_encrypted_reasoning: bool,
    current_issuer_kind: Option<&str>,
) -> Vec<Value> {
    let mut items = Vec::new();
    let mut seen_item_ids: HashSet<String> = HashSet::new();

    for msg in messages {
        let Some(obj) = msg.as_object() else {
            continue;
        };
        let role = obj.get("role").and_then(|v| v.as_str());
        let Some(role) = role else {
            continue;
        };
        if role == "system" {
            continue;
        }

        if role == "user" || role == "assistant" {
            let empty_content = Value::String(String::new());
            let content = obj.get("content").unwrap_or(&empty_content);
            let (content_parts, content_text) = if content.is_array() {
                let parts = chat_content_to_responses_parts(content, role);
                let text_type = if role == "assistant" {
                    "output_text"
                } else {
                    "input_text"
                };
                let text: String = parts
                    .iter()
                    .filter(|p| p.get("type").and_then(|v| v.as_str()) == Some(text_type))
                    .filter_map(|p| p.get("text").and_then(|v| v.as_str()))
                    .collect();
                (parts, text)
            } else {
                let text = value_to_string(content);
                (Vec::new(), text)
            };

            if role == "assistant" {
                append_assistant_responses_items(
                    obj,
                    &content_parts,
                    &content_text,
                    replay_encrypted_reasoning,
                    current_issuer_kind,
                    &mut items,
                    &mut seen_item_ids,
                );
                continue;
            }

            if !content_parts.is_empty() {
                items.push(json!({"role": role, "content": content_parts}));
            } else {
                items.push(json!({"role": role, "content": content_text}));
            }
            continue;
        }

        if role == "tool" {
            append_tool_responses_item(obj, &mut items);
        }
    }

    items
}

fn append_assistant_responses_items(
    msg: &Map<String, Value>,
    content_parts: &[Value],
    content_text: &str,
    replay_encrypted_reasoning: bool,
    current_issuer_kind: Option<&str>,
    items: &mut Vec<Value>,
    seen_item_ids: &mut HashSet<String>,
) {
    let codex_reasoning = if replay_encrypted_reasoning {
        msg.get("codex_reasoning_items")
    } else {
        None
    };
    let mut has_codex_reasoning = false;

    if let Some(Value::Array(reasoning_list)) = codex_reasoning {
        for ri in reasoning_list {
            let Some(ri_obj) = ri.as_object() else {
                continue;
            };
            let encrypted = ri_obj
                .get("encrypted_content")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty());
            let Some(encrypted) = encrypted else {
                continue;
            };
            let item_id = ri_obj.get("id").and_then(|v| v.as_str());
            if let Some(id) = item_id {
                if seen_item_ids.contains(id) {
                    continue;
                }
            }
            let item_issuer = ri_obj.get("_issuer_kind").and_then(|v| v.as_str());
            if let (Some(current), Some(stamped)) = (current_issuer_kind, item_issuer) {
                if stamped != current {
                    if !CROSS_ISSUER_WARN_EMITTED.swap(true, Ordering::Relaxed) {
                        tracing::warn!(
                            stamped_issuer = stamped,
                            current_issuer = current,
                            "Dropping reasoning item minted by different Responses issuer"
                        );
                    }
                    continue;
                }
            }
            let mut replay_item = Map::new();
            for (k, v) in ri_obj {
                if k == "id" || k == "_issuer_kind" {
                    continue;
                }
                replay_item.insert(k.clone(), v.clone());
            }
            replay_item.insert("type".to_string(), json!("reasoning"));
            replay_item.insert("encrypted_content".to_string(), json!(encrypted));
            items.push(Value::Object(replay_item));
            if let Some(id) = item_id {
                seen_item_ids.insert(id.to_string());
            }
            has_codex_reasoning = true;
        }
    }

    let mut replayed_message_items = 0usize;
    if let Some(Value::Array(message_items)) = msg.get("codex_message_items") {
        for raw_item in message_items {
            let Some(raw_obj) = raw_item.as_object() else {
                continue;
            };
            if raw_obj.get("type").and_then(|v| v.as_str()) != Some("message") {
                continue;
            }
            if raw_obj.get("role").and_then(|v| v.as_str()) != Some("assistant") {
                continue;
            }
            let Some(Value::Array(raw_content)) = raw_obj.get("content") else {
                continue;
            };
            let mut normalized_content_parts = Vec::new();
            for part in raw_content {
                let Some(part_obj) = part.as_object() else {
                    continue;
                };
                let part_type = part_obj
                    .get("type")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if part_type != "output_text" && part_type != "text" {
                    continue;
                }
                let text = part_obj
                    .get("text")
                    .map(value_to_string)
                    .unwrap_or_default();
                normalized_content_parts.push(json!({"type": "output_text", "text": text}));
            }
            if normalized_content_parts.is_empty() {
                continue;
            }
            let mut replay_item = json!({
                "type": "message",
                "role": "assistant",
                "status": normalize_responses_message_status(raw_obj.get("status"), "completed"),
                "content": normalized_content_parts,
            });
            if let Some(id) = raw_obj.get("id").and_then(|v| v.as_str()) {
                if !id.trim().is_empty() {
                    replay_item["id"] = json!(id.trim());
                }
            }
            if let Some(phase) = raw_obj.get("phase").and_then(|v| v.as_str()) {
                if !phase.trim().is_empty() {
                    replay_item["phase"] = json!(phase.trim());
                }
            }
            items.push(replay_item);
            replayed_message_items += 1;
        }
    }

    if replayed_message_items == 0 {
        if !content_parts.is_empty() {
            items.push(json!({"role": "assistant", "content": content_parts}));
        } else if !content_text.trim().is_empty() {
            items.push(json!({"role": "assistant", "content": content_text}));
        } else if has_codex_reasoning {
            items.push(json!({"role": "assistant", "content": ""}));
        }
    }

    if let Some(Value::Array(tool_calls)) = msg.get("tool_calls") {
        for tc in tool_calls {
            append_function_call_item(tc, items);
        }
    }
}

fn append_function_call_item(tc: &Value, items: &mut Vec<Value>) {
    let Some(tc_obj) = tc.as_object() else {
        return;
    };
    let fn_obj = match tc_obj.get("function").and_then(|v| v.as_object()) {
        Some(f) => f,
        None => return,
    };
    let fn_name = fn_obj
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let Some(fn_name) = fn_name else {
        return;
    };

    let (embedded_call_id, embedded_response_item_id) =
        split_responses_tool_id(tc_obj.get("id").and_then(|v| v.as_str()));
    let mut call_id = tc_obj
        .get("call_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or(embedded_call_id);
    if call_id.is_none() {
        if let Some(ref fc_id) = embedded_response_item_id {
            if fc_id.starts_with("fc_") && fc_id.len() > "fc_".len() {
                call_id = Some(format!("call_{}", &fc_id["fc_".len()..]));
            }
        }
    }
    if call_id.is_none() {
        let raw_args = fn_obj
            .get("arguments")
            .map(|v| {
                if v.is_string() {
                    v.as_str().unwrap_or("{}").to_string()
                } else {
                    v.to_string()
                }
            })
            .unwrap_or_else(|| "{}".to_string());
        call_id = Some(deterministic_call_id(fn_name, &raw_args, items.len()));
    }
    let call_id = call_id.unwrap();

    let arguments = normalize_arguments_value(fn_obj.get("arguments"));

    items.push(json!({
        "type": "function_call",
        "call_id": call_id,
        "name": fn_name,
        "arguments": arguments,
    }));
}

fn append_tool_responses_item(msg: &Map<String, Value>, items: &mut Vec<Value>) {
    let raw_tool_call_id = msg
        .get("tool_call_id")
        .and_then(|v| v.as_str());
    let (mut call_id, _) = split_responses_tool_id(raw_tool_call_id);
    if call_id.is_none() {
        if let Some(raw) = raw_tool_call_id.map(str::trim).filter(|s| !s.is_empty()) {
            call_id = Some(raw.to_string());
        }
    }
    let Some(call_id) = call_id else {
        return;
    };

    let tool_content = msg.get("content");
    let output_value = if let Some(Value::Array(list)) = tool_content {
        let converted = chat_content_to_responses_parts(&Value::Array(list.clone()), "user");
        if converted.is_empty() {
            Value::String(String::new())
        } else {
            Value::Array(converted)
        }
    } else {
        Value::String(value_to_string(tool_content.unwrap_or(&Value::Null)))
    };

    items.push(json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": output_value,
    }));
}

fn normalize_arguments_value(arguments: Option<&Value>) -> String {
    let default_args = json!("{}");
    let arguments = arguments.unwrap_or(&default_args);
    let s = match arguments {
        Value::String(s) => s.clone(),
        Value::Object(_) | Value::Array(_) => {
            serde_json::to_string(arguments).unwrap_or_else(|_| arguments.to_string())
        }
        _ => arguments.to_string(),
    };
    let trimmed = s.trim();
    if trimmed.is_empty() {
        "{}".to_string()
    } else {
        trimmed.to_string()
    }
}

pub fn preflight_codex_input_items(raw_items: &Value) -> Result<Vec<Value>, CodexAdapterError> {
    let Value::Array(raw_list) = raw_items else {
        return Err(CodexAdapterError::value_error(
            "Codex Responses input must be a list of input items.",
        ));
    };

    let mut normalized = Vec::new();
    let mut seen_ids: HashSet<String> = HashSet::new();

    for (idx, item) in raw_list.iter().enumerate() {
        let Some(obj) = item.as_object() else {
            return Err(CodexAdapterError::value_error(format!(
                "Codex Responses input[{idx}] must be an object."
            )));
        };

        let item_type = obj.get("type").and_then(|v| v.as_str());

        if item_type == Some("function_call") {
            normalized.push(normalize_function_call_item(obj, idx)?);
            continue;
        }

        if item_type == Some("function_call_output") {
            normalized.push(normalize_function_call_output_item(obj, idx)?);
            continue;
        }

        if item_type == Some("reasoning") {
            if let Some(reasoning_item) = normalize_reasoning_item(obj, &mut seen_ids) {
                normalized.push(reasoning_item);
            }
            continue;
        }

        if item_type == Some("message") {
            normalized.push(normalize_message_item(obj, idx)?);
            continue;
        }

        let role = obj.get("role").and_then(|v| v.as_str());
        if role == Some("user") || role == Some("assistant") {
            normalized.push(normalize_role_content_item(obj, idx, role.unwrap())?);
            continue;
        }

        return Err(CodexAdapterError::value_error(format!(
            "Codex Responses input[{idx}] has unsupported item shape (type={item_type:?}, role={role:?})."
        )));
    }

    Ok(normalized)
}

fn normalize_function_call_item(
    obj: &Map<String, Value>,
    idx: usize,
) -> Result<Value, CodexAdapterError> {
    let call_id = obj
        .get("call_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let call_id = call_id.ok_or_else(|| {
        CodexAdapterError::value_error(format!(
            "Codex Responses input[{idx}] function_call is missing call_id."
        ))
    })?;
    let name = obj
        .get("name")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let name = name.ok_or_else(|| {
        CodexAdapterError::value_error(format!(
            "Codex Responses input[{idx}] function_call is missing name."
        ))
    })?;
    let arguments = normalize_arguments_value(obj.get("arguments"));
    Ok(json!({
        "type": "function_call",
        "call_id": call_id,
        "name": name,
        "arguments": arguments,
    }))
}

fn normalize_function_call_output_item(
    obj: &Map<String, Value>,
    idx: usize,
) -> Result<Value, CodexAdapterError> {
    let call_id = obj
        .get("call_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let call_id = call_id.ok_or_else(|| {
        CodexAdapterError::value_error(format!(
            "Codex Responses input[{idx}] function_call_output is missing call_id."
        ))
    })?;
    let output = obj.get("output").cloned().unwrap_or(Value::String(String::new()));
    let output = if output.is_null() {
        Value::String(String::new())
    } else {
        output
    };

    if let Value::Array(parts) = output {
        let mut cleaned = Vec::new();
        for part in parts {
            let Some(part_obj) = part.as_object() else {
                continue;
            };
            let ptype = part_obj.get("type").and_then(|v| v.as_str());
            if ptype == Some("input_text") {
                if let Some(text) = part_obj.get("text").and_then(|v| v.as_str()) {
                    if !text.is_empty() {
                        cleaned.push(json!({"type": "input_text", "text": text}));
                    }
                }
            } else if ptype == Some("input_image") {
                if let Some(url) = part_obj.get("image_url").and_then(|v| v.as_str()) {
                    if !url.is_empty() {
                        let mut entry = json!({"type": "input_image", "image_url": url});
                        if let Some(detail) = part_obj.get("detail").and_then(|v| v.as_str()) {
                            if !detail.trim().is_empty() {
                                entry["detail"] = json!(detail.trim());
                            }
                        }
                        cleaned.push(entry);
                    }
                }
            }
        }
        let out = if cleaned.is_empty() {
            Value::String(String::new())
        } else {
            Value::Array(cleaned)
        };
        return Ok(json!({
            "type": "function_call_output",
            "call_id": call_id,
            "output": out,
        }));
    }

    let out_str = if let Some(s) = output.as_str() {
        s.to_string()
    } else {
        output.to_string()
    };
    Ok(json!({
        "type": "function_call_output",
        "call_id": call_id,
        "output": out_str,
    }))
}

fn normalize_reasoning_item(
    obj: &Map<String, Value>,
    seen_ids: &mut HashSet<String>,
) -> Option<Value> {
    let encrypted = obj
        .get("encrypted_content")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())?;
    if let Some(id) = obj.get("id").and_then(|v| v.as_str()) {
        if !id.is_empty() {
            if seen_ids.contains(id) {
                return None;
            }
            seen_ids.insert(id.to_string());
        }
    }
    let summary = obj
        .get("summary")
        .filter(|v| v.is_array())
        .cloned()
        .unwrap_or_else(|| json!([]));
    Some(json!({
        "type": "reasoning",
        "encrypted_content": encrypted,
        "summary": summary,
    }))
}

fn normalize_message_item(
    obj: &Map<String, Value>,
    idx: usize,
) -> Result<Value, CodexAdapterError> {
    if obj.get("role").and_then(|v| v.as_str()) != Some("assistant") {
        return Err(CodexAdapterError::value_error(format!(
            "Codex Responses input[{idx}] message items must have role='assistant'."
        )));
    }
    let Some(Value::Array(content)) = obj.get("content") else {
        return Err(CodexAdapterError::value_error(format!(
            "Codex Responses input[{idx}] message item must have content list."
        )));
    };
    let mut normalized_content = Vec::new();
    for (part_idx, part) in content.iter().enumerate() {
        let Some(part_obj) = part.as_object() else {
            return Err(CodexAdapterError::value_error(format!(
                "Codex Responses input[{idx}] message content[{part_idx}] must be an object."
            )));
        };
        let part_type = part_obj.get("type").and_then(|v| v.as_str());
        if part_type != Some("output_text") && part_type != Some("text") {
            return Err(CodexAdapterError::value_error(format!(
                "Codex Responses input[{idx}] message content[{part_idx}] has unsupported type {part_type:?}."
            )));
        }
        let text = part_obj
            .get("text")
            .map(value_to_string)
            .unwrap_or_default();
        normalized_content.push(json!({"type": "output_text", "text": text}));
    }
    if normalized_content.is_empty() {
        return Err(CodexAdapterError::value_error(format!(
            "Codex Responses input[{idx}] message item must contain at least one text part."
        )));
    }
    let mut normalized_item = json!({
        "type": "message",
        "role": "assistant",
        "status": normalize_responses_message_status(obj.get("status"), "completed"),
        "content": normalized_content,
    });
    if let Some(id) = obj.get("id").and_then(|v| v.as_str()) {
        if !id.trim().is_empty() {
            normalized_item["id"] = json!(id.trim());
        }
    }
    if let Some(phase) = obj.get("phase").and_then(|v| v.as_str()) {
        if !phase.trim().is_empty() {
            normalized_item["phase"] = json!(phase.trim());
        }
    }
    Ok(normalized_item)
}

fn normalize_role_content_item(
    obj: &Map<String, Value>,
    idx: usize,
    role: &str,
) -> Result<Value, CodexAdapterError> {
    let content = obj.get("content").cloned().unwrap_or(Value::String(String::new()));
    let content = if content.is_null() {
        Value::String(String::new())
    } else {
        content
    };

    if let Value::Array(parts) = content {
        let text_type = if role == "assistant" {
            "output_text"
        } else {
            "input_text"
        };
        let mut validated = Vec::new();
        for (part_idx, part) in parts.iter().enumerate() {
            if let Some(s) = part.as_str() {
                if !s.is_empty() {
                    validated.push(json!({"type": text_type, "text": s}));
                }
                continue;
            }
            let Some(part_obj) = part.as_object() else {
                return Err(CodexAdapterError::value_error(format!(
                    "Codex Responses input[{idx}].content[{part_idx}] must be an object or string."
                )));
            };
            let ptype = part_obj
                .get("type")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            if matches!(ptype.as_str(), "input_text" | "text" | "output_text") {
                let text = part_obj
                    .get("text")
                    .map(value_to_string)
                    .unwrap_or_default();
                validated.push(json!({"type": text_type, "text": text}));
            } else if matches!(ptype.as_str(), "input_image" | "image_url") {
                let mut detail = part_obj.get("detail").and_then(|v| v.as_str()).map(str::to_string);
                let url = match part_obj.get("image_url") {
                    Some(Value::Object(ir)) => {
                        detail = ir
                            .get("detail")
                            .and_then(|v| v.as_str())
                            .map(str::to_string)
                            .or(detail);
                        ir.get("url").map(value_to_string)
                    }
                    Some(other) => Some(value_to_string(other)),
                    None => None,
                }
                .unwrap_or_default();
                let mut image_part = json!({"type": "input_image", "image_url": url});
                if let Some(d) = detail.as_deref() {
                    if !d.trim().is_empty() {
                        image_part["detail"] = json!(d.trim());
                    }
                }
                validated.push(image_part);
            } else {
                return Err(CodexAdapterError::value_error(format!(
                    "Codex Responses input[{idx}].content[{part_idx}] has unsupported type {:?}.",
                    part_obj.get("type")
                )));
            }
        }
        return Ok(json!({"role": role, "content": validated}));
    }

    let content_str = if let Some(s) = content.as_str() {
        s.to_string()
    } else {
        content.to_string()
    };
    Ok(json!({"role": role, "content": content_str}))
}

pub fn preflight_codex_api_kwargs(
    api_kwargs: &Value,
    allow_stream: bool,
) -> Result<Value, CodexAdapterError> {
    let Some(obj) = api_kwargs.as_object() else {
        return Err(CodexAdapterError::value_error(
            "Codex Responses request must be a dict.",
        ));
    };

    let missing: Vec<&str> = ["model", "instructions", "input"]
        .iter()
        .copied()
        .filter(|k| !obj.contains_key(*k))
        .collect();
    if !missing.is_empty() {
        return Err(CodexAdapterError::value_error(format!(
            "Codex Responses request missing required field(s): {}.",
            missing.join(", ")
        )));
    }

    let model = obj
        .get("model")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| {
            CodexAdapterError::value_error(
                "Codex Responses request 'model' must be a non-empty string.",
            )
        })?;

    let instructions = match obj.get("instructions") {
        None | Some(Value::Null) => String::new(),
        Some(v) => value_to_string(v),
    };
    let instructions = instructions.trim();
    let instructions = if instructions.is_empty() {
        DEFAULT_AGENT_IDENTITY.to_string()
    } else {
        instructions.to_string()
    };

    let normalized_input =
        preflight_codex_input_items(obj.get("input").unwrap_or(&Value::Null))?;

    let mut normalized_tools: Option<Vec<Value>> = None;
    if let Some(tools) = obj.get("tools").filter(|v| !v.is_null()) {
        let Value::Array(tool_list) = tools else {
            return Err(CodexAdapterError::value_error(
                "Codex Responses request 'tools' must be a list when provided.",
            ));
        };
        let mut out = Vec::new();
        for (idx, tool) in tool_list.iter().enumerate() {
            let Some(tool_obj) = tool.as_object() else {
                return Err(CodexAdapterError::value_error(format!(
                    "Codex Responses tools[{idx}] must be an object."
                )));
            };
            if tool_obj.get("type").and_then(|v| v.as_str()) != Some("function") {
                return Err(CodexAdapterError::value_error(format!(
                    "Codex Responses tools[{idx}] has unsupported type {:?}.",
                    tool_obj.get("type")
                )));
            }
            let name = tool_obj
                .get("name")
                .and_then(|v| v.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let name = name.ok_or_else(|| {
                CodexAdapterError::value_error(format!(
                    "Codex Responses tools[{idx}] is missing a valid name."
                ))
            })?;
            let parameters = tool_obj.get("parameters");
            if !parameters.map(|p| p.is_object()).unwrap_or(false) {
                return Err(CodexAdapterError::value_error(format!(
                    "Codex Responses tools[{idx}] is missing valid parameters."
                )));
            }
            let description = tool_obj
                .get("description")
                .map(value_to_string)
                .unwrap_or_default();
            let strict = tool_obj
                .get("strict")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            out.push(json!({
                "type": "function",
                "name": name,
                "description": description,
                "strict": strict,
                "parameters": parameters,
            }));
        }
        normalized_tools = Some(out);
    }

    if obj.get("store").and_then(|v| v.as_bool()) != Some(false) {
        return Err(CodexAdapterError::value_error(
            "Codex Responses contract requires 'store' to be false.",
        ));
    }

    let mut allowed_keys: HashSet<&str> = [
        "model",
        "instructions",
        "input",
        "tools",
        "store",
        "reasoning",
        "include",
        "max_output_tokens",
        "temperature",
        "tool_choice",
        "parallel_tool_calls",
        "prompt_cache_key",
        "service_tier",
        "extra_headers",
        "extra_body",
        "timeout",
    ]
    .into_iter()
    .collect();

    let mut normalized = json!({
        "model": model,
        "instructions": instructions,
        "input": normalized_input,
        "store": false,
    });
    if let Some(tools) = normalized_tools {
        normalized["tools"] = json!(tools);
    }

    if let Some(reasoning) = obj.get("reasoning") {
        if reasoning.is_object() {
            normalized["reasoning"] = reasoning.clone();
        }
    }
    if let Some(Value::Array(include)) = obj.get("include") {
        normalized["include"] = json!(include);
    }
    if let Some(tier) = obj.get("service_tier").and_then(|v| v.as_str()) {
        if !tier.trim().is_empty() {
            normalized["service_tier"] = json!(tier.trim());
        }
    }
    if let Some(max_out) = obj.get("max_output_tokens").and_then(value_as_f64) {
        if max_out > 0.0 {
            normalized["max_output_tokens"] = json!(max_out as i64);
        }
    }
    if let Some(timeout) = obj.get("timeout").and_then(value_as_f64) {
        if timeout > 0.0 && timeout.is_finite() {
            normalized["timeout"] = json!(timeout);
        }
    }
    if let Some(temp) = obj.get("temperature").and_then(value_as_f64) {
        normalized["temperature"] = json!(temp);
    }
    for key in ["tool_choice", "parallel_tool_calls", "prompt_cache_key"] {
        if let Some(val) = obj.get(key) {
            if !val.is_null() {
                normalized[key] = val.clone();
            }
        }
    }

    if let Some(extra_headers) = obj.get("extra_headers") {
        let Some(headers_obj) = extra_headers.as_object() else {
            return Err(CodexAdapterError::value_error(
                "Codex Responses request 'extra_headers' must be an object.",
            ));
        };
        let mut normalized_headers = Map::new();
        for (key, value) in headers_obj {
            if key.trim().is_empty() {
                return Err(CodexAdapterError::value_error(
                    "Codex Responses request 'extra_headers' keys must be non-empty strings.",
                ));
            }
            if value.is_null() {
                continue;
            }
            normalized_headers.insert(key.trim().to_string(), json!(value_to_string(value)));
        }
        if !normalized_headers.is_empty() {
            normalized["extra_headers"] = Value::Object(normalized_headers);
        }
    }

    if let Some(extra_body) = obj.get("extra_body") {
        let Some(body_obj) = extra_body.as_object() else {
            return Err(CodexAdapterError::value_error(
                "Codex Responses request 'extra_body' must be an object.",
            ));
        };
        if !body_obj.is_empty() {
            normalized["extra_body"] = extra_body.clone();
        }
    }

    if allow_stream {
        if let Some(stream) = obj.get("stream") {
            if stream.as_bool() != Some(true) {
                return Err(CodexAdapterError::value_error(
                    "Codex Responses 'stream' must be true when set.",
                ));
            }
            if stream.as_bool() == Some(true) {
                normalized["stream"] = json!(true);
            }
        }
        allowed_keys.insert("stream");
    } else if obj.contains_key("stream") {
        return Err(CodexAdapterError::value_error(
            "Codex Responses stream flag is only allowed in fallback streaming requests.",
        ));
    }

    let model_lower = obj
        .get("model")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let is_xai_model = model_lower.starts_with("grok-") || model_lower.starts_with("x-ai/grok-");
    if is_xai_model {
        if let Some(Value::Array(tools)) = normalized.get_mut("tools") {
            let _ = strip_slash_enum(tools);
        }
    }

    let unexpected: Vec<_> = obj
        .keys()
        .filter(|k| !allowed_keys.contains(k.as_str()))
        .map(|k| k.as_str())
        .collect();
    if !unexpected.is_empty() {
        let mut sorted = unexpected;
        sorted.sort_unstable();
        return Err(CodexAdapterError::value_error(format!(
            "Codex Responses request has unsupported field(s): {}.",
            sorted.join(", ")
        )));
    }

    Ok(normalized)
}

pub fn extract_responses_message_text(item: &Value) -> String {
    let Some(Value::Array(content)) = item.get("content") else {
        return String::new();
    };
    let mut chunks = Vec::new();
    for part in content {
        let ptype = part.get("type").and_then(|v| v.as_str());
        if ptype != Some("output_text") && ptype != Some("text") {
            continue;
        }
        if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
            if !text.is_empty() {
                chunks.push(text);
            }
        }
    }
    chunks.join("").trim().to_string()
}

pub fn extract_responses_reasoning_text(item: &Value) -> String {
    if let Some(Value::Array(summary)) = item.get("summary") {
        let mut chunks = Vec::new();
        for part in summary {
            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                if !text.is_empty() {
                    chunks.push(text);
                }
            }
        }
        if !chunks.is_empty() {
            return chunks.join("\n").trim().to_string();
        }
    }
    if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
        if !text.is_empty() {
            return text.trim().to_string();
        }
    }
    String::new()
}

pub fn format_responses_error(error_obj: Option<&Value>, response_status: &str) -> String {
    let (code, message) = match error_obj {
        Some(Value::Object(obj)) => (
            obj.get("code").cloned(),
            obj.get("message").cloned(),
        ),
        Some(other) => (other.get("code").cloned(), other.get("message").cloned()),
        None => (None, None),
    };

    let code_str = error_field_to_string(code.as_ref());
    let message_str = error_field_to_string(message.as_ref());

    if !code_str.is_empty() && !message_str.is_empty() {
        return format!("{code_str}: {message_str}");
    }
    if !message_str.is_empty() {
        return message_str;
    }
    if !code_str.is_empty() {
        return code_str;
    }
    if let Some(obj) = error_obj {
        return obj.to_string();
    }
    format!("Responses API returned status '{response_status}'")
}

fn error_field_to_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(v) if !v.is_null() => value_to_string(v).trim().to_string(),
        _ => String::new(),
    }
}

pub fn normalize_codex_response(
    response: &Value,
    issuer_kind: Option<&str>,
) -> Result<(CodexAssistantMessage, String), CodexAdapterError> {
    let mut output = response.get("output").cloned();
    let output_empty = output
        .as_ref()
        .and_then(|v| v.as_array())
        .map(|a| a.is_empty())
        .unwrap_or(true);

    if output_empty {
        if let Some(out_text) = response.get("output_text").and_then(|v| v.as_str()) {
            if !out_text.trim().is_empty() {
                tracing::debug!(
                    chars = out_text.trim().len(),
                    "Codex response has empty output but output_text is present; synthesizing output item"
                );
                output = Some(json!([{
                    "type": "message",
                    "role": "assistant",
                    "status": "completed",
                    "content": [{"type": "output_text", "text": out_text.trim()}],
                }]));
            }
        }
    }

    let output_list = output
        .as_ref()
        .and_then(|v| v.as_array())
        .filter(|a| !a.is_empty())
        .ok_or_else(|| {
            CodexAdapterError::runtime_error("Responses API returned no output items")
        })?;

    let response_status = response
        .get("status")
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_ascii_lowercase());

    if matches!(response_status.as_deref(), Some("failed") | Some("cancelled")) {
        let status = response_status.as_deref().unwrap_or("failed");
        let error_obj = response.get("error");
        return Err(CodexAdapterError::runtime_error(format_responses_error(
            error_obj,
            status,
        )));
    }

    let mut content_parts: Vec<String> = Vec::new();
    let mut reasoning_parts: Vec<String> = Vec::new();
    let mut reasoning_items_raw: Vec<Value> = Vec::new();
    let mut message_items_raw: Vec<Value> = Vec::new();
    let mut tool_calls: Vec<CodexToolCall> = Vec::new();
    let mut has_incomplete_items = matches!(
        response_status.as_deref(),
        Some("queued") | Some("in_progress") | Some("incomplete")
    );
    let mut saw_commentary_phase = false;
    let mut saw_final_answer_phase = false;
    let mut saw_reasoning_item = false;

    for item in output_list {
        let item_type = item.get("type").and_then(|v| v.as_str());
        let item_status = item
            .get("status")
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_ascii_lowercase());
        if matches!(
            item_status.as_deref(),
            Some("queued") | Some("in_progress") | Some("incomplete")
        ) {
            has_incomplete_items = true;
        }

        match item_type {
            Some("message") => {
                let normalized_phase = item.get("phase").and_then(|v| v.as_str()).map(|p| {
                    let p = p.trim().to_ascii_lowercase();
                    if matches!(p.as_str(), "commentary" | "analysis") {
                        saw_commentary_phase = true;
                    } else if matches!(p.as_str(), "final_answer" | "final") {
                        saw_final_answer_phase = true;
                    }
                    p
                });
                let message_text = extract_responses_message_text(item);
                if !message_text.is_empty() {
                    content_parts.push(message_text.clone());
                    let mut raw_message_item = json!({
                        "type": "message",
                        "role": "assistant",
                        "status": normalize_responses_message_status(item.get("status"), "completed"),
                        "content": [{"type": "output_text", "text": message_text}],
                    });
                    if let Some(id) = item.get("id").and_then(|v| v.as_str()) {
                        if !id.is_empty() {
                            raw_message_item["id"] = json!(id);
                        }
                    }
                    if let Some(phase) = normalized_phase.as_deref() {
                        if !phase.is_empty() {
                            raw_message_item["phase"] = json!(phase);
                        }
                    }
                    message_items_raw.push(raw_message_item);
                }
                let _ = normalized_phase;
            }
            Some("reasoning") => {
                saw_reasoning_item = true;
                let reasoning_text = extract_responses_reasoning_text(item);
                if !reasoning_text.is_empty() {
                    reasoning_parts.push(reasoning_text);
                }
                let encrypted = item
                    .get("encrypted_content")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty());
                if let Some(encrypted) = encrypted {
                    let mut raw_item = json!({
                        "type": "reasoning",
                        "encrypted_content": encrypted,
                    });
                    if let Some(kind) = issuer_kind {
                        raw_item["_issuer_kind"] = json!(kind);
                    }
                    let item_id = item.get("id").and_then(|v| v.as_str());
                    if item_id.is_some_and(|id| id.starts_with("rs_tmp_")) {
                        tracing::debug!(
                            id = item_id.unwrap_or(""),
                            "Skipping transient Codex reasoning item during normalization"
                        );
                        continue;
                    }
                    if let Some(id) = item_id {
                        if !id.is_empty() {
                            raw_item["id"] = json!(id);
                        }
                    }
                    if let Some(Value::Array(summary)) = item.get("summary") {
                        let mut raw_summary = Vec::new();
                        for part in summary {
                            if let Some(text) = part.get("text").and_then(|v| v.as_str()) {
                                raw_summary.push(json!({"type": "summary_text", "text": text}));
                            }
                        }
                        raw_item["summary"] = json!(raw_summary);
                    }
                    reasoning_items_raw.push(raw_item);
                }
            }
            Some("function_call") => {
                if matches!(
                    item_status.as_deref(),
                    Some("queued") | Some("in_progress") | Some("incomplete")
                ) {
                    continue;
                }
                append_normalized_tool_call(item, &mut tool_calls, false);
            }
            Some("custom_tool_call") => {
                append_normalized_tool_call(item, &mut tool_calls, true);
            }
            _ => {}
        }
    }

    let mut final_text: String = content_parts
        .into_iter()
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string();
    if final_text.is_empty() {
        if let Some(out_text) = response.get("output_text").and_then(|v| v.as_str()) {
            final_text = out_text.trim().to_string();
        }
    }

    let mut leaked_tool_call_text = false;
    if !final_text.is_empty()
        && tool_calls.is_empty()
        && tool_call_leak_pattern().is_match(&final_text)
    {
        leaked_tool_call_text = true;
        tracing::warn!(
            snippet = %final_text.chars().take(300).collect::<String>(),
            "Codex response contains leaked tool-call text; treating as incomplete"
        );
        final_text.clear();
    }

    let finish_reason = if !tool_calls.is_empty() {
        "tool_calls".to_string()
    } else if leaked_tool_call_text {
        "incomplete".to_string()
    } else if has_incomplete_items || (saw_commentary_phase && !saw_final_answer_phase) {
        "incomplete".to_string()
    } else if (!reasoning_items_raw.is_empty() || !reasoning_parts.is_empty() || saw_reasoning_item)
        && final_text.is_empty()
    {
        "incomplete".to_string()
    } else {
        "stop".to_string()
    };

    let assistant_message = CodexAssistantMessage {
        content: final_text,
        tool_calls,
        reasoning: if reasoning_parts.is_empty() {
            None
        } else {
            Some(reasoning_parts.join("\n\n").trim().to_string())
        },
        reasoning_content: None,
        reasoning_details: None,
        codex_reasoning_items: if reasoning_items_raw.is_empty() {
            None
        } else {
            Some(reasoning_items_raw)
        },
        codex_message_items: if message_items_raw.is_empty() {
            None
        } else {
            Some(message_items_raw)
        },
    };

    Ok((assistant_message, finish_reason))
}

fn append_normalized_tool_call(
    item: &Value,
    tool_calls: &mut Vec<CodexToolCall>,
    is_custom: bool,
) {
    let fn_name = item
        .get("name")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let arguments = if is_custom {
        item.get("input")
    } else {
        item.get("arguments")
    };
    let arguments = match arguments {
        Some(Value::String(s)) => s.clone(),
        Some(v) if v.is_object() || v.is_array() => {
            serde_json::to_string(v).unwrap_or_else(|_| v.to_string())
        }
        Some(v) => v.to_string(),
        None => "{}".to_string(),
    };

    let (embedded_call_id, _) =
        split_responses_tool_id(item.get("id").and_then(|v| v.as_str()));
    let mut call_id = item
        .get("call_id")
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or(embedded_call_id);
    if call_id.is_none() {
        call_id = Some(deterministic_call_id(&fn_name, &arguments, tool_calls.len()));
    }
    let call_id = call_id.unwrap();
    let raw_item_id = item.get("id").and_then(|v| v.as_str());
    let response_item_id = derive_responses_function_call_id(&call_id, raw_item_id);

    tool_calls.push(CodexToolCall {
        id: call_id.clone(),
        call_id: call_id.clone(),
        response_item_id,
        type_: "function".to_string(),
        function: CodexFunctionCall {
            name: fn_name,
            arguments,
        },
    });
}

fn tool_call_leak_pattern() -> &'static Regex {
    TOOL_CALL_LEAK_PATTERN.get_or_init(|| {
        Regex::new(r"(?:^|[\s>|])to=functions\.[A-Za-z_][\w.]*")
            .expect("valid TOOL_CALL_LEAK_PATTERN")
    })
}

fn value_to_string(value: &Value) -> String {
    match value {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn value_as_f64(value: &Value) -> Option<f64> {
    if value.is_boolean() {
        return None;
    }
    value.as_f64().or_else(|| value.as_i64().map(|i| i as f64))
}

#[cfg(test)]
mod tests;
