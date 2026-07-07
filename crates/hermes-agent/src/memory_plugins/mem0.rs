//! Mem0 memory provider plugin.
//!
//! Implements `MemoryProviderPlugin` for Mem0 — server-side LLM fact
//! extraction, semantic search with reranking, and automatic deduplication.
//!
//! Mirrors the Python `plugins/memory/mem0/__init__.py`.
//!
//! Configuration:
//!   - `MEM0_API_KEY` (required for cloud, optional for self-hosted)
//!   - `MEM0_HOST` / `MEM0_BASE_URL` (self-hosted Mem0 URL or API base URL)
//!   - `MEM0_USER_ID` (default: "hermes-user")
//!   - `MEM0_AGENT_ID` (default: "hermes")
//!   - `$HERMES_HOME/mem0.json` overrides

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::Method;
use serde_json::{json, Value};

use crate::memory_manager::MemoryProviderPlugin;
use crate::memory_plugins::config_io;

const BREAKER_THRESHOLD: u32 = 5;
const BREAKER_COOLDOWN_SECS: u64 = 120;
const MEM0_CLOUD_BASE_URL: &str = "https://api.mem0.ai/v1";

// ---------------------------------------------------------------------------
// Tool schemas
// ---------------------------------------------------------------------------

fn list_schema() -> Value {
    json!({
        "name": "mem0_list",
        "description": "List all stored memories about the user. Use at conversation start for a full overview.",
        "parameters": {
            "type": "object",
            "properties": {
                "page": {"type": "integer", "description": "Page number (default: 1)."},
                "page_size": {"type": "integer", "description": "Results per page (default: 100, max: 200)."}
            },
            "required": []
        }
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

fn add_schema() -> Value {
    json!({
        "name": "mem0_add",
        "description": "Store a durable fact about the user. Use for explicit preferences, corrections, or decisions.",
        "parameters": {
            "type": "object",
            "properties": {
                "content": {"type": "string", "description": "The fact to store."}
            },
            "required": ["content"]
        }
    })
}

fn update_schema() -> Value {
    json!({
        "name": "mem0_update",
        "description": "Update an existing memory by ID.",
        "parameters": {
            "type": "object",
            "properties": {
                "memory_id": {"type": "string", "description": "Memory ID to update."},
                "text": {"type": "string", "description": "New memory text."}
            },
            "required": ["memory_id", "text"]
        }
    })
}

fn delete_schema() -> Value {
    json!({
        "name": "mem0_delete",
        "description": "Delete an existing memory by ID.",
        "parameters": {
            "type": "object",
            "properties": {
                "memory_id": {"type": "string", "description": "Memory ID to delete."}
            },
            "required": ["memory_id"]
        }
    })
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct Mem0Config {
    mode: String,
    api_key: String,
    user_id: String,
    agent_id: String,
    base_url: String,
    api_timeout_secs: f64,
    rerank: bool,
}

impl Mem0Config {
    fn config_path(hermes_home: &str) -> std::path::PathBuf {
        std::path::Path::new(hermes_home).join("mem0.json")
    }

    fn default_config_path() -> std::path::PathBuf {
        config_io::default_hermes_home().join("mem0.json")
    }

    fn configured_at(path: &std::path::Path) -> bool {
        config_io::json_file_has_nonempty_string(path, &["api_key", "host", "base_url"])
    }

    fn load(hermes_home: &str) -> Self {
        let env_host = std::env::var("MEM0_HOST").unwrap_or_default();
        let env_base_url = std::env::var("MEM0_BASE_URL").unwrap_or_default();
        let mut config = Self {
            mode: std::env::var("MEM0_MODE").unwrap_or_else(|_| {
                if env_host.trim().is_empty() {
                    "platform".into()
                } else {
                    "oss".into()
                }
            }),
            api_key: std::env::var("MEM0_API_KEY").unwrap_or_default(),
            user_id: std::env::var("MEM0_USER_ID").unwrap_or_else(|_| "hermes-user".into()),
            agent_id: std::env::var("MEM0_AGENT_ID").unwrap_or_else(|_| "hermes".into()),
            base_url: if !env_base_url.trim().is_empty() {
                env_base_url
            } else if !env_host.trim().is_empty() {
                env_host
            } else {
                MEM0_CLOUD_BASE_URL.into()
            },
            api_timeout_secs: 10.0,
            rerank: false,
        };

        let config_path = Self::config_path(hermes_home);
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
                if let Some(mode) = raw.get("mode").and_then(|v| v.as_str()) {
                    if !mode.trim().is_empty() {
                        config.mode = mode.to_string();
                    }
                }
                if let Some(base_url) = raw
                    .get("base_url")
                    .or_else(|| raw.get("host"))
                    .and_then(|v| v.as_str())
                {
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

        config.mode = normalize_mem0_mode(&config.mode);
        config.base_url = config.base_url.trim_end_matches('/').to_string();
        config
    }
}

fn normalize_mem0_mode(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "oss" | "selfhosted" | "self-hosted" | "self_hosted" | "local" => "oss".to_string(),
        _ => "platform".to_string(),
    }
}

fn mem0_base_url_is_cloud(base_url: &str) -> bool {
    let normalized = base_url.trim().trim_end_matches('/');
    normalized.eq_ignore_ascii_case(MEM0_CLOUD_BASE_URL)
        || normalized.eq_ignore_ascii_case("https://api.mem0.ai")
}

fn mem0_mode_label(config: &Mem0Config) -> String {
    if config.mode == "oss" {
        "OSS/self-hosted".to_string()
    } else if !mem0_base_url_is_cloud(&config.base_url) {
        format!("self-hosted HTTP at {}", config.base_url)
    } else {
        "platform".to_string()
    }
}

fn validate_mem0_memory_id(raw: &str) -> Result<String, String> {
    let id = raw.trim();
    if id.is_empty() {
        return Err("Missing required parameter: memory_id".to_string());
    }
    if id.contains('/') || id.contains('?') || id.contains('#') {
        return Err("memory_id must be an exact Mem0 memory ID, not a path or URL".to_string());
    }
    Ok(id.to_string())
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
            .header("Content-Type", "application/json");
        if !config.api_key.trim().is_empty() {
            req = req
                .header("Authorization", format!("Bearer {}", config.api_key))
                .header("X-API-Key", &config.api_key);
        }
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

    fn list_memories(config: &Mem0Config, page: u64, page_size: u64) -> Result<Vec<Value>, String> {
        let query = vec![
            ("user_id", config.user_id.clone()),
            ("agent_id", config.agent_id.clone()),
            ("page", page.max(1).to_string()),
            ("page_size", page_size.clamp(1, 200).to_string()),
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

    fn update_memory(config: &Mem0Config, memory_id: &str, text: &str) -> Result<Value, String> {
        let memory_id = validate_mem0_memory_id(memory_id)?;
        let text = text.trim();
        if text.is_empty() {
            return Err("Missing required parameter: text".to_string());
        }
        let payload = json!({
            "memory_id": memory_id,
            "text": text,
            "memory": text,
        });
        let mut last_error = String::new();
        for (method, path) in [
            (Method::PATCH, format!("memories/{memory_id}")),
            (Method::PUT, format!("memories/{memory_id}")),
            (Method::POST, "memories/update".to_string()),
            (Method::PATCH, format!("memory/{memory_id}")),
            (Method::PUT, format!("memory/{memory_id}")),
        ] {
            match Self::send_json(config, method, &path, Some(&payload), None) {
                Ok(v) => return Ok(v),
                Err(e) => last_error = e,
            }
        }
        Err(last_error)
    }

    fn delete_memory(config: &Mem0Config, memory_id: &str) -> Result<Value, String> {
        let memory_id = validate_mem0_memory_id(memory_id)?;
        let payload = json!({"memory_id": memory_id});
        let mut last_error = String::new();
        for (method, path, body) in [
            (Method::DELETE, format!("memories/{memory_id}"), None),
            (Method::DELETE, format!("memory/{memory_id}"), None),
            (Method::POST, "memories/delete".to_string(), Some(&payload)),
        ] {
            match Self::send_json(config, method, &path, body, None) {
                Ok(v) => return Ok(v),
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
        !std::env::var("MEM0_API_KEY")
            .unwrap_or_default()
            .trim()
            .is_empty()
            || !std::env::var("MEM0_HOST")
                .unwrap_or_default()
                .trim()
                .is_empty()
            || Mem0Config::configured_at(&Mem0Config::default_config_path())
    }

    fn initialize(&self, session_id: &str, hermes_home: &str) {
        let config = Mem0Config::load(hermes_home);
        *self.config.lock().unwrap() = Some(config);
        tracing::info!("Mem0 memory plugin initialized for session {}", session_id);
    }

    fn system_prompt_block(&self) -> String {
        let config = self.config.lock().unwrap();
        let (user_id, mode_label) = config
            .as_ref()
            .map(|c| (c.user_id.as_str(), mem0_mode_label(c)))
            .unwrap_or(("hermes-user", "platform".to_string()));
        format!(
            "# Mem0 Memory\n\
             Active. Mode: {}. User: {}.\n\
             Use mem0_search to find memories, mem0_add to store facts, \
             mem0_list for a full overview, and mem0_update/mem0_delete to manage by ID.",
            mode_label, user_id
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
        vec![
            list_schema(),
            search_schema(),
            add_schema(),
            update_schema(),
            delete_schema(),
        ]
    }

    fn handle_tool_call(&self, tool_name: &str, args: &Value) -> String {
        if self.is_breaker_open() {
            return json!({
                "error": "Mem0 API temporarily unavailable (circuit breaker). Will retry automatically."
            })
            .to_string();
        }

        match tool_name {
            "mem0_list" | "mem0_profile" => {
                let page = args
                    .get("page")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(1)
                    .max(1);
                let page_size = args
                    .get("page_size")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(100)
                    .clamp(1, 200);
                let config = match self.config_snapshot() {
                    Ok(c) => c,
                    Err(e) => return json!({"error": e}).to_string(),
                };
                match Self::list_memories(&config, page, page_size) {
                    Ok(memories) => {
                        self.record_success();
                        let items: Vec<Value> = memories
                            .iter()
                            .filter_map(|item| {
                                let memory = Self::extract_memory_text(item)?;
                                Some(json!({
                                    "id": item.get("id").cloned().unwrap_or(Value::Null),
                                    "memory": memory,
                                }))
                            })
                            .collect();
                        if items.is_empty() {
                            return json!({"result": "No memories stored yet."}).to_string();
                        }
                        if tool_name == "mem0_profile" {
                            let lines = items
                                .iter()
                                .filter_map(|item| item.get("memory").and_then(Value::as_str))
                                .collect::<Vec<_>>();
                            return json!({"result": lines.join("\n"), "count": lines.len()})
                                .to_string();
                        }
                        json!({"results": items, "count": items.len(), "page": page, "page_size": page_size}).to_string()
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
                    .unwrap_or_else(|| self.config_snapshot().map(|c| c.rerank).unwrap_or(false));
                let top_k = args
                    .get("top_k")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(10)
                    .clamp(1, 50);
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
            "mem0_add" | "mem0_conclude" => {
                let content = args
                    .get("content")
                    .or_else(|| args.get("conclusion"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if content.is_empty() {
                    return json!({"error": "Missing required parameter: content"}).to_string();
                }
                let config = match self.config_snapshot() {
                    Ok(c) => c,
                    Err(e) => return json!({"error": e}).to_string(),
                };
                let messages = vec![json!({"role":"user","content": content})];
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
            "mem0_update" => {
                let memory_id = args.get("memory_id").and_then(Value::as_str).unwrap_or("");
                let text = args.get("text").and_then(Value::as_str).unwrap_or("");
                let config = match self.config_snapshot() {
                    Ok(c) => c,
                    Err(e) => return json!({"error": e}).to_string(),
                };
                match Self::update_memory(&config, memory_id, text) {
                    Ok(v) => {
                        self.record_success();
                        json!({"result": "Memory updated.", "memory_id": memory_id, "response": v})
                            .to_string()
                    }
                    Err(e) => json!({"error": format!("Update failed: {e}")}).to_string(),
                }
            }
            "mem0_delete" => {
                let memory_id = args.get("memory_id").and_then(Value::as_str).unwrap_or("");
                let config = match self.config_snapshot() {
                    Ok(c) => c,
                    Err(e) => return json!({"error": e}).to_string(),
                };
                match Self::delete_memory(&config, memory_id) {
                    Ok(v) => {
                        self.record_success();
                        json!({"result": "Memory deleted.", "memory_id": memory_id, "response": v})
                            .to_string()
                    }
                    Err(e) => json!({"error": format!("Delete failed: {e}")}).to_string(),
                }
            }
            _ => json!({"error": format!("Unknown tool: {}", tool_name)}).to_string(),
        }
    }

    fn shutdown(&self) {
        tracing::debug!("Mem0 memory plugin shutdown");
    }

    fn get_config_schema(&self) -> Option<Value> {
        let mode = std::env::var("MEM0_MODE")
            .ok()
            .map(|value| normalize_mem0_mode(&value))
            .unwrap_or_else(|| "platform".to_string());
        let api_key_required = mode != "oss";
        Some(json!([
            {"key": "mode", "description": "Mem0 mode", "default": "platform", "choices": ["platform", "selfhosted", "oss"], "env_var": "MEM0_MODE"},
            {"key": "api_key", "description": "Mem0 API key", "secret": true, "required": api_key_required, "env_var": "MEM0_API_KEY", "url": "https://app.mem0.ai"},
            {"key": "host", "description": "Self-hosted Mem0 URL (alias for base_url)", "default": "", "env_var": "MEM0_HOST"},
            {"key": "user_id", "description": "User identifier", "default": "hermes-user"},
            {"key": "agent_id", "description": "Agent identifier", "default": "hermes"},
            {"key": "base_url", "description": "Mem0 API base URL", "default": MEM0_CLOUD_BASE_URL, "env_var": "MEM0_BASE_URL"},
            {"key": "api_timeout_secs", "description": "HTTP timeout seconds", "default": "10"},
            {"key": "rerank", "description": "Enable reranking for recall", "default": "false"}
        ]))
    }

    fn save_config(&self, config: &Value) -> Result<(), String> {
        config_io::merge_and_write_owner_only(&Mem0Config::default_config_path(), config)
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
    fn test_mem0_plugin_name() {
        let plugin = Mem0MemoryPlugin::new();
        assert_eq!(plugin.name(), "mem0");
    }

    #[test]
    fn test_mem0_tool_schemas() {
        let plugin = Mem0MemoryPlugin::new();
        let schemas = plugin.get_tool_schemas();
        let names = schemas
            .iter()
            .filter_map(|schema| schema.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "mem0_list",
                "mem0_search",
                "mem0_add",
                "mem0_update",
                "mem0_delete"
            ]
        );
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

    #[test]
    fn test_mem0_config_file_activates_provider() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let _api = EnvGuard::remove("MEM0_API_KEY");
        std::fs::write(tmp.path().join("mem0.json"), r#"{"api_key":"m0-secret"}"#)
            .expect("write config");

        assert!(Mem0MemoryPlugin::new().is_available());
    }

    #[test]
    fn test_mem0_host_env_activates_provider_and_sets_base_url() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let _api = EnvGuard::remove("MEM0_API_KEY");
        let _base = EnvGuard::remove("MEM0_BASE_URL");
        let _host = EnvGuard::set("MEM0_HOST", "http://127.0.0.1:24220/");

        assert!(Mem0MemoryPlugin::new().is_available());
        let cfg = Mem0Config::load(tmp.path().to_str().expect("utf8"));
        assert_eq!(cfg.mode, "oss");
        assert_eq!(cfg.base_url, "http://127.0.0.1:24220");
    }

    #[test]
    fn test_mem0_prompt_label_matches_effective_self_hosted_route() {
        let plugin = Mem0MemoryPlugin::new();
        let cfg = Mem0Config {
            mode: "platform".to_string(),
            api_key: String::new(),
            user_id: "operator".to_string(),
            agent_id: "hermes".to_string(),
            base_url: "http://127.0.0.1:24220".to_string(),
            api_timeout_secs: 5.0,
            rerank: false,
        };
        *plugin.config.lock().expect("config lock") = Some(cfg);

        let block = plugin.system_prompt_block();

        assert!(block.contains("self-hosted HTTP at http://127.0.0.1:24220"));
        assert!(!block.contains("Mode: platform"));
    }

    #[test]
    fn test_mem0_load_defaults_rerank_to_false() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _api = EnvGuard::remove("MEM0_API_KEY");
        let _host = EnvGuard::remove("MEM0_HOST");
        let _base = EnvGuard::remove("MEM0_BASE_URL");

        let cfg = Mem0Config::load(tmp.path().to_str().expect("utf8"));

        assert!(!cfg.rerank);
    }

    #[test]
    fn test_mem0_config_schema_marks_api_key_optional_in_oss_mode() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let _mode = EnvGuard::set("MEM0_MODE", "selfhosted");

        let schema = Mem0MemoryPlugin::new().get_config_schema().expect("schema");
        let mode = schema
            .as_array()
            .expect("array")
            .iter()
            .find(|item| item.get("key").and_then(Value::as_str) == Some("mode"))
            .expect("mode field");
        assert!(mode
            .get("choices")
            .and_then(Value::as_array)
            .expect("mode choices")
            .iter()
            .any(|choice| choice.as_str() == Some("selfhosted")));
        let api_key = schema
            .as_array()
            .expect("array")
            .iter()
            .find(|item| item.get("key").and_then(Value::as_str) == Some("api_key"))
            .expect("api key field");
        assert_eq!(api_key.get("required"), Some(&Value::Bool(false)));
        let rerank = schema
            .as_array()
            .expect("array")
            .iter()
            .find(|item| item.get("key").and_then(Value::as_str) == Some("rerank"))
            .expect("rerank field");
        assert_eq!(rerank.get("default").and_then(Value::as_str), Some("false"));
    }

    #[test]
    fn test_validate_mem0_memory_id_rejects_paths() {
        assert_eq!(
            validate_mem0_memory_id(" mem-123 ").expect("valid"),
            "mem-123"
        );
        assert!(validate_mem0_memory_id("mem/123").is_err());
        assert!(validate_mem0_memory_id("mem-123?delete=true").is_err());
    }

    fn one_shot_json_server(body: &'static str) -> (String, mpsc::Receiver<String>) {
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

    fn test_config_for_base_url(base_url: String) -> Mem0Config {
        Mem0Config {
            mode: "oss".to_string(),
            api_key: String::new(),
            user_id: "u1".to_string(),
            agent_id: "a1".to_string(),
            base_url,
            api_timeout_secs: 5.0,
            rerank: true,
        }
    }

    #[test]
    fn test_mem0_update_uses_patch_memory_endpoint_without_empty_auth() {
        let (base_url, rx) = one_shot_json_server(r#"{"status":"ok"}"#);
        let cfg = test_config_for_base_url(base_url);

        let result = Mem0MemoryPlugin::update_memory(&cfg, "mem-1", "new text").expect("update");

        assert_eq!(result["status"], "ok");
        let request = rx.recv_timeout(StdDuration::from_secs(2)).expect("request");
        assert!(request.starts_with("PATCH /memories/mem-1 HTTP/1.1"));
        assert!(!request.contains("Authorization: Bearer"));
        assert!(request.contains("\"text\":\"new text\""));
    }

    #[test]
    fn test_mem0_delete_uses_delete_memory_endpoint() {
        let (base_url, rx) = one_shot_json_server(r#"{"status":"ok"}"#);
        let cfg = test_config_for_base_url(base_url);

        let result = Mem0MemoryPlugin::delete_memory(&cfg, "mem-2").expect("delete");

        assert_eq!(result["status"], "ok");
        let request = rx.recv_timeout(StdDuration::from_secs(2)).expect("request");
        assert!(request.starts_with("DELETE /memories/mem-2 HTTP/1.1"));
    }

    #[test]
    fn test_mem0_save_config_merges_and_writes_owner_only() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let path = tmp.path().join("mem0.json");
        std::fs::write(&path, r#"{"agent_id":"existing"}"#).expect("write existing");

        Mem0MemoryPlugin::new()
            .save_config(&json!({"api_key":"m0-secret","user_id":"operator"}))
            .expect("save config");

        let parsed: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read config"))
                .expect("parse config");
        assert_eq!(parsed["agent_id"], "existing");
        assert_eq!(parsed["api_key"], "m0-secret");
        assert_eq!(parsed["user_id"], "operator");

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
}
