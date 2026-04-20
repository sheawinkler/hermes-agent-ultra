//! Honcho memory provider plugin.
//!
//! Implements `MemoryProviderPlugin` for Honcho's AI-native cross-session
//! user modeling. Provides context recall, peer-card access, semantic search,
//! and persistent conclusions via the Honcho API.
//!
//! Mirrors the Python `plugins/memory/honcho/__init__.py` at the capability
//! level, while using direct HTTP calls instead of the Python SDK.
//!
//! Configuration chain:
//!   1. `$HERMES_HOME/honcho.json`
//!   2. Environment variables (`HONCHO_API_KEY`, `HONCHO_BASE_URL`)
//!   3. Defaults

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::Method;
use serde_json::{json, Value};

use crate::memory_manager::MemoryProviderPlugin;

// ---------------------------------------------------------------------------
// Tool schemas
// ---------------------------------------------------------------------------

fn profile_schema() -> Value {
    json!({
        "name": "honcho_profile",
        "description": "Retrieve the user's peer card from Honcho — a curated list of key facts about them. Fast, no LLM reasoning.",
        "parameters": {"type": "object", "properties": {"peer": {"type":"string"}}, "required": []}
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
                "max_tokens": {"type": "integer", "description": "Token budget for returned context (default 800, max 2000)."},
                "peer": {"type":"string", "description": "Peer alias or peer id (default user)."}
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
                "conclusion": {"type": "string", "description": "A factual statement about the user to persist."},
                "delete_id": {"type": "string", "description": "Optional conclusion id to delete."},
                "peer": {"type":"string", "description": "Peer alias or peer id (default user)."}
            },
            "required": []
        }
    })
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct HonchoConfig {
    api_key: String,
    base_url: String,
    enabled: bool,
    recall_mode: String,
    context_tokens: Option<usize>,
    workspace_id: String,
    peer_name: Option<String>,
    ai_peer: String,
    timeout_secs: f64,
    endpoints: HashMap<String, String>,
}

impl HonchoConfig {
    fn from_env() -> Self {
        let api_key = std::env::var("HONCHO_API_KEY").unwrap_or_default();
        let base_url = std::env::var("HONCHO_BASE_URL")
            .unwrap_or_else(|_| "https://api.honcho.dev".to_string());
        let timeout_secs = std::env::var("HONCHO_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse::<f64>().ok())
            .unwrap_or(12.0)
            .clamp(1.0, 60.0);
        let mut endpoints = HashMap::new();
        for (key, env) in [
            ("profile", "HONCHO_ENDPOINT_PROFILE"),
            ("search", "HONCHO_ENDPOINT_SEARCH"),
            ("context", "HONCHO_ENDPOINT_CONTEXT"),
            ("conclude", "HONCHO_ENDPOINT_CONCLUDE"),
            ("messages", "HONCHO_ENDPOINT_MESSAGES"),
            ("flush", "HONCHO_ENDPOINT_FLUSH"),
        ] {
            if let Ok(value) = std::env::var(env) {
                if !value.trim().is_empty() {
                    endpoints.insert(key.to_string(), value);
                }
            }
        }
        Self {
            enabled: !api_key.is_empty() || !base_url.trim().is_empty(),
            api_key,
            base_url,
            recall_mode: "hybrid".to_string(),
            context_tokens: Some(800),
            workspace_id: "hermes".to_string(),
            peer_name: None,
            ai_peer: "hermes".to_string(),
            timeout_secs,
            endpoints,
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
                if let Some(url) = raw
                    .get("baseUrl")
                    .or_else(|| raw.get("base_url"))
                    .and_then(|v| v.as_str())
                {
                    if !url.is_empty() {
                        config.base_url = url.to_string();
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
                    config.context_tokens = Some(tokens.clamp(32, 4000) as usize);
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
                if let Some(timeout) = raw
                    .get("timeout")
                    .or_else(|| raw.get("requestTimeout"))
                    .and_then(|v| v.as_f64())
                {
                    config.timeout_secs = timeout.clamp(1.0, 60.0);
                }
                if let Some(enabled) = raw.get("enabled").and_then(|v| v.as_bool()) {
                    config.enabled = enabled;
                } else {
                    config.enabled =
                        !config.api_key.is_empty() || !config.base_url.trim().is_empty();
                }
                if let Some(map) = raw.get("endpoints").and_then(|v| v.as_object()) {
                    for (k, v) in map {
                        if let Some(path) = v.as_str() {
                            if !path.trim().is_empty() {
                                config.endpoints.insert(k.to_string(), path.to_string());
                            }
                        }
                    }
                }
            }
        }
        config.base_url = config.base_url.trim_end_matches('/').to_string();
        config
    }

    fn endpoint<'a>(&'a self, key: &str, default: &'a str) -> String {
        self.endpoints
            .get(key)
            .cloned()
            .unwrap_or_else(|| default.to_string())
    }
}

// ---------------------------------------------------------------------------
// HonchoMemoryPlugin
// ---------------------------------------------------------------------------

/// Honcho AI-native memory with dialectic Q&A and persistent user modeling.
pub struct HonchoMemoryPlugin {
    config: Mutex<Option<HonchoConfig>>,
    session_key: Mutex<String>,
    prefetch_result: Arc<Mutex<String>>,
    turn_count: Mutex<u32>,
    recall_mode: Mutex<String>,
}

impl HonchoMemoryPlugin {
    pub fn new() -> Self {
        Self {
            config: Mutex::new(None),
            session_key: Mutex::new(String::new()),
            prefetch_result: Arc::new(Mutex::new(String::new())),
            turn_count: Mutex::new(0),
            recall_mode: Mutex::new("hybrid".to_string()),
        }
    }

    fn config_snapshot(&self) -> Result<HonchoConfig, String> {
        self.config
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "Honcho is not active for this session.".to_string())
    }

    fn build_url(config: &HonchoConfig, path: &str) -> String {
        format!(
            "{}/{}",
            config.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    fn apply_template(path: &str, peer: &str, session_id: &str) -> String {
        path.replace("{peer}", peer)
            .replace("{session}", session_id)
            .replace("{session_id}", session_id)
    }

    fn extract_peer(config: &HonchoConfig, args: &Value) -> String {
        match args.get("peer").and_then(|v| v.as_str()).unwrap_or("user") {
            "ai" => config.ai_peer.clone(),
            "user" => config
                .peer_name
                .clone()
                .unwrap_or_else(|| "user".to_string()),
            other => other.to_string(),
        }
    }

    fn client(config: &HonchoConfig) -> Result<reqwest::blocking::Client, String> {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs_f64(config.timeout_secs))
            .build()
            .map_err(|e| format!("Honcho HTTP client build failed: {e}"))
    }

    fn send_json(
        config: &HonchoConfig,
        method: Method,
        path: &str,
        body: Option<&Value>,
        query: Option<&[(&str, String)]>,
    ) -> Result<Value, String> {
        let client = Self::client(config)?;
        let url = Self::build_url(config, path);
        let mut req = client
            .request(method.clone(), &url)
            .header("Content-Type", "application/json");
        if !config.api_key.is_empty() {
            req = req
                .header("Authorization", format!("Bearer {}", config.api_key))
                .header("X-API-Key", &config.api_key);
        }
        if let Some(items) = query {
            req = req.query(items);
        }
        if let Some(payload) = body {
            req = req.json(payload);
        }
        let resp = req
            .send()
            .map_err(|e| format!("Honcho request {} {} failed: {e}", method, url))?;
        let status = resp.status();
        let body_text = resp.text().unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "Honcho request {} {} returned {}: {}",
                method, url, status, body_text
            ));
        }
        if body_text.trim().is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_str::<Value>(&body_text)
            .map_err(|e| format!("Honcho response parse error: {e}; body={body_text}"))
    }

    fn extract_text_result(v: &Value) -> Option<String> {
        if let Some(s) = v.get("result").and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
        if let Some(s) = v.get("context").and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
        if let Some(s) = v.get("answer").and_then(|v| v.as_str()) {
            return Some(s.to_string());
        }
        if let Some(arr) = v.get("results").and_then(|v| v.as_array()) {
            let lines: Vec<String> = arr
                .iter()
                .filter_map(|item| {
                    item.get("memory")
                        .or_else(|| item.get("content"))
                        .or_else(|| item.get("text"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.trim().to_string())
                })
                .filter(|s| !s.is_empty())
                .collect();
            if !lines.is_empty() {
                return Some(lines.join("\n"));
            }
        }
        None
    }

    fn context_query(
        config: &HonchoConfig,
        session_id: &str,
        query: &str,
        max_tokens: usize,
        peer: &str,
    ) -> Result<Value, String> {
        let path = config.endpoint("context", "/v1/context/query");
        let payload = json!({
            "workspace_id": config.workspace_id,
            "session_id": session_id,
            "peer": peer,
            "query": query,
            "max_tokens": max_tokens,
        });
        Self::send_json(config, Method::POST, &path, Some(&payload), None)
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
        *self.prefetch_result.lock().unwrap() = String::new();

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

    fn prefetch(&self, _query: &str, _session_id: &str) -> String {
        let mode = self.recall_mode.lock().unwrap().clone();
        if mode == "tools" {
            return String::new();
        }

        let result = {
            let mut lock = self.prefetch_result.lock().unwrap();
            let value = lock.clone();
            lock.clear();
            value
        };
        if result.is_empty() {
            return String::new();
        }
        format!("## Honcho Context\n{}", result)
    }

    fn queue_prefetch(&self, query: &str, _session_id: &str) {
        let mode = self.recall_mode.lock().unwrap().clone();
        if mode == "tools" {
            return;
        }
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return;
        }
        let config = match self.config_snapshot() {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("Honcho prefetch skipped: {}", e);
                return;
            }
        };
        let session_id = self.session_key.lock().unwrap().clone();
        let out = Arc::clone(&self.prefetch_result);
        let query = trimmed.to_string();
        let max_tokens = config.context_tokens.unwrap_or(800).min(2000).max(64);
        std::thread::spawn(move || {
            match Self::context_query(&config, &session_id, &query, max_tokens, "user") {
                Ok(v) => {
                    if let Some(text) = Self::extract_text_result(&v) {
                        if !text.trim().is_empty() {
                            *out.lock().unwrap() = text;
                        }
                    }
                }
                Err(e) => tracing::debug!("Honcho prefetch failed: {}", e),
            }
        });
    }

    fn sync_turn(&self, user_content: &str, assistant_content: &str, _session_id: &str) {
        if self.config.lock().unwrap().is_none() {
            return;
        }
        let user_content = user_content.trim();
        let assistant_content = assistant_content.trim();
        if user_content.is_empty() || assistant_content.is_empty() {
            return;
        }
        let config = match self.config_snapshot() {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("Honcho sync skipped: {}", e);
                return;
            }
        };
        let session_id = self.session_key.lock().unwrap().clone();
        let path = config.endpoint("messages", "/v1/sessions/{session}/messages");
        let payload = json!({
            "workspace_id": config.workspace_id,
            "session_id": session_id,
            "messages": [
                {"role":"user", "content": user_content.chars().take(8000).collect::<String>()},
                {"role":"assistant", "content": assistant_content.chars().take(8000).collect::<String>()}
            ]
        });
        let rendered_path = Self::apply_template(&path, "user", &session_id);
        std::thread::spawn(move || {
            if let Err(e) =
                Self::send_json(&config, Method::POST, &rendered_path, Some(&payload), None)
            {
                tracing::debug!("Honcho sync_turn failed: {}", e);
            }
        });
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
        let config = match self.config_snapshot() {
            Ok(c) => c,
            Err(e) => return json!({"error": e}).to_string(),
        };
        let session_id = self.session_key.lock().unwrap().clone();
        let peer = Self::extract_peer(&config, args);

        match tool_name {
            "honcho_profile" => {
                let template = config.endpoint("profile", "/v1/peers/{peer}/card");
                let path = Self::apply_template(&template, &peer, &session_id);
                let maybe_card = args.get("card").and_then(|v| v.as_array()).cloned();
                let result = if let Some(card) = maybe_card {
                    let body = json!({
                        "workspace_id": config.workspace_id,
                        "session_id": session_id,
                        "peer": peer,
                        "card": card
                    });
                    Self::send_json(&config, Method::POST, &path, Some(&body), None)
                } else {
                    let query = vec![
                        ("workspace_id", config.workspace_id.clone()),
                        ("session_id", session_id.clone()),
                        ("peer", peer.clone()),
                    ];
                    Self::send_json(&config, Method::GET, &path, None, Some(&query))
                };
                match result {
                    Ok(v) => {
                        if let Some(card) = v
                            .get("card")
                            .or_else(|| v.get("result"))
                            .and_then(|c| c.as_array())
                        {
                            return json!({"result": card, "count": card.len()}).to_string();
                        }
                        json!({"result": v}).to_string()
                    }
                    Err(e) => json!({"error": format!("Honcho profile failed: {e}")}).to_string(),
                }
            }
            "honcho_search" => {
                let query_text = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                if query_text.is_empty() {
                    return json!({"error": "Missing required parameter: query"}).to_string();
                }
                let max_tokens = args
                    .get("max_tokens")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(800)
                    .clamp(64, 2000) as usize;
                let path = config.endpoint("search", "/v1/context/search");
                let body = json!({
                    "workspace_id": config.workspace_id,
                    "session_id": session_id,
                    "peer": peer,
                    "query": query_text,
                    "max_tokens": max_tokens
                });
                match Self::send_json(&config, Method::POST, &path, Some(&body), None) {
                    Ok(v) => {
                        let text = Self::extract_text_result(&v).unwrap_or_default();
                        if text.is_empty() {
                            return json!({"result": "No relevant context found.", "raw": v})
                                .to_string();
                        }
                        json!({"result": text, "raw": v}).to_string()
                    }
                    Err(e) => json!({"error": format!("Honcho search failed: {e}")}).to_string(),
                }
            }
            "honcho_context" => {
                let query_text = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                if query_text.is_empty() {
                    return json!({"error": "Missing required parameter: query"}).to_string();
                }
                let max_tokens = config.context_tokens.unwrap_or(800).min(2000).max(64);
                match Self::context_query(&config, &session_id, query_text, max_tokens, &peer) {
                    Ok(v) => {
                        let text = Self::extract_text_result(&v).unwrap_or_default();
                        if text.is_empty() {
                            return json!({"result": "No result from Honcho.", "raw": v})
                                .to_string();
                        }
                        json!({"result": text, "raw": v}).to_string()
                    }
                    Err(e) => json!({"error": format!("Honcho context failed: {e}")}).to_string(),
                }
            }
            "honcho_conclude" => {
                let conclusion = args
                    .get("conclusion")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                let delete_id = args
                    .get("delete_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if conclusion.is_empty() && delete_id.is_empty() {
                    return json!({"error": "Provide conclusion or delete_id"}).to_string();
                }
                if !conclusion.is_empty() && !delete_id.is_empty() {
                    return json!({"error": "Provide either conclusion or delete_id, not both"})
                        .to_string();
                }
                let template = config.endpoint("conclude", "/v1/conclusions");
                if !delete_id.is_empty() {
                    let delete_path = format!("{}/{}", template.trim_end_matches('/'), delete_id);
                    let query = vec![
                        ("workspace_id", config.workspace_id.clone()),
                        ("session_id", session_id.clone()),
                        ("peer", peer.clone()),
                    ];
                    return match Self::send_json(
                        &config,
                        Method::DELETE,
                        &delete_path,
                        None,
                        Some(&query),
                    ) {
                        Ok(v) => json!({"result":"Conclusion deleted", "raw": v}).to_string(),
                        Err(e) => {
                            json!({"error": format!("Honcho delete failed: {e}")}).to_string()
                        }
                    };
                }
                let body = json!({
                    "workspace_id": config.workspace_id,
                    "session_id": session_id,
                    "peer": peer,
                    "conclusion": conclusion
                });
                match Self::send_json(&config, Method::POST, &template, Some(&body), None) {
                    Ok(v) => json!({"result": "Conclusion saved.", "raw": v}).to_string(),
                    Err(e) => json!({"error": format!("Honcho conclude failed: {e}")}).to_string(),
                }
            }
            _ => json!({"error": format!("Unknown tool: {}", tool_name)}).to_string(),
        }
    }

    fn on_turn_start(&self, turn_number: u32, _message: &str) {
        *self.turn_count.lock().unwrap() = turn_number;
    }

    fn on_session_end(&self, _messages: &[Value]) {
        let config = match self.config_snapshot() {
            Ok(c) => c,
            Err(_) => return,
        };
        let session_id = self.session_key.lock().unwrap().clone();
        let template = config.endpoint("flush", "/v1/sessions/{session}/flush");
        let path = Self::apply_template(&template, "user", &session_id);
        let body = json!({
            "workspace_id": config.workspace_id,
            "session_id": session_id
        });
        if let Err(e) = Self::send_json(&config, Method::POST, &path, Some(&body), None) {
            tracing::debug!("Honcho session end flush failed: {}", e);
        }
    }

    fn on_memory_write(&self, action: &str, target: &str, content: &str) {
        if action != "add" || target != "user" || content.trim().is_empty() {
            return;
        }
        let config = match self.config_snapshot() {
            Ok(c) => c,
            Err(_) => return,
        };
        let session_id = self.session_key.lock().unwrap().clone();
        let template = config.endpoint("conclude", "/v1/conclusions");
        let body = json!({
            "workspace_id": config.workspace_id,
            "session_id": session_id,
            "peer": "user",
            "conclusion": content.trim(),
            "source": "memory_write_hook"
        });
        std::thread::spawn(move || {
            if let Err(e) = Self::send_json(&config, Method::POST, &template, Some(&body), None) {
                tracing::debug!("Honcho memory mirror failed: {}", e);
            }
        });
    }

    fn shutdown(&self) {
        tracing::debug!("Honcho memory plugin shutdown");
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key": "api_key", "description": "Honcho API key", "secret": true, "env_var": "HONCHO_API_KEY", "url": "https://app.honcho.dev"},
            {"key": "baseUrl", "description": "Honcho base URL (for self-hosted)"},
            {"key": "timeout", "description": "HTTP timeout seconds", "default": 12},
            {"key": "endpoints", "description": "Optional endpoint path overrides"}
        ]))
    }

    fn save_config(&self, config: &Value) -> Result<(), String> {
        if !config.is_object() {
            return Err("config must be a JSON object".into());
        }
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

    #[test]
    fn test_apply_template() {
        let path =
            HonchoMemoryPlugin::apply_template("/v1/sessions/{session}/peers/{peer}", "user", "s1");
        assert_eq!(path, "/v1/sessions/s1/peers/user");
    }
}
