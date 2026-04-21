//! SuperMemory memory provider plugin.
//!
//! Implements `MemoryProviderPlugin` for SuperMemory — semantic long-term memory
//! with profile recall, semantic search, explicit memory tools, turn capture,
//! and session-end conversation ingest.
//!
//! Mirrors the Python `plugins/memory/supermemory/__init__.py` at capability
//! level while using direct HTTP requests.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::Method;
use serde_json::{json, Value};

use crate::memory_manager::MemoryProviderPlugin;

const DEFAULT_CONTAINER_TAG: &str = "hermes";
const DEFAULT_MAX_RECALL: usize = 10;
const DEFAULT_BASE_URL: &str = "https://api.supermemory.ai";

// ---------------------------------------------------------------------------
// Tool schemas
// ---------------------------------------------------------------------------

fn store_schema() -> Value {
    json!({
        "name": "supermemory_store",
        "description": "Store an explicit memory for future recall.",
        "parameters": {
            "type": "object",
            "properties": {
                "content": {"type": "string", "description": "The memory content to store."},
                "metadata": {"type": "object", "description": "Optional metadata attached to the memory."},
                "container_tag": {"type": "string", "description": "Optional container tag override."}
            },
            "required": ["content"]
        }
    })
}

fn search_schema() -> Value {
    json!({
        "name": "supermemory_search",
        "description": "Search long-term memory by semantic similarity.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "What to search for."},
                "limit": {"type": "integer", "description": "Maximum results to return, 1 to 20."},
                "container_tag": {"type": "string", "description": "Optional container tag override."}
            },
            "required": ["query"]
        }
    })
}

fn forget_schema() -> Value {
    json!({
        "name": "supermemory_forget",
        "description": "Forget a memory by exact id or by best-match query.",
        "parameters": {
            "type": "object",
            "properties": {
                "id": {"type": "string", "description": "Exact memory id to delete."},
                "query": {"type": "string", "description": "Query used to find the memory to forget."},
                "container_tag": {"type": "string", "description": "Optional container tag override."}
            }
        }
    })
}

fn profile_schema() -> Value {
    json!({
        "name": "supermemory_profile",
        "description": "Retrieve persistent profile facts and recent memory context.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "Optional query to focus the profile response."},
                "container_tag": {"type": "string", "description": "Optional container tag override."}
            }
        }
    })
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct SupermemoryConfig {
    api_key: String,
    base_url: String,
    container_tag: String,
    auto_recall: bool,
    auto_capture: bool,
    max_recall_results: usize,
    search_mode: String,
    api_timeout: f64,
}

impl SupermemoryConfig {
    fn load(hermes_home: &str) -> Self {
        let mut config = Self {
            api_key: std::env::var("SUPERMEMORY_API_KEY").unwrap_or_default(),
            base_url: std::env::var("SUPERMEMORY_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_BASE_URL.to_string()),
            container_tag: std::env::var("SUPERMEMORY_CONTAINER_TAG")
                .unwrap_or_else(|_| DEFAULT_CONTAINER_TAG.to_string()),
            auto_recall: true,
            auto_capture: true,
            max_recall_results: DEFAULT_MAX_RECALL,
            search_mode: "hybrid".to_string(),
            api_timeout: 6.0,
        };

        let config_path = std::path::Path::new(hermes_home).join("supermemory.json");
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            if let Ok(raw) = serde_json::from_str::<Value>(&content) {
                if let Some(tag) = raw.get("container_tag").and_then(|v| v.as_str()) {
                    if !tag.is_empty() {
                        config.container_tag = sanitize_tag(tag);
                    }
                }
                if let Some(base_url) = raw.get("base_url").and_then(|v| v.as_str()) {
                    if !base_url.is_empty() {
                        config.base_url = base_url.to_string();
                    }
                }
                if let Some(ar) = raw.get("auto_recall").and_then(|v| v.as_bool()) {
                    config.auto_recall = ar;
                }
                if let Some(ac) = raw.get("auto_capture").and_then(|v| v.as_bool()) {
                    config.auto_capture = ac;
                }
                if let Some(mr) = raw.get("max_recall_results").and_then(|v| v.as_u64()) {
                    config.max_recall_results = mr.clamp(1, 20) as usize;
                }
                if let Some(mode) = raw.get("search_mode").and_then(|v| v.as_str()) {
                    if ["hybrid", "memories", "documents"].contains(&mode) {
                        config.search_mode = mode.to_string();
                    }
                }
                if let Some(timeout) = raw
                    .get("api_timeout")
                    .or_else(|| raw.get("timeout"))
                    .and_then(|v| v.as_f64())
                {
                    config.api_timeout = timeout.clamp(1.0, 60.0);
                }
            }
        }
        config.base_url = config.base_url.trim_end_matches('/').to_string();
        config
    }
}

fn sanitize_tag(raw: &str) -> String {
    let tag: String = raw
        .chars()
        .map(|c| {
            if c.is_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let tag = tag.trim_matches('_').to_string();
    if tag.is_empty() {
        DEFAULT_CONTAINER_TAG.to_string()
    } else {
        tag
    }
}

fn format_prefetch_context(
    static_facts: &[String],
    dynamic_facts: &[String],
    search_results: &[Value],
) -> String {
    let mut sections = Vec::new();
    if !static_facts.is_empty() {
        sections.push(format!(
            "## User Profile (Persistent)\n{}",
            static_facts
                .iter()
                .map(|s| format!("- {s}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if !dynamic_facts.is_empty() {
        sections.push(format!(
            "## Recent Context\n{}",
            dynamic_facts
                .iter()
                .map(|s| format!("- {s}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    let memories: Vec<String> = search_results
        .iter()
        .filter_map(|r| {
            r.get("memory")
                .or_else(|| r.get("content"))
                .or_else(|| r.get("text"))
                .and_then(|v| v.as_str())
                .map(|s| s.trim().to_string())
        })
        .filter(|s| !s.is_empty())
        .collect();
    if !memories.is_empty() {
        sections.push(format!(
            "## Relevant Memories\n{}",
            memories
                .iter()
                .map(|s| format!("- {s}"))
                .collect::<Vec<_>>()
                .join("\n")
        ));
    }
    if sections.is_empty() {
        String::new()
    } else {
        format!(
            "<supermemory-context>\nThe following is background context from long-term memory. Use it silently when relevant.\n\n{}\n</supermemory-context>",
            sections.join("\n\n")
        )
    }
}

// ---------------------------------------------------------------------------
// SupermemoryMemoryPlugin
// ---------------------------------------------------------------------------

/// SuperMemory semantic long-term memory with profile recall and search.
pub struct SupermemoryMemoryPlugin {
    config: Mutex<Option<SupermemoryConfig>>,
    session_id: Mutex<String>,
    turn_count: Mutex<u32>,
    active: Mutex<bool>,
    prefetch_result: Arc<Mutex<String>>,
}

impl SupermemoryMemoryPlugin {
    pub fn new() -> Self {
        Self {
            config: Mutex::new(None),
            session_id: Mutex::new(String::new()),
            turn_count: Mutex::new(0),
            active: Mutex::new(false),
            prefetch_result: Arc::new(Mutex::new(String::new())),
        }
    }

    fn config_snapshot(&self) -> Result<SupermemoryConfig, String> {
        self.config
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "Supermemory is not configured".to_string())
    }

    fn client(config: &SupermemoryConfig) -> Result<reqwest::blocking::Client, String> {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs_f64(config.api_timeout))
            .build()
            .map_err(|e| format!("Supermemory HTTP client build failed: {e}"))
    }

    fn build_url(config: &SupermemoryConfig, path: &str) -> String {
        format!(
            "{}/{}",
            config.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    fn send_json(
        config: &SupermemoryConfig,
        method: Method,
        path: &str,
        payload: Option<&Value>,
    ) -> Result<Value, String> {
        let client = Self::client(config)?;
        let url = Self::build_url(config, path);
        let mut req = client
            .request(method.clone(), &url)
            .header("Authorization", format!("Bearer {}", config.api_key))
            .header("X-API-Key", &config.api_key)
            .header("Content-Type", "application/json");
        if let Some(body) = payload {
            req = req.json(body);
        }
        let resp = req
            .send()
            .map_err(|e| format!("Supermemory request {} {} failed: {e}", method, url))?;
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "Supermemory request {} {} returned {}: {}",
                method, url, status, body
            ));
        }
        if body.trim().is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_str(&body)
            .map_err(|e| format!("Supermemory response parse error: {e}; body={body}"))
    }

    fn add_memory(
        config: &SupermemoryConfig,
        content: &str,
        metadata: Option<Value>,
        container_tag: Option<&str>,
    ) -> Result<Value, String> {
        let tag = container_tag.unwrap_or(&config.container_tag);
        let payload = json!({
            "content": content,
            "container_tags": [tag],
            "container_tag": tag,
            "metadata": metadata.unwrap_or_else(|| json!({})),
        });
        let mut last_error = String::new();
        for path in ["/v3/memories", "/v3/documents", "/v1/memories"] {
            match Self::send_json(config, Method::POST, path, Some(&payload)) {
                Ok(v) => return Ok(v),
                Err(e) => last_error = e,
            }
        }
        Err(last_error)
    }

    fn search_memories(
        config: &SupermemoryConfig,
        query: &str,
        limit: usize,
        container_tag: Option<&str>,
    ) -> Result<Vec<Value>, String> {
        let limit = limit.clamp(1, 20);
        let tag = container_tag.unwrap_or(&config.container_tag);
        let payload = json!({
            "q": query,
            "query": query,
            "container_tag": tag,
            "limit": limit,
            "search_mode": config.search_mode,
        });
        let mut last_error = String::new();
        for path in [
            "/v3/search/memories",
            "/v3/memories/search",
            "/v1/memory/search",
        ] {
            match Self::send_json(config, Method::POST, path, Some(&payload)) {
                Ok(v) => {
                    if let Some(arr) = v.get("results").and_then(|r| r.as_array()) {
                        return Ok(arr.clone());
                    }
                    if let Some(arr) = v.as_array() {
                        return Ok(arr.clone());
                    }
                    return Ok(Vec::new());
                }
                Err(e) => last_error = e,
            }
        }
        Err(last_error)
    }

    fn get_profile(
        config: &SupermemoryConfig,
        query: Option<&str>,
        container_tag: Option<&str>,
    ) -> Result<Value, String> {
        let tag = container_tag.unwrap_or(&config.container_tag);
        let payload = json!({
            "container_tag": tag,
            "q": query.unwrap_or("")
        });
        let mut last_error = String::new();
        for path in ["/v3/profile", "/v1/profile"] {
            if let Ok(v) = Self::send_json(config, Method::POST, path, Some(&payload)) {
                return Ok(v);
            }
            let get_path = format!("{}?container_tag={}&q={}", path, tag, query.unwrap_or(""));
            match Self::send_json(config, Method::GET, &get_path, None) {
                Ok(v) => return Ok(v),
                Err(e) => last_error = e,
            }
        }
        Err(last_error)
    }

    fn forget_memory(config: &SupermemoryConfig, memory_id: &str) -> Result<(), String> {
        let mut last_error = String::new();
        for path in [
            format!("/v3/memories/{memory_id}"),
            format!("/v1/memories/{memory_id}"),
        ] {
            match Self::send_json(config, Method::DELETE, &path, None) {
                Ok(_) => return Ok(()),
                Err(e) => last_error = e,
            }
        }
        Err(last_error)
    }

    fn ingest_conversation(
        config: &SupermemoryConfig,
        session_id: &str,
        messages: &[Value],
    ) -> Result<(), String> {
        let payload = json!({
            "conversationId": session_id,
            "messages": messages,
            "containerTags": [config.container_tag],
        });
        Self::send_json(config, Method::POST, "/v4/conversations", Some(&payload)).map(|_| ())
    }

    fn extract_profile_lists(profile: &Value) -> (Vec<String>, Vec<String>, Vec<Value>) {
        let static_facts = profile
            .pointer("/profile/static")
            .or_else(|| profile.get("static"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect::<Vec<_>>();

        let dynamic_facts = profile
            .pointer("/profile/dynamic")
            .or_else(|| profile.get("dynamic"))
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default()
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect::<Vec<_>>();

        let search_results = profile
            .get("search_results")
            .or_else(|| profile.get("searchResults"))
            .and_then(|v| {
                v.get("results")
                    .and_then(|r| r.as_array())
                    .cloned()
                    .or_else(|| v.as_array().cloned())
            })
            .unwrap_or_default();

        (static_facts, dynamic_facts, search_results)
    }
}

impl MemoryProviderPlugin for SupermemoryMemoryPlugin {
    fn name(&self) -> &str {
        "supermemory"
    }

    fn is_available(&self) -> bool {
        !std::env::var("SUPERMEMORY_API_KEY")
            .unwrap_or_default()
            .is_empty()
    }

    fn initialize(&self, session_id: &str, hermes_home: &str) {
        let config = SupermemoryConfig::load(hermes_home);
        let has_key = !config.api_key.is_empty();

        *self.session_id.lock().unwrap() = session_id.to_string();
        *self.turn_count.lock().unwrap() = 0;
        *self.active.lock().unwrap() = has_key;
        *self.config.lock().unwrap() = Some(config);
        *self.prefetch_result.lock().unwrap() = String::new();

        if has_key {
            tracing::info!("SuperMemory plugin initialized for session {}", session_id);
        }
    }

    fn system_prompt_block(&self) -> String {
        if !*self.active.lock().unwrap() {
            return String::new();
        }
        let tag = self
            .config
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| c.container_tag.clone())
            .unwrap_or_else(|| DEFAULT_CONTAINER_TAG.to_string());
        format!(
            "# Supermemory\n\
             Active. Container: {}.\n\
             Use supermemory_search, supermemory_store, supermemory_forget, and \
             supermemory_profile for explicit memory operations.",
            tag
        )
    }

    fn prefetch(&self, query: &str, _session_id: &str) -> String {
        if !*self.active.lock().unwrap() {
            return String::new();
        }
        let queued = {
            let mut lock = self.prefetch_result.lock().unwrap();
            let value = lock.clone();
            lock.clear();
            value
        };
        if !queued.is_empty() {
            return queued;
        }

        let query = query.trim();
        if query.is_empty() {
            return String::new();
        }
        let config = match self.config_snapshot() {
            Ok(c) => c,
            Err(_) => return String::new(),
        };
        if !config.auto_recall {
            return String::new();
        }

        match Self::get_profile(
            &config,
            Some(&query.chars().take(200).collect::<String>()),
            None,
        ) {
            Ok(profile) => {
                let (static_facts, dynamic_facts, search_results) =
                    Self::extract_profile_lists(&profile);
                format_prefetch_context(&static_facts, &dynamic_facts, &search_results)
            }
            Err(e) => {
                tracing::debug!("Supermemory prefetch failed: {}", e);
                String::new()
            }
        }
    }

    fn sync_turn(&self, user_content: &str, assistant_content: &str, _session_id: &str) {
        if !*self.active.lock().unwrap() {
            return;
        }
        let config = match self.config_snapshot() {
            Ok(c) => c,
            Err(_) => return,
        };
        if !config.auto_capture {
            return;
        }
        let clean_user = user_content.trim();
        let clean_assistant = assistant_content.trim();
        if clean_user.is_empty() || clean_assistant.is_empty() {
            return;
        }
        let payload = format!(
            "[role: user]\n{}\n[user:end]\n\n[role: assistant]\n{}\n[assistant:end]",
            clean_user.chars().take(2500).collect::<String>(),
            clean_assistant.chars().take(2500).collect::<String>()
        );
        let metadata = json!({"source":"hermes", "type":"conversation_turn"});
        std::thread::spawn(move || {
            if let Err(e) = Self::add_memory(&config, &payload, Some(metadata), None) {
                tracing::debug!("Supermemory sync_turn failed: {}", e);
            }
        });
    }

    fn get_tool_schemas(&self) -> Vec<Value> {
        vec![
            store_schema(),
            search_schema(),
            forget_schema(),
            profile_schema(),
        ]
    }

    fn handle_tool_call(&self, tool_name: &str, args: &Value) -> String {
        if !*self.active.lock().unwrap() {
            return json!({"error": "Supermemory is not configured"}).to_string();
        }
        let config = match self.config_snapshot() {
            Ok(c) => c,
            Err(e) => return json!({"error": e}).to_string(),
        };
        let container_tag = args.get("container_tag").and_then(|v| v.as_str());

        match tool_name {
            "supermemory_store" => {
                let content = args
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if content.is_empty() {
                    return json!({"error": "content is required"}).to_string();
                }
                let metadata = args.get("metadata").cloned().filter(|v| v.is_object());
                match Self::add_memory(&config, content, metadata, container_tag) {
                    Ok(v) => {
                        let preview = content.chars().take(80).collect::<String>();
                        json!({"saved": true, "preview": preview, "raw": v}).to_string()
                    }
                    Err(e) => json!({"error": e}).to_string(),
                }
            }
            "supermemory_search" => {
                let query = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if query.is_empty() {
                    return json!({"error": "query is required"}).to_string();
                }
                let limit =
                    args.get("limit")
                        .and_then(|v| v.as_u64())
                        .unwrap_or(config.max_recall_results as u64) as usize;
                match Self::search_memories(&config, query, limit, container_tag) {
                    Ok(results) => json!({"results": results, "count": results.len()}).to_string(),
                    Err(e) => json!({"error": e}).to_string(),
                }
            }
            "supermemory_forget" => {
                let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("").trim();
                let query = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if id.is_empty() && query.is_empty() {
                    return json!({"error": "Provide either id or query"}).to_string();
                }
                if !id.is_empty() {
                    return match Self::forget_memory(&config, id) {
                        Ok(()) => json!({"forgotten": true, "id": id}).to_string(),
                        Err(e) => json!({"error": e}).to_string(),
                    };
                }
                match Self::search_memories(&config, query, 5, container_tag) {
                    Ok(results) => {
                        let target_id = results
                            .iter()
                            .find_map(|r| r.get("id").and_then(|v| v.as_str()))
                            .unwrap_or("")
                            .to_string();
                        if target_id.is_empty() {
                            return json!({"error": "No matching memory id found"}).to_string();
                        }
                        match Self::forget_memory(&config, &target_id) {
                            Ok(()) => json!({"forgotten": true, "id": target_id}).to_string(),
                            Err(e) => json!({"error": e}).to_string(),
                        }
                    }
                    Err(e) => json!({"error": e}).to_string(),
                }
            }
            "supermemory_profile" => {
                let query = args.get("query").and_then(|v| v.as_str());
                match Self::get_profile(&config, query, container_tag) {
                    Ok(profile) => {
                        let (static_facts, dynamic_facts, search_results) =
                            Self::extract_profile_lists(&profile);
                        json!({
                            "profile": profile,
                            "static_count": static_facts.len(),
                            "dynamic_count": dynamic_facts.len(),
                            "search_count": search_results.len()
                        })
                        .to_string()
                    }
                    Err(e) => json!({"error": e}).to_string(),
                }
            }
            _ => json!({"error": format!("Unknown tool: {}", tool_name)}).to_string(),
        }
    }

    fn on_turn_start(&self, turn_number: u32, _message: &str) {
        *self.turn_count.lock().unwrap() = turn_number;
    }

    fn on_session_end(&self, messages: &[Value]) {
        if !*self.active.lock().unwrap() {
            return;
        }
        if messages.is_empty() {
            return;
        }
        let config = match self.config_snapshot() {
            Ok(c) => c,
            Err(_) => return,
        };
        let session_id = self.session_id.lock().unwrap().clone();
        let cleaned: Vec<Value> = messages
            .iter()
            .filter_map(|m| {
                let role = m.get("role").and_then(|v| v.as_str())?;
                if role != "user" && role != "assistant" {
                    return None;
                }
                let content = m
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim()
                    .to_string();
                if content.is_empty() {
                    return None;
                }
                Some(json!({"role": role, "content": content.chars().take(3000).collect::<String>()}))
            })
            .collect();
        if cleaned.is_empty() {
            return;
        }

        std::thread::spawn(move || {
            if let Err(e) = Self::ingest_conversation(&config, &session_id, &cleaned) {
                tracing::debug!("Supermemory session ingest failed: {}", e);
            }
        });
    }

    fn on_memory_write(&self, action: &str, _target: &str, content: &str) {
        if action != "add" || content.trim().is_empty() {
            return;
        }
        if !*self.active.lock().unwrap() {
            return;
        }
        let config = match self.config_snapshot() {
            Ok(c) => c,
            Err(_) => return,
        };
        let content = content.trim().to_string();
        std::thread::spawn(move || {
            let metadata = json!({"source":"hermes_memory", "type":"explicit_memory"});
            if let Err(e) = Self::add_memory(&config, &content, Some(metadata), None) {
                tracing::debug!("Supermemory memory mirror failed: {}", e);
            }
        });
    }

    fn shutdown(&self) {
        tracing::debug!("SuperMemory plugin shutdown");
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key": "api_key", "description": "Supermemory API key", "secret": true, "required": true, "env_var": "SUPERMEMORY_API_KEY", "url": "https://supermemory.ai"},
            {"key": "base_url", "description": "Supermemory API base URL", "default": DEFAULT_BASE_URL},
            {"key": "container_tag", "description": "Container tag", "default": DEFAULT_CONTAINER_TAG},
            {"key": "search_mode", "description": "hybrid|memories|documents", "default": "hybrid"}
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_supermemory_plugin_name() {
        let plugin = SupermemoryMemoryPlugin::new();
        assert_eq!(plugin.name(), "supermemory");
    }

    #[test]
    fn test_supermemory_tool_schemas() {
        let plugin = SupermemoryMemoryPlugin::new();
        let schemas = plugin.get_tool_schemas();
        assert_eq!(schemas.len(), 4);
    }

    #[test]
    fn test_sanitize_tag() {
        assert_eq!(sanitize_tag("my-tag"), "my_tag");
        assert_eq!(sanitize_tag("hello world"), "hello_world");
        assert_eq!(sanitize_tag("___"), DEFAULT_CONTAINER_TAG);
        assert_eq!(sanitize_tag("valid_tag"), "valid_tag");
    }

    #[test]
    fn test_format_prefetch_context() {
        let out = format_prefetch_context(
            &["prefers rust".to_string()],
            &["working on parity".to_string()],
            &[json!({"memory":"commit tests"})],
        );
        assert!(out.contains("User Profile"));
        assert!(out.contains("Relevant Memories"));
    }
}
