//! Hindsight memory provider plugin.
//!
//! Implements `MemoryProviderPlugin` for Hindsight — long-term memory with
//! knowledge graph, entity resolution, and multi-strategy retrieval.
//!
//! Mirrors the Python `plugins/memory/hindsight/__init__.py`.
//!
//! Configuration:
//!   - `HINDSIGHT_API_KEY` (required for cloud mode)
//!   - `HINDSIGHT_BANK_ID` (default: "hermes")
//!   - `HINDSIGHT_BUDGET` (default: "mid")
//!   - `HINDSIGHT_API_URL` (default: "https://api.hindsight.vectorize.io")
//!   - `HINDSIGHT_MODE` (default: "cloud")
//!   - `$HERMES_HOME/hindsight/config.json` overrides

use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::blocking::Client;
use serde_json::{json, Value};

use crate::memory_manager::MemoryProviderPlugin;

const DEFAULT_API_URL: &str = "https://api.hindsight.vectorize.io";
const DEFAULT_LOCAL_URL: &str = "http://localhost:8888";
const VALID_BUDGETS: &[&str] = &["low", "mid", "high"];

// ---------------------------------------------------------------------------
// Tool schemas
// ---------------------------------------------------------------------------

fn retain_schema() -> Value {
    json!({
        "name": "hindsight_retain",
        "description": "Store information to long-term memory. Hindsight automatically extracts structured facts, resolves entities, and indexes for retrieval.",
        "parameters": {
            "type": "object",
            "properties": {
                "content": {"type": "string", "description": "The information to store."},
                "context": {"type": "string", "description": "Short label (e.g. 'user preference', 'project decision')."}
            },
            "required": ["content"]
        }
    })
}

fn recall_schema() -> Value {
    json!({
        "name": "hindsight_recall",
        "description": "Search long-term memory. Returns memories ranked by relevance using semantic search, keyword matching, entity graph traversal, and reranking.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "What to search for."}
            },
            "required": ["query"]
        }
    })
}

fn reflect_schema() -> Value {
    json!({
        "name": "hindsight_reflect",
        "description": "Synthesize a reasoned answer from long-term memories. Unlike recall, this reasons across all stored memories to produce a coherent response.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string", "description": "The question to reflect on."}
            },
            "required": ["query"]
        }
    })
}

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
struct HindsightConfig {
    api_key: String,
    api_url: String,
    bank_id: String,
    bank_id_template: String,
    budget: String,
    mode: String,
    memory_mode: String,
    prefetch_method: String,
    auto_retain: bool,
    auto_recall: bool,
    retain_every_n_turns: u32,
    retain_context: String,
    recall_max_tokens: usize,
    recall_max_input_chars: usize,
    recall_prompt_preamble: String,
    bank_mission: String,
    retain_async: bool,
    timeout_secs: u64,
}

impl HindsightConfig {
    fn load(hermes_home: &str) -> Self {
        let mut config = Self {
            api_key: std::env::var("HINDSIGHT_API_KEY").unwrap_or_default(),
            api_url: std::env::var("HINDSIGHT_API_URL").ok().unwrap_or_default(),
            bank_id: std::env::var("HINDSIGHT_BANK_ID").unwrap_or_else(|_| "hermes".into()),
            bank_id_template: std::env::var("HINDSIGHT_BANK_ID_TEMPLATE").unwrap_or_default(),
            budget: std::env::var("HINDSIGHT_BUDGET").unwrap_or_else(|_| "mid".into()),
            mode: std::env::var("HINDSIGHT_MODE").unwrap_or_else(|_| "cloud".into()),
            memory_mode: "hybrid".into(),
            prefetch_method: "recall".into(),
            auto_retain: true,
            auto_recall: true,
            retain_every_n_turns: 1,
            retain_context: "conversation between Hermes Agent and the User".into(),
            recall_max_tokens: 4096,
            recall_max_input_chars: 800,
            recall_prompt_preamble: String::new(),
            bank_mission: String::new(),
            retain_async: true,
            timeout_secs: std::env::var("HINDSIGHT_TIMEOUT")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(90),
        };

        // Try profile-scoped config first, then legacy path
        let profile_path = std::path::Path::new(hermes_home)
            .join("hindsight")
            .join("config.json");
        let legacy_path = dirs::home_dir().map(|h| h.join(".hindsight").join("config.json"));

        let content = std::fs::read_to_string(&profile_path)
            .ok()
            .or_else(|| legacy_path.and_then(|p| std::fs::read_to_string(p).ok()));

        if let Some(text) = content {
            if let Ok(raw) = serde_json::from_str::<Value>(&text) {
                if let Some(key) = raw
                    .get("apiKey")
                    .or(raw.get("api_key"))
                    .and_then(|v| v.as_str())
                {
                    if !key.is_empty() {
                        config.api_key = key.to_string();
                    }
                }
                if let Some(url) = raw.get("api_url").and_then(|v| v.as_str()) {
                    if !url.is_empty() {
                        config.api_url = url.to_string();
                    }
                }
                if let Some(mode) = raw.get("mode").and_then(|v| v.as_str()) {
                    config.mode = mode.to_string();
                }
                if let Some(bank) = raw.get("bank_id").and_then(|v| v.as_str()) {
                    config.bank_id = bank.to_string();
                } else if let Some(banks) = raw.get("banks").and_then(|v| v.get("hermes")) {
                    if let Some(bid) = banks.get("bankId").and_then(|v| v.as_str()) {
                        config.bank_id = bid.to_string();
                    }
                    if let Some(budget) = banks.get("budget").and_then(|v| v.as_str()) {
                        if VALID_BUDGETS.contains(&budget) {
                            config.budget = budget.to_string();
                        }
                    }
                }
                if let Some(budget) = raw
                    .get("recall_budget")
                    .or(raw.get("budget"))
                    .and_then(|v| v.as_str())
                {
                    if VALID_BUDGETS.contains(&budget) {
                        config.budget = budget.to_string();
                    }
                }
                if let Some(template) = raw.get("bank_id_template").and_then(|v| v.as_str()) {
                    config.bank_id_template = template.to_string();
                }
                if let Some(mm) = raw.get("memory_mode").and_then(|v| v.as_str()) {
                    if ["context", "tools", "hybrid"].contains(&mm) {
                        config.memory_mode = mm.to_string();
                    }
                }
                if let Some(pm) = raw.get("recall_prefetch_method").and_then(|v| v.as_str()) {
                    if ["recall", "reflect"].contains(&pm) {
                        config.prefetch_method = pm.to_string();
                    }
                }
                if let Some(ar) = raw.get("auto_retain").and_then(|v| v.as_bool()) {
                    config.auto_retain = ar;
                }
                if let Some(ar) = raw.get("auto_recall").and_then(|v| v.as_bool()) {
                    config.auto_recall = ar;
                }
                if let Some(n) = raw.get("retain_every_n_turns").and_then(|v| v.as_u64()) {
                    config.retain_every_n_turns = n.max(1) as u32;
                }
                if let Some(ctx) = raw.get("retain_context").and_then(|v| v.as_str()) {
                    config.retain_context = ctx.to_string();
                }
                if let Some(mt) = raw.get("recall_max_tokens").and_then(|v| v.as_u64()) {
                    config.recall_max_tokens = mt as usize;
                }
                if let Some(mic) = raw.get("recall_max_input_chars").and_then(|v| v.as_u64()) {
                    config.recall_max_input_chars = mic as usize;
                }
                if let Some(p) = raw.get("recall_prompt_preamble").and_then(|v| v.as_str()) {
                    config.recall_prompt_preamble = p.to_string();
                }
                if let Some(bm) = raw.get("bank_mission").and_then(|v| v.as_str()) {
                    config.bank_mission = bm.to_string();
                }
                if let Some(ra) = raw.get("retain_async").and_then(|v| v.as_bool()) {
                    config.retain_async = ra;
                }
                if let Some(timeout) = raw
                    .get("hindsight_timeout")
                    .or(raw.get("timeout"))
                    .and_then(|v| v.as_u64())
                {
                    config.timeout_secs = timeout.max(1);
                }
            }
        }

        // Apply defaults for api_url based on mode
        if config.api_url.is_empty() {
            config.api_url = match config.mode.as_str() {
                "local_embedded" | "local_external" | "local" => DEFAULT_LOCAL_URL.to_string(),
                _ => DEFAULT_API_URL.to_string(),
            };
        }

        // Normalize "local" → "local_embedded"
        if config.mode == "local" {
            config.mode = "local_embedded".to_string();
        }

        config
    }
}

// ---------------------------------------------------------------------------
// HindsightPlugin
// ---------------------------------------------------------------------------

/// Hindsight long-term memory with knowledge graph and multi-strategy retrieval.
pub struct HindsightPlugin {
    config: Mutex<Option<HindsightConfig>>,
    session_id: Mutex<String>,
    prefetch_result: Arc<Mutex<String>>,
    turn_counter: Mutex<u32>,
    session_turns: Mutex<Vec<String>>,
}

impl HindsightPlugin {
    pub fn new() -> Self {
        Self {
            config: Mutex::new(None),
            session_id: Mutex::new(String::new()),
            prefetch_result: Arc::new(Mutex::new(String::new())),
            turn_counter: Mutex::new(0),
            session_turns: Mutex::new(Vec::new()),
        }
    }

    fn memory_mode(&self) -> String {
        self.config
            .lock()
            .unwrap()
            .as_ref()
            .map(|c| c.memory_mode.clone())
            .unwrap_or_else(|| "hybrid".to_string())
    }
}

impl MemoryProviderPlugin for HindsightPlugin {
    fn name(&self) -> &str {
        "hindsight"
    }

    fn is_available(&self) -> bool {
        let api_key = std::env::var("HINDSIGHT_API_KEY").unwrap_or_default();
        let api_url = std::env::var("HINDSIGHT_API_URL").unwrap_or_default();
        let mode = std::env::var("HINDSIGHT_MODE").unwrap_or_default();
        if matches!(mode.as_str(), "local" | "local_embedded" | "local_external") {
            return true;
        }
        // Cloud mode requires credentials (or explicit API URL for self-hosted).
        !api_key.is_empty() || !api_url.is_empty()
    }

    fn initialize(&self, session_id: &str, hermes_home: &str) {
        let mut config = HindsightConfig::load(hermes_home);
        config.bank_id = resolve_bank_id_template(
            &config.bank_id_template,
            &config.bank_id,
            &[
                (
                    "profile",
                    std::env::var("HERMES_PROFILE").unwrap_or_default(),
                ),
                (
                    "workspace",
                    std::env::var("HERMES_WORKSPACE").unwrap_or_default(),
                ),
                (
                    "platform",
                    std::env::var("HERMES_PLATFORM").unwrap_or_default(),
                ),
                ("user", std::env::var("HERMES_USER_ID").unwrap_or_default()),
                ("session", session_id.to_string()),
            ],
        );
        tracing::info!(
            "Hindsight initialized: mode={}, api_url={}, bank={}, budget={}, memory_mode={}",
            config.mode,
            config.api_url,
            config.bank_id,
            config.budget,
            config.memory_mode
        );
        *self.session_id.lock().unwrap() = session_id.to_string();
        *self.turn_counter.lock().unwrap() = 0;
        self.session_turns.lock().unwrap().clear();
        *self.config.lock().unwrap() = Some(config);
    }

    fn system_prompt_block(&self) -> String {
        let config = self.config.lock().unwrap();
        let config = match config.as_ref() {
            Some(c) => c,
            None => return String::new(),
        };
        match config.memory_mode.as_str() {
            "context" => format!(
                "# Hindsight Memory\n\
                 Active (context mode). Bank: {}, budget: {}.\n\
                 Relevant memories are automatically injected into context.",
                config.bank_id, config.budget
            ),
            "tools" => format!(
                "# Hindsight Memory\n\
                 Active (tools mode). Bank: {}, budget: {}.\n\
                 Use hindsight_recall to search, hindsight_reflect for synthesis, \
                 hindsight_retain to store facts.",
                config.bank_id, config.budget
            ),
            _ => format!(
                "# Hindsight Memory\n\
                 Active. Bank: {}, budget: {}.\n\
                 Relevant memories are automatically injected into context. \
                 Use hindsight_recall to search, hindsight_reflect for synthesis, \
                 hindsight_retain to store facts.",
                config.bank_id, config.budget
            ),
        }
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

        let config = self.config.lock().unwrap();
        let preamble = config
            .as_ref()
            .and_then(|c| {
                if c.recall_prompt_preamble.is_empty() {
                    None
                } else {
                    Some(c.recall_prompt_preamble.clone())
                }
            })
            .unwrap_or_else(|| {
                "# Hindsight Memory (persistent cross-session context)\n\
                 Use this to answer questions about the user and prior sessions. \
                 Do not call tools to look up information that is already present here."
                    .to_string()
            });

        format!("{}\n\n{}", preamble, result)
    }

    fn queue_prefetch(&self, query: &str, _session_id: &str) {
        let config = self.config.lock().unwrap();
        let config = match config.as_ref() {
            Some(c) => c,
            None => return,
        };
        if config.memory_mode == "tools" || !config.auto_recall {
            return;
        }

        let mut q = query.to_string();
        if config.recall_max_input_chars > 0 && q.len() > config.recall_max_input_chars {
            q.truncate(config.recall_max_input_chars);
        }

        let cfg = config.clone();
        let out = Arc::clone(&self.prefetch_result);
        std::thread::spawn(move || {
            let client = match Client::builder()
                .timeout(Duration::from_secs(cfg.timeout_secs))
                .build()
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::debug!("Hindsight prefetch client build failed: {}", e);
                    return;
                }
            };
            let base = cfg.api_url.trim_end_matches('/').to_string();
            let bank = urlencode_path(&cfg.bank_id);
            let text = if cfg.prefetch_method == "reflect" {
                match hindsight_reflect(&client, &base, &bank, &cfg.api_key, &q, &cfg.budget) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::debug!("Hindsight reflect prefetch failed: {}", e);
                        return;
                    }
                }
            } else {
                match hindsight_recall(
                    &client,
                    &base,
                    &bank,
                    &cfg.api_key,
                    &q,
                    &cfg.budget,
                    cfg.recall_max_tokens,
                ) {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::debug!("Hindsight recall prefetch failed: {}", e);
                        return;
                    }
                }
            };
            if !text.is_empty() {
                let mut lock = out.lock().unwrap();
                *lock = text;
            }
        });
    }

    fn sync_turn(&self, user_content: &str, assistant_content: &str, session_id: &str) {
        let config = self.config.lock().unwrap();
        let config = match config.as_ref() {
            Some(c) => c,
            None => return,
        };
        if !config.auto_retain {
            return;
        }

        let mut counter = self.turn_counter.lock().unwrap();
        *counter += 1;
        if *counter % config.retain_every_n_turns != 0 {
            return;
        }

        let now = chrono::Utc::now().to_rfc3339();
        let turn = json!([
            {"role": "user", "content": user_content, "timestamp": &now},
            {"role": "assistant", "content": assistant_content, "timestamp": &now},
        ])
        .to_string();

        self.session_turns.lock().unwrap().push(turn);

        let cfg = config.clone();
        let sid = session_id.to_string();
        let content = {
            let mut turns = self.session_turns.lock().unwrap();
            let joined = turns.join(",");
            turns.clear();
            format!("[{}]", joined)
        };

        std::thread::spawn(move || {
            let client = match Client::builder()
                .timeout(Duration::from_secs(cfg.timeout_secs))
                .build()
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::debug!("Hindsight sync client: {}", e);
                    return;
                }
            };
            let base = cfg.api_url.trim_end_matches('/').to_string();
            let bank = urlencode_path(&cfg.bank_id);
            let url = format!("{}/v1/default/banks/{}/memories", base, bank);
            let body = json!({
                "items": [{"content": content, "context": cfg.retain_context}],
                "async": cfg.retain_async,
            });
            let mut req = client.post(&url).json(&body);
            if !cfg.api_key.is_empty() {
                req = req.bearer_auth(&cfg.api_key);
            }
            match req.send() {
                Ok(resp) if resp.status().is_success() => {
                    tracing::debug!("Hindsight retain_batch ok for session {}", sid);
                }
                Ok(resp) => {
                    tracing::debug!(
                        "Hindsight retain_batch HTTP {} for session {}",
                        resp.status(),
                        sid
                    );
                }
                Err(e) => tracing::debug!("Hindsight retain_batch error: {}", e),
            }
        });
    }

    fn get_tool_schemas(&self) -> Vec<Value> {
        if self.memory_mode() == "context" {
            return Vec::new();
        }
        vec![retain_schema(), recall_schema(), reflect_schema()]
    }

    fn handle_tool_call(&self, tool_name: &str, args: &Value) -> String {
        let cfg = match self.config.lock().unwrap().clone() {
            Some(c) => c,
            None => return json!({"error": "Hindsight not initialized"}).to_string(),
        };

        let client = match Client::builder()
            .timeout(Duration::from_secs(cfg.timeout_secs))
            .build()
        {
            Ok(c) => c,
            Err(e) => return json!({"error": format!("HTTP client: {}", e)}).to_string(),
        };
        let base = cfg.api_url.trim_end_matches('/').to_string();
        let bank = urlencode_path(&cfg.bank_id);

        match tool_name {
            "hindsight_retain" => {
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                if content.is_empty() {
                    return json!({"error": "Missing required parameter: content"}).to_string();
                }
                let ctx = args.get("context").and_then(|v| v.as_str());
                match hindsight_retain(
                    &client,
                    &base,
                    &bank,
                    &cfg.api_key,
                    content,
                    ctx,
                    cfg.retain_async,
                ) {
                    Ok(()) => json!({"result": "Memory stored successfully."}).to_string(),
                    Err(e) => json!({"error": e}).to_string(),
                }
            }
            "hindsight_recall" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                if query.is_empty() {
                    return json!({"error": "Missing required parameter: query"}).to_string();
                }
                match hindsight_recall(
                    &client,
                    &base,
                    &bank,
                    &cfg.api_key,
                    query,
                    &cfg.budget,
                    cfg.recall_max_tokens,
                ) {
                    Ok(text) if !text.is_empty() => json!({"result": text}).to_string(),
                    Ok(_) => json!({"result": "No relevant memories found."}).to_string(),
                    Err(e) => json!({"error": e}).to_string(),
                }
            }
            "hindsight_reflect" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                if query.is_empty() {
                    return json!({"error": "Missing required parameter: query"}).to_string();
                }
                match hindsight_reflect(&client, &base, &bank, &cfg.api_key, query, &cfg.budget) {
                    Ok(text) if !text.is_empty() => json!({"result": text}).to_string(),
                    Ok(_) => json!({"result": "No relevant memories found."}).to_string(),
                    Err(e) => json!({"error": e}).to_string(),
                }
            }
            _ => json!({"error": format!("Unknown tool: {}", tool_name)}).to_string(),
        }
    }

    fn on_turn_start(&self, turn_number: u32, _message: &str) {
        *self.turn_counter.lock().unwrap() = turn_number;
    }

    fn on_session_end(&self, _messages: &[Value]) {
        tracing::debug!("Hindsight session end");
    }

    fn shutdown(&self) {
        tracing::debug!("Hindsight memory plugin shutdown");
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key": "mode", "description": "Connection mode", "default": "cloud", "choices": ["cloud", "local_embedded", "local_external"]},
            {"key": "api_url", "description": "Hindsight API URL", "default": DEFAULT_API_URL},
            {"key": "api_key", "description": "Hindsight API key", "secret": true, "env_var": "HINDSIGHT_API_KEY", "url": "https://ui.hindsight.vectorize.io"},
            {"key": "bank_id", "description": "Memory bank name", "default": "hermes"},
            {"key": "bank_id_template", "description": "Optional dynamic bank template with placeholders: {profile}, {workspace}, {platform}, {user}, {session}", "default": ""},
            {"key": "hindsight_timeout", "description": "HTTP timeout in seconds", "default": 90},
            {"key": "recall_budget", "description": "Recall thoroughness", "default": "mid", "choices": ["low", "mid", "high"]},
            {"key": "memory_mode", "description": "Memory integration mode", "default": "hybrid", "choices": ["hybrid", "context", "tools"]}
        ]))
    }

    fn save_config(&self, config: &Value) -> Result<(), String> {
        // Would write to $HERMES_HOME/hindsight/config.json
        let _ = config;
        Ok(())
    }
}

fn urlencode_path(seg: &str) -> String {
    seg.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u8),
        })
        .collect()
}

fn sanitize_bank_segment(value: &str) -> String {
    if value.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
            out.push(ch);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches(|c| c == '-' || c == '_').to_string()
}

fn resolve_bank_id_template(
    template: &str,
    fallback: &str,
    placeholders: &[(&str, String)],
) -> String {
    if template.trim().is_empty() {
        return fallback.to_string();
    }
    let mut rendered = template.to_string();
    for (key, value) in placeholders {
        let token = format!("{{{}}}", key);
        rendered = rendered.replace(&token, &sanitize_bank_segment(value));
    }
    if rendered.contains('{') || rendered.contains('}') {
        return fallback.to_string();
    }
    while rendered.contains("--") {
        rendered = rendered.replace("--", "-");
    }
    while rendered.contains("__") {
        rendered = rendered.replace("__", "_");
    }
    let normalized = rendered.trim_matches(|c| c == '-' || c == '_').to_string();
    if normalized.is_empty() {
        fallback.to_string()
    } else {
        normalized
    }
}

fn hindsight_recall(
    client: &Client,
    base: &str,
    bank: &str,
    api_key: &str,
    query: &str,
    budget: &str,
    max_tokens: usize,
) -> Result<String, String> {
    let url = format!("{}/v1/default/banks/{}/memories/recall", base, bank);
    let body = json!({
        "query": query,
        "budget": budget,
        "max_tokens": max_tokens,
    });
    let mut req = client.post(&url).json(&body);
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let resp = req.send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!(
            "recall HTTP {}: {}",
            resp.status(),
            resp.text().unwrap_or_default()
        ));
    }
    let v: Value = resp.json().map_err(|e| e.to_string())?;
    let lines: Vec<String> = v
        .get("results")
        .and_then(|r| r.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|item| {
                    item.get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                })
                .collect()
        })
        .unwrap_or_default();
    if lines.is_empty() {
        return Ok(String::new());
    }
    Ok(lines
        .into_iter()
        .map(|l| format!("- {}", l))
        .collect::<Vec<_>>()
        .join("\n"))
}

fn hindsight_reflect(
    client: &Client,
    base: &str,
    bank: &str,
    api_key: &str,
    query: &str,
    budget: &str,
) -> Result<String, String> {
    let url = format!("{}/v1/default/banks/{}/reflect", base, bank);
    let body = json!({ "query": query, "budget": budget });
    let mut req = client.post(&url).json(&body);
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let resp = req.send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!(
            "reflect HTTP {}: {}",
            resp.status(),
            resp.text().unwrap_or_default()
        ));
    }
    let v: Value = resp.json().map_err(|e| e.to_string())?;
    Ok(v.get("text")
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string())
}

fn hindsight_retain(
    client: &Client,
    base: &str,
    bank: &str,
    api_key: &str,
    content: &str,
    context: Option<&str>,
    async_mode: bool,
) -> Result<(), String> {
    let url = format!("{}/v1/default/banks/{}/memories", base, bank);
    let mut item = json!({ "content": content });
    if let Some(ctx) = context {
        item["context"] = json!(ctx);
    }
    let body = json!({ "items": [item], "async": async_mode });
    let mut req = client.post(&url).json(&body);
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let resp = req.send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!(
            "retain HTTP {}: {}",
            resp.status(),
            resp.text().unwrap_or_default()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hindsight_plugin_name() {
        let plugin = HindsightPlugin::new();
        assert_eq!(plugin.name(), "hindsight");
    }

    #[test]
    fn test_hindsight_tool_schemas() {
        let plugin = HindsightPlugin::new();
        let schemas = plugin.get_tool_schemas();
        assert_eq!(schemas.len(), 3);
        let names: Vec<&str> = schemas
            .iter()
            .filter_map(|s| s.get("name").and_then(|n| n.as_str()))
            .collect();
        assert!(names.contains(&"hindsight_retain"));
        assert!(names.contains(&"hindsight_recall"));
        assert!(names.contains(&"hindsight_reflect"));
    }

    #[test]
    fn test_hindsight_context_mode_hides_tools() {
        let plugin = HindsightPlugin::new();
        *plugin.config.lock().unwrap() = Some(HindsightConfig {
            api_key: "test".into(),
            api_url: DEFAULT_API_URL.into(),
            bank_id: "hermes".into(),
            bank_id_template: String::new(),
            budget: "mid".into(),
            mode: "cloud".into(),
            memory_mode: "context".into(),
            prefetch_method: "recall".into(),
            auto_retain: true,
            auto_recall: true,
            retain_every_n_turns: 1,
            retain_context: String::new(),
            recall_max_tokens: 4096,
            recall_max_input_chars: 800,
            recall_prompt_preamble: String::new(),
            bank_mission: String::new(),
            retain_async: true,
            timeout_secs: 90,
        });
        assert!(plugin.get_tool_schemas().is_empty());
    }

    #[test]
    fn test_hindsight_system_prompt_modes() {
        let plugin = HindsightPlugin::new();
        let make_config = |mode: &str| HindsightConfig {
            api_key: "test".into(),
            api_url: DEFAULT_API_URL.into(),
            bank_id: "hermes".into(),
            bank_id_template: String::new(),
            budget: "mid".into(),
            mode: "cloud".into(),
            memory_mode: mode.into(),
            prefetch_method: "recall".into(),
            auto_retain: true,
            auto_recall: true,
            retain_every_n_turns: 1,
            retain_context: String::new(),
            recall_max_tokens: 4096,
            recall_max_input_chars: 800,
            recall_prompt_preamble: String::new(),
            bank_mission: String::new(),
            retain_async: true,
            timeout_secs: 90,
        };

        *plugin.config.lock().unwrap() = Some(make_config("hybrid"));
        assert!(plugin.system_prompt_block().contains("hindsight_recall"));

        *plugin.config.lock().unwrap() = Some(make_config("context"));
        assert!(plugin.system_prompt_block().contains("context mode"));

        *plugin.config.lock().unwrap() = Some(make_config("tools"));
        assert!(plugin.system_prompt_block().contains("tools mode"));
    }

    #[test]
    fn test_hindsight_handle_tool_missing_args() {
        let plugin = HindsightPlugin::new();
        let result = plugin.handle_tool_call("hindsight_recall", &json!({}));
        assert!(result.contains("error"));
    }

    #[test]
    fn test_resolve_bank_id_template_sanitizes_and_collapses() {
        let bank = resolve_bank_id_template(
            "hermes-{profile}-{user}-{session}",
            "hermes",
            &[
                ("profile", "dev/workspace".to_string()),
                ("workspace", String::new()),
                ("platform", String::new()),
                ("user", "u@id".to_string()),
                ("session", "sess_123".to_string()),
            ],
        );
        assert_eq!(bank, "hermes-dev-workspace-u-id-sess_123");
    }

    #[test]
    fn test_resolve_bank_id_template_unknown_placeholder_falls_back() {
        let bank = resolve_bank_id_template(
            "hermes-{unknown}",
            "fallback-bank",
            &[("profile", "p1".to_string())],
        );
        assert_eq!(bank, "fallback-bank");
    }
}
