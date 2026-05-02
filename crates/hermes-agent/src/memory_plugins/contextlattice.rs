//! ContextLattice memory provider plugin.
//!
//! Native integration with a ContextLattice orchestrator service using
//! `/memory/search`, `/memory/context-pack`, and `/memory/write`.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use reqwest::Method;
use serde_json::{json, Value};

use crate::memory_manager::MemoryProviderPlugin;

const DEFAULT_ORCHESTRATOR_URL: &str = "http://127.0.0.1:8075";

fn search_schema() -> Value {
    json!({
        "name": "contextlattice_search",
        "description": "Search ContextLattice memory for relevant context with source grounding.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type":"string", "description":"Search query."},
                "topic_path": {"type":"string", "description":"Optional topic scope (example: runbooks/backend/parity)."},
                "limit": {"type":"integer", "description":"Optional max results."},
                "retrieval_mode": {"type":"string", "description":"fast|balanced|deep"}
            },
            "required": ["query"]
        }
    })
}

fn context_pack_schema() -> Value {
    json!({
        "name": "contextlattice_context_pack",
        "description": "Fetch a context pack from ContextLattice for broad multi-file grounding.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type":"string"},
                "topic_path": {"type":"string"},
                "retrieval_mode": {"type":"string", "description":"fast|balanced|deep"}
            },
            "required": ["query"]
        }
    })
}

fn write_schema() -> Value {
    json!({
        "name": "contextlattice_write",
        "description": "Write a durable checkpoint into ContextLattice memory.",
        "parameters": {
            "type": "object",
            "properties": {
                "content": {"type":"string"},
                "file_name": {"type":"string", "description":"Logical file key in memory store."},
                "topic_path": {"type":"string"}
            },
            "required": ["content"]
        }
    })
}

#[derive(Debug, Clone)]
struct ContextLatticeConfig {
    orchestrator_url: String,
    project: String,
    topic_path: String,
    agent_id: String,
    timeout_secs: f64,
    include_grounding: bool,
    include_retrieval_debug: bool,
    default_retrieval_mode: String,
    default_file_name: String,
    pressure_aware: bool,
    pressure_pending_threshold: u64,
    pressure_cooldown_threshold: u64,
    pressure_status_ttl_secs: f64,
    pressure_fast_sources: Vec<String>,
    pressure_force_fast_mode: bool,
    pressure_force_fast_prefetch: bool,
    api_key: Option<String>,
}

#[derive(Debug, Clone)]
struct QueuePressureSnapshot {
    pending_total: u64,
    cooldown_active: u64,
    pressure_active: bool,
    captured_at: Instant,
}

fn parse_bool_flag(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn parse_csv_sources(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|item| item.trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

impl ContextLatticeConfig {
    fn load(hermes_home: &str) -> Self {
        let project_default = std::path::Path::new(hermes_home)
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|s| !s.is_empty() && *s != ".hermes")
            .unwrap_or("hermes-agent-rs")
            .to_string();

        let mut cfg = Self {
            orchestrator_url: std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL")
                .or_else(|_| std::env::var("MEMMCP_ORCHESTRATOR_URL"))
                .unwrap_or_else(|_| DEFAULT_ORCHESTRATOR_URL.to_string()),
            project: std::env::var("CONTEXTLATTICE_PROJECT")
                .or_else(|_| std::env::var("HERMES_CONTEXTLATTICE_PROJECT"))
                .unwrap_or(project_default),
            topic_path: std::env::var("CONTEXTLATTICE_TOPIC_PATH")
                .unwrap_or_else(|_| "runbooks/hermes".to_string()),
            agent_id: std::env::var("CONTEXTLATTICE_AGENT_ID")
                .or_else(|_| std::env::var("MEMMCP_AGENT_ID"))
                .unwrap_or_else(|_| "hermes_agent_rs".to_string()),
            timeout_secs: std::env::var("CONTEXTLATTICE_TIMEOUT_SECS")
                .ok()
                .and_then(|s| s.parse::<f64>().ok())
                .unwrap_or(10.0)
                .clamp(1.0, 60.0),
            include_grounding: true,
            include_retrieval_debug: true,
            default_retrieval_mode: "balanced".to_string(),
            default_file_name: "notes/hermes-agent.md".to_string(),
            pressure_aware: true,
            pressure_pending_threshold: 12,
            pressure_cooldown_threshold: 3,
            pressure_status_ttl_secs: 2.5,
            pressure_fast_sources: vec![
                "topic_rollups".to_string(),
                "qdrant".to_string(),
                "weaviate".to_string(),
                "postgres_pgvector".to_string(),
            ],
            pressure_force_fast_mode: true,
            pressure_force_fast_prefetch: true,
            api_key: std::env::var("CONTEXTLATTICE_API_KEY").ok(),
        };

        let path = std::path::Path::new(hermes_home).join("contextlattice.json");
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Ok(raw) = serde_json::from_str::<Value>(&content) {
                if let Some(url) = raw.get("orchestrator_url").and_then(|v| v.as_str()) {
                    if !url.is_empty() {
                        cfg.orchestrator_url = url.to_string();
                    }
                }
                if let Some(project) = raw.get("project").and_then(|v| v.as_str()) {
                    if !project.is_empty() {
                        cfg.project = project.to_string();
                    }
                }
                if let Some(topic) = raw.get("topic_path").and_then(|v| v.as_str()) {
                    cfg.topic_path = topic.to_string();
                }
                if let Some(agent) = raw.get("agent_id").and_then(|v| v.as_str()) {
                    if !agent.is_empty() {
                        cfg.agent_id = agent.to_string();
                    }
                }
                if let Some(timeout) = raw.get("timeout_secs").and_then(|v| v.as_f64()) {
                    cfg.timeout_secs = timeout.clamp(1.0, 60.0);
                }
                if let Some(enabled) = raw.get("include_grounding").and_then(|v| v.as_bool()) {
                    cfg.include_grounding = enabled;
                }
                if let Some(enabled) = raw.get("include_retrieval_debug").and_then(|v| v.as_bool())
                {
                    cfg.include_retrieval_debug = enabled;
                }
                if let Some(mode) = raw.get("retrieval_mode").and_then(|v| v.as_str()) {
                    cfg.default_retrieval_mode = mode.to_string();
                }
                if let Some(file_name) = raw.get("file_name").and_then(|v| v.as_str()) {
                    if !file_name.is_empty() {
                        cfg.default_file_name = file_name.to_string();
                    }
                }
                if let Some(enabled) = raw.get("pressure_aware").and_then(|v| v.as_bool()) {
                    cfg.pressure_aware = enabled;
                }
                if let Some(value) = raw
                    .get("pressure_pending_threshold")
                    .and_then(|v| v.as_u64())
                {
                    cfg.pressure_pending_threshold = value;
                }
                if let Some(value) = raw
                    .get("pressure_cooldown_threshold")
                    .and_then(|v| v.as_u64())
                {
                    cfg.pressure_cooldown_threshold = value;
                }
                if let Some(value) = raw.get("pressure_status_ttl_secs").and_then(|v| v.as_f64()) {
                    cfg.pressure_status_ttl_secs = value.clamp(0.2, 30.0);
                }
                if let Some(values) = raw.get("pressure_fast_sources").and_then(|v| v.as_array()) {
                    let parsed = values
                        .iter()
                        .filter_map(|item| item.as_str())
                        .map(str::trim)
                        .filter(|item| !item.is_empty())
                        .map(ToString::to_string)
                        .collect::<Vec<_>>();
                    if !parsed.is_empty() {
                        cfg.pressure_fast_sources = parsed;
                    }
                }
                if let Some(value) = raw
                    .get("pressure_force_fast_mode")
                    .and_then(|v| v.as_bool())
                {
                    cfg.pressure_force_fast_mode = value;
                }
                if let Some(value) = raw
                    .get("pressure_force_fast_prefetch")
                    .and_then(|v| v.as_bool())
                {
                    cfg.pressure_force_fast_prefetch = value;
                }
                if let Some(api_key) = raw.get("api_key").and_then(|v| v.as_str()) {
                    if !api_key.is_empty() {
                        cfg.api_key = Some(api_key.to_string());
                    }
                }
            }
        }

        if let Ok(raw) = std::env::var("CONTEXTLATTICE_PRESSURE_AWARE") {
            if let Some(v) = parse_bool_flag(&raw) {
                cfg.pressure_aware = v;
            }
        }
        if let Ok(raw) = std::env::var("CONTEXTLATTICE_PRESSURE_PENDING_THRESHOLD") {
            if let Ok(v) = raw.trim().parse::<u64>() {
                cfg.pressure_pending_threshold = v;
            }
        }
        if let Ok(raw) = std::env::var("CONTEXTLATTICE_PRESSURE_COOLDOWN_THRESHOLD") {
            if let Ok(v) = raw.trim().parse::<u64>() {
                cfg.pressure_cooldown_threshold = v;
            }
        }
        if let Ok(raw) = std::env::var("CONTEXTLATTICE_PRESSURE_STATUS_TTL_SECS") {
            if let Ok(v) = raw.trim().parse::<f64>() {
                cfg.pressure_status_ttl_secs = v.clamp(0.2, 30.0);
            }
        }
        if let Ok(raw) = std::env::var("CONTEXTLATTICE_PRESSURE_FAST_SOURCES") {
            let parsed = parse_csv_sources(&raw);
            if !parsed.is_empty() {
                cfg.pressure_fast_sources = parsed;
            }
        }
        if let Ok(raw) = std::env::var("CONTEXTLATTICE_PRESSURE_FORCE_FAST_MODE") {
            if let Some(v) = parse_bool_flag(&raw) {
                cfg.pressure_force_fast_mode = v;
            }
        }
        if let Ok(raw) = std::env::var("CONTEXTLATTICE_PRESSURE_FORCE_FAST_PREFETCH") {
            if let Some(v) = parse_bool_flag(&raw) {
                cfg.pressure_force_fast_prefetch = v;
            }
        }

        cfg.orchestrator_url = cfg.orchestrator_url.trim_end_matches('/').to_string();
        cfg
    }
}

pub struct ContextLatticeMemoryPlugin {
    config: Mutex<Option<ContextLatticeConfig>>,
    session_id: Mutex<String>,
    prefetch_result: Arc<Mutex<String>>,
    pressure_cache: Arc<Mutex<Option<QueuePressureSnapshot>>>,
}

impl ContextLatticeMemoryPlugin {
    pub fn new() -> Self {
        Self {
            config: Mutex::new(None),
            session_id: Mutex::new(String::new()),
            prefetch_result: Arc::new(Mutex::new(String::new())),
            pressure_cache: Arc::new(Mutex::new(None)),
        }
    }

    fn config_snapshot(&self) -> Result<ContextLatticeConfig, String> {
        self.config
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| "ContextLattice plugin not initialized".to_string())
    }

    fn client(config: &ContextLatticeConfig) -> Result<reqwest::blocking::Client, String> {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs_f64(config.timeout_secs))
            .build()
            .map_err(|e| format!("ContextLattice HTTP client build failed: {e}"))
    }

    fn send_json(
        config: &ContextLatticeConfig,
        method: Method,
        path: &str,
        payload: &Value,
    ) -> Result<Value, String> {
        let client = Self::client(config)?;
        let url = format!(
            "{}/{}",
            config.orchestrator_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        let mut req = client
            .request(method.clone(), &url)
            .header("Content-Type", "application/json")
            .json(payload);
        if let Some(api_key) = &config.api_key {
            req = req
                .header("Authorization", format!("Bearer {api_key}"))
                .header("X-API-Key", api_key);
        }
        let resp = req
            .send()
            .map_err(|e| format!("ContextLattice request {} {} failed: {e}", method, url))?;
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "ContextLattice request {} {} returned {}: {}",
                method, url, status, body
            ));
        }
        if body.trim().is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_str(&body)
            .map_err(|e| format!("ContextLattice response parse error: {e}; body={body}"))
    }

    fn send_no_body(
        config: &ContextLatticeConfig,
        method: Method,
        path: &str,
    ) -> Result<Value, String> {
        let client = Self::client(config)?;
        let url = format!(
            "{}/{}",
            config.orchestrator_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        );
        let mut req = client.request(method.clone(), &url);
        if let Some(api_key) = &config.api_key {
            req = req
                .header("Authorization", format!("Bearer {api_key}"))
                .header("X-API-Key", api_key);
        }
        let resp = req
            .send()
            .map_err(|e| format!("ContextLattice request {} {} failed: {e}", method, url))?;
        let status = resp.status();
        let body = resp.text().unwrap_or_default();
        if !status.is_success() {
            return Err(format!(
                "ContextLattice request {} {} returned {}: {}",
                method, url, status, body
            ));
        }
        if body.trim().is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_str(&body)
            .map_err(|e| format!("ContextLattice response parse error: {e}; body={body}"))
    }

    fn queue_pressure_snapshot(
        config: &ContextLatticeConfig,
        cache: &Arc<Mutex<Option<QueuePressureSnapshot>>>,
    ) -> Option<QueuePressureSnapshot> {
        if !config.pressure_aware {
            return None;
        }
        let ttl = Duration::from_secs_f64(config.pressure_status_ttl_secs.clamp(0.2, 30.0));
        if let Some(cached) = cache
            .lock()
            .ok()
            .and_then(|guard| guard.clone())
            .filter(|snapshot| snapshot.captured_at.elapsed() <= ttl)
        {
            return Some(cached);
        }

        let info = match Self::send_no_body(config, Method::GET, "/v1/info") {
            Ok(v) => v,
            Err(e) => {
                tracing::debug!("ContextLattice pressure status read failed: {}", e);
                return None;
            }
        };
        let queue = info
            .get("status")
            .and_then(|v| v.get("queue"))
            .and_then(|v| v.as_object())?;
        let pending_total = queue
            .get("pendingTotal")
            .or_else(|| queue.get("pending_total"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let cooldown_active = queue
            .get("cooldownActive")
            .or_else(|| queue.get("cooldown_active"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0);
        let pressure_active = pending_total >= config.pressure_pending_threshold
            || cooldown_active >= config.pressure_cooldown_threshold;
        let snapshot = QueuePressureSnapshot {
            pending_total,
            cooldown_active,
            pressure_active,
            captured_at: Instant::now(),
        };
        if let Ok(mut guard) = cache.lock() {
            *guard = Some(snapshot.clone());
        }
        Some(snapshot)
    }

    fn apply_pressure_tuning_to_payload(
        config: &ContextLatticeConfig,
        payload: &mut Value,
        pressure: Option<&QueuePressureSnapshot>,
        prefer_fast_mode: bool,
    ) {
        let Some(snapshot) = pressure else {
            return;
        };
        if !snapshot.pressure_active {
            return;
        }
        if prefer_fast_mode && config.pressure_force_fast_mode {
            payload["retrieval_mode"] = json!("fast");
        }
        if !config.pressure_fast_sources.is_empty() {
            payload["sources"] = json!(config.pressure_fast_sources);
        }
        tracing::debug!(
            "ContextLattice pressure-aware tuning active: pending_total={} cooldown_active={} mode={} sources={}",
            snapshot.pending_total,
            snapshot.cooldown_active,
            payload.get("retrieval_mode").and_then(|v| v.as_str()).unwrap_or("balanced"),
            payload.get("sources").map(|v| v.to_string()).unwrap_or_else(|| "[]".to_string()),
        );
    }

    fn extract_search_lines(v: &Value, limit: usize) -> Vec<String> {
        let results = v
            .get("results")
            .and_then(|r| r.as_array())
            .cloned()
            .or_else(|| {
                v.get("context_pack")
                    .and_then(|cp| cp.get("results"))
                    .and_then(|r| r.as_array())
                    .cloned()
            })
            .unwrap_or_default();

        results
            .iter()
            .take(limit.max(1))
            .filter_map(|item| {
                let summary = item
                    .get("summary")
                    .or_else(|| item.get("content"))
                    .or_else(|| item.get("text"))
                    .and_then(|v| v.as_str())?;
                let trimmed = summary.trim();
                if trimmed.is_empty() {
                    return None;
                }
                let file = item
                    .get("file")
                    .and_then(|v| v.as_str())
                    .unwrap_or("memory");
                Some(format!(
                    "- [{}] {}",
                    file,
                    trimmed.chars().take(240).collect::<String>()
                ))
            })
            .collect()
    }

    fn build_search_payload(
        config: &ContextLatticeConfig,
        query: &str,
        topic_path: Option<&str>,
        retrieval_mode: Option<&str>,
        limit: Option<u64>,
    ) -> Value {
        let mut payload = json!({
            "project": config.project,
            "query": query,
            "agent_id": config.agent_id,
            "include_grounding": config.include_grounding,
            "include_retrieval_debug": config.include_retrieval_debug,
            "retrieval_mode": retrieval_mode.unwrap_or(&config.default_retrieval_mode),
        });
        let topic = topic_path.unwrap_or(&config.topic_path);
        if !topic.trim().is_empty() {
            payload["topic_path"] = json!(topic);
        }
        if let Some(l) = limit {
            payload["limit"] = json!(l);
        }
        payload
    }
}

impl MemoryProviderPlugin for ContextLatticeMemoryPlugin {
    fn name(&self) -> &str {
        "contextlattice"
    }

    fn is_available(&self) -> bool {
        std::env::var("HERMES_ENABLE_CONTEXTLATTICE")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
            || std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL").is_ok()
            || std::env::var("MEMMCP_ORCHESTRATOR_URL").is_ok()
    }

    fn initialize(&self, session_id: &str, hermes_home: &str) {
        *self.config.lock().unwrap() = Some(ContextLatticeConfig::load(hermes_home));
        *self.session_id.lock().unwrap() = session_id.to_string();
        *self.prefetch_result.lock().unwrap() = String::new();
        *self.pressure_cache.lock().unwrap() = None;
        tracing::info!(
            "ContextLattice memory plugin initialized for session {}",
            session_id
        );
    }

    fn system_prompt_block(&self) -> String {
        let cfg = self.config.lock().unwrap();
        let project = cfg
            .as_ref()
            .map(|c| c.project.as_str())
            .unwrap_or("contextlattice");
        format!(
            "# ContextLattice Memory\n\
             Active. Project: {}.\n\
             Use contextlattice_search for recall, contextlattice_context_pack for broader grounding, \
             and contextlattice_write for explicit checkpoints.",
            project
        )
    }

    fn prefetch(&self, _query: &str, _session_id: &str) -> String {
        let result = {
            let mut lock = self.prefetch_result.lock().unwrap();
            let value = lock.clone();
            lock.clear();
            value
        };
        if result.is_empty() {
            String::new()
        } else {
            format!("## ContextLattice Memory\n{}", result)
        }
    }

    fn queue_prefetch(&self, query: &str, _session_id: &str) {
        let query = query.trim();
        if query.is_empty() {
            return;
        }
        let cfg = match self.config_snapshot() {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("ContextLattice prefetch skipped: {}", e);
                return;
            }
        };
        let out = Arc::clone(&self.prefetch_result);
        let pressure_cache = Arc::clone(&self.pressure_cache);
        let query_owned = query.to_string();
        std::thread::spawn(move || {
            let mut payload = Self::build_search_payload(
                &cfg,
                &query_owned,
                None,
                Some(if cfg.pressure_force_fast_prefetch {
                    "fast"
                } else {
                    "balanced"
                }),
                Some(4),
            );
            let pressure = Self::queue_pressure_snapshot(&cfg, &pressure_cache);
            Self::apply_pressure_tuning_to_payload(
                &cfg,
                &mut payload,
                pressure.as_ref(),
                cfg.pressure_force_fast_prefetch,
            );
            match Self::send_json(&cfg, Method::POST, "/memory/search", &payload) {
                Ok(v) => {
                    let lines = Self::extract_search_lines(&v, 5);
                    if !lines.is_empty() {
                        *out.lock().unwrap() = lines.join("\n");
                    }
                }
                Err(e) => tracing::debug!("ContextLattice prefetch failed: {}", e),
            }
        });
    }

    fn sync_turn(&self, user_content: &str, assistant_content: &str, session_id: &str) {
        let user = user_content.trim();
        let assistant = assistant_content.trim();
        if user.is_empty() || assistant.is_empty() {
            return;
        }
        let cfg = match self.config_snapshot() {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!("ContextLattice sync skipped: {}", e);
                return;
            }
        };
        let sid = if session_id.is_empty() {
            self.session_id.lock().unwrap().clone()
        } else {
            session_id.to_string()
        };
        let payload = json!({
            "projectName": cfg.project,
            "fileName": cfg.default_file_name,
            "topicPath": cfg.topic_path,
            "content": format!(
                "### session={}\nuser: {}\nassistant: {}",
                sid,
                user.chars().take(4000).collect::<String>(),
                assistant.chars().take(4000).collect::<String>(),
            )
        });
        std::thread::spawn(move || {
            if let Err(e) = Self::send_json(&cfg, Method::POST, "/memory/write", &payload) {
                tracing::debug!("ContextLattice sync_turn write failed: {}", e);
            }
        });
    }

    fn get_tool_schemas(&self) -> Vec<Value> {
        vec![search_schema(), context_pack_schema(), write_schema()]
    }

    fn handle_tool_call(&self, tool_name: &str, args: &Value) -> String {
        let cfg = match self.config_snapshot() {
            Ok(c) => c,
            Err(e) => return json!({"error": e}).to_string(),
        };
        match tool_name {
            "contextlattice_search" => {
                let query = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if query.is_empty() {
                    return json!({"error":"query is required"}).to_string();
                }
                let topic_path = args.get("topic_path").and_then(|v| v.as_str());
                let retrieval_mode = args.get("retrieval_mode").and_then(|v| v.as_str());
                let limit = args.get("limit").and_then(|v| v.as_u64());
                let mut payload =
                    Self::build_search_payload(&cfg, query, topic_path, retrieval_mode, limit);
                let pressure = Self::queue_pressure_snapshot(&cfg, &self.pressure_cache);
                if retrieval_mode.is_none() {
                    Self::apply_pressure_tuning_to_payload(
                        &cfg,
                        &mut payload,
                        pressure.as_ref(),
                        true,
                    );
                }
                match Self::send_json(&cfg, Method::POST, "/memory/search", &payload) {
                    Ok(v) => v.to_string(),
                    Err(e) => json!({"error": e}).to_string(),
                }
            }
            "contextlattice_context_pack" => {
                let query = args
                    .get("query")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if query.is_empty() {
                    return json!({"error":"query is required"}).to_string();
                }
                let topic_path = args.get("topic_path").and_then(|v| v.as_str());
                let retrieval_mode = args.get("retrieval_mode").and_then(|v| v.as_str());
                let mut payload = json!({
                    "project": cfg.project,
                    "query": query,
                    "agent_id": cfg.agent_id,
                    "include_grounding": cfg.include_grounding,
                    "include_retrieval_debug": cfg.include_retrieval_debug,
                    "retrieval_mode": retrieval_mode.unwrap_or(&cfg.default_retrieval_mode),
                });
                let topic = topic_path.unwrap_or(&cfg.topic_path);
                if !topic.trim().is_empty() {
                    payload["topic_path"] = json!(topic);
                }
                let pressure = Self::queue_pressure_snapshot(&cfg, &self.pressure_cache);
                if retrieval_mode.is_none() {
                    Self::apply_pressure_tuning_to_payload(
                        &cfg,
                        &mut payload,
                        pressure.as_ref(),
                        true,
                    );
                }
                match Self::send_json(&cfg, Method::POST, "/memory/context-pack", &payload) {
                    Ok(v) => v.to_string(),
                    Err(e) => json!({"error": e}).to_string(),
                }
            }
            "contextlattice_write" => {
                let content = args
                    .get("content")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .trim();
                if content.is_empty() {
                    return json!({"error":"content is required"}).to_string();
                }
                let file_name = args
                    .get("file_name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&cfg.default_file_name);
                let topic_path = args
                    .get("topic_path")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&cfg.topic_path);
                let payload = json!({
                    "projectName": cfg.project,
                    "fileName": file_name,
                    "content": content,
                    "topicPath": topic_path,
                });
                match Self::send_json(&cfg, Method::POST, "/memory/write", &payload) {
                    Ok(v) => v.to_string(),
                    Err(e) => json!({"error": e}).to_string(),
                }
            }
            _ => json!({"error": format!("Unknown tool: {}", tool_name)}).to_string(),
        }
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key":"orchestrator_url", "description":"ContextLattice orchestrator URL", "default": DEFAULT_ORCHESTRATOR_URL, "env_var":"CONTEXTLATTICE_ORCHESTRATOR_URL"},
            {"key":"project", "description":"Default project for reads/writes"},
            {"key":"topic_path", "description":"Default topic path scope"},
            {"key":"agent_id", "description":"Agent id for retrieval profiles", "default":"hermes_agent_rs"},
            {"key":"timeout_secs", "description":"HTTP timeout in seconds", "default":10},
            {"key":"pressure_aware", "description":"Enable queue-pressure-aware retrieval tuning", "default": true, "env_var":"CONTEXTLATTICE_PRESSURE_AWARE"},
            {"key":"pressure_pending_threshold", "description":"Pending-total threshold for pressure mode", "default": 12, "env_var":"CONTEXTLATTICE_PRESSURE_PENDING_THRESHOLD"},
            {"key":"pressure_cooldown_threshold", "description":"Cooldown-active threshold for pressure mode", "default": 3, "env_var":"CONTEXTLATTICE_PRESSURE_COOLDOWN_THRESHOLD"},
            {"key":"pressure_status_ttl_secs", "description":"Cache TTL for queue status polling", "default": 2.5, "env_var":"CONTEXTLATTICE_PRESSURE_STATUS_TTL_SECS"},
            {"key":"pressure_fast_sources", "description":"Preferred fast sources under pressure", "default":["topic_rollups","qdrant","weaviate","postgres_pgvector"], "env_var":"CONTEXTLATTICE_PRESSURE_FAST_SOURCES"},
            {"key":"api_key", "description":"Optional bearer token", "secret": true, "env_var":"CONTEXTLATTICE_API_KEY"}
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
    fn test_extract_search_lines_prefers_summary() {
        let v = json!({
            "results": [
                {"file":"a.md", "summary":"alpha"},
                {"file":"b.md", "content":"beta"}
            ]
        });
        let lines = ContextLatticeMemoryPlugin::extract_search_lines(&v, 10);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("alpha"));
        assert!(lines[1].contains("beta"));
    }

    #[test]
    fn test_build_search_payload_includes_project_and_topic() {
        let cfg = ContextLatticeConfig {
            orchestrator_url: DEFAULT_ORCHESTRATOR_URL.to_string(),
            project: "hermes-agent-rs".to_string(),
            topic_path: "runbooks/backend/parity".to_string(),
            agent_id: "codex".to_string(),
            timeout_secs: 10.0,
            include_grounding: true,
            include_retrieval_debug: true,
            default_retrieval_mode: "balanced".to_string(),
            default_file_name: "notes/test.md".to_string(),
            pressure_aware: true,
            pressure_pending_threshold: 12,
            pressure_cooldown_threshold: 3,
            pressure_status_ttl_secs: 2.5,
            pressure_fast_sources: vec![
                "topic_rollups".to_string(),
                "qdrant".to_string(),
                "weaviate".to_string(),
                "postgres_pgvector".to_string(),
            ],
            pressure_force_fast_mode: true,
            pressure_force_fast_prefetch: true,
            api_key: None,
        };
        let payload =
            ContextLatticeMemoryPlugin::build_search_payload(&cfg, "hello", None, None, Some(5));
        assert_eq!(payload["project"], "hermes-agent-rs");
        assert_eq!(payload["topic_path"], "runbooks/backend/parity");
        assert_eq!(payload["query"], "hello");
        assert_eq!(payload["limit"], 5);
    }

    #[test]
    fn test_apply_pressure_tuning_forces_fast_mode_and_sources() {
        let cfg = ContextLatticeConfig {
            orchestrator_url: DEFAULT_ORCHESTRATOR_URL.to_string(),
            project: "hermes-agent-rs".to_string(),
            topic_path: "runbooks/backend/parity".to_string(),
            agent_id: "codex".to_string(),
            timeout_secs: 10.0,
            include_grounding: true,
            include_retrieval_debug: true,
            default_retrieval_mode: "balanced".to_string(),
            default_file_name: "notes/test.md".to_string(),
            pressure_aware: true,
            pressure_pending_threshold: 12,
            pressure_cooldown_threshold: 3,
            pressure_status_ttl_secs: 2.5,
            pressure_fast_sources: vec!["topic_rollups".to_string(), "qdrant".to_string()],
            pressure_force_fast_mode: true,
            pressure_force_fast_prefetch: true,
            api_key: None,
        };
        let mut payload = ContextLatticeMemoryPlugin::build_search_payload(
            &cfg,
            "hello",
            None,
            Some("balanced"),
            Some(5),
        );
        let pressure = QueuePressureSnapshot {
            pending_total: 42,
            cooldown_active: 6,
            pressure_active: true,
            captured_at: Instant::now(),
        };
        ContextLatticeMemoryPlugin::apply_pressure_tuning_to_payload(
            &cfg,
            &mut payload,
            Some(&pressure),
            true,
        );
        assert_eq!(payload["retrieval_mode"], "fast");
        assert_eq!(payload["sources"], json!(["topic_rollups", "qdrant"]));
    }
}
