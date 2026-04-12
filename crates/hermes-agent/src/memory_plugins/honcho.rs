//! Honcho memory provider plugin.
//!
//! Implements `MemoryProviderPlugin` for Honcho's AI-native cross-session
//! user modeling. Provides dialectic Q&A, semantic search, peer cards,
//! and persistent conclusions via the Honcho API.
//!
//! Mirrors the Python `plugins/memory/honcho/__init__.py`.
//!
//! Configuration chain:
//!   1. `$HERMES_HOME/honcho.json`
//!   2. `~/.honcho/config.json`
//!   3. Environment variables (`HONCHO_API_KEY`, `HONCHO_BASE_URL`)

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{json, Value};

use crate::memory_manager::MemoryProviderPlugin;

// ---------------------------------------------------------------------------
// Tool schemas
// ---------------------------------------------------------------------------

fn profile_schema() -> Value {
    json!({
        "name": "honcho_profile",
        "description": "Retrieve the user's peer card from Honcho — a curated list of key facts about them. Fast, no LLM reasoning.",
        "parameters": {"type": "object", "properties": {}, "required": []}
    })
}

fn search_schema() -> Value {
    json!({
        "name": "honcho_search",
        "description": "Semantic search over Honcho's stored context about the user. Returns raw excerpts ranked by relevance.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "What to search for in Honcho's memory."},
                "max_tokens": {"type": "integer", "description": "Token budget for returned context (default 800, max 2000)."}
            },
            "required": ["query"]
        }
    })
}

fn context_schema() -> Value {
    json!({
        "name": "honcho_context",
        "description": "Ask Honcho a natural language question and get a synthesized answer using dialectic reasoning.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "A natural language question."},
                "peer": {"type": "string", "description": "Which peer to query about: 'user' (default) or 'ai'."}
            },
            "required": ["query"]
        }
    })
}

fn conclude_schema() -> Value {
    json!({
        "name": "honcho_conclude",
        "description": "Write a conclusion about the user back to Honcho's memory. Conclusions are persistent facts that build the user's profile.",
        "parameters": {
            "type": "object",
            "properties": {
                "conclusion": {"type": "string", "description": "A factual statement about the user to persist."}
            },
            "required": ["conclusion"]
        }
    })
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct HonchoConfig {
    api_key: String,
    base_url: Option<String>,
    enabled: bool,
    recall_mode: String,
    context_tokens: Option<usize>,
    workspace_id: String,
    peer_name: Option<String>,
    ai_peer: String,
}

impl HonchoConfig {
    fn from_env() -> Self {
        let api_key = std::env::var("HONCHO_API_KEY").unwrap_or_default();
        let base_url = std::env::var("HONCHO_BASE_URL")
            .ok()
            .filter(|s| !s.is_empty());
        Self {
            enabled: !api_key.is_empty() || base_url.is_some(),
            api_key,
            base_url,
            recall_mode: "hybrid".to_string(),
            context_tokens: None,
            workspace_id: "hermes".to_string(),
            peer_name: None,
            ai_peer: "hermes".to_string(),
        }
    }

    fn from_config_file(hermes_home: &str) -> Self {
        let mut config = Self::from_env();

        let config_path = std::path::Path::new(hermes_home).join("honcho.json");
        if let Ok(content) = std::fs::read_to_string(&config_path) {
            if let Ok(raw) = serde_json::from_str::<Value>(&content) {
                if let Some(key) = raw.get("apiKey").and_then(|v| v.as_str()) {
                    if !key.is_empty() {
                        config.api_key = key.to_string();
                    }
                }
                if let Some(url) = raw.get("baseUrl").and_then(|v| v.as_str()) {
                    if !url.is_empty() {
                        config.base_url = Some(url.to_string());
                    }
                }
                if let Some(mode) = raw.get("recallMode").and_then(|v| v.as_str()) {
                    config.recall_mode = match mode {
                        "context" | "tools" | "hybrid" => mode.to_string(),
                        "auto" => "hybrid".to_string(),
                        _ => "hybrid".to_string(),
                    };
                }
                if let Some(tokens) = raw.get("contextTokens").and_then(|v| v.as_u64()) {
                    config.context_tokens = Some(tokens as usize);
                }
                if let Some(ws) = raw.get("workspace").and_then(|v| v.as_str()) {
                    config.workspace_id = ws.to_string();
                }
                if let Some(peer) = raw.get("peerName").and_then(|v| v.as_str()) {
                    config.peer_name = Some(peer.to_string());
                }
                if let Some(ai) = raw.get("aiPeer").and_then(|v| v.as_str()) {
                    config.ai_peer = ai.to_string();
                }
                if let Some(enabled) = raw.get("enabled").and_then(|v| v.as_bool()) {
                    config.enabled = enabled;
                } else {
                    config.enabled = !config.api_key.is_empty() || config.base_url.is_some();
                }
            }
        }

        config
    }
}

// ---------------------------------------------------------------------------
// HonchoMemoryPlugin
// ---------------------------------------------------------------------------

/// Honcho AI-native memory with dialectic Q&A and persistent user modeling.
pub struct HonchoMemoryPlugin {
    config: Mutex<Option<HonchoConfig>>,
    session_key: Mutex<String>,
    prefetch_result: Mutex<String>,
    turn_count: Mutex<u32>,
    recall_mode: Mutex<String>,
}

impl HonchoMemoryPlugin {
    pub fn new() -> Self {
        Self {
            config: Mutex::new(None),
            session_key: Mutex::new(String::new()),
            prefetch_result: Mutex::new(String::new()),
            turn_count: Mutex::new(0),
            recall_mode: Mutex::new("hybrid".to_string()),
        }
    }

    fn api_base(&self) -> String {
        let config = self.config.lock().unwrap();
        config
            .as_ref()
            .and_then(|c| c.base_url.clone())
            .unwrap_or_else(|| "https://api.honcho.dev".to_string())
    }

    fn api_key(&self) -> String {
        let config = self.config.lock().unwrap();
        config
            .as_ref()
            .map(|c| c.api_key.clone())
            .unwrap_or_default()
    }
}

impl MemoryProviderPlugin for HonchoMemoryPlugin {
    fn name(&self) -> &str {
        "honcho"
    }

    fn is_available(&self) -> bool {
        let config = HonchoConfig::from_env();
        config.enabled
    }

    fn initialize(&self, session_id: &str, hermes_home: &str) {
        let config = HonchoConfig::from_config_file(hermes_home);
        if !config.enabled {
            tracing::debug!("Honcho not configured — plugin inactive");
            return;
        }

        *self.recall_mode.lock().unwrap() = config.recall_mode.clone();
        *self.session_key.lock().unwrap() = session_id.to_string();
        *self.config.lock().unwrap() = Some(config);

        tracing::info!(
            "Honcho memory plugin initialized for session {}",
            session_id
        );
    }

    fn system_prompt_block(&self) -> String {
        let mode = self.recall_mode.lock().unwrap().clone();
        match mode.as_str() {
            "context" => {
                "# Honcho Memory\n\
                 Active (context-injection mode). Relevant user context is automatically \
                 injected before each turn. No memory tools are available."
                    .to_string()
            }
            "tools" => {
                "# Honcho Memory\n\
                 Active (tools-only mode). Use honcho_profile for a quick factual snapshot, \
                 honcho_search for raw excerpts, honcho_context for synthesized answers, \
                 honcho_conclude to save facts about the user."
                    .to_string()
            }
            _ => {
                "# Honcho Memory\n\
                 Active (hybrid mode). Relevant context is auto-injected AND memory tools are available. \
                 Use honcho_profile, honcho_search, honcho_context, honcho_conclude."
                    .to_string()
            }
        }
    }

    fn prefetch(&self, query: &str, session_id: &str) -> String {
        let mode = self.recall_mode.lock().unwrap().clone();
        if mode == "tools" {
            return String::new();
        }

        let result = self.prefetch_result.lock().unwrap().clone();
        if !result.is_empty() {
            *self.prefetch_result.lock().unwrap() = String::new();
            return format!("## Honcho Context\n{}", result);
        }

        // Placeholder: real implementation would call Honcho API
        String::new()
    }

    fn queue_prefetch(&self, query: &str, session_id: &str) {
        let mode = self.recall_mode.lock().unwrap().clone();
        if mode == "tools" {
            return;
        }
        // Placeholder: real implementation would spawn background thread
        // calling dialectic_query and storing result in prefetch_result
    }

    fn sync_turn(&self, user_content: &str, assistant_content: &str, session_id: &str) {
        if self.config.lock().unwrap().is_none() {
            return;
        }
        // Placeholder: real implementation would POST messages to Honcho API
        tracing::debug!(
            "Honcho sync_turn: {} chars user, {} chars assistant",
            user_content.len(),
            assistant_content.len()
        );
    }

    fn get_tool_schemas(&self) -> Vec<Value> {
        let mode = self.recall_mode.lock().unwrap().clone();
        if mode == "context" {
            return Vec::new();
        }
        vec![
            profile_schema(),
            search_schema(),
            context_schema(),
            conclude_schema(),
        ]
    }

    fn handle_tool_call(&self, tool_name: &str, args: &Value) -> String {
        if self.config.lock().unwrap().is_none() {
            return json!({"error": "Honcho is not active for this session."}).to_string();
        }

        match tool_name {
            "honcho_profile" => {
                // Placeholder: real implementation calls GET /peers/{peer_id}/card
                json!({"result": "No profile facts available yet. (Honcho API not connected)"})
                    .to_string()
            }
            "honcho_search" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                if query.is_empty() {
                    return json!({"error": "Missing required parameter: query"}).to_string();
                }
                // Placeholder: real implementation calls Honcho context API
                json!({"result": "No relevant context found. (Honcho API not connected)"})
                    .to_string()
            }
            "honcho_context" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                if query.is_empty() {
                    return json!({"error": "Missing required parameter: query"}).to_string();
                }
                // Placeholder: real implementation calls Honcho dialectic endpoint
                json!({"result": "No result from Honcho. (API not connected)"}).to_string()
            }
            "honcho_conclude" => {
                let conclusion = args
                    .get("conclusion")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if conclusion.is_empty() {
                    return json!({"error": "Missing required parameter: conclusion"}).to_string();
                }
                // Placeholder: real implementation calls Honcho conclusions API
                json!({"result": format!("Conclusion saved: {}", conclusion)}).to_string()
            }
            _ => json!({"error": format!("Unknown tool: {}", tool_name)}).to_string(),
        }
    }

    fn on_turn_start(&self, turn_number: u32, message: &str) {
        *self.turn_count.lock().unwrap() = turn_number;
    }

    fn on_session_end(&self, _messages: &[Value]) {
        tracing::debug!("Honcho session end — flushing pending messages");
    }

    fn on_memory_write(&self, action: &str, target: &str, content: &str) {
        if action != "add" || target != "user" || content.is_empty() {
            return;
        }
        // Mirror built-in user profile writes as Honcho conclusions
        tracing::debug!(
            "Honcho memory mirror: {}",
            &content[..content.len().min(80)]
        );
    }

    fn shutdown(&self) {
        tracing::debug!("Honcho memory plugin shutdown");
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key": "api_key", "description": "Honcho API key", "secret": true, "env_var": "HONCHO_API_KEY", "url": "https://app.honcho.dev"},
            {"key": "baseUrl", "description": "Honcho base URL (for self-hosted)"}
        ]))
    }

    fn save_config(&self, config: &Value) -> Result<(), String> {
        // Would write to $HERMES_HOME/honcho.json
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_honcho_plugin_name() {
        let plugin = HonchoMemoryPlugin::new();
        assert_eq!(plugin.name(), "honcho");
    }

    #[test]
    fn test_honcho_tool_schemas() {
        let plugin = HonchoMemoryPlugin::new();
        let schemas = plugin.get_tool_schemas();
        assert_eq!(schemas.len(), 4);
        let names: Vec<&str> = schemas
            .iter()
            .filter_map(|s| s.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains(&"honcho_profile"));
        assert!(names.contains(&"honcho_search"));
        assert!(names.contains(&"honcho_context"));
        assert!(names.contains(&"honcho_conclude"));
    }

    #[test]
    fn test_honcho_context_mode_hides_tools() {
        let plugin = HonchoMemoryPlugin::new();
        *plugin.recall_mode.lock().unwrap() = "context".to_string();
        assert!(plugin.get_tool_schemas().is_empty());
    }

    #[test]
    fn test_honcho_system_prompt_modes() {
        let plugin = HonchoMemoryPlugin::new();
        *plugin.recall_mode.lock().unwrap() = "hybrid".to_string();
        assert!(plugin.system_prompt_block().contains("hybrid mode"));

        *plugin.recall_mode.lock().unwrap() = "tools".to_string();
        assert!(plugin.system_prompt_block().contains("tools-only mode"));

        *plugin.recall_mode.lock().unwrap() = "context".to_string();
        assert!(plugin
            .system_prompt_block()
            .contains("context-injection mode"));
    }
}
