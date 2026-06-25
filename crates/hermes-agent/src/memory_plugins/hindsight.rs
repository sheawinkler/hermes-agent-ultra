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
//!   - `HINDSIGHT_RETAIN_TAGS` (comma-separated tags attached to retained memories)
//!   - `HINDSIGHT_RETAIN_OBSERVATION_SCOPES` (per_tag/combined/all_combinations or JSON scopes)
//!   - `$HERMES_HOME/hindsight/config.json` overrides

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use reqwest::blocking::Client;
use serde_json::{json, Value};

use crate::memory_manager::MemoryProviderPlugin;
use crate::memory_plugins::config_io;

const DEFAULT_API_URL: &str = "https://api.hindsight.vectorize.io";
const DEFAULT_LOCAL_URL: &str = "http://localhost:8888";
const DEFAULT_RECALL_TYPE: &str = "observation";
const DEFAULT_TIMEOUT_SECS: u64 = 120;
const MIN_VERSION_FOR_UPDATE_MODE_APPEND: &str = "0.5.0";
const VALID_BUDGETS: &[&str] = &["low", "mid", "high"];
static DOCUMENT_ID_COUNTER: AtomicU64 = AtomicU64::new(1);
static APPEND_CAPABILITY_CACHE: OnceLock<Mutex<HashMap<String, bool>>> = OnceLock::new();

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
                "context": {"type": "string", "description": "Short label (e.g. 'user preference', 'project decision')."},
                "tags": {
                    "type": "array",
                    "items": {"type": "string"},
                    "description": "Optional per-call tags to merge with configured default retain tags."
                }
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
    recall_types: Vec<String>,
    recall_prompt_preamble: String,
    bank_mission: String,
    retain_async: bool,
    retain_tags: Vec<String>,
    observation_scopes: Option<Value>,
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
            recall_types: default_recall_types(),
            recall_prompt_preamble: String::new(),
            bank_mission: String::new(),
            retain_async: true,
            retain_tags: parse_retain_tags_value(&Value::String(
                std::env::var("HINDSIGHT_RETAIN_TAGS").unwrap_or_default(),
            ))
            .unwrap_or_default(),
            observation_scopes: parse_observation_scopes_value(&Value::String(
                std::env::var("HINDSIGHT_RETAIN_OBSERVATION_SCOPES").unwrap_or_default(),
            )),
            timeout_secs: std::env::var("HINDSIGHT_TIMEOUT")
                .ok()
                .and_then(|v| v.parse::<u64>().ok())
                .map(|v| v.max(1))
                .unwrap_or(DEFAULT_TIMEOUT_SECS),
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
                if let Some(types) = raw.get("recall_types").and_then(parse_recall_types_value) {
                    config.recall_types = types;
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
                if let Some(tags) = raw.get("retain_tags").and_then(parse_retain_tags_value) {
                    config.retain_tags = tags;
                }
                if let Some(scopes) = raw
                    .get("observation_scopes")
                    .and_then(parse_observation_scopes_value)
                {
                    config.observation_scopes = Some(scopes);
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

        config.mode = normalize_hindsight_mode(&config.mode);

        // Apply defaults for api_url based on mode
        if config.api_url.is_empty() {
            config.api_url = match config.mode.as_str() {
                "local_external" => DEFAULT_LOCAL_URL.to_string(),
                _ => DEFAULT_API_URL.to_string(),
            };
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
    document_id: Mutex<String>,
    prefetch_result: Arc<Mutex<String>>,
    turn_counter: Mutex<u32>,
    session_turns: Mutex<Vec<String>>,
    retain_batch_counter: Mutex<u64>,
}

impl HindsightPlugin {
    pub fn new() -> Self {
        Self {
            config: Mutex::new(None),
            session_id: Mutex::new(String::new()),
            document_id: Mutex::new(String::new()),
            prefetch_result: Arc::new(Mutex::new(String::new())),
            turn_counter: Mutex::new(0),
            session_turns: Mutex::new(Vec::new()),
            retain_batch_counter: Mutex::new(0),
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

    fn next_retain_document_id(&self) -> String {
        let document_id = self.document_id.lock().unwrap().clone();
        let mut counter = self.retain_batch_counter.lock().unwrap();
        *counter = counter.saturating_add(1);
        format!("{}-batch-{}", document_id, *counter)
    }

    fn take_pending_turns(&self) -> Vec<String> {
        let mut turns = self.session_turns.lock().unwrap();
        if turns.is_empty() {
            Vec::new()
        } else {
            std::mem::take(&mut *turns)
        }
    }

    fn flush_pending_turns(&self, reason: &'static str) {
        let cfg = match self.config.lock().unwrap().clone() {
            Some(c) => c,
            None => return,
        };
        let turns = self.take_pending_turns();
        if turns.is_empty() {
            return;
        }
        let sid = self.session_id.lock().unwrap().clone();
        let fallback_document_id = self.next_retain_document_id();
        std::thread::spawn(move || {
            retain_hindsight_turns(&cfg, &sid, &fallback_document_id, &turns, reason);
        });
    }
}

impl MemoryProviderPlugin for HindsightPlugin {
    fn name(&self) -> &str {
        "hindsight"
    }

    fn backup_paths(&self) -> Vec<std::path::PathBuf> {
        dirs::home_dir()
            .map(|home| vec![home.join(".hindsight")])
            .unwrap_or_default()
    }

    fn is_available(&self) -> bool {
        let api_key = std::env::var("HINDSIGHT_API_KEY").unwrap_or_default();
        let api_url = std::env::var("HINDSIGHT_API_URL").unwrap_or_default();
        let mode = normalize_hindsight_mode(&std::env::var("HINDSIGHT_MODE").unwrap_or_default());
        if mode == "local_external" {
            return true;
        }
        let config_path = config_io::default_hermes_home()
            .join("hindsight")
            .join("config.json");
        let config = config_io::read_json_object(&config_path);
        if config
            .get("mode")
            .and_then(Value::as_str)
            .map(normalize_hindsight_mode)
            .is_some_and(|mode| mode == "local_external")
        {
            return true;
        }
        // Cloud mode requires credentials (or explicit API URL for self-hosted).
        !api_key.is_empty()
            || !api_url.is_empty()
            || ["api_key", "apiKey", "api_url"].iter().any(|key| {
                config
                    .get(*key)
                    .and_then(Value::as_str)
                    .is_some_and(|value| !value.trim().is_empty())
            })
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
        *self.document_id.lock().unwrap() = scoped_document_id(session_id);
        *self.turn_counter.lock().unwrap() = 0;
        *self.retain_batch_counter.lock().unwrap() = 0;
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
                    HindsightRecallRequest {
                        base: &base,
                        bank: &bank,
                        api_key: &cfg.api_key,
                        query: &q,
                        budget: &cfg.budget,
                        max_tokens: cfg.recall_max_tokens,
                        recall_types: &cfg.recall_types,
                    },
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

        let now = chrono::Utc::now().to_rfc3339();
        let turn = hindsight_turn_payload(user_content, assistant_content, &now);
        self.session_turns.lock().unwrap().push(turn);

        let mut counter = self.turn_counter.lock().unwrap();
        *counter += 1;
        if *counter % config.retain_every_n_turns != 0 {
            return;
        }

        let cfg = config.clone();
        let sid = session_id.to_string();
        let fallback_document_id = self.next_retain_document_id();
        let turns = self.take_pending_turns();
        if turns.is_empty() {
            return;
        }

        std::thread::spawn(move || {
            retain_hindsight_turns(&cfg, &sid, &fallback_document_id, &turns, "sync_turn");
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
                let call_tags = args
                    .get("tags")
                    .and_then(parse_retain_tags_value)
                    .unwrap_or_default();
                let retain_tags = merge_retain_tags(&cfg.retain_tags, &call_tags);
                match hindsight_retain(
                    &client,
                    HindsightRetainRequest {
                        base: &base,
                        bank: &bank,
                        api_key: &cfg.api_key,
                        content,
                        context: ctx,
                        async_mode: cfg.retain_async,
                        retain_tags: &retain_tags,
                        observation_scopes: cfg.observation_scopes.as_ref(),
                    },
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
                    HindsightRecallRequest {
                        base: &base,
                        bank: &bank,
                        api_key: &cfg.api_key,
                        query,
                        budget: &cfg.budget,
                        max_tokens: cfg.recall_max_tokens,
                        recall_types: &cfg.recall_types,
                    },
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
        self.flush_pending_turns("session_end");
        tracing::debug!("Hindsight session end");
    }

    fn on_session_switch(&self, new_session_id: &str, parent_session_id: &str, reset: bool) {
        let new_session_id = new_session_id.trim();
        if new_session_id.is_empty() {
            return;
        }
        self.flush_pending_turns("session_switch");
        *self.prefetch_result.lock().unwrap() = String::new();
        *self.session_id.lock().unwrap() = new_session_id.to_string();
        *self.document_id.lock().unwrap() = scoped_document_id(new_session_id);
        *self.turn_counter.lock().unwrap() = 0;
        *self.retain_batch_counter.lock().unwrap() = 0;
        self.session_turns.lock().unwrap().clear();
        tracing::debug!(
            "Hindsight session switch: new_session={} parent={} reset={}",
            new_session_id,
            parent_session_id,
            reset
        );
    }

    fn shutdown(&self) {
        self.flush_pending_turns("shutdown");
        tracing::debug!("Hindsight memory plugin shutdown");
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key": "mode", "description": "Connection mode. Legacy local/local_embedded values are treated as local_external in the Rust runtime.", "default": "cloud", "choices": ["cloud", "local_external"]},
            {"key": "api_url", "description": "Hindsight API URL", "default": DEFAULT_API_URL},
            {"key": "api_key", "description": "Hindsight API key", "secret": true, "env_var": "HINDSIGHT_API_KEY", "url": "https://ui.hindsight.vectorize.io"},
            {"key": "bank_id", "description": "Memory bank name", "default": "hermes"},
            {"key": "bank_id_template", "description": "Optional dynamic bank template with placeholders: {profile}, {workspace}, {platform}, {user}, {session}", "default": ""},
            {"key": "hindsight_timeout", "description": "HTTP timeout in seconds", "default": DEFAULT_TIMEOUT_SECS},
            {"key": "recall_budget", "description": "Recall thoroughness", "default": "mid", "choices": ["low", "mid", "high"]},
            {"key": "recall_types", "description": "Fact types returned by recall", "default": DEFAULT_RECALL_TYPE},
            {"key": "memory_mode", "description": "Memory integration mode", "default": "hybrid", "choices": ["hybrid", "context", "tools"]},
            {"key": "retain_tags", "description": "Default tags applied to retained memories (comma-separated or list)", "default": ""},
            {"key": "observation_scopes", "description": "Hindsight observation scoping: combined, per_tag, all_combinations, or JSON list of tag lists", "default": ""}
        ]))
    }

    fn save_config(&self, config: &Value) -> Result<(), String> {
        let path = config_io::default_hermes_home()
            .join("hindsight")
            .join("config.json");
        config_io::merge_and_write_owner_only(&path, config)
    }
}

fn normalize_hindsight_mode(mode: &str) -> String {
    match mode.trim() {
        "local" | "local_embedded" => "local_external".to_string(),
        other => other.to_string(),
    }
}

fn nonempty_str(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
    }
}

fn scoped_document_id(session_id: &str) -> String {
    let counter = DOCUMENT_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    let started_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let suffix = format!("{}-{}-{}", std::process::id(), started_at, counter);
    let session = session_id.trim();
    if session.is_empty() {
        suffix
    } else {
        format!("{}-{}", session, suffix)
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

fn default_recall_types() -> Vec<String> {
    vec![DEFAULT_RECALL_TYPE.to_string()]
}

fn parse_recall_types_str(value: &str) -> Option<Vec<String>> {
    let types: Vec<String> = value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect();
    if types.is_empty() {
        None
    } else {
        Some(types)
    }
}

fn parse_recall_types_value(value: &Value) -> Option<Vec<String>> {
    if let Some(text) = value.as_str() {
        return parse_recall_types_str(text);
    }
    let items = value.as_array()?;
    let types: Vec<String> = items
        .iter()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|item| !item.is_empty())
        .map(ToString::to_string)
        .collect();
    if types.is_empty() {
        None
    } else {
        Some(types)
    }
}

fn push_unique_trimmed(items: &mut Vec<String>, value: &str) {
    let trimmed = value.trim();
    if !trimmed.is_empty() && !items.iter().any(|existing| existing == trimmed) {
        items.push(trimmed.to_string());
    }
}

fn parse_retain_tags_str(value: &str) -> Option<Vec<String>> {
    let mut tags = Vec::new();
    for item in value.split(',') {
        push_unique_trimmed(&mut tags, item);
    }
    if tags.is_empty() {
        None
    } else {
        Some(tags)
    }
}

fn parse_retain_tags_value(value: &Value) -> Option<Vec<String>> {
    if let Some(text) = value.as_str() {
        return parse_retain_tags_str(text);
    }
    let items = value.as_array()?;
    let mut tags = Vec::new();
    for item in items.iter().filter_map(Value::as_str) {
        push_unique_trimmed(&mut tags, item);
    }
    if tags.is_empty() {
        None
    } else {
        Some(tags)
    }
}

fn merge_retain_tags(default_tags: &[String], call_tags: &[String]) -> Vec<String> {
    let mut tags = Vec::new();
    for tag in default_tags.iter().chain(call_tags.iter()) {
        push_unique_trimmed(&mut tags, tag);
    }
    tags
}

fn parse_observation_scopes_value(value: &Value) -> Option<Value> {
    if let Some(text) = value.as_str() {
        let trimmed = text.trim();
        if trimmed.is_empty() {
            return None;
        }
        if matches!(trimmed, "per_tag" | "combined" | "all_combinations") {
            return Some(Value::String(trimmed.to_string()));
        }
        if trimmed.starts_with('[') {
            return serde_json::from_str::<Value>(trimmed)
                .ok()
                .and_then(|parsed| parse_observation_scopes_value(&parsed));
        }
        return None;
    }

    let items = value.as_array()?;
    if items.iter().all(Value::is_string) {
        let tags = parse_retain_tags_value(value)?;
        return Some(json!([tags]));
    }

    let scopes = items
        .iter()
        .filter_map(parse_retain_tags_value)
        .filter(|scope| !scope.is_empty())
        .collect::<Vec<_>>();
    if scopes.is_empty() {
        None
    } else {
        Some(json!(scopes))
    }
}

fn apply_retain_item_options(
    item: &mut Value,
    retain_tags: &[String],
    observation_scopes: Option<&Value>,
) {
    if !retain_tags.is_empty() {
        item["tags"] = json!(retain_tags);
    }
    if let Some(scopes) = observation_scopes {
        item["observation_scopes"] = scopes.clone();
    }
}

fn hindsight_recall_body(
    query: &str,
    budget: &str,
    max_tokens: usize,
    recall_types: &[String],
) -> Value {
    json!({
        "query": query,
        "budget": budget,
        "max_tokens": max_tokens,
        "types": recall_types,
    })
}

fn hindsight_turn_payload(user_content: &str, assistant_content: &str, timestamp: &str) -> String {
    json!([
        {"role": "user", "content": user_content, "timestamp": timestamp},
        {"role": "assistant", "content": assistant_content, "timestamp": timestamp},
    ])
    .to_string()
}

fn hindsight_sync_turn_body(
    content: &str,
    context: &str,
    async_mode: bool,
    document_id: Option<&str>,
    update_mode: Option<&str>,
    retain_tags: &[String],
    observation_scopes: Option<&Value>,
) -> Value {
    let mut item = json!({"content": content, "context": context});
    if let Some(update_mode) = update_mode {
        item["update_mode"] = json!(update_mode);
    }
    apply_retain_item_options(&mut item, retain_tags, observation_scopes);
    let mut body = json!({
        "items": [item],
        "async": async_mode,
    });
    if let Some(document_id) = document_id {
        body["document_id"] = json!(document_id);
    }
    body
}

fn retain_hindsight_turns(
    cfg: &HindsightConfig,
    session_id: &str,
    fallback_document_id: &str,
    turns: &[String],
    reason: &str,
) {
    if turns.is_empty() {
        return;
    }
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
    let (document_id, update_mode) = hindsight_retain_target(
        &client,
        &base,
        &cfg.api_key,
        session_id,
        fallback_document_id,
    );
    let content = format!("[{}]", turns.join(","));
    let url = format!("{}/v1/default/banks/{}/memories", base, bank);
    let lineage_tags = nonempty_str(session_id)
        .map(|session_id| vec![format!("session:{session_id}")])
        .unwrap_or_default();
    let retain_tags = merge_retain_tags(&cfg.retain_tags, &lineage_tags);
    let body = hindsight_sync_turn_body(
        &content,
        &cfg.retain_context,
        cfg.retain_async,
        nonempty_str(&document_id),
        update_mode,
        &retain_tags,
        cfg.observation_scopes.as_ref(),
    );
    let mut req = client.post(&url).json(&body);
    if !cfg.api_key.is_empty() {
        req = req.bearer_auth(&cfg.api_key);
    }
    match req.send() {
        Ok(resp) if resp.status().is_success() => {
            tracing::debug!(
                "Hindsight retain_batch ok for session {} (reason={}, turns={}, update_mode={:?})",
                session_id,
                reason,
                turns.len(),
                update_mode
            );
        }
        Ok(resp) => {
            tracing::debug!(
                "Hindsight retain_batch HTTP {} for session {} (reason={})",
                resp.status(),
                session_id,
                reason
            );
        }
        Err(e) => tracing::debug!("Hindsight retain_batch error (reason={}): {}", reason, e),
    }
}

fn hindsight_retain_target(
    client: &Client,
    base: &str,
    api_key: &str,
    session_id: &str,
    fallback_document_id: &str,
) -> (String, Option<&'static str>) {
    let stable_session = session_id.trim();
    if !stable_session.is_empty() && hindsight_api_supports_append(client, base, api_key) {
        (stable_session.to_string(), Some("append"))
    } else {
        (fallback_document_id.to_string(), None)
    }
}

fn hindsight_api_supports_append(client: &Client, base: &str, api_key: &str) -> bool {
    let base = base.trim_end_matches('/').to_string();
    if base.is_empty() {
        return false;
    }
    let cache = APPEND_CAPABILITY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(cached) = cache.lock().unwrap().get(&base).copied() {
        return cached;
    }

    let url = format!("{}/version", base);
    let mut req = client.get(&url);
    if !api_key.is_empty() {
        req = req.bearer_auth(api_key);
    }
    let version = req
        .send()
        .ok()
        .filter(|resp| resp.status().is_success())
        .and_then(|resp| resp.json::<Value>().ok())
        .and_then(|value| {
            value
                .get("version")
                .or_else(|| value.get("api_version"))
                .and_then(Value::as_str)
                .map(ToString::to_string)
        });
    let supported =
        hindsight_version_meets_minimum(version.as_deref(), MIN_VERSION_FOR_UPDATE_MODE_APPEND);
    cache.lock().unwrap().insert(base.clone(), supported);
    if !supported {
        tracing::warn!(
            "Hindsight API at {} does not report {}+ append support; using unique batch document ids",
            base,
            MIN_VERSION_FOR_UPDATE_MODE_APPEND
        );
    }
    supported
}

fn hindsight_version_meets_minimum(actual: Option<&str>, required: &str) -> bool {
    let Some(actual) = actual.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    let Some(actual_parts) = parse_semver_core(actual) else {
        return false;
    };
    let Some(required_parts) = parse_semver_core(required) else {
        return false;
    };
    actual_parts >= required_parts
}

fn parse_semver_core(raw: &str) -> Option<[u64; 3]> {
    let core = raw
        .trim()
        .trim_start_matches('v')
        .split(['-', '+'])
        .next()
        .unwrap_or("");
    let mut parts = [0_u64; 3];
    for (idx, piece) in core.split('.').take(3).enumerate() {
        parts[idx] = piece.parse::<u64>().ok()?;
    }
    Some(parts)
}

struct HindsightRecallRequest<'a> {
    base: &'a str,
    bank: &'a str,
    api_key: &'a str,
    query: &'a str,
    budget: &'a str,
    max_tokens: usize,
    recall_types: &'a [String],
}

fn hindsight_recall(
    client: &Client,
    request: HindsightRecallRequest<'_>,
) -> Result<String, String> {
    let url = format!(
        "{}/v1/default/banks/{}/memories/recall",
        request.base, request.bank
    );
    let body = hindsight_recall_body(
        request.query,
        request.budget,
        request.max_tokens,
        request.recall_types,
    );
    let mut req = client.post(&url).json(&body);
    if !request.api_key.is_empty() {
        req = req.bearer_auth(request.api_key);
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

struct HindsightRetainRequest<'a> {
    base: &'a str,
    bank: &'a str,
    api_key: &'a str,
    content: &'a str,
    context: Option<&'a str>,
    async_mode: bool,
    retain_tags: &'a [String],
    observation_scopes: Option<&'a Value>,
}

fn hindsight_retain(client: &Client, request: HindsightRetainRequest<'_>) -> Result<(), String> {
    let url = format!(
        "{}/v1/default/banks/{}/memories",
        request.base, request.bank
    );
    let mut item = json!({ "content": request.content });
    if let Some(ctx) = request.context {
        item["context"] = json!(ctx);
    }
    apply_retain_item_options(&mut item, request.retain_tags, request.observation_scopes);
    let body = json!({ "items": [item], "async": request.async_mode });
    let mut req = client.post(&url).json(&body);
    if !request.api_key.is_empty() {
        req = req.bearer_auth(request.api_key);
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
            recall_types: default_recall_types(),
            recall_prompt_preamble: String::new(),
            bank_mission: String::new(),
            retain_async: true,
            retain_tags: Vec::new(),
            observation_scopes: None,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
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
            recall_types: default_recall_types(),
            recall_prompt_preamble: String::new(),
            bank_mission: String::new(),
            retain_async: true,
            retain_tags: Vec::new(),
            observation_scopes: None,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
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
    fn test_hindsight_recall_body_defaults_to_observations() {
        let body = hindsight_recall_body("dark mode", "mid", 4096, &default_recall_types());
        assert_eq!(body["query"], "dark mode");
        assert_eq!(body["budget"], "mid");
        assert_eq!(body["max_tokens"], 4096);
        assert_eq!(body["types"], json!(["observation"]));
    }

    #[test]
    fn test_hindsight_save_config_writes_owner_only() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let path = tmp.path().join("hindsight").join("config.json");

        HindsightPlugin::new()
            .save_config(&json!({"api_key":"hd-secret"}))
            .expect("save config");

        let parsed: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read config"))
                .expect("parse config");
        assert_eq!(parsed["api_key"], "hd-secret");

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
    fn test_parse_recall_types_accepts_string_or_list() {
        assert_eq!(
            parse_recall_types_value(&json!("observation,world, experience")),
            Some(vec![
                "observation".to_string(),
                "world".to_string(),
                "experience".to_string()
            ])
        );
        assert_eq!(
            parse_recall_types_value(&json!(["world", " experience ", "", 7])),
            Some(vec!["world".to_string(), "experience".to_string()])
        );
        assert_eq!(parse_recall_types_value(&json!(" , ")), None);
    }

    #[test]
    fn test_parse_retain_tags_and_observation_scopes() {
        assert_eq!(
            parse_retain_tags_value(&json!("project:ultra, session:s1, project:ultra")),
            Some(vec!["project:ultra".to_string(), "session:s1".to_string()])
        );
        assert_eq!(
            parse_retain_tags_value(&json!(["alpha", " beta ", "", "alpha"])),
            Some(vec!["alpha".to_string(), "beta".to_string()])
        );
        assert_eq!(
            merge_retain_tags(
                &["alpha".into(), "beta".into()],
                &["beta".into(), "gamma".into()]
            ),
            vec!["alpha".to_string(), "beta".to_string(), "gamma".to_string()]
        );
        assert_eq!(
            parse_observation_scopes_value(&json!("per_tag")),
            Some(json!("per_tag"))
        );
        assert_eq!(
            parse_observation_scopes_value(&json!(["alpha", "beta"])),
            Some(json!([["alpha", "beta"]]))
        );
        assert_eq!(
            parse_observation_scopes_value(&json!("[[\"alpha\"],[\"alpha\",\"beta\"]]")),
            Some(json!([["alpha"], ["alpha", "beta"]]))
        );
        assert_eq!(parse_observation_scopes_value(&json!("invalid")), None);
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

    fn write_hindsight_config(hermes_home: &std::path::Path, value: &Value) {
        let path = hermes_home.join("hindsight").join("config.json");
        std::fs::create_dir_all(path.parent().expect("config parent")).expect("mkdir config");
        std::fs::write(
            &path,
            serde_json::to_vec_pretty(value).expect("serialize config"),
        )
        .expect("write config");
    }

    #[test]
    fn test_config_accepts_snake_case_api_key_and_timeout_aliases() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let _api_key = EnvGuard::remove("HINDSIGHT_API_KEY");
        let _api_url = EnvGuard::remove("HINDSIGHT_API_URL");
        let _bank_id = EnvGuard::remove("HINDSIGHT_BANK_ID");
        let _mode = EnvGuard::remove("HINDSIGHT_MODE");
        let _timeout = EnvGuard::remove("HINDSIGHT_TIMEOUT");
        let _retain_tags = EnvGuard::remove("HINDSIGHT_RETAIN_TAGS");
        let _observation_scopes = EnvGuard::remove("HINDSIGHT_RETAIN_OBSERVATION_SCOPES");

        let tmp = tempfile::tempdir().expect("tempdir");
        write_hindsight_config(
            tmp.path(),
            &json!({
                "mode": "cloud",
                "api_key": "snake-secret",
                "timeout": 42,
                "retain_tags": ["project:ultra", "session:s1", "project:ultra"],
                "observation_scopes": [["project:ultra"], ["project:ultra", "session:s1"]]
            }),
        );
        let cfg = HindsightConfig::load(tmp.path().to_str().expect("tmp path"));
        assert_eq!(cfg.api_key, "snake-secret");
        assert_eq!(cfg.timeout_secs, 42);
        assert_eq!(
            cfg.retain_tags,
            vec!["project:ultra".to_string(), "session:s1".to_string()]
        );
        assert_eq!(
            cfg.observation_scopes,
            Some(json!([["project:ultra"], ["project:ultra", "session:s1"]]))
        );

        write_hindsight_config(
            tmp.path(),
            &json!({"mode": "cloud", "apiKey": "camel-secret", "hindsight_timeout": 17}),
        );
        let cfg = HindsightConfig::load(tmp.path().to_str().expect("tmp path"));
        assert_eq!(cfg.api_key, "camel-secret");
        assert_eq!(cfg.timeout_secs, 17);
    }

    #[test]
    fn test_config_uses_env_timeout_and_normalizes_legacy_local_mode() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let _api_key = EnvGuard::remove("HINDSIGHT_API_KEY");
        let _api_url = EnvGuard::remove("HINDSIGHT_API_URL");
        let _bank_id = EnvGuard::remove("HINDSIGHT_BANK_ID");
        let _mode = EnvGuard::set("HINDSIGHT_MODE", "local_embedded");
        let _timeout = EnvGuard::set("HINDSIGHT_TIMEOUT", "77");

        let tmp = tempfile::tempdir().expect("tempdir");
        write_hindsight_config(tmp.path(), &json!({}));

        let cfg = HindsightConfig::load(tmp.path().to_str().expect("tmp path"));
        assert_eq!(cfg.mode, "local_external");
        assert_eq!(cfg.api_url, DEFAULT_LOCAL_URL);
        assert_eq!(cfg.timeout_secs, 77);
    }

    #[test]
    fn test_available_with_local_external_config_mode() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let _ultra_home = EnvGuard::remove("HERMES_AGENT_ULTRA_HOME");
        let _api_key = EnvGuard::remove("HINDSIGHT_API_KEY");
        let _api_url = EnvGuard::remove("HINDSIGHT_API_URL");
        let _mode = EnvGuard::remove("HINDSIGHT_MODE");

        write_hindsight_config(tmp.path(), &json!({"mode": "local_external"}));

        assert!(HindsightPlugin::new().is_available());
    }

    #[test]
    fn test_turn_payload_preserves_non_ascii_and_document_id() {
        let turn =
            hindsight_turn_payload("Café 東京 🚀", "Zażółć gęślą jaźń", "2026-06-08T00:00:00Z");
        assert!(turn.contains("Café 東京 🚀"));
        assert!(turn.contains("Zażółć gęślą jaźń"));
        assert!(!turn.contains("\\u"));

        let parsed: Value = serde_json::from_str(&turn).expect("turn json");
        assert_eq!(parsed[0]["role"], "user");
        assert_eq!(parsed[0]["content"], "Café 東京 🚀");
        assert_eq!(parsed[1]["role"], "assistant");
        assert_eq!(parsed[1]["content"], "Zażółć gęślą jaźń");

        let content = format!("[{}]", turn);
        let body = hindsight_sync_turn_body(
            &content,
            "conversation",
            false,
            Some("session-1-doc"),
            Some("append"),
            &["project:ultra".to_string(), "session:session-1".to_string()],
            Some(&json!("per_tag")),
        );
        assert_eq!(body["async"], false);
        assert_eq!(body["document_id"], "session-1-doc");
        assert_eq!(body["items"][0]["update_mode"], "append");
        assert_eq!(body["items"][0]["content"], content);
        assert_eq!(body["items"][0]["context"], "conversation");
        assert_eq!(
            body["items"][0]["tags"],
            json!(["project:ultra", "session:session-1"])
        );
        assert_eq!(body["items"][0]["observation_scopes"], json!("per_tag"));
    }

    #[test]
    fn test_hindsight_version_probe_semver_gate() {
        assert!(hindsight_version_meets_minimum(Some("0.5.0"), "0.5.0"));
        assert!(hindsight_version_meets_minimum(Some("v0.5.6"), "0.5.0"));
        assert!(hindsight_version_meets_minimum(
            Some("0.6.0+local"),
            "0.5.0"
        ));
        assert!(!hindsight_version_meets_minimum(Some("0.4.99"), "0.5.0"));
        assert!(!hindsight_version_meets_minimum(
            Some("not-a-version"),
            "0.5.0"
        ));
        assert!(!hindsight_version_meets_minimum(None, "0.5.0"));
    }

    #[test]
    fn test_sync_turn_buffers_until_retain_threshold_then_drains() {
        let plugin = HindsightPlugin::new();
        *plugin.config.lock().unwrap() = Some(HindsightConfig {
            api_key: "test".into(),
            api_url: DEFAULT_API_URL.into(),
            bank_id: "hermes".into(),
            bank_id_template: String::new(),
            budget: "mid".into(),
            mode: "cloud".into(),
            memory_mode: "hybrid".into(),
            prefetch_method: "recall".into(),
            auto_retain: true,
            auto_recall: true,
            retain_every_n_turns: 3,
            retain_context: "conversation".into(),
            recall_max_tokens: 4096,
            recall_max_input_chars: 800,
            recall_types: default_recall_types(),
            recall_prompt_preamble: String::new(),
            bank_mission: String::new(),
            retain_async: true,
            retain_tags: Vec::new(),
            observation_scopes: None,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        });

        plugin.sync_turn("u1", "a1", "session-1");
        assert_eq!(plugin.session_turns.lock().unwrap().len(), 1);
        plugin.sync_turn("u2", "a2", "session-1");
        assert_eq!(plugin.session_turns.lock().unwrap().len(), 2);
        plugin.sync_turn("u3", "a3", "session-1");
        assert!(plugin.session_turns.lock().unwrap().is_empty());
    }

    #[test]
    fn test_session_switch_flushes_pending_turns_and_clears_prefetch() {
        let plugin = HindsightPlugin::new();
        *plugin.config.lock().unwrap() = Some(HindsightConfig {
            api_key: "test".into(),
            api_url: DEFAULT_API_URL.into(),
            bank_id: "hermes".into(),
            bank_id_template: String::new(),
            budget: "mid".into(),
            mode: "cloud".into(),
            memory_mode: "hybrid".into(),
            prefetch_method: "recall".into(),
            auto_retain: true,
            auto_recall: true,
            retain_every_n_turns: 10,
            retain_context: "conversation".into(),
            recall_max_tokens: 4096,
            recall_max_input_chars: 800,
            recall_types: default_recall_types(),
            recall_prompt_preamble: String::new(),
            bank_mission: String::new(),
            retain_async: true,
            retain_tags: Vec::new(),
            observation_scopes: None,
            timeout_secs: DEFAULT_TIMEOUT_SECS,
        });
        *plugin.session_id.lock().unwrap() = "old-session".into();
        *plugin.document_id.lock().unwrap() = "old-doc".into();
        *plugin.prefetch_result.lock().unwrap() = "stale context".into();
        plugin
            .session_turns
            .lock()
            .unwrap()
            .push(hindsight_turn_payload("u", "a", "2026-06-08T00:00:00Z"));

        plugin.on_session_switch("new-session", "old-session", false);

        assert!(plugin.session_turns.lock().unwrap().is_empty());
        assert_eq!(*plugin.session_id.lock().unwrap(), "new-session");
        assert!(plugin
            .document_id
            .lock()
            .unwrap()
            .starts_with("new-session-"));
        assert!(plugin.prefetch_result.lock().unwrap().is_empty());
        assert_eq!(*plugin.turn_counter.lock().unwrap(), 0);
    }

    #[test]
    fn test_initialize_scopes_document_id_per_lifecycle() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let _api_key = EnvGuard::remove("HINDSIGHT_API_KEY");
        let _api_url = EnvGuard::remove("HINDSIGHT_API_URL");
        let _bank_id = EnvGuard::remove("HINDSIGHT_BANK_ID");
        let _mode = EnvGuard::remove("HINDSIGHT_MODE");
        let _timeout = EnvGuard::remove("HINDSIGHT_TIMEOUT");

        let tmp = tempfile::tempdir().expect("tempdir");
        write_hindsight_config(tmp.path(), &json!({"mode": "cloud", "api_key": "test"}));

        let plugin = HindsightPlugin::new();
        plugin.initialize("session-1", tmp.path().to_str().expect("tmp path"));
        let first = plugin.document_id.lock().unwrap().clone();
        plugin.initialize("session-1", tmp.path().to_str().expect("tmp path"));
        let second = plugin.document_id.lock().unwrap().clone();

        assert!(first.starts_with("session-1-"));
        assert!(second.starts_with("session-1-"));
        assert_ne!(first, second);
    }
}
