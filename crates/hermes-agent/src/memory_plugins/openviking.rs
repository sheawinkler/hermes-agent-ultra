//! OpenViking memory provider — REST context database.
//!
//! Mirrors Python `plugins/memory/openviking/__init__.py` (HTTP subset).

use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use reqwest::blocking::multipart::{Form, Part};
use reqwest::blocking::Client;
use serde_json::{json, Value};

use crate::memory_manager::MemoryProviderPlugin;
use crate::memory_plugins::config_io;

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:1933";
const DEFAULT_AGENT: &str = "hermes";
const DEFAULT_MEMORY_SUBDIR: &str = "preferences";
const DEFAULT_SESSION_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
const REMOTE_RESOURCE_PREFIXES: &[&str] = &["http://", "https://", "git@", "ssh://", "git://"];
const SYNC_TRACE_ENV: &str = "HERMES_OPENVIKING_SYNC_TRACE";
const VIKING_SEARCH_TOOL: &str = "viking_search";
const VIKING_READ_TOOL: &str = "viking_read";
const VIKING_BROWSE_TOOL: &str = "viking_browse";
const VIKING_REMEMBER_TOOL: &str = "viking_remember";
const VIKING_FORGET_TOOL: &str = "viking_forget";
const VIKING_ADD_RESOURCE_TOOL: &str = "viking_add_resource";
const TOOL_STATUS_COMPLETED: &str = "completed";
const TOOL_STATUS_ERROR: &str = "error";
const TOOL_STATUS_PENDING: &str = "pending";
const TOOL_STATUS_ERROR_ALIASES: &[&str] = &["error", "failed", "failure"];
const TOOL_STATUS_COMPLETED_ALIASES: &[&str] = &["completed", "complete", "success", "succeeded"];
const GENERATED_MEMORY_SUMMARY_FILENAMES: &[&str] = &[".abstract.md", ".overview.md"];

fn search_schema() -> Value {
    json!({
        "name": VIKING_SEARCH_TOOL,
        "description": "Semantic search over the OpenViking knowledge base.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "mode": {"type": "string", "description": "auto|fast|deep"},
                "scope": {"type": "string"},
                "limit": {"type": "integer"}
            },
            "required": ["query"]
        }
    })
}

fn read_schema() -> Value {
    json!({
        "name": VIKING_READ_TOOL,
        "description": "Read content at a viking:// URI (abstract|overview|full).",
        "parameters": {
            "type": "object",
            "properties": {
                "uri": {"type": "string"},
                "level": {"type": "string"}
            },
            "required": ["uri"]
        }
    })
}

fn browse_schema() -> Value {
    json!({
        "name": VIKING_BROWSE_TOOL,
        "description": "Browse OpenViking store (tree|list|stat).",
        "parameters": {
            "type": "object",
            "properties": {
                "action": {"type": "string"},
                "path": {"type": "string"}
            },
            "required": ["action"]
        }
    })
}

fn remember_schema() -> Value {
    json!({
        "name": VIKING_REMEMBER_TOOL,
        "description": "Store a fact directly in the OpenViking memory tree.",
        "parameters": {
            "type": "object",
            "properties": {
                "content": {"type": "string"},
                "category": {"type": "string"}
            },
            "required": ["content"]
        }
    })
}

fn forget_schema() -> Value {
    json!({
        "name": VIKING_FORGET_TOOL,
        "description": "Delete one OpenViking memory file by exact viking:// URI. Rejects resources, directories, summaries, broad deletes, and non-memory URIs.",
        "parameters": {
            "type": "object",
            "properties": {
                "uri": {"type": "string", "description": "Exact viking:// user memory file URI ending in .md."}
            },
            "required": ["uri"]
        }
    })
}

fn add_resource_schema() -> Value {
    json!({
        "name": VIKING_ADD_RESOURCE_TOOL,
        "description": "Add a remote URL or local file/directory to the knowledge base.",
        "parameters": {
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "reason": {"type": "string"},
                "to": {"type": "string"},
                "parent": {"type": "string"},
                "instruction": {"type": "string"},
                "wait": {"type": "boolean"},
                "timeout": {"type": "number"}
            },
            "required": ["url"]
        }
    })
}

#[derive(Debug, Clone)]
struct OpenVikingConfig {
    endpoint: String,
    api_key: String,
    api_key_type: String,
    account: String,
    user: String,
    agent: String,
}

impl OpenVikingConfig {
    fn config_path(hermes_home: &str) -> PathBuf {
        Path::new(hermes_home).join("openviking.json")
    }

    fn default_config_path() -> PathBuf {
        config_io::default_hermes_home().join("openviking.json")
    }

    fn configured_at(path: &Path) -> bool {
        let object = config_io::read_json_object(path);
        if object
            .get("enabled")
            .and_then(Value::as_bool)
            .is_some_and(|enabled| enabled)
        {
            return true;
        }
        ["endpoint", "api_key", "root_api_key"].iter().any(|key| {
            object
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
        })
    }

    fn load(hermes_home: &str) -> Self {
        let mut config = Self {
            endpoint: std::env::var("OPENVIKING_ENDPOINT")
                .unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string()),
            api_key: std::env::var("OPENVIKING_API_KEY").unwrap_or_default(),
            api_key_type: std::env::var("OPENVIKING_API_KEY_TYPE")
                .unwrap_or_else(|_| "user".to_string()),
            account: std::env::var("OPENVIKING_ACCOUNT").unwrap_or_else(|_| "default".into()),
            user: std::env::var("OPENVIKING_USER").unwrap_or_else(|_| "default".into()),
            agent: std::env::var("OPENVIKING_AGENT").unwrap_or_else(|_| DEFAULT_AGENT.into()),
        };

        let path = Self::config_path(hermes_home);
        let raw = config_io::read_json_object(&path);
        apply_openviking_config_map(&mut config, &raw);

        config.endpoint = normalize_openviking_endpoint(&config.endpoint);
        config.api_key_type = normalize_openviking_key_type(&config.api_key_type);
        config.account = nonempty_or(&config.account, "default");
        config.user = nonempty_or(&config.user, "default");
        config.agent = nonempty_or(&config.agent, DEFAULT_AGENT);
        config
    }
}

fn apply_openviking_config_map(
    config: &mut OpenVikingConfig,
    raw: &serde_json::Map<String, Value>,
) {
    if let Some(endpoint) = raw
        .get("endpoint")
        .or(raw.get("base_url"))
        .or(raw.get("baseUrl"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        config.endpoint = endpoint.to_string();
    }
    if let Some(api_key) = raw
        .get("api_key")
        .or(raw.get("apiKey"))
        .or(raw.get("root_api_key"))
        .or(raw.get("rootApiKey"))
        .and_then(Value::as_str)
    {
        config.api_key = api_key.to_string();
    }
    if let Some(key_type) = raw
        .get("api_key_type")
        .or(raw.get("apiKeyType"))
        .and_then(Value::as_str)
    {
        config.api_key_type = key_type.to_string();
    }
    if let Some(account) = raw.get("account").and_then(Value::as_str) {
        config.account = account.to_string();
    }
    if let Some(user) = raw.get("user").and_then(Value::as_str) {
        config.user = user.to_string();
    }
    if let Some(agent) = raw.get("agent").and_then(Value::as_str) {
        config.agent = agent.to_string();
    }
}

fn normalize_openviking_endpoint(raw: &str) -> String {
    let value = raw.trim();
    let with_scheme = if value.is_empty() {
        DEFAULT_ENDPOINT.to_string()
    } else if value.contains("://") {
        value.to_string()
    } else {
        format!("http://{value}")
    };
    with_scheme.trim_end_matches('/').to_string()
}

fn normalize_openviking_key_type(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "root" | "root_api_key" | "root-api-key" => "root".to_string(),
        "none" | "dev" | "local" | "no_api_key" | "no-api-key" => "none".to_string(),
        _ => "user".to_string(),
    }
}

fn nonempty_or(raw: &str, default: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}

#[derive(Clone)]
struct VikingState {
    client: Client,
    endpoint: String,
    api_key: String,
    account: String,
    user: String,
    agent: String,
    session_id: String,
    turn_count: u32,
}

pub struct OpenVikingMemoryPlugin {
    state: Mutex<Option<VikingState>>,
    prefetch: Arc<Mutex<String>>,
    inflight_writers: Arc<Mutex<HashMap<String, Vec<JoinHandle<()>>>>>,
}

fn viking_headers(st: &VikingState) -> reqwest::header::HeaderMap {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert("Content-Type", "application/json".parse().expect("mime"));
    append_viking_tenant_headers(&mut h, st);
    h
}

fn viking_multipart_headers(st: &VikingState) -> reqwest::header::HeaderMap {
    let mut h = reqwest::header::HeaderMap::new();
    append_viking_tenant_headers(&mut h, st);
    h
}

fn append_viking_tenant_headers(h: &mut reqwest::header::HeaderMap, st: &VikingState) {
    h.insert("X-OpenViking-Account", st.account.parse().expect("account"));
    h.insert("X-OpenViking-User", st.user.parse().expect("user"));
    h.insert("X-OpenViking-Agent", st.agent.parse().expect("agent"));
    if !st.api_key.is_empty() {
        h.insert("X-API-Key", st.api_key.parse().expect("key"));
        h.insert(
            "Authorization",
            format!("Bearer {}", st.api_key).parse().expect("bearer"),
        );
    }
}

fn viking_uri_segment(raw: &str) -> String {
    let sanitized = raw
        .trim()
        .chars()
        .map(|ch| match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '-' => ch,
            _ => '_',
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "default".to_string()
    } else {
        sanitized
    }
}

fn memory_subdir_for_category(category: &str) -> &'static str {
    match category.trim().to_ascii_lowercase().as_str() {
        "preference" | "preferences" => "preferences",
        "entity" | "entities" => "entities",
        "event" | "events" => "events",
        "case" | "cases" => "cases",
        "pattern" | "patterns" => "patterns",
        _ => DEFAULT_MEMORY_SUBDIR,
    }
}

fn memory_subdir_for_target(target: &str) -> &'static str {
    match target.trim().to_ascii_lowercase().as_str() {
        "memory" | "memories" => "patterns",
        "user" | "preferences" => "preferences",
        _ => DEFAULT_MEMORY_SUBDIR,
    }
}

fn memory_segment_index(parts: &[&str]) -> Option<usize> {
    if parts.len() >= 2 && parts[0] == "user" && parts[1] == "memories" {
        return Some(1);
    }
    if parts.len() >= 3 && parts[0] == "user" && parts[2] == "memories" {
        return Some(2);
    }
    if parts.len() >= 4 && parts[0] == "user" && parts[1] == "peers" && parts[3] == "memories" {
        return Some(3);
    }
    if parts.len() >= 5 && parts[0] == "user" && parts[2] == "peers" && parts[4] == "memories" {
        return Some(4);
    }
    None
}

fn validate_forget_memory_uri(raw_uri: Option<&str>) -> Result<String, String> {
    let uri = raw_uri.unwrap_or("").trim();
    if uri.is_empty() {
        return Err("uri is required".to_string());
    }
    if !uri.starts_with("viking://") {
        return Err("viking_forget only accepts viking:// memory file URIs".to_string());
    }
    if uri.contains('?') || uri.contains('#') {
        return Err("viking_forget requires an exact URI without query or fragment".to_string());
    }
    if uri.ends_with('/') || !uri.ends_with(".md") {
        return Err("viking_forget only deletes concrete .md memory files".to_string());
    }

    let parts = uri["viking://".len()..]
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let memories_idx = memory_segment_index(&parts)
        .ok_or_else(|| "viking_forget only deletes user memory file URIs".to_string())?;
    if parts.len() < memories_idx + 2 {
        return Err("viking_forget only deletes user memory file URIs".to_string());
    }

    let filename = uri.rsplit('/').next().unwrap_or("");
    if GENERATED_MEMORY_SUMMARY_FILENAMES.contains(&filename) {
        return Err("viking_forget cannot delete generated memory summary files".to_string());
    }

    Ok(uri.to_string())
}

fn build_memory_uri(user: &str, _agent: &str, subdir: &str) -> String {
    let slug = uuid::Uuid::new_v4().simple().to_string();
    format!(
        "viking://user/{}/memories/{}/mem_{}.md",
        viking_uri_segment(user),
        viking_uri_segment(subdir),
        &slug[..12]
    )
}

fn content_write_body(st: &VikingState, subdir: &str, content: &str) -> Value {
    json!({
        "uri": build_memory_uri(&st.user, &st.agent, subdir),
        "content": content,
        "mode": "create",
    })
}

fn openviking_sync_trace_enabled() -> bool {
    std::env::var(SYNC_TRACE_ENV)
        .ok()
        .map(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

fn preview_sync_value(value: impl AsRef<str>) -> String {
    let mut text = value.as_ref().replace('\n', "\\n");
    if text.len() > 160 {
        text.truncate(160);
        text.push_str("...");
    }
    text
}

fn is_openviking_recall_tool_name(tool_name: &str) -> bool {
    matches!(
        tool_name.trim().to_ascii_lowercase().as_str(),
        VIKING_SEARCH_TOOL | VIKING_READ_TOOL | VIKING_BROWSE_TOOL
    )
}

fn value_field<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_object().and_then(|object| object.get(key))
}

fn text_from_part(part: &Value) -> String {
    match part {
        Value::String(text) => text.clone(),
        Value::Object(_) => {
            let part_type = value_field(part, "type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase();
            if matches!(
                part_type.as_str(),
                "image" | "image_url" | "input_image" | "audio" | "input_audio"
            ) {
                return String::new();
            }
            if let Some(text) = [
                "text",
                "content",
                "input_text",
                "output_text",
                "summary_text",
            ]
            .iter()
            .find_map(|key| {
                value_field(part, key)
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            }) {
                text
            } else if part_type.is_empty() {
                part.to_string()
            } else {
                String::new()
            }
        }
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

fn message_text_from_content(content: Option<&Value>) -> String {
    match content {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .map(text_from_part)
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Some(Value::Object(_)) => text_from_part(content.expect("object content present")),
        Some(Value::Null) | None => String::new(),
        Some(other) => other.to_string(),
    }
}

fn message_text(message: &Value) -> String {
    message_text_from_content(value_field(message, "content"))
}

fn message_matches_text(message: &Value, expected: &str) -> bool {
    !expected.trim().is_empty() && message_text(message).trim() == expected.trim()
}

fn extract_current_turn_messages(
    messages: &[Value],
    user_content: &str,
    assistant_content: &str,
) -> Vec<Value> {
    if messages.is_empty() {
        return Vec::new();
    }

    let mut end_idx = None;
    if !assistant_content.trim().is_empty() {
        for (idx, message) in messages.iter().enumerate().rev() {
            if message.get("role").and_then(Value::as_str) == Some("assistant")
                && message_matches_text(message, assistant_content)
            {
                end_idx = Some(idx);
                break;
            }
        }
    }
    if end_idx.is_none() {
        for (idx, message) in messages.iter().enumerate().rev() {
            if message.get("role").and_then(Value::as_str) == Some("assistant") {
                end_idx = Some(idx);
                break;
            }
        }
    }
    let mut end_idx = end_idx.unwrap_or_else(|| messages.len().saturating_sub(1));
    while end_idx + 1 < messages.len()
        && messages[end_idx + 1].get("role").and_then(Value::as_str) == Some("tool")
    {
        end_idx += 1;
    }

    let mut start_idx = None;
    if !user_content.trim().is_empty() {
        for idx in (0..=end_idx).rev() {
            let message = &messages[idx];
            if message.get("role").and_then(Value::as_str) == Some("user")
                && message_matches_text(message, user_content)
            {
                start_idx = Some(idx);
                break;
            }
        }
    }
    if start_idx.is_none() {
        for idx in (0..=end_idx).rev() {
            if messages[idx].get("role").and_then(Value::as_str) == Some("user") {
                start_idx = Some(idx);
                break;
            }
        }
    }
    let Some(start_idx) = start_idx else {
        return Vec::new();
    };
    messages[start_idx..=end_idx].to_vec()
}

fn tool_call_id(tool_call: &Value) -> String {
    tool_call
        .get("id")
        .or_else(|| tool_call.get("tool_call_id"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn tool_call_name(tool_call: &Value) -> String {
    tool_call
        .get("function")
        .and_then(|function| function.get("name"))
        .or_else(|| tool_call.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn tool_call_input(tool_call: &Value) -> Value {
    let raw_args = tool_call
        .get("function")
        .and_then(|function| function.get("arguments"))
        .or_else(|| tool_call.get("arguments"))
        .or_else(|| tool_call.get("args"));
    match raw_args {
        Some(Value::Object(_)) => raw_args.cloned().unwrap_or_else(|| json!({})),
        Some(Value::String(raw)) => {
            let raw = raw.trim();
            if raw.is_empty() {
                json!({})
            } else {
                match serde_json::from_str::<Value>(raw) {
                    Ok(Value::Object(map)) => Value::Object(map),
                    Ok(parsed) => json!({"value": parsed}),
                    Err(_) => json!({"value": raw}),
                }
            }
        }
        Some(Value::Null) | None => json!({}),
        Some(other) => json!({"value": other}),
    }
}

fn tool_result_status(message: &Value) -> &'static str {
    let raw_status = message
        .get("status")
        .or_else(|| message.get("tool_status"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    if TOOL_STATUS_ERROR_ALIASES.contains(&raw_status.as_str()) {
        return TOOL_STATUS_ERROR;
    }
    if TOOL_STATUS_COMPLETED_ALIASES.contains(&raw_status.as_str()) {
        return TOOL_STATUS_COMPLETED;
    }

    let text = message_text(message);
    if !text.trim().is_empty() {
        if let Ok(parsed) = serde_json::from_str::<Value>(&text) {
            let exit_code = parsed.get("exit_code").and_then(Value::as_i64);
            if parsed
                .get("is_error")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                || parsed
                    .get("success")
                    .and_then(Value::as_bool)
                    .is_some_and(|success| !success)
                || parsed.get("error").is_some_and(|error| !error.is_null())
                || exit_code.is_some_and(|code| code != 0)
            {
                return TOOL_STATUS_ERROR;
            }
        }
    }
    TOOL_STATUS_COMPLETED
}

fn payload_message(role: &str, parts: Vec<Value>, assistant_peer_id: Option<&str>) -> Value {
    let mut payload = json!({"role": role, "parts": parts});
    if role == "assistant" {
        if let Some(peer_id) = assistant_peer_id {
            if !peer_id.trim().is_empty() {
                payload["peer_id"] = json!(peer_id);
            }
        }
    }
    payload
}

fn messages_to_openviking_batch(messages: &[Value], assistant_peer_id: Option<&str>) -> Vec<Value> {
    let mut tool_calls_by_id: HashMap<String, (String, Value)> = HashMap::new();
    let mut completed_tool_ids: HashSet<String> = HashSet::new();
    let mut skipped_tool_ids: HashSet<String> = HashSet::new();

    for message in messages {
        match message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default()
        {
            "tool" => {
                let tool_id = message
                    .get("tool_call_id")
                    .or_else(|| message.get("id"))
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                if !tool_id.is_empty() {
                    completed_tool_ids.insert(tool_id.clone());
                    if message
                        .get("name")
                        .and_then(Value::as_str)
                        .is_some_and(is_openviking_recall_tool_name)
                    {
                        skipped_tool_ids.insert(tool_id);
                    }
                }
            }
            "assistant" => {
                for tool_call in message
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if !tool_call.is_object() {
                        continue;
                    }
                    let tool_id = tool_call_id(tool_call);
                    let tool_name = tool_call_name(tool_call);
                    if !tool_id.is_empty() {
                        tool_calls_by_id.insert(
                            tool_id.clone(),
                            (tool_name.clone(), tool_call_input(tool_call)),
                        );
                        if is_openviking_recall_tool_name(&tool_name) {
                            skipped_tool_ids.insert(tool_id);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    let mut payload_messages = Vec::new();
    let mut pending_tool_parts = Vec::new();
    let flush_tool_parts = |payload_messages: &mut Vec<Value>,
                            pending_tool_parts: &mut Vec<Value>| {
        if !pending_tool_parts.is_empty() {
            payload_messages.push(payload_message(
                "assistant",
                std::mem::take(pending_tool_parts),
                assistant_peer_id,
            ));
        }
    };

    for message in messages {
        let role = message
            .get("role")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if matches!(role, "system" | "developer") {
            continue;
        }

        if role == "tool" {
            let tool_id = message
                .get("tool_call_id")
                .or_else(|| message.get("id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let prior_call = tool_calls_by_id.get(&tool_id);
            let tool_name = message
                .get("name")
                .and_then(Value::as_str)
                .map(ToString::to_string)
                .or_else(|| prior_call.map(|(name, _)| name.clone()))
                .unwrap_or_default();
            if skipped_tool_ids.contains(&tool_id) || is_openviking_recall_tool_name(&tool_name) {
                continue;
            }
            let tool_input = prior_call
                .map(|(_, input)| input.clone())
                .unwrap_or_else(|| json!({}));
            pending_tool_parts.push(json!({
                "type": "tool",
                "tool_id": tool_id,
                "tool_name": tool_name,
                "tool_input": tool_input,
                "tool_output": message_text(message),
                "tool_status": tool_result_status(message),
            }));
            continue;
        }

        if !matches!(role, "user" | "assistant") {
            continue;
        }

        flush_tool_parts(&mut payload_messages, &mut pending_tool_parts);
        let mut parts = Vec::new();
        let text = message_text(message);
        if !text.is_empty() {
            parts.push(json!({"type": "text", "text": text}));
        }

        if role == "assistant" {
            for tool_call in message
                .get("tool_calls")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if !tool_call.is_object() {
                    continue;
                }
                let tool_id = tool_call_id(tool_call);
                let tool_name = tool_call_name(tool_call);
                if skipped_tool_ids.contains(&tool_id) || is_openviking_recall_tool_name(&tool_name)
                {
                    continue;
                }
                if completed_tool_ids.contains(&tool_id) {
                    continue;
                }
                let tool_input = tool_calls_by_id
                    .get(&tool_id)
                    .map(|(_, input)| input.clone())
                    .unwrap_or_else(|| tool_call_input(tool_call));
                parts.push(json!({
                    "type": "tool",
                    "tool_id": tool_id,
                    "tool_name": tool_name,
                    "tool_input": tool_input,
                    "tool_status": TOOL_STATUS_PENDING,
                }));
            }
        }

        if !parts.is_empty() {
            payload_messages.push(payload_message(role, parts, assistant_peer_id));
        }
    }
    flush_tool_parts(&mut payload_messages, &mut pending_tool_parts);
    payload_messages
}

fn fallback_turn_batch(
    user_content: &str,
    assistant_content: &str,
    assistant_peer_id: &str,
) -> Vec<Value> {
    let mut messages = Vec::new();
    if !user_content.trim().is_empty() {
        messages.push(payload_message(
            "user",
            vec![json!({"type": "text", "text": user_content.chars().take(4000).collect::<String>()})],
            None,
        ));
    }
    if !messages.is_empty() {
        messages.push(payload_message(
            "assistant",
            vec![json!({"type": "text", "text": assistant_content.chars().take(4000).collect::<String>()})],
            Some(assistant_peer_id),
        ));
    }
    messages
}

fn post_openviking_batch(st: &VikingState, batch_messages: &[Value]) -> Result<(), String> {
    if batch_messages.is_empty() {
        return Ok(());
    }
    let url = format!(
        "{}/api/v1/sessions/{}/messages/batch",
        st.endpoint, st.session_id
    );
    let resp = st
        .client
        .post(&url)
        .headers(viking_headers(st))
        .json(&json!({"messages": batch_messages}))
        .send()
        .map_err(|e| format!("OpenViking structured sync failed: {e}"))?;
    if resp.status().is_success() {
        Ok(())
    } else {
        Err(format!("OpenViking structured sync HTTP {}", resp.status()))
    }
}

fn post_openviking_text_turn(
    st: &VikingState,
    user_content: &str,
    assistant_content: &str,
) -> Result<(), String> {
    let url = format!("{}/api/v1/sessions/{}/messages", st.endpoint, st.session_id);
    let user_status = st
        .client
        .post(&url)
        .headers(viking_headers(st))
        .json(&json!({"role": "user", "content": user_content.chars().take(4000).collect::<String>()}))
        .send()
        .map_err(|e| format!("OpenViking text user sync failed: {e}"))?
        .status();
    if !user_status.is_success() {
        return Err(format!("OpenViking text user sync HTTP {user_status}"));
    }

    let assistant_status = st
        .client
        .post(&url)
        .headers(viking_headers(st))
        .json(&json!({"role": "assistant", "content": assistant_content.chars().take(4000).collect::<String>()}))
        .send()
        .map_err(|e| format!("OpenViking text assistant sync failed: {e}"))?
        .status();
    if assistant_status.is_success() {
        Ok(())
    } else {
        Err(format!(
            "OpenViking text assistant sync HTTP {assistant_status}"
        ))
    }
}

fn is_remote_resource_source(value: &str) -> bool {
    REMOTE_RESOURCE_PREFIXES
        .iter()
        .any(|prefix| value.starts_with(prefix))
}

fn is_windows_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

fn is_local_path_reference(value: &str) -> bool {
    if value.is_empty() || value.contains('\n') || value.contains('\r') {
        return false;
    }
    if is_remote_resource_source(value) {
        return false;
    }
    is_windows_absolute_path(value)
        || value.starts_with('/')
        || value.starts_with("./")
        || value.starts_with("../")
        || value.starts_with("~/")
        || value.starts_with(".\\")
        || value.starts_with("..\\")
        || value.starts_with("~\\")
        || value.contains('/')
        || value.contains('\\')
}

fn file_uri_to_path(uri: &str) -> Result<PathBuf, String> {
    let Some(rest) = uri.strip_prefix("file://") else {
        return Err(format!("Unsupported file URI: {uri}"));
    };
    let path = if let Some(path) = rest.strip_prefix("localhost/") {
        format!("/{path}")
    } else if rest.starts_with('/') {
        rest.to_string()
    } else {
        return Err(format!("Unsupported non-local file URI: {uri}"));
    };
    percent_decode_path(&path).map(PathBuf::from)
}

fn percent_decode_path(raw: &str) -> Result<String, String> {
    let mut out = Vec::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'%' {
            if idx + 2 >= bytes.len() {
                return Err(format!("Invalid percent escape in path: {raw}"));
            }
            let hex = std::str::from_utf8(&bytes[idx + 1..idx + 3])
                .map_err(|_| format!("Invalid percent escape in path: {raw}"))?;
            let value = u8::from_str_radix(hex, 16)
                .map_err(|_| format!("Invalid percent escape in path: {raw}"))?;
            out.push(value);
            idx += 3;
        } else {
            out.push(bytes[idx]);
            idx += 1;
        }
    }
    String::from_utf8(out).map_err(|_| format!("Invalid UTF-8 in path URI: {raw}"))
}

fn zip_directory(dir_path: &Path) -> Result<PathBuf, String> {
    let zip_path = std::env::temp_dir().join(format!(
        "openviking_upload_{}.zip",
        uuid::Uuid::new_v4().simple()
    ));
    let file = std::fs::File::create(&zip_path)
        .map_err(|e| format!("create {}: {e}", zip_path.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    add_directory_to_zip(dir_path, dir_path, &mut zip, options)?;
    zip.finish()
        .map_err(|e| format!("finish {}: {e}", zip_path.display()))?;
    Ok(zip_path)
}

fn add_directory_to_zip(
    root: &Path,
    current: &Path,
    zip: &mut zip::ZipWriter<std::fs::File>,
    options: zip::write::SimpleFileOptions,
) -> Result<(), String> {
    for entry in
        std::fs::read_dir(current).map_err(|e| format!("read_dir {}: {e}", current.display()))?
    {
        let entry = entry.map_err(|e| format!("read_dir entry {}: {e}", current.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("file_type {}: {e}", path.display()))?;
        if file_type.is_symlink() {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|e| format!("metadata {}: {e}", path.display()))?;
        if metadata.is_dir() {
            add_directory_to_zip(root, &path, zip, options)?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|e| format!("strip_prefix {}: {e}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        zip.start_file(rel, options)
            .map_err(|e| format!("zip start_file {}: {e}", path.display()))?;
        let mut file =
            std::fs::File::open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        zip.write_all(&buffer)
            .map_err(|e| format!("zip write {}: {e}", path.display()))?;
    }
    Ok(())
}

fn upload_temp_file(st: &VikingState, file_path: &Path) -> Result<String, String> {
    let file_name = file_path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("upload.bin")
        .to_string();
    let bytes =
        std::fs::read(file_path).map_err(|e| format!("read {}: {e}", file_path.display()))?;
    let part = Part::bytes(bytes).file_name(file_name);
    let form = Form::new().part("file", part);
    let url = format!("{}/api/v1/resources/temp_upload", st.endpoint);
    let resp = st
        .client
        .post(&url)
        .headers(viking_multipart_headers(st))
        .multipart(form)
        .send()
        .map_err(|e| format!("OpenViking temp_upload failed: {e}"))?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        return Err(format!("OpenViking temp_upload HTTP {status}: {text}"));
    }
    let value: Value =
        serde_json::from_str(&text).map_err(|e| format!("OpenViking temp_upload JSON: {e}"))?;
    value
        .get("result")
        .and_then(|result| result.get("temp_file_id"))
        .or_else(|| value.get("temp_file_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| "OpenViking temp_upload did not return temp_file_id".to_string())
}

fn add_resource_payload_for_source(
    source: &str,
    args: &Value,
) -> Result<(Value, Option<PathBuf>), String> {
    if args
        .get("to")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
        && args
            .get("parent")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    {
        return Err("Cannot specify both 'to' and 'parent'".to_string());
    }

    let mut body = json!({});
    for key in ["reason", "to", "parent", "instruction"] {
        if let Some(value) = args.get(key).and_then(Value::as_str) {
            if !value.trim().is_empty() {
                body[key] = json!(value);
            }
        }
    }
    for key in ["wait", "timeout"] {
        if let Some(value) = args.get(key) {
            if !value.is_null() {
                body[key] = value.clone();
            }
        }
    }

    let source = source.trim();
    if is_remote_resource_source(source) {
        body["path"] = json!(source);
        return Ok((body, None));
    }

    let path = if source.starts_with("file://") {
        file_uri_to_path(source)?
    } else if source.contains("://") && !is_windows_absolute_path(source) {
        body["path"] = json!(source);
        return Ok((body, None));
    } else {
        PathBuf::from(source).expanduser()
    };

    if !path.exists() {
        if is_local_path_reference(source) {
            return Err(format!("Local resource path does not exist: {source}"));
        }
        body["path"] = json!(source);
        return Ok((body, None));
    }

    if path
        .symlink_metadata()
        .map_err(|e| format!("metadata {}: {e}", path.display()))?
        .file_type()
        .is_symlink()
    {
        return Err(format!(
            "Local resource path is a symlink and will not be uploaded: {source}"
        ));
    }

    if path.is_file() {
        body["source_name"] = json!(path.file_name().and_then(|v| v.to_str()).unwrap_or("file"));
        Ok((body, Some(path)))
    } else if path.is_dir() {
        body["source_name"] = json!(path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("directory"));
        Ok((body, Some(zip_directory(&path)?)))
    } else {
        Err(format!("Unsupported local resource path: {source}"))
    }
}

trait ExpandUserPath {
    fn expanduser(self) -> PathBuf;
}

impl ExpandUserPath for PathBuf {
    fn expanduser(self) -> PathBuf {
        let raw = self.to_string_lossy();
        if raw == "~" {
            if let Some(home) = dirs::home_dir() {
                return home;
            }
        }
        if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
            if let Some(home) = dirs::home_dir() {
                return home.join(rest);
            }
        }
        self
    }
}

type InflightWriters = Arc<Mutex<HashMap<String, Vec<JoinHandle<()>>>>>;

fn openviking_session_drain_timeout() -> Duration {
    std::env::var("OPENVIKING_SESSION_DRAIN_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_SESSION_DRAIN_TIMEOUT)
}

fn drain_writers_for_session(
    writers: &InflightWriters,
    session_id: &str,
    timeout: Duration,
) -> bool {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return true;
    }
    let deadline = Instant::now() + timeout;
    loop {
        let finished = {
            let mut guard = writers.lock().unwrap();
            let mut finished = Vec::new();
            let mut pending = Vec::new();
            let remove_session = {
                let Some(handles) = guard.get_mut(session_id) else {
                    return true;
                };
                for handle in handles.drain(..) {
                    if handle.is_finished() {
                        finished.push(handle);
                    } else {
                        pending.push(handle);
                    }
                }
                if pending.is_empty() {
                    true
                } else {
                    *handles = pending;
                    false
                }
            };
            if remove_session {
                guard.remove(session_id);
            }
            finished
        };
        for handle in finished {
            if handle.join().is_err() {
                tracing::warn!("OpenViking writer for {session_id} panicked");
            }
        }
        if writers
            .lock()
            .unwrap()
            .get(session_id)
            .is_none_or(|handles| handles.is_empty())
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn commit_openviking_session(st: &VikingState) -> bool {
    if st.session_id.trim().is_empty() {
        return false;
    }
    let h = viking_headers(st);
    let url = format!("{}/api/v1/sessions/{}/commit", st.endpoint, st.session_id);
    match st.client.post(&url).headers(h).send() {
        Ok(resp) if resp.status().is_success() => true,
        Ok(resp) => {
            tracing::warn!(
                "OpenViking session commit for {} returned HTTP {}",
                st.session_id,
                resp.status()
            );
            false
        }
        Err(err) => {
            tracing::warn!(
                "OpenViking session commit for {} failed: {err}",
                st.session_id
            );
            false
        }
    }
}

fn spawn_deferred_commit(st: VikingState, writers: InflightWriters, context: &'static str) {
    std::thread::spawn(move || {
        if !drain_writers_for_session(&writers, &st.session_id, openviking_session_drain_timeout())
        {
            tracing::warn!(
                "OpenViking writer for {} still alive after drain during {context}; leaving session uncommitted",
                st.session_id
            );
            return;
        }
        let _ = commit_openviking_session(&st);
    });
}

impl OpenVikingMemoryPlugin {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(None),
            prefetch: Arc::new(Mutex::new(String::new())),
            inflight_writers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn spawn_session_writer<F>(&self, session_id: String, job: F)
    where
        F: FnOnce() + Send + 'static,
    {
        if session_id.trim().is_empty() {
            return;
        }
        let handle = std::thread::spawn(job);
        self.inflight_writers
            .lock()
            .unwrap()
            .entry(session_id)
            .or_default()
            .push(handle);
    }
}

impl MemoryProviderPlugin for OpenVikingMemoryPlugin {
    fn name(&self) -> &str {
        "openviking"
    }

    fn backup_paths(&self) -> Vec<PathBuf> {
        dirs::home_dir()
            .map(|home| vec![home.join(".openviking")])
            .unwrap_or_default()
    }

    fn is_available(&self) -> bool {
        std::env::var("OPENVIKING_ENDPOINT")
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
            || OpenVikingConfig::configured_at(&OpenVikingConfig::default_config_path())
    }

    fn initialize(&self, session_id: &str, hermes_home: &str) {
        let config = OpenVikingConfig::load(hermes_home);
        let api_key_type = config.api_key_type.clone();
        let client = match Client::builder().timeout(Duration::from_secs(45)).build() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("OpenViking client: {}", e);
                return;
            }
        };
        let st = VikingState {
            client,
            endpoint: config.endpoint,
            api_key: config.api_key,
            account: config.account,
            user: config.user,
            agent: config.agent,
            session_id: session_id.to_string(),
            turn_count: 0,
        };
        let health_url = format!("{}/health", st.endpoint);
        let h = viking_headers(&st);
        if st
            .client
            .get(&health_url)
            .headers(h)
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            *self.state.lock().unwrap() = Some(st);
            tracing::info!("OpenViking memory plugin initialized ({api_key_type} credential mode)");
        } else {
            tracing::warn!(
                "OpenViking health check failed for {}; OpenViking memory disabled for this session",
                st.endpoint
            );
            *self.state.lock().unwrap() = None;
        }
    }

    fn system_prompt_block(&self) -> String {
        let guard = self.state.lock().unwrap();
        let ep = guard.as_ref().map(|s| s.endpoint.as_str()).unwrap_or("");
        if ep.is_empty() {
            return String::new();
        }
        format!(
            "# OpenViking Knowledge Base\n\
             Active. Endpoint: {}.\n\
             Use viking_search, viking_read, viking_browse, viking_remember, viking_forget, viking_add_resource.",
            ep
        )
    }

    fn prefetch(&self, _query: &str, _session_id: &str) -> String {
        let mut g = self.prefetch.lock().unwrap();
        let r = g.clone();
        g.clear();
        if r.is_empty() {
            return String::new();
        }
        format!("## OpenViking Context\n{}", r)
    }

    fn queue_prefetch(&self, query: &str, _session_id: &str) {
        let st = match self.state.lock().unwrap().clone() {
            Some(s) => s,
            None => return,
        };
        if query.trim().is_empty() {
            return;
        }
        let q = query.to_string();
        let out = std::sync::Arc::clone(&self.prefetch);
        let plugin = OpenVikingPrefetch { st, q };
        std::thread::spawn(move || {
            if let Ok(s) = plugin.run() {
                if !s.is_empty() {
                    *out.lock().unwrap() = s;
                }
            }
        });
    }

    fn sync_turn(&self, user_content: &str, assistant_content: &str, session_id: &str) {
        self.sync_turn_with_messages(user_content, assistant_content, session_id, &[]);
    }

    fn sync_turn_with_messages(
        &self,
        user_content: &str,
        assistant_content: &str,
        session_id: &str,
        messages: &[Value],
    ) {
        if user_content.trim().is_empty() {
            return;
        }
        let (sid, stc) = {
            let mut lock = self.state.lock().unwrap();
            let st = match lock.as_mut() {
                Some(s) => s,
                None => return,
            };
            let sid = if session_id.trim().is_empty() {
                st.session_id.clone()
            } else {
                session_id.trim().to_string()
            };
            if sid.is_empty() {
                return;
            }
            st.turn_count = st.turn_count.saturating_add(1);
            let mut stc = st.clone();
            stc.session_id = sid.clone();
            (sid, stc)
        };

        let mut turn_messages = if messages.is_empty() {
            Vec::new()
        } else {
            extract_current_turn_messages(messages, user_content, assistant_content)
        };
        if !turn_messages.is_empty() {
            for message in &mut turn_messages {
                if message.get("role").and_then(Value::as_str) == Some("user") {
                    if let Some(object) = message.as_object_mut() {
                        object.insert(
                            "content".to_string(),
                            Value::String(user_content.to_string()),
                        );
                    }
                    break;
                }
            }
        }

        let mut batch_messages = messages_to_openviking_batch(&turn_messages, Some(&stc.agent));
        if batch_messages.is_empty() {
            batch_messages = fallback_turn_batch(user_content, assistant_content, &stc.agent);
        }
        if batch_messages.is_empty() {
            return;
        }

        if openviking_sync_trace_enabled() {
            tracing::info!(
                "OpenViking sync_turn trace: session_arg={:?} cached_session={:?} messages_present={} message_count={} turn_message_count={} batch_message_count={} user_len={} assistant_len={} user_preview={:?} assistant_preview={:?}",
                session_id,
                stc.session_id,
                !messages.is_empty(),
                messages.len(),
                turn_messages.len(),
                batch_messages.len(),
                user_content.len(),
                assistant_content.len(),
                preview_sync_value(user_content),
                preview_sync_value(assistant_content),
            );
        }

        let u = user_content.to_string();
        let a = assistant_content.to_string();
        self.spawn_session_writer(sid, move || {
            if let Err(batch_error) = post_openviking_batch(&stc, &batch_messages) {
                tracing::warn!(
                    "OpenViking structured sync failed; falling back to text sync: {}",
                    batch_error
                );
                if let Err(text_error) = post_openviking_text_turn(&stc, &u, &a) {
                    tracing::warn!("OpenViking text sync fallback failed: {}", text_error);
                }
            }
        });
    }

    fn on_session_end(&self, _messages: &[Value]) {
        let st = match self.state.lock().unwrap().clone() {
            Some(s) => s,
            None => return,
        };
        if st.turn_count == 0 {
            return;
        }
        if !drain_writers_for_session(
            &self.inflight_writers,
            &st.session_id,
            openviking_session_drain_timeout(),
        ) {
            tracing::warn!(
                "OpenViking writer for {} still alive after drain; skipping session commit",
                st.session_id
            );
            return;
        }
        if commit_openviking_session(&st) {
            if let Some(current) = self.state.lock().unwrap().as_mut() {
                if current.session_id == st.session_id {
                    current.turn_count = 0;
                }
            }
        }
    }

    fn on_session_switch(&self, new_session_id: &str, _parent_session_id: &str, _reset: bool) {
        let new_session_id = new_session_id.trim();
        if new_session_id.is_empty() {
            return;
        }
        *self.prefetch.lock().unwrap() = String::new();
        let old_state = {
            let mut guard = self.state.lock().unwrap();
            let Some(st) = guard.as_mut() else {
                return;
            };
            if st.session_id == new_session_id {
                return;
            }
            let old = st.clone();
            st.session_id = new_session_id.to_string();
            st.turn_count = 0;
            old
        };
        if old_state.turn_count > 0 {
            spawn_deferred_commit(
                old_state,
                Arc::clone(&self.inflight_writers),
                "session switch",
            );
        }
    }

    fn on_memory_write(&self, action: &str, target: &str, content: &str) {
        if !action.trim().eq_ignore_ascii_case("add") || content.trim().is_empty() {
            return;
        }
        let st = match self.state.lock().unwrap().clone() {
            Some(s) => s,
            None => return,
        };
        let h = viking_headers(&st);
        let url = format!("{}/api/v1/content/write", st.endpoint);
        let body = content_write_body(&st, memory_subdir_for_target(target), content);
        self.spawn_session_writer(st.session_id.clone(), move || {
            let _ = st.client.post(&url).headers(h).json(&body).send();
        });
    }

    fn get_tool_schemas(&self) -> Vec<Value> {
        vec![
            search_schema(),
            read_schema(),
            browse_schema(),
            remember_schema(),
            forget_schema(),
            add_resource_schema(),
        ]
    }

    fn handle_tool_call(&self, tool_name: &str, args: &Value) -> String {
        let st = match self.state.lock().unwrap().clone() {
            Some(s) => s,
            None => return json!({"error": "OpenViking not initialized"}).to_string(),
        };
        let h = viking_headers(&st);
        match tool_name {
            "viking_search" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                if query.is_empty() {
                    return json!({"error": "query is required"}).to_string();
                }
                let mut body = json!({"query": query, "top_k": args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10)});
                if let Some(m) = args.get("mode").and_then(|v| v.as_str()) {
                    if m != "auto" {
                        body["mode"] = json!(m);
                    }
                }
                if let Some(s) = args.get("scope").and_then(|v| v.as_str()) {
                    body["target_uri"] = json!(s);
                }
                let url = format!("{}/api/v1/search/find", st.endpoint);
                match st.client.post(&url).headers(h).json(&body).send() {
                    Ok(r) if r.status().is_success() => {
                        r.json::<Value>().unwrap_or(json!({})).to_string()
                    }
                    Ok(r) => json!({"error": format!("HTTP {}", r.status())}).to_string(),
                    Err(e) => json!({"error": e.to_string()}).to_string(),
                }
            }
            "viking_read" => {
                let uri = args.get("uri").and_then(|v| v.as_str()).unwrap_or("");
                if uri.is_empty() {
                    return json!({"error": "uri is required"}).to_string();
                }
                let level = args
                    .get("level")
                    .and_then(|v| v.as_str())
                    .unwrap_or("overview");
                let path = match level {
                    "abstract" => "/api/v1/content/abstract",
                    "full" => "/api/v1/content/read",
                    _ => "/api/v1/content/overview",
                };
                let url = format!("{}{}", st.endpoint, path);
                match st.client.get(&url).headers(h).query(&[("uri", uri)]).send() {
                    Ok(r) if r.status().is_success() => match r.json::<Value>() {
                        Ok(v) => v.to_string(),
                        Err(e) => json!({"error": e.to_string()}).to_string(),
                    },
                    Ok(r) => json!({"error": format!("HTTP {}", r.status())}).to_string(),
                    Err(e) => json!({"error": e.to_string()}).to_string(),
                }
            }
            "viking_browse" => {
                let action = args
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("list");
                let path_uri = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("viking://");
                let ep = match action {
                    "tree" => "/api/v1/fs/tree",
                    "stat" => "/api/v1/fs/stat",
                    _ => "/api/v1/fs/ls",
                };
                let url = format!("{}{}", st.endpoint, ep);
                match st
                    .client
                    .get(&url)
                    .headers(h)
                    .query(&[("uri", path_uri)])
                    .send()
                {
                    Ok(r) if r.status().is_success() => r.json().unwrap_or(json!({})).to_string(),
                    Ok(r) => json!({"error": format!("HTTP {}", r.status())}).to_string(),
                    Err(e) => json!({"error": e.to_string()}).to_string(),
                }
            }
            "viking_remember" => {
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                if content.is_empty() {
                    return json!({"error": "content is required"}).to_string();
                }
                let cat = args.get("category").and_then(|v| v.as_str()).unwrap_or("");
                let body = content_write_body(&st, memory_subdir_for_category(cat), content);
                let uri = body.get("uri").cloned().unwrap_or(Value::Null);
                let url = format!("{}/api/v1/content/write", st.endpoint);
                match st.client.post(&url).headers(h).json(&body).send() {
                    Ok(r) if r.status().is_success() => {
                        json!({"status": "stored", "uri": uri}).to_string()
                    }
                    Ok(r) => json!({"error": format!("HTTP {}", r.status())}).to_string(),
                    Err(e) => json!({"error": e.to_string()}).to_string(),
                }
            }
            "viking_forget" => {
                let uri = match validate_forget_memory_uri(args.get("uri").and_then(Value::as_str))
                {
                    Ok(uri) => uri,
                    Err(e) => return json!({"error": e}).to_string(),
                };
                let url = format!("{}/api/v1/fs", st.endpoint);
                match st
                    .client
                    .delete(&url)
                    .headers(h)
                    .query(&[("uri", uri.as_str()), ("recursive", "false")])
                    .send()
                {
                    Ok(resp) if resp.status().is_success() => {
                        let result = resp.json::<Value>().unwrap_or(json!({}));
                        let mut payload = json!({"status": "deleted", "uri": uri});
                        if let Some(obj) = result.get("result").and_then(Value::as_object) {
                            for key in [
                                "estimated_deleted_count",
                                "memory_cleanup",
                                "semantic_root_uri",
                                "semantic_status",
                            ] {
                                if let Some(value) = obj.get(key) {
                                    payload[key] = value.clone();
                                }
                            }
                            if let Some(result_uri) = obj.get("uri").and_then(Value::as_str) {
                                payload["uri"] = json!(result_uri);
                            }
                        }
                        payload.to_string()
                    }
                    Ok(r) => json!({"error": format!("HTTP {}", r.status())}).to_string(),
                    Err(e) => json!({"error": e.to_string()}).to_string(),
                }
            }
            "viking_add_resource" => {
                let url_arg = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
                if url_arg.is_empty() {
                    return json!({"error": "url is required"}).to_string();
                }
                let (mut body, upload_path) = match add_resource_payload_for_source(url_arg, args) {
                    Ok(value) => value,
                    Err(e) => return json!({"error": e}).to_string(),
                };
                let cleanup_path = upload_path
                    .as_ref()
                    .filter(|path| {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with("openviking_upload_"))
                    })
                    .cloned();
                if let Some(path) = upload_path.as_deref() {
                    match upload_temp_file(&st, path) {
                        Ok(temp_file_id) => body["temp_file_id"] = json!(temp_file_id),
                        Err(e) => {
                            if let Some(cleanup_path) = cleanup_path {
                                let _ = std::fs::remove_file(cleanup_path);
                            }
                            return json!({"error": e}).to_string();
                        }
                    }
                }
                let url = format!("{}/api/v1/resources", st.endpoint);
                let result = match st.client.post(&url).headers(h).json(&body).send() {
                    Ok(resp) if resp.status().is_success() => resp
                        .json()
                        .unwrap_or(json!({"status": "added"}))
                        .to_string(),
                    Ok(r) => json!({"error": format!("HTTP {}", r.status())}).to_string(),
                    Err(e) => json!({"error": e.to_string()}).to_string(),
                };
                if let Some(cleanup_path) = cleanup_path {
                    let _ = std::fs::remove_file(cleanup_path);
                }
                result
            }
            _ => json!({"error": format!("Unknown tool: {}", tool_name)}).to_string(),
        }
    }

    fn shutdown(&self) {
        let writers = Arc::clone(&self.inflight_writers);
        let session_ids = writers.lock().unwrap().keys().cloned().collect::<Vec<_>>();
        for session_id in session_ids {
            let _ = drain_writers_for_session(
                &writers,
                &session_id,
                openviking_session_drain_timeout(),
            );
        }
        *self.state.lock().unwrap() = None;
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key": "endpoint", "description": "OpenViking server URL", "env_var": "OPENVIKING_ENDPOINT", "default": DEFAULT_ENDPOINT},
            {"key": "api_key", "description": "API key", "secret": true, "env_var": "OPENVIKING_API_KEY"},
            {"key": "api_key_type", "description": "Credential type: none|user|root", "default": "user", "env_var": "OPENVIKING_API_KEY_TYPE"},
            {"key": "account", "description": "Tenant account for root/local trusted mode", "env_var": "OPENVIKING_ACCOUNT", "default": "default"},
            {"key": "user", "description": "Tenant user for root/local trusted mode", "env_var": "OPENVIKING_USER", "default": "default"},
            {"key": "agent", "description": "OpenViking agent namespace", "env_var": "OPENVIKING_AGENT", "default": DEFAULT_AGENT}
        ]))
    }

    fn save_config(&self, config: &Value) -> Result<(), String> {
        let path = OpenVikingConfig::default_config_path();
        config_io::merge_and_write_owner_only(&path, config)
    }
}

struct OpenVikingPrefetch {
    st: VikingState,
    q: String,
}

impl OpenVikingPrefetch {
    fn run(self) -> Result<String, ()> {
        let h = viking_headers(&self.st);
        let url = format!("{}/api/v1/search/find", self.st.endpoint);
        let body = json!({"query": self.q, "top_k": 5u64});
        let resp = self
            .st
            .client
            .post(&url)
            .headers(h)
            .json(&body)
            .send()
            .map_err(|_| ())?;
        if !resp.status().is_success() {
            return Err(());
        }
        let v: Value = resp.json().map_err(|_| ())?;
        let result = v.get("result").cloned().unwrap_or(json!({}));
        let mut parts = Vec::new();
        for key in ["memories", "resources"] {
            if let Some(arr) = result.get(key).and_then(|a| a.as_array()) {
                for item in arr.iter().take(3) {
                    let uri = item.get("uri").and_then(|u| u.as_str()).unwrap_or("");
                    let ab = item.get("abstract").and_then(|u| u.as_str()).unwrap_or("");
                    let score = item.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0);
                    if !ab.is_empty() {
                        parts.push(format!("- [{:.2}] {} ({})", score, ab, uri));
                    }
                }
            }
        }
        if parts.is_empty() {
            Err(())
        } else {
            Ok(parts.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Duration as StdDuration;

    struct EnvGuard {
        key: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var(key).ok();
            std::env::set_var(key, value);
            Self { key, previous }
        }

        fn remove(key: &'static str) -> Self {
            let previous = std::env::var(key).ok();
            std::env::remove_var(key);
            Self { key, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var(self.key, value),
                None => std::env::remove_var(self.key),
            }
        }
    }

    #[test]
    fn name() {
        let p = OpenVikingMemoryPlugin::new();
        assert_eq!(p.name(), "openviking");
    }

    #[test]
    fn config_file_activates_provider_and_loads_values() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let _endpoint = EnvGuard::remove("OPENVIKING_ENDPOINT");
        let _api_key = EnvGuard::remove("OPENVIKING_API_KEY");
        let _account = EnvGuard::remove("OPENVIKING_ACCOUNT");
        let _user = EnvGuard::remove("OPENVIKING_USER");
        let _agent = EnvGuard::remove("OPENVIKING_AGENT");
        std::fs::write(
            tmp.path().join("openviking.json"),
            r#"{
                "enabled": true,
                "endpoint": "localhost:1934/",
                "api_key": "ov-secret",
                "api_key_type": "root",
                "account": "acct",
                "user": "operator",
                "agent": "ultra"
            }"#,
        )
        .expect("write config");

        assert!(OpenVikingMemoryPlugin::new().is_available());
        let config = OpenVikingConfig::load(tmp.path().to_str().expect("home"));
        assert_eq!(config.endpoint, "http://localhost:1934");
        assert_eq!(config.api_key, "ov-secret");
        assert_eq!(config.api_key_type, "root");
        assert_eq!(config.account, "acct");
        assert_eq!(config.user, "operator");
        assert_eq!(config.agent, "ultra");
    }

    #[test]
    fn save_config_merges_and_writes_owner_only() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let path = tmp.path().join("openviking.json");
        std::fs::write(&path, r#"{"agent":"existing"}"#).expect("write existing");

        OpenVikingMemoryPlugin::new()
            .save_config(&json!({
                "enabled": true,
                "endpoint": "https://openviking.example",
                "api_key": "ov-secret"
            }))
            .expect("save config");

        let parsed: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read config"))
                .expect("json");
        assert_eq!(parsed["agent"], "existing");
        assert_eq!(parsed["enabled"], true);
        assert_eq!(parsed["endpoint"], "https://openviking.example");
        assert_eq!(parsed["api_key"], "ov-secret");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path)
                    .expect("metadata")
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
        }
    }

    #[test]
    fn memory_uri_sanitizes_tenant_segments_without_agent_scope() {
        let uri = build_memory_uri("user/name", "agent one", "patterns");
        assert!(uri.starts_with("viking://user/user_name/memories/patterns/mem_"));
        assert!(uri.ends_with(".md"));
        assert!(!uri.contains("user/name"));
        assert!(!uri.contains("agent one"));
    }

    #[test]
    fn memory_subdir_mapping_matches_write_targets_and_categories() {
        assert_eq!(memory_subdir_for_category("entity"), "entities");
        assert_eq!(memory_subdir_for_category("event"), "events");
        assert_eq!(memory_subdir_for_category("case"), "cases");
        assert_eq!(memory_subdir_for_category("pattern"), "patterns");
        assert_eq!(memory_subdir_for_category("unknown"), "preferences");
        assert_eq!(memory_subdir_for_target("memory"), "patterns");
        assert_eq!(memory_subdir_for_target("user"), "preferences");
    }

    #[test]
    fn tool_schemas_include_narrow_forget_tool() {
        let plugin = OpenVikingMemoryPlugin::new();

        let names = plugin
            .get_tool_schemas()
            .into_iter()
            .filter_map(|schema| {
                schema
                    .get("name")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .collect::<Vec<_>>();

        assert!(names.iter().any(|name| name == VIKING_FORGET_TOOL));
    }

    #[test]
    fn validate_forget_memory_uri_accepts_exact_user_memory_files() {
        assert_eq!(
            validate_forget_memory_uri(Some(
                "viking://user/peers/hermes/memories/preferences/mem_abc123.md"
            ))
            .expect("valid"),
            "viking://user/peers/hermes/memories/preferences/mem_abc123.md"
        );
        assert_eq!(
            validate_forget_memory_uri(Some("viking://user/default/memories/profile.md"))
                .expect("valid"),
            "viking://user/default/memories/profile.md"
        );
        assert_eq!(
            validate_forget_memory_uri(Some("viking://user/default/memories/.full.md"))
                .expect("valid"),
            "viking://user/default/memories/.full.md"
        );
    }

    #[test]
    fn validate_forget_memory_uri_rejects_broad_or_non_memory_targets() {
        for uri in [
            "",
            "viking:/user/memories/preferences/mem_abc123.md",
            "viking://resources/project/doc.md",
            "viking://resources/project/memories/mem_abc123.md",
            "viking://agent/hermes/memories/preferences/mem_abc123.md",
            "viking://user/skills/example/SKILL.md",
            "viking://user/sessions/session-1/messages.jsonl",
            "viking://user/memories/preferences/",
            "viking://user/memories/preferences/.overview.md",
            "viking://user/memories/preferences/.abstract.md",
            "viking://user/memories/preferences/.relations.json",
            "viking://user/memories/preferences/mem_abc123.md?recursive=true",
        ] {
            assert!(
                validate_forget_memory_uri(Some(uri)).is_err(),
                "{uri} should be rejected"
            );
        }
    }

    fn one_shot_openviking_server(body: &'static str) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            stream
                .set_read_timeout(Some(StdDuration::from_secs(2)))
                .expect("timeout");
            let mut buf = [0u8; 8192];
            let n = stream.read(&mut buf).expect("read");
            let request = String::from_utf8_lossy(&buf[..n]).to_string();
            tx.send(request).expect("send request");
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                body.len(),
                body
            );
            stream.write_all(response.as_bytes()).expect("write");
        });
        (format!("http://{addr}"), rx)
    }

    #[test]
    fn handle_tool_call_forget_deletes_exact_memory_file_uri() {
        let uri = "viking://user/peers/hermes/memories/preferences/mem_abc123.md";
        let body = r#"{"status":"ok","result":{"uri":"viking://user/peers/hermes/memories/preferences/mem_abc123.md","estimated_deleted_count":1}}"#;
        let (endpoint, rx) = one_shot_openviking_server(body);
        let plugin = OpenVikingMemoryPlugin::new();
        *plugin.state.lock().unwrap() = Some(VikingState {
            client: Client::new(),
            endpoint,
            api_key: "test-key".to_string(),
            account: "acct".to_string(),
            user: "usr".to_string(),
            agent: "hermes".to_string(),
            session_id: "sid".to_string(),
            turn_count: 0,
        });

        let result: Value = serde_json::from_str(
            &plugin.handle_tool_call(VIKING_FORGET_TOOL, &json!({"uri": uri})),
        )
        .expect("json");

        assert_eq!(result["status"], "deleted");
        assert_eq!(result["uri"], uri);
        assert_eq!(result["estimated_deleted_count"], 1);
        let request = rx.recv_timeout(StdDuration::from_secs(2)).expect("request");
        assert!(request.starts_with("DELETE /api/v1/fs?"));
        assert!(request.contains("recursive=false"));
        assert!(request
            .to_ascii_lowercase()
            .contains("authorization: bearer test-key"));
    }

    #[test]
    fn extract_current_turn_anchors_on_latest_matching_user_and_assistant() {
        let messages = vec![
            json!({"role": "user", "content": "Please inspect the repository for assemble hooks."}),
            json!({"role": "assistant", "content": "Earlier answer."}),
            json!({"role": "user", "content": "Please inspect the repository for assemble hooks."}),
            json!({
                "role": "assistant",
                "content": "I will search the codebase.",
                "tool_calls": [{
                    "id": "call_rg_1",
                    "type": "function",
                    "function": {
                        "name": "shell_command",
                        "arguments": serde_json::to_string(&json!({"command": "rg assemble"})).unwrap(),
                    },
                }],
            }),
            json!({
                "role": "tool",
                "tool_call_id": "call_rg_1",
                "name": "shell_command",
                "content": "agent/context_engine.py: no preassemble hook",
            }),
            json!({"role": "assistant", "content": "The current main does not expose assemble."}),
        ];

        let turn = extract_current_turn_messages(
            &messages,
            "Please inspect the repository for assemble hooks.",
            "The current main does not expose assemble.",
        );

        assert_eq!(turn, messages[2..].to_vec());
    }

    #[test]
    fn extract_current_turn_includes_trailing_tool_result_after_empty_assistant() {
        let messages = vec![
            json!({"role": "user", "content": "Run the check."}),
            json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_check",
                    "type": "function",
                    "function": {
                        "name": "terminal",
                        "arguments": serde_json::to_string(&json!({"cmd": "cargo test"})).unwrap(),
                    },
                }],
            }),
            json!({
                "role": "tool",
                "tool_call_id": "call_check",
                "name": "terminal",
                "content": "test result: ok",
            }),
        ];

        let turn = extract_current_turn_messages(&messages, "Run the check.", "");

        assert_eq!(turn, messages);
    }

    #[test]
    fn messages_to_openviking_batch_coalesces_tool_results() {
        let turn = vec![
            json!({"role": "user", "content": "Please inspect the repository for assemble hooks."}),
            json!({
                "role": "assistant",
                "content": "I will search the codebase.",
                "tool_calls": [{
                    "id": "call_rg_1",
                    "type": "function",
                    "function": {
                        "name": "shell_command",
                        "arguments": serde_json::to_string(&json!({"command": "rg assemble"})).unwrap(),
                    },
                }],
            }),
            json!({
                "role": "tool",
                "tool_call_id": "call_rg_1",
                "name": "shell_command",
                "content": "agent/context_engine.py: no preassemble hook",
            }),
            json!({"role": "assistant", "content": "The current main does not expose assemble."}),
        ];

        let batch = messages_to_openviking_batch(&turn, None);

        let roles = batch
            .iter()
            .filter_map(|message| message.get("role").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(roles, vec!["user", "assistant", "assistant", "assistant"]);
        assert_eq!(
            batch[2]["parts"],
            json!([{
                "type": "tool",
                "tool_id": "call_rg_1",
                "tool_name": "shell_command",
                "tool_input": {"command": "rg assemble"},
                "tool_output": "agent/context_engine.py: no preassemble hook",
                "tool_status": "completed",
            }])
        );
    }

    #[test]
    fn messages_to_openviking_batch_marks_json_tool_error_results() {
        let turn = vec![
            json!({"role": "user", "content": "Check the file."}),
            json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_read_1",
                    "type": "function",
                    "function": {
                        "name": "read_file",
                        "arguments": serde_json::to_string(&json!({"path": "missing.md"})).unwrap(),
                    },
                }],
            }),
            json!({
                "role": "tool",
                "tool_call_id": "call_read_1",
                "name": "read_file",
                "content": serde_json::to_string(&json!({"error": "File not found", "exit_code": 1})).unwrap(),
            }),
        ];

        let batch = messages_to_openviking_batch(&turn, None);

        assert_eq!(batch[1]["role"], "assistant");
        assert_eq!(batch[1]["parts"][0]["tool_status"], TOOL_STATUS_ERROR);
        assert_eq!(
            batch[1]["parts"][0]["tool_input"],
            json!({"path": "missing.md"})
        );
    }

    #[test]
    fn messages_to_openviking_batch_keeps_pending_tool_call_without_result() {
        let turn = vec![
            json!({"role": "user", "content": "Start a long running check."}),
            json!({
                "role": "assistant",
                "content": "Starting it now.",
                "tool_calls": [{
                    "id": "call_long_1",
                    "type": "function",
                    "function": {
                        "name": "long_check",
                        "arguments": serde_json::to_string(&json!({"target": "repo"})).unwrap(),
                    },
                }],
            }),
        ];

        let batch = messages_to_openviking_batch(&turn, None);

        assert_eq!(
            batch[1]["parts"],
            json!([
                {"type": "text", "text": "Starting it now."},
                {
                    "type": "tool",
                    "tool_id": "call_long_1",
                    "tool_name": "long_check",
                    "tool_input": {"target": "repo"},
                    "tool_status": "pending",
                }
            ])
        );
    }

    #[test]
    fn messages_to_openviking_batch_skips_recall_results_without_reingesting_echoes() {
        let turn = vec![
            json!({"role": "user", "content": "What did we decide about context assembly?"}),
            json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [
                    {
                        "id": "call_recall_1",
                        "type": "function",
                        "function": {
                            "name": VIKING_SEARCH_TOOL,
                            "arguments": serde_json::to_string(&json!({"query": "context assembly decision"})).unwrap(),
                        },
                    },
                    {
                        "id": "call_shell_1",
                        "type": "function",
                        "function": {
                            "name": "shell_command",
                            "arguments": serde_json::to_string(&json!({"command": "rg preassemble"})).unwrap(),
                        },
                    },
                ],
            }),
            json!({
                "role": "tool",
                "tool_call_id": "call_recall_1",
                "name": VIKING_SEARCH_TOOL,
                "content": {"results": [{"uri": "viking://user/hermes/memories/context", "abstract": "Old OpenViking memory content"}]},
            }),
            json!({
                "role": "tool",
                "tool_call_id": "call_shell_1",
                "name": "shell_command",
                "content": "plugins/memory/openviking/__init__.py",
            }),
            json!({"role": "assistant", "content": "We decided to keep sync_turn scoped to ingestion."}),
        ];

        let batch = messages_to_openviking_batch(&turn, None);
        let batch_text = serde_json::to_string(&batch).unwrap();

        assert!(!batch_text.contains(VIKING_SEARCH_TOOL));
        assert!(!batch_text.contains("Old OpenViking memory content"));
        assert!(batch_text.contains("shell_command"));
        assert!(batch_text.contains("plugins/memory/openviking/__init__.py"));
    }

    #[test]
    fn empty_recall_tool_id_does_not_skip_other_empty_id_tool_results() {
        let turn = vec![
            json!({"role": "user", "content": "Run tools."}),
            json!({
                "role": "tool",
                "tool_call_id": "",
                "name": VIKING_SEARCH_TOOL,
                "content": "recalled old memory",
            }),
            json!({
                "role": "tool",
                "tool_call_id": "",
                "name": "shell_command",
                "content": "fresh shell output",
            }),
        ];

        let batch = messages_to_openviking_batch(&turn, None);
        let batch_text = serde_json::to_string(&batch).unwrap();

        assert!(!batch_text.contains("recalled old memory"));
        assert!(batch_text.contains("fresh shell output"));
    }

    #[test]
    fn messages_to_openviking_batch_preserves_responses_text_parts_and_peer_id() {
        let turn = vec![
            json!({"role": "user", "content": [{"type": "input_text", "text": "hello"}]}),
            json!({"role": "assistant", "content": [{"type": "output_text", "text": "answer"}]}),
        ];

        let batch = messages_to_openviking_batch(&turn, Some("hermes"));

        assert_eq!(
            batch,
            vec![
                json!({"role": "user", "parts": [{"type": "text", "text": "hello"}]}),
                json!({"role": "assistant", "parts": [{"type": "text", "text": "answer"}], "peer_id": "hermes"}),
            ]
        );
    }

    #[test]
    fn fallback_turn_batch_preserves_empty_assistant_turn() {
        let batch = fallback_turn_batch("hello", "", "hermes");

        assert_eq!(
            batch,
            vec![
                json!({"role": "user", "parts": [{"type": "text", "text": "hello"}]}),
                json!({"role": "assistant", "parts": [{"type": "text", "text": ""}], "peer_id": "hermes"}),
            ]
        );
    }

    #[test]
    fn rust_flattened_tool_calls_reuse_cached_top_level_arguments() {
        let turn = vec![
            json!({"role": "user", "content": "Run it."}),
            json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_terminal",
                    "name": "terminal",
                    "arguments": serde_json::to_string(&json!({"cmd": "pwd"})).unwrap(),
                }],
            }),
            json!({
                "role": "tool",
                "tool_call_id": "call_terminal",
                "name": "terminal",
                "content": "/repo",
            }),
        ];

        let batch = messages_to_openviking_batch(&turn, None);

        assert_eq!(batch[1]["parts"][0]["tool_name"], "terminal");
        assert_eq!(batch[1]["parts"][0]["tool_input"], json!({"cmd": "pwd"}));
        assert_eq!(batch[1]["parts"][0]["tool_status"], TOOL_STATUS_COMPLETED);
    }

    #[test]
    fn object_tool_outputs_are_preserved_as_json_text() {
        let turn = vec![
            json!({"role": "user", "content": "Inspect structured output."}),
            json!({
                "role": "assistant",
                "content": "",
                "tool_calls": [{
                    "id": "call_structured",
                    "name": "structured_tool",
                    "arguments": "{}",
                }],
            }),
            json!({
                "role": "tool",
                "tool_call_id": "call_structured",
                "name": "structured_tool",
                "content": {"answer": "kept", "success": true},
            }),
        ];

        let batch = messages_to_openviking_batch(&turn, None);

        assert_eq!(
            batch[1]["parts"][0]["tool_output"],
            json!({"answer": "kept", "success": true}).to_string()
        );
        assert_eq!(batch[1]["parts"][0]["tool_status"], TOOL_STATUS_COMPLETED);
    }

    #[test]
    fn headers_include_agent_and_bearer_key() {
        let st = VikingState {
            client: Client::new(),
            endpoint: DEFAULT_ENDPOINT.to_string(),
            api_key: "secret".to_string(),
            account: "acct".to_string(),
            user: "user".to_string(),
            agent: "agent".to_string(),
            session_id: "session".to_string(),
            turn_count: 0,
        };
        let headers = viking_headers(&st);
        assert_eq!(headers["X-OpenViking-Agent"], "agent");
        assert_eq!(headers["X-API-Key"], "secret");
        assert_eq!(headers["Authorization"], "Bearer secret");
    }

    #[test]
    fn content_write_body_uses_user_scoped_create_uri() {
        let st = VikingState {
            client: Client::new(),
            endpoint: DEFAULT_ENDPOINT.to_string(),
            api_key: String::new(),
            account: "acct".to_string(),
            user: "she/a".to_string(),
            agent: "hermes ultra".to_string(),
            session_id: "session".to_string(),
            turn_count: 0,
        };
        let body = content_write_body(&st, "patterns", "fact");
        let uri = body["uri"].as_str().expect("uri");
        assert!(uri.starts_with("viking://user/she_a/memories/patterns/"));
        assert_eq!(body["content"], "fact");
        assert_eq!(body["mode"], "create");
    }

    #[test]
    fn session_switch_updates_session_and_clears_prefetch() {
        let plugin = OpenVikingMemoryPlugin::new();
        *plugin.state.lock().unwrap() = Some(VikingState {
            client: Client::new(),
            endpoint: DEFAULT_ENDPOINT.to_string(),
            api_key: String::new(),
            account: "acct".to_string(),
            user: "user".to_string(),
            agent: "agent".to_string(),
            session_id: "old".to_string(),
            turn_count: 0,
        });
        *plugin.prefetch.lock().unwrap() = "stale".to_string();

        plugin.on_session_switch("new", "old", false);

        let state = plugin.state.lock().unwrap().clone().expect("state");
        assert_eq!(state.session_id, "new");
        assert_eq!(state.turn_count, 0);
        assert!(plugin.prefetch.lock().unwrap().is_empty());
    }

    #[test]
    fn drain_writers_waits_for_all_finished_session_writers() {
        let plugin = OpenVikingMemoryPlugin::new();
        plugin.spawn_session_writer("sid".to_string(), || {});
        plugin.spawn_session_writer("sid".to_string(), || {});

        assert!(drain_writers_for_session(
            &plugin.inflight_writers,
            "sid",
            Duration::from_secs(1)
        ));
        assert!(plugin.inflight_writers.lock().unwrap().get("sid").is_none());
    }

    #[test]
    fn session_end_skips_commit_when_writer_outlives_drain() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let _timeout = EnvGuard::set("OPENVIKING_SESSION_DRAIN_TIMEOUT_MS", "1");
        let plugin = OpenVikingMemoryPlugin::new();
        *plugin.state.lock().unwrap() = Some(VikingState {
            client: Client::new(),
            endpoint: "http://127.0.0.1:9".to_string(),
            api_key: String::new(),
            account: "acct".to_string(),
            user: "user".to_string(),
            agent: "agent".to_string(),
            session_id: "old".to_string(),
            turn_count: 2,
        });
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        plugin.spawn_session_writer("old".to_string(), move || {
            let _ = release_rx.recv();
        });

        plugin.on_session_end(&[]);

        assert_eq!(
            plugin
                .state
                .lock()
                .unwrap()
                .as_ref()
                .expect("state")
                .turn_count,
            2
        );
        release_tx.send(()).expect("release writer");
        assert!(drain_writers_for_session(
            &plugin.inflight_writers,
            "old",
            Duration::from_secs(1)
        ));
    }

    #[test]
    fn session_switch_rotates_without_waiting_for_old_writer() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let _timeout = EnvGuard::set("OPENVIKING_SESSION_DRAIN_TIMEOUT_MS", "1");
        let plugin = OpenVikingMemoryPlugin::new();
        *plugin.state.lock().unwrap() = Some(VikingState {
            client: Client::new(),
            endpoint: "http://127.0.0.1:9".to_string(),
            api_key: String::new(),
            account: "acct".to_string(),
            user: "user".to_string(),
            agent: "agent".to_string(),
            session_id: "old".to_string(),
            turn_count: 2,
        });
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        plugin.spawn_session_writer("old".to_string(), move || {
            let _ = release_rx.recv();
        });
        let start = Instant::now();

        plugin.on_session_switch("new", "old", false);

        assert!(start.elapsed() < Duration::from_millis(100));
        let state = plugin.state.lock().unwrap().clone().expect("state");
        assert_eq!(state.session_id, "new");
        assert_eq!(state.turn_count, 0);
        release_tx.send(()).expect("release writer");
        assert!(drain_writers_for_session(
            &plugin.inflight_writers,
            "old",
            Duration::from_secs(1)
        ));
    }

    #[test]
    fn add_resource_payload_routes_remote_url_as_path() {
        let (body, upload) = add_resource_payload_for_source(
            "https://example.com/doc.md",
            &json!({"reason": "docs", "wait": true}),
        )
        .expect("payload");

        assert_eq!(body["path"], "https://example.com/doc.md");
        assert_eq!(body["reason"], "docs");
        assert_eq!(body["wait"], true);
        assert!(upload.is_none());
    }

    #[test]
    fn add_resource_payload_uploads_existing_local_file_and_file_uri() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sample = tmp.path().join("sample file.md");
        std::fs::write(&sample, "# Local\n").expect("write sample");

        let (body, upload) =
            add_resource_payload_for_source(sample.to_str().expect("sample path"), &json!({}))
                .expect("payload");
        assert_eq!(body["source_name"], "sample file.md");
        assert_eq!(upload.as_deref(), Some(sample.as_path()));

        let uri = format!("file://{}", sample.to_string_lossy().replace(' ', "%20"));
        let (body, upload) = add_resource_payload_for_source(&uri, &json!({"reason": "file uri"}))
            .expect("file uri payload");
        assert_eq!(body["source_name"], "sample file.md");
        assert_eq!(body["reason"], "file uri");
        assert_eq!(upload.as_deref(), Some(sample.as_path()));
    }

    #[test]
    fn add_resource_payload_rejects_missing_local_path_and_to_parent_conflict() {
        let err = add_resource_payload_for_source("./definitely-missing-openviking.md", &json!({}))
            .expect_err("missing local path");
        assert!(err.contains("does not exist"));

        let err = add_resource_payload_for_source(
            "https://example.com/doc.md",
            &json!({"to": "viking://a", "parent": "viking://b"}),
        )
        .expect_err("to parent conflict");
        assert!(err.contains("Cannot specify both"));
    }

    #[test]
    fn add_resource_payload_zips_directory_and_skips_symlinks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let docs = tmp.path().join("docs");
        std::fs::create_dir_all(docs.join("nested")).expect("mkdir");
        std::fs::write(docs.join("guide.md"), "# Guide\n").expect("write guide");
        std::fs::write(docs.join("nested").join("api.md"), "# API\n").expect("write api");
        #[cfg(unix)]
        std::os::unix::fs::symlink(docs.join("guide.md"), docs.join("guide-link.md"))
            .expect("symlink");

        let (body, upload) =
            add_resource_payload_for_source(docs.to_str().expect("docs path"), &json!({}))
                .expect("payload");
        let zip_path = upload.expect("zip path");
        assert_eq!(body["source_name"], "docs");
        assert!(zip_path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("openviking_upload_")));

        let zip_file = std::fs::File::open(&zip_path).expect("open zip");
        let mut archive = zip::ZipArchive::new(zip_file).expect("zip archive");
        let mut names = Vec::new();
        for idx in 0..archive.len() {
            names.push(archive.by_index(idx).expect("zip entry").name().to_string());
        }
        assert!(names.contains(&"guide.md".to_string()));
        assert!(names.contains(&"nested/api.md".to_string()));
        assert!(!names.contains(&"guide-link.md".to_string()));

        std::fs::remove_file(zip_path).expect("cleanup zip");
    }
}
