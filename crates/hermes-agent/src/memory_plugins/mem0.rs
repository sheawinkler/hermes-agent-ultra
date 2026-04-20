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
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::Method;
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
    base_url: String,
    api_timeout_secs: f64,
    rerank: bool,
}

impl Mem0Config {
    fn load(hermes_home: &str) -> Self {
        let mut config = Self {
            api_key: std::env::var("MEM0_API_KEY").unwrap_or_default(),
            user_id: std::env::var("MEM0_USER_ID").unwrap_or_else(|_| "hermes-user".into()),
            agent_id: std::env::var("MEM0_AGENT_ID").unwrap_or_else(|_| "hermes".into()),
            base_url: std::env::var("MEM0_BASE_URL")
                .unwrap_or_else(|_| "https://api.mem0.ai/v1".into()),
            api_timeout_secs: 10.0,
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
                if let Some(base_url) = raw.get("base_url").and_then(|v| v.as_str()) {
                    if !base_url.is_empty() {
                        config.base_url = base_url.to_string();
                    }
                }
                if let Some(timeout) = raw
                    .get("api_timeout_secs")
                    .or_else(|| raw.get("timeout"))
                    .and_then(|v| v.as_f64())
                {
                    config.api_timeout_secs = timeout.clamp(1.0, 60.0);
                }
                if let Some(rr) = raw.get("rerank").and_then(|v| v.as_bool()) {
                    config.rerank = rr;
                }
            }
        }

        config.base_url = config.base_url.trim_end_matches('/').to_string();
        config
    }
}

// ---------------------------------------------------------------------------
// Mem0MemoryPlugin
// ---------------------------------------------------------------------------

/// Mem0 Platform memory with server-side extraction and semantic search.
pub struct Mem0MemoryPlugin {
    config: Mutex<Option<Mem0Config>>,
    prefetch_result: Arc<Mutex<String>>,
    consecutive_failures: Arc<AtomicU32>,
    breaker_open_until: Arc<Mutex<Option<Instant>>>,
}

impl Mem0MemoryPlugin {
    pub fn new() -> Self {
        Self {
            config: Mutex::new(None),
            prefetch_result: Arc::new(Mutex::new(String::new())),
            consecutive_failures: Arc::new(AtomicU32::new(0)),
            breaker_open_until: Arc::new(Mutex::new(None)),
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
        Self::record_success_shared(&self.consecutive_failures);
    }

    fn record_failure(&self) {
        Self::record_failure_shared(&self.consecutive_failures, &self.breaker_open_until);
    }

    fn record_success_shared(failures: &Arc<AtomicU32>) {
        failures.store(0, Ordering::Relaxed);
    }

    fn record_failure_shared(
        failures: &Arc<AtomicU32>,
        breaker_open_until: &Arc<Mutex<Option<Instant>>>,
    ) {
        let prev = failures.fetch_add(1, Ordering::Relaxed);
        if prev + 1 >= BREAKER_THRESHOLD {
            let deadline = Instant::now() + Duration::from_secs(BREAKER_COOLDOWN_SECS);
            *breaker_open_until.lock().unwrap() = Some(deadline);
            tracing::warn!(
                "Mem0 circuit breaker tripped after {} failures. Pausing for {}s.",
                prev + 1,
                BREAKER_COOLDOWN_SECS
            );
        }
    }

    fn client(config: &Mem0Config) -> Result<reqwest::blocking::Client, String> {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs_f64(config.api_timeout_secs))
            .build()
            .map_err(|e| format!("Mem0 HTTP client build failed: {e}"))
    }

    fn config_snapshot(&self) -> Result<Mem0Config, String> {
        self.config
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "Mem0 plugin is not initialized".to_string())
    }

    fn build_url(config: &Mem0Config, path: &str) -> String {
        format!(
            "{}/{}",
            config.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    fn send_json(
        config: &Mem0Config,
        method: Method,
        path: &str,
        body: Option<&Value>,
        query: Option<&[(&str, String)]>,
    ) -> Result<Value, String> {
        let client = Self::client(config)?;
        let url = Self::build_url(config, path);
        let mut req = client
            .request(method.clone(), &url)
            .header("Authorization", format!("Bearer {}", config.api_key))
            .header("X-API-Key", &config.api_key)
            .header("Content-Type", "application/json");
        if let Some(items) = query {
            req = req.query(items);
        }
        if let Some(json_body) = body {
            req = req.json(json_body);
        }
        let resp = req
            .send()
            .map_err(|e| format!("Mem0 request {} {} failed: {e}", method, url))?;
        let status = resp.status();
        let body_text = resp.text().unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "Mem0 request {} {} returned {}: {}",
                method, url, status, body_text
            ));
        }
        if body_text.trim().is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_str::<Value>(&body_text)
            .map_err(|e| format!("Mem0 response parse error: {e}; body={body_text}"))
    }

    fn unwrap_results(response: Value) -> Vec<Value> {
        if let Some(arr) = response.as_array() {
            return arr.clone();
        }
        for key in ["results", "memories", "items", "data"] {
            if let Some(arr) = response.get(key).and_then(|v| v.as_array()) {
                return arr.clone();
            }
        }
        Vec::new()
    }

    fn extract_memory_text(item: &Value) -> Option<String> {
        item.get("memory")
            .or_else(|| item.get("content"))
            .or_else(|| item.pointer("/message/content"))
            .and_then(|v| v.as_str())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
    }

    fn search_memories(
        config: &Mem0Config,
        query: &str,
        rerank: bool,
        top_k: u64,
    ) -> Result<Vec<Value>, String> {
        let payload = json!({
            "query": query,
            "filters": {"user_id": config.user_id},
            "rerank": rerank,
            "top_k": top_k.min(50),
        });
        let mut last_error = String::new();
        for path in ["memories/search", "memory/search"] {
            match Self::send_json(config, Method::POST, path, Some(&payload), None) {
                Ok(v) => return Ok(Self::unwrap_results(v)),
                Err(e) => last_error = e,
            }
        }
        Err(last_error)
    }

    fn list_memories(config: &Mem0Config) -> Result<Vec<Value>, String> {
        let query = vec![
            ("user_id", config.user_id.clone()),
            ("agent_id", config.agent_id.clone()),
        ];
        let mut last_error = String::new();
        for path in ["memories", "memory"] {
            match Self::send_json(config, Method::GET, path, None, Some(&query)) {
                Ok(v) => return Ok(Self::unwrap_results(v)),
                Err(e) => last_error = e,
            }
        }
        match Self::search_memories(config, "*", config.rerank, 20) {
            Ok(results) => Ok(results),
            Err(search_err) => {
                if !last_error.is_empty() {
                    Err(format!(
                        "{last_error}; fallback search failed: {search_err}"
                    ))
                } else {
                    Err(search_err)
                }
            }
        }
    }

    fn add_messages(
        config: &Mem0Config,
        messages: Vec<Value>,
        infer: Option<bool>,
    ) -> Result<(), String> {
        let mut payload = json!({
            "messages": messages,
            "user_id": config.user_id,
            "agent_id": config.agent_id,
            "filters": {"user_id": config.user_id, "agent_id": config.agent_id},
        });
        if let Some(infer_val) = infer {
            payload["infer"] = json!(infer_val);
        }
        let mut last_error = String::new();
        for path in ["memories", "memories/add", "memory"] {
            match Self::send_json(config, Method::POST, path, Some(&payload), None) {
                Ok(_) => return Ok(()),
                Err(e) => last_error = e,
            }
        }
        Err(last_error)
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
        let trimmed = query.trim();
        if trimmed.is_empty() {
            return;
        }
        let config = match self.config_snapshot() {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::debug!("Mem0 prefetch skipped: {}", e);
                return;
            }
        };
        let out = Arc::clone(&self.prefetch_result);
        let failures = Arc::clone(&self.consecutive_failures);
        let breaker = Arc::clone(&self.breaker_open_until);
        let query = trimmed.to_string();
        std::thread::spawn(move || {
            match Self::search_memories(&config, &query, config.rerank, 5) {
                Ok(results) => {
                    let lines: Vec<String> = results
                        .iter()
                        .filter_map(Self::extract_memory_text)
                        .map(|m| format!("- {m}"))
                        .collect();
                    if !lines.is_empty() {
                        *out.lock().unwrap() = lines.join("\n");
                    }
                    Self::record_success_shared(&failures);
                }
                Err(e) => {
                    tracing::debug!("Mem0 prefetch failed: {}", e);
                    Self::record_failure_shared(&failures, &breaker);
                }
            }
        });
    }

    fn sync_turn(&self, user_content: &str, assistant_content: &str, _session_id: &str) {
        if self.is_breaker_open() {
            return;
        }
        let trimmed_user = user_content.trim();
        let trimmed_assistant = assistant_content.trim();
        if trimmed_user.is_empty() || trimmed_assistant.is_empty() {
            return;
        }
        let config = match self.config_snapshot() {
            Ok(cfg) => cfg,
            Err(e) => {
                tracing::debug!("Mem0 sync skipped: {}", e);
                return;
            }
        };
        let failures = Arc::clone(&self.consecutive_failures);
        let breaker = Arc::clone(&self.breaker_open_until);
        let user_message = trimmed_user.chars().take(4000).collect::<String>();
        let assistant_message = trimmed_assistant.chars().take(4000).collect::<String>();
        std::thread::spawn(move || {
            let messages = vec![
                json!({"role":"user","content": user_message}),
                json!({"role":"assistant","content": assistant_message}),
            ];
            match Self::add_messages(&config, messages, None) {
                Ok(()) => Self::record_success_shared(&failures),
                Err(e) => {
                    tracing::warn!("Mem0 sync failed: {}", e);
                    Self::record_failure_shared(&failures, &breaker);
                }
            }
        });
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
                let config = match self.config_snapshot() {
                    Ok(c) => c,
                    Err(e) => return json!({"error": e}).to_string(),
                };
                match Self::list_memories(&config) {
                    Ok(memories) => {
                        self.record_success();
                        let lines: Vec<String> = memories
                            .iter()
                            .filter_map(Self::extract_memory_text)
                            .collect();
                        if lines.is_empty() {
                            return json!({"result": "No memories stored yet."}).to_string();
                        }
                        json!({"result": lines.join("\n"), "count": lines.len()}).to_string()
                    }
                    Err(e) => {
                        self.record_failure();
                        json!({"error": format!("Failed to fetch profile: {e}")}).to_string()
                    }
                }
            }
            "mem0_search" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                if query.is_empty() {
                    return json!({"error": "Missing required parameter: query"}).to_string();
                }
                let rerank = args
                    .get("rerank")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let top_k = args
                    .get("top_k")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10)
                    .min(50);
                let config = match self.config_snapshot() {
                    Ok(c) => c,
                    Err(e) => return json!({"error": e}).to_string(),
                };
                match Self::search_memories(&config, query, rerank, top_k) {
                    Ok(results) => {
                        self.record_success();
                        if results.is_empty() {
                            return json!({"result": "No relevant memories found."}).to_string();
                        }
                        let items: Vec<Value> = results
                            .iter()
                            .filter_map(|r| {
                                let memory = Self::extract_memory_text(r)?;
                                let score = r
                                    .get("score")
                                    .or_else(|| r.get("similarity"))
                                    .cloned()
                                    .unwrap_or(json!(null));
                                Some(json!({
                                    "id": r.get("id").cloned().unwrap_or(json!(null)),
                                    "memory": memory,
                                    "score": score,
                                }))
                            })
                            .collect();
                        json!({"results": items, "count": items.len()}).to_string()
                    }
                    Err(e) => {
                        self.record_failure();
                        json!({"error": format!("Search failed: {e}")}).to_string()
                    }
                }
            }
            "mem0_conclude" => {
                let conclusion = args
                    .get("conclusion")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if conclusion.is_empty() {
                    return json!({"error": "Missing required parameter: conclusion"}).to_string();
                }
                let config = match self.config_snapshot() {
                    Ok(c) => c,
                    Err(e) => return json!({"error": e}).to_string(),
                };
                let messages = vec![json!({"role":"user","content": conclusion})];
                match Self::add_messages(&config, messages, Some(false)) {
                    Ok(()) => {
                        self.record_success();
                        json!({"result": "Fact stored."}).to_string()
                    }
                    Err(e) => {
                        self.record_failure();
                        json!({"error": format!("Failed to store: {e}")}).to_string()
                    }
                }
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
            {"key": "base_url", "description": "Mem0 API base URL", "default": "https://api.mem0.ai/v1", "env_var": "MEM0_BASE_URL"},
            {"key": "api_timeout_secs", "description": "HTTP timeout seconds", "default": "10"},
            {"key": "rerank", "description": "Enable reranking for recall", "default": "true"}
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

    #[test]
    fn test_unwrap_results_variants() {
        let as_array = json!([{"memory":"a"}]);
        assert_eq!(Mem0MemoryPlugin::unwrap_results(as_array).len(), 1);
        let as_obj = json!({"results":[{"memory":"b"}]});
        assert_eq!(Mem0MemoryPlugin::unwrap_results(as_obj).len(), 1);
    }

    #[test]
    fn test_extract_memory_text() {
        let item = json!({"memory":"hello"});
        assert_eq!(
            Mem0MemoryPlugin::extract_memory_text(&item).as_deref(),
            Some("hello")
        );
        let item2 = json!({"content":"world"});
        assert_eq!(
            Mem0MemoryPlugin::extract_memory_text(&item2).as_deref(),
            Some("world")
        );
    }
}
