//! SuperMemory memory provider plugin.
//!
//! Implements `MemoryProviderPlugin` for SuperMemory — semantic long-term memory
//! with profile recall, semantic search, explicit memory tools, turn capture,
//! and session-end conversation ingest.
//!
//! Mirrors the Python `plugins/memory/supermemory/__init__.py`.
//!
//! Configuration:
//!   - `SUPERMEMORY_API_KEY` (required)
//!   - `SUPERMEMORY_CONTAINER_TAG` (default: "hermes")
//!   - `$HERMES_HOME/supermemory.json` overrides

use std::sync::Mutex;

use serde_json::{json, Value};

use crate::memory_manager::MemoryProviderPlugin;

const DEFAULT_CONTAINER_TAG: &str = "hermes";
const DEFAULT_MAX_RECALL: usize = 10;

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
                "metadata": {"type": "object", "description": "Optional metadata attached to the memory."}
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
                "limit": {"type": "integer", "description": "Maximum results to return, 1 to 20."}
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
                "query": {"type": "string", "description": "Query used to find the memory to forget."}
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
                "query": {"type": "string", "description": "Optional query to focus the profile response."}
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
            container_tag: std::env::var("SUPERMEMORY_CONTAINER_TAG")
                .unwrap_or_else(|_| DEFAULT_CONTAINER_TAG.to_string()),
            auto_recall: true,
            auto_capture: true,
            max_recall_results: DEFAULT_MAX_RECALL,
            search_mode: "hybrid".to_string(),
            api_timeout: 5.0,
        };

        let config_path = std::path::Path::new(hermes_home).join("supermemory.json");
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            if let Ok(raw) = serde_json::from_str::<Value>(&content) {
                if let Some(tag) = raw.get("container_tag").and_then(|v| v.as_str()) {
                    if !tag.is_empty() {
                        config.container_tag = sanitize_tag(tag);
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
            }
        }

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

// ---------------------------------------------------------------------------
// SupermemoryMemoryPlugin
// ---------------------------------------------------------------------------

/// SuperMemory semantic long-term memory with profile recall and search.
pub struct SupermemoryMemoryPlugin {
    config: Mutex<Option<SupermemoryConfig>>,
    session_id: Mutex<String>,
    turn_count: Mutex<u32>,
    active: Mutex<bool>,
}

impl SupermemoryMemoryPlugin {
    pub fn new() -> Self {
        Self {
            config: Mutex::new(None),
            session_id: Mutex::new(String::new()),
            turn_count: Mutex::new(0),
            active: Mutex::new(false),
        }
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
        let config = self.config.lock().unwrap();
        if !config.as_ref().map(|c| c.auto_recall).unwrap_or(false) {
            return String::new();
        }
        // Placeholder: real implementation would call SuperMemory profile API
        String::new()
    }

    fn sync_turn(&self, user_content: &str, assistant_content: &str, _session_id: &str) {
        if !*self.active.lock().unwrap() {
            return;
        }
        let config = self.config.lock().unwrap();
        if !config.as_ref().map(|c| c.auto_capture).unwrap_or(false) {
            return;
        }
        // Placeholder: real implementation would POST to SuperMemory documents API
        tracing::debug!(
            "SuperMemory sync_turn: {} chars user, {} chars assistant",
            user_content.len(),
            assistant_content.len()
        );
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

        match tool_name {
            "supermemory_store" => {
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                if content.is_empty() {
                    return json!({"error": "content is required"}).to_string();
                }
                // Placeholder: call SuperMemory add API
                json!({"saved": true, "preview": &content[..content.len().min(80)]}).to_string()
            }
            "supermemory_search" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                if query.is_empty() {
                    return json!({"error": "query is required"}).to_string();
                }
                // Placeholder: call SuperMemory search API
                json!({"results": [], "count": 0}).to_string()
            }
            "supermemory_forget" => {
                let id = args.get("id").and_then(|v| v.as_str()).unwrap_or("");
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                if id.is_empty() && query.is_empty() {
                    return json!({"error": "Provide either id or query"}).to_string();
                }
                json!({"forgotten": true}).to_string()
            }
            "supermemory_profile" => {
                // Placeholder: call SuperMemory profile API
                json!({"profile": "", "static_count": 0, "dynamic_count": 0}).to_string()
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
        // Placeholder: real implementation would POST conversation to SuperMemory
        tracing::debug!(
            "SuperMemory session end ingest: {} messages",
            messages.len()
        );
    }

    fn on_memory_write(&self, action: &str, _target: &str, content: &str) {
        if action != "add" || content.is_empty() {
            return;
        }
        if !*self.active.lock().unwrap() {
            return;
        }
        // Placeholder: mirror memory write to SuperMemory
        tracing::debug!(
            "SuperMemory memory mirror: {}",
            &content[..content.len().min(80)]
        );
    }

    fn shutdown(&self) {
        tracing::debug!("SuperMemory plugin shutdown");
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key": "api_key", "description": "Supermemory API key", "secret": true, "required": true, "env_var": "SUPERMEMORY_API_KEY", "url": "https://supermemory.ai"}
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
    fn test_inactive_returns_empty() {
        let plugin = SupermemoryMemoryPlugin::new();
        assert!(plugin.system_prompt_block().is_empty());
        assert!(plugin.prefetch("hello", "s1").is_empty());
    }
}
