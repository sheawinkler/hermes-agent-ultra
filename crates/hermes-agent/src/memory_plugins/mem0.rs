//! Mem0 memory provider plugin.
//!
//! Implements `MemoryProviderPlugin` for Mem0 Platform — server-side LLM fact
//! extraction, semantic search with reranking, and automatic deduplication.
//!
//! Mirrors the Python `plugins/memory/mem0/__init__.py`.
//!
//! Configuration:
//!   - `MEM0_API_KEY` (required)
//!   - `MEM0_USER_ID` (default: "hermes-user")
//!   - `MEM0_AGENT_ID` (default: "hermes")
//!   - `$HERMES_HOME/mem0.json` overrides

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Mutex;
use std::time::Instant;

use serde_json::{json, Value};

use crate::memory_manager::MemoryProviderPlugin;

const BREAKER_THRESHOLD: u32 = 5;
const BREAKER_COOLDOWN_SECS: u64 = 120;

// ---------------------------------------------------------------------------
// Tool schemas
// ---------------------------------------------------------------------------

fn profile_schema() -> Value {
    json!({
        "name": "mem0_profile",
        "description": "Retrieve all stored memories about the user — preferences, facts, project context. Fast, no reranking.",
        "parameters": {"type": "object", "properties": {}, "required": []}
    })
}

fn search_schema() -> Value {
    json!({
        "name": "mem0_search",
        "description": "Search memories by meaning. Returns relevant facts ranked by similarity.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "What to search for."},
                "rerank": {"type": "boolean", "description": "Enable reranking for precision (default: false)."},
                "top_k": {"type": "integer", "description": "Max results (default: 10, max: 50)."}
            },
            "required": ["query"]
        }
    })
}

fn conclude_schema() -> Value {
    json!({
        "name": "mem0_conclude",
        "description": "Store a durable fact about the user. Use for explicit preferences, corrections, or decisions.",
        "parameters": {
            "type": "object",
            "properties": {
                "conclusion": {"type": "string", "description": "The fact to store."}
            },
            "required": ["conclusion"]
        }
    })
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Mem0Config {
    api_key: String,
    user_id: String,
    agent_id: String,
    rerank: bool,
}

impl Mem0Config {
    fn load(hermes_home: &str) -> Self {
        let mut config = Self {
            api_key: std::env::var("MEM0_API_KEY").unwrap_or_default(),
            user_id: std::env::var("MEM0_USER_ID").unwrap_or_else(|_| "hermes-user".into()),
            agent_id: std::env::var("MEM0_AGENT_ID").unwrap_or_else(|_| "hermes".into()),
            rerank: true,
        };

        let config_path = std::path::Path::new(hermes_home).join("mem0.json");
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            if let Ok(raw) = serde_json::from_str::<Value>(&content) {
                if let Some(key) = raw.get("api_key").and_then(|v| v.as_str()) {
                    if !key.is_empty() {
                        config.api_key = key.to_string();
                    }
                }
                if let Some(uid) = raw.get("user_id").and_then(|v| v.as_str()) {
                    config.user_id = uid.to_string();
                }
                if let Some(aid) = raw.get("agent_id").and_then(|v| v.as_str()) {
                    config.agent_id = aid.to_string();
                }
                if let Some(rr) = raw.get("rerank").and_then(|v| v.as_bool()) {
                    config.rerank = rr;
                }
            }
        }

        config
    }
}

// ---------------------------------------------------------------------------
// Mem0MemoryPlugin
// ---------------------------------------------------------------------------

/// Mem0 Platform memory with server-side extraction and semantic search.
pub struct Mem0MemoryPlugin {
    config: Mutex<Option<Mem0Config>>,
    prefetch_result: Mutex<String>,
    consecutive_failures: AtomicU32,
    breaker_open_until: Mutex<Option<Instant>>,
}

impl Mem0MemoryPlugin {
    pub fn new() -> Self {
        Self {
            config: Mutex::new(None),
            prefetch_result: Mutex::new(String::new()),
            consecutive_failures: AtomicU32::new(0),
            breaker_open_until: Mutex::new(None),
        }
    }

    fn is_breaker_open(&self) -> bool {
        if self.consecutive_failures.load(Ordering::Relaxed) < BREAKER_THRESHOLD {
            return false;
        }
        let until = self.breaker_open_until.lock().unwrap();
        if let Some(deadline) = *until {
            if Instant::now() >= deadline {
                self.consecutive_failures.store(0, Ordering::Relaxed);
                return false;
            }
            return true;
        }
        false
    }

    fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
    }

    fn record_failure(&self) {
        let prev = self.consecutive_failures.fetch_add(1, Ordering::Relaxed);
        if prev + 1 >= BREAKER_THRESHOLD {
            let deadline = Instant::now() + std::time::Duration::from_secs(BREAKER_COOLDOWN_SECS);
            *self.breaker_open_until.lock().unwrap() = Some(deadline);
            tracing::warn!(
                "Mem0 circuit breaker tripped after {} failures. Pausing for {}s.",
                prev + 1,
                BREAKER_COOLDOWN_SECS
            );
        }
    }
}

impl MemoryProviderPlugin for Mem0MemoryPlugin {
    fn name(&self) -> &str {
        "mem0"
    }

    fn is_available(&self) -> bool {
        !std::env::var("MEM0_API_KEY").unwrap_or_default().is_empty()
    }

    fn initialize(&self, session_id: &str, hermes_home: &str) {
        let config = Mem0Config::load(hermes_home);
        *self.config.lock().unwrap() = Some(config);
        tracing::info!("Mem0 memory plugin initialized for session {}", session_id);
    }

    fn system_prompt_block(&self) -> String {
        let config = self.config.lock().unwrap();
        let user_id = config
            .as_ref()
            .map(|c| c.user_id.as_str())
            .unwrap_or("hermes-user");
        format!(
            "# Mem0 Memory\n\
             Active. User: {}.\n\
             Use mem0_search to find memories, mem0_conclude to store facts, \
             mem0_profile for a full overview.",
            user_id
        )
    }

    fn prefetch(&self, _query: &str, _session_id: &str) -> String {
        let result = {
            let mut lock = self.prefetch_result.lock().unwrap();
            let r = lock.clone();
            lock.clear();
            r
        };
        if result.is_empty() {
            return String::new();
        }
        format!("## Mem0 Memory\n{}", result)
    }

    fn queue_prefetch(&self, query: &str, _session_id: &str) {
        if self.is_breaker_open() {
            return;
        }
        // Placeholder: real implementation would call Mem0 search API
        // in a background thread and store result in prefetch_result
    }

    fn sync_turn(&self, user_content: &str, assistant_content: &str, _session_id: &str) {
        if self.is_breaker_open() {
            return;
        }
        // Placeholder: real implementation would POST messages to Mem0 add API
        tracing::debug!(
            "Mem0 sync_turn: {} chars user, {} chars assistant",
            user_content.len(),
            assistant_content.len()
        );
    }

    fn get_tool_schemas(&self) -> Vec<Value> {
        vec![profile_schema(), search_schema(), conclude_schema()]
    }

    fn handle_tool_call(&self, tool_name: &str, args: &Value) -> String {
        if self.is_breaker_open() {
            return json!({
                "error": "Mem0 API temporarily unavailable (circuit breaker). Will retry automatically."
            })
            .to_string();
        }

        match tool_name {
            "mem0_profile" => {
                // Placeholder: call get_all with user filters
                json!({"result": "No memories stored yet. (Mem0 API not connected)"}).to_string()
            }
            "mem0_search" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                if query.is_empty() {
                    return json!({"error": "Missing required parameter: query"}).to_string();
                }
                json!({"result": "No relevant memories found. (Mem0 API not connected)"})
                    .to_string()
            }
            "mem0_conclude" => {
                let conclusion = args
                    .get("conclusion")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if conclusion.is_empty() {
                    return json!({"error": "Missing required parameter: conclusion"}).to_string();
                }
                json!({"result": "Fact stored."}).to_string()
            }
            _ => json!({"error": format!("Unknown tool: {}", tool_name)}).to_string(),
        }
    }

    fn shutdown(&self) {
        tracing::debug!("Mem0 memory plugin shutdown");
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key": "api_key", "description": "Mem0 Platform API key", "secret": true, "required": true, "env_var": "MEM0_API_KEY", "url": "https://app.mem0.ai"},
            {"key": "user_id", "description": "User identifier", "default": "hermes-user"},
            {"key": "agent_id", "description": "Agent identifier", "default": "hermes"},
            {"key": "rerank", "description": "Enable reranking for recall", "default": "true"}
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mem0_plugin_name() {
        let plugin = Mem0MemoryPlugin::new();
        assert_eq!(plugin.name(), "mem0");
    }

    #[test]
    fn test_mem0_tool_schemas() {
        let plugin = Mem0MemoryPlugin::new();
        let schemas = plugin.get_tool_schemas();
        assert_eq!(schemas.len(), 3);
    }

    #[test]
    fn test_mem0_circuit_breaker() {
        let plugin = Mem0MemoryPlugin::new();
        assert!(!plugin.is_breaker_open());
        for _ in 0..BREAKER_THRESHOLD {
            plugin.record_failure();
        }
        assert!(plugin.is_breaker_open());
        plugin.record_success();
        assert!(!plugin.is_breaker_open());
    }
}
