//! RetainDB memory provider — cloud long-term memory API.
//!
//! Mirrors Python `plugins/memory/retaindb/__init__.py` (core memory + ingest).

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use reqwest::blocking::{
    multipart::{Form, Part},
    Client, RequestBuilder,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

use crate::memory_manager::MemoryProviderPlugin;

const DEFAULT_BASE: &str = "https://api.retaindb.com";

fn profile_schema() -> Value {
    json!({
        "name": "retaindb_profile",
        "description": "Get the user's stable profile from RetainDB.",
        "parameters": {"type": "object", "properties": {}, "required": []}
    })
}

fn search_schema() -> Value {
    json!({
        "name": "retaindb_search",
        "description": "Semantic search across RetainDB memories.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "top_k": {"type": "integer"}
            },
            "required": ["query"]
        }
    })
}

fn context_schema() -> Value {
    json!({
        "name": "retaindb_context",
        "description": "Synthesized context for the current task from RetainDB.",
        "parameters": {
            "type": "object",
            "properties": { "query": {"type": "string"} },
            "required": ["query"]
        }
    })
}

fn remember_schema() -> Value {
    json!({
        "name": "retaindb_remember",
        "description": "Persist a fact to RetainDB long-term memory.",
        "parameters": {
            "type": "object",
            "properties": {
                "content": {"type": "string"},
                "memory_type": {"type": "string"},
                "importance": {"type": "number"}
            },
            "required": ["content"]
        }
    })
}

fn forget_schema() -> Value {
    json!({
        "name": "retaindb_forget",
        "description": "Delete a RetainDB memory by id.",
        "parameters": {
            "type": "object",
            "properties": { "memory_id": {"type": "string"} },
            "required": ["memory_id"]
        }
    })
}

fn upload_file_schema() -> Value {
    json!({
        "name": "retaindb_upload_file",
        "description": "Upload a file to the shared RetainDB file store. Returns an rdb:// URI any agent can reference.",
        "parameters": {
            "type": "object",
            "properties": {
                "local_path": {"type": "string", "description": "Local file path to upload."},
                "remote_path": {"type": "string", "description": "Destination path, e.g. /reports/q1.pdf"},
                "scope": {"type": "string", "enum": ["USER", "PROJECT", "ORG"], "description": "Access scope (default: PROJECT)."},
                "ingest": {"type": "boolean", "description": "Also extract memories from file after upload (default: false)."}
            },
            "required": ["local_path"]
        }
    })
}

fn list_files_schema() -> Value {
    json!({
        "name": "retaindb_list_files",
        "description": "List files in the shared RetainDB file store.",
        "parameters": {
            "type": "object",
            "properties": {
                "prefix": {"type": "string", "description": "Path prefix to filter by, e.g. /reports/"},
                "limit": {"type": "integer", "description": "Max results (default: 50)."}
            },
            "required": []
        }
    })
}

fn read_file_schema() -> Value {
    json!({
        "name": "retaindb_read_file",
        "description": "Read the text content of a stored RetainDB file by file ID.",
        "parameters": {
            "type": "object",
            "properties": { "file_id": {"type": "string", "description": "File ID returned from upload or list."} },
            "required": ["file_id"]
        }
    })
}

fn ingest_file_schema() -> Value {
    json!({
        "name": "retaindb_ingest_file",
        "description": "Chunk, embed, and extract memories from a stored RetainDB file.",
        "parameters": {
            "type": "object",
            "properties": { "file_id": {"type": "string", "description": "File ID to ingest."} },
            "required": ["file_id"]
        }
    })
}

fn delete_file_schema() -> Value {
    json!({
        "name": "retaindb_delete_file",
        "description": "Delete a stored RetainDB file.",
        "parameters": {
            "type": "object",
            "properties": { "file_id": {"type": "string", "description": "File ID to delete."} },
            "required": ["file_id"]
        }
    })
}

#[derive(Clone)]
struct RetainState {
    client: Client,
    base: String,
    token: String,
    project: String,
    user_id: String,
    agent_id: String,
    session_id: String,
    queue_db_path: PathBuf,
}

fn bearer_token(raw: &str) -> String {
    raw.trim()
        .strip_prefix("Bearer ")
        .unwrap_or(raw)
        .to_string()
}

fn with_retain_headers(req: RequestBuilder, token: &str) -> RequestBuilder {
    let t = bearer_token(token);
    req.header("Authorization", format!("Bearer {}", t))
        .header("X-API-Key", t)
        .header("Content-Type", "application/json")
        .header("x-sdk-runtime", "hermes-agent-rust")
}

fn with_retain_auth_headers(req: RequestBuilder, token: &str) -> RequestBuilder {
    let t = bearer_token(token);
    req.header("Authorization", format!("Bearer {}", t))
        .header("x-sdk-runtime", "hermes-agent-rust")
}

pub struct RetainDbMemoryPlugin {
    state: Mutex<Option<RetainState>>,
    prefetch_ctx: std::sync::Arc<Mutex<String>>,
}

impl RetainDbMemoryPlugin {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(None),
            prefetch_ctx: std::sync::Arc::new(Mutex::new(String::new())),
        }
    }
}

impl MemoryProviderPlugin for RetainDbMemoryPlugin {
    fn name(&self) -> &str {
        "retaindb"
    }

    fn is_available(&self) -> bool {
        !std::env::var("RETAINDB_API_KEY")
            .unwrap_or_default()
            .is_empty()
    }

    fn initialize(&self, session_id: &str, hermes_home: &str) {
        let api_key = std::env::var("RETAINDB_API_KEY").unwrap_or_default();
        if api_key.is_empty() {
            return;
        }
        let base = std::env::var("RETAINDB_BASE_URL")
            .unwrap_or_else(|_| DEFAULT_BASE.to_string())
            .trim_end_matches('/')
            .to_string();
        let project = std::env::var("RETAINDB_PROJECT").unwrap_or_else(|_| {
            let profile = std::path::Path::new(hermes_home)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("default");
            if profile.is_empty() || profile == ".hermes" {
                "default".into()
            } else {
                format!("hermes-{}", profile)
            }
        });
        let client = match Client::builder().timeout(Duration::from_secs(45)).build() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("RetainDB client build failed: {}", e);
                return;
            }
        };
        let queue_db_path = Path::new(hermes_home).join("retaindb_queue.db");
        *self.state.lock().unwrap() = Some(RetainState {
            client,
            base,
            token: api_key,
            project,
            user_id: "default".into(),
            agent_id: "hermes".into(),
            session_id: session_id.to_string(),
            queue_db_path,
        });
        tracing::info!("RetainDB memory plugin initialized");
        if let Some(st) = self.state.lock().unwrap().clone() {
            ensure_retaindb_queue(&st.queue_db_path);
            seed_retaindb_soul(&st, Path::new(hermes_home));
            drain_retaindb_queue_async(st);
        }
    }

    fn system_prompt_block(&self) -> String {
        let guard = self.state.lock().unwrap();
        let proj = guard
            .as_ref()
            .map(|s| s.project.as_str())
            .unwrap_or("retaindb");
        format!(
            "# RetainDB Memory\n\
             Active. Project: {}.\n\
             Use retaindb_search, retaindb_remember, retaindb_profile, retaindb_context, and RetainDB file tools.",
            proj
        )
    }

    fn queue_prefetch(&self, query: &str, _session_id: &str) {
        let st = match self.state.lock().unwrap().clone() {
            Some(s) => s,
            None => return,
        };
        let q = query.to_string();
        let out = std::sync::Arc::clone(&self.prefetch_ctx);
        let st2 = st.clone();
        std::thread::spawn(move || {
            if let Ok(text) = retaindb_prefetch_overlay(&st2, &q) {
                if !text.is_empty() {
                    *out.lock().unwrap() = text;
                }
            }
        });
    }

    fn prefetch(&self, _query: &str, _session_id: &str) -> String {
        let mut lock = self.prefetch_ctx.lock().unwrap();
        let r = lock.clone();
        lock.clear();
        r
    }

    fn sync_turn(&self, user_content: &str, assistant_content: &str, session_id: &str) {
        let st = match self.state.lock().unwrap().clone() {
            Some(s) => s,
            None => return,
        };
        if user_content.trim().is_empty() {
            return;
        }
        let now = chrono::Utc::now().to_rfc3339();
        let sid = if session_id.is_empty() {
            st.session_id.clone()
        } else {
            session_id.to_string()
        };
        let body = json!({
            "session_id": sid,
            "user_id": st.user_id,
            "messages": [
                {"role": "user", "content": user_content, "timestamp": now},
                {"role": "assistant", "content": assistant_content, "timestamp": now}
            ]
        });
        if let Some(row_id) = enqueue_retaindb_turn(&st.queue_db_path, &body) {
            std::thread::spawn(move || {
                let _ = flush_retaindb_queue_row(&st, row_id, body);
                drain_retaindb_queue(&st);
            });
        }
    }

    fn get_tool_schemas(&self) -> Vec<Value> {
        vec![
            profile_schema(),
            search_schema(),
            context_schema(),
            remember_schema(),
            forget_schema(),
            upload_file_schema(),
            list_files_schema(),
            read_file_schema(),
            ingest_file_schema(),
            delete_file_schema(),
        ]
    }

    fn handle_tool_call(&self, tool_name: &str, args: &Value) -> String {
        let st = match self.state.lock().unwrap().as_ref() {
            Some(s) => s.clone(),
            None => return json!({"error": "RetainDB not initialized"}).to_string(),
        };
        match tool_dispatch(&st, tool_name, args) {
            Ok(v) => v.to_string(),
            Err(e) => json!({"error": e}).to_string(),
        }
    }

    fn shutdown(&self) {
        *self.state.lock().unwrap() = None;
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key": "api_key", "description": "RetainDB API key", "secret": true, "env_var": "RETAINDB_API_KEY", "url": "https://retaindb.com"},
            {"key": "base_url", "description": "API base URL", "default": DEFAULT_BASE},
            {"key": "project", "description": "Project id", "env_var": "RETAINDB_PROJECT"}
        ]))
    }
}

fn retaindb_prefetch_overlay(st: &RetainState, query: &str) -> Result<String, String> {
    let qbody = json!({
        "project": st.project,
        "query": query,
        "user_id": st.user_id,
        "session_id": st.session_id,
        "include_memories": true,
        "max_tokens": 1200u32,
    });
    let url = format!("{}/v1/context/query", st.base);
    let req = st.client.post(&url).json(&qbody);
    let req = with_retain_headers(req, &st.token);
    let resp = req.send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("context query {}", resp.status()));
    }
    let query_result: Value = resp.json().map_err(|e| e.to_string())?;

    let purl = format!(
        "{}/v1/memory/profile/{}?project={}&include_pending=true",
        st.base,
        urlencoding_query(&st.user_id),
        urlencoding_query(&st.project)
    );
    let preq = st.client.get(&purl);
    let preq = with_retain_headers(preq, &st.token);
    let profile: Value = match preq.send() {
        Ok(r) if r.status().is_success() => r.json().unwrap_or(json!({})),
        _ => json!({}),
    };

    let mut parts = Vec::new();
    let overlay = build_overlay(&profile, &query_result);
    if !overlay.trim().is_empty() {
        parts.push(overlay);
    }
    if let Ok(answer) = retaindb_user_synthesis(st, query) {
        if !answer.trim().is_empty() {
            parts.push(format!("[RetainDB User Synthesis]\n{}", answer.trim()));
        }
    }
    if let Ok(model) = retaindb_agent_self_model(st) {
        if !model.trim().is_empty() {
            parts.push(model);
        }
    }
    Ok(parts.join("\n\n"))
}

fn urlencoding_query(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' | '.' | '~' => c.to_string(),
            _ => format!("%{:02X}", c as u8),
        })
        .collect()
}

fn build_overlay(profile: &Value, query_result: &Value) -> String {
    let mut lines = vec!["[RetainDB Context]".to_string(), "Profile:".to_string()];
    let mems = profile
        .get("memories")
        .and_then(|m| m.as_array())
        .cloned()
        .unwrap_or_default();
    if mems.is_empty() {
        lines.push("- None".into());
    } else {
        for m in mems.iter().take(5) {
            if let Some(c) = m.get("content").and_then(|x| x.as_str()) {
                let short: String = c.chars().take(200).collect();
                lines.push(format!("- {}", short));
            }
        }
    }
    lines.push("Relevant memories:".into());
    let results = query_result
        .get("results")
        .and_then(|r| r.as_array())
        .cloned()
        .unwrap_or_default();
    if results.is_empty() {
        lines.push("- None".into());
    } else {
        for r in results.iter().take(5) {
            if let Some(c) = r.get("content").and_then(|x| x.as_str()) {
                let short: String = c.chars().take(200).collect();
                lines.push(format!("- {}", short));
            }
        }
    }
    lines.join("\n")
}

fn retaindb_user_synthesis(st: &RetainState, query: &str) -> Result<String, String> {
    let body = json!({
        "project": st.project,
        "query": query,
        "reasoning_level": reasoning_level_for_query(query),
    });
    let url = format!(
        "{}/v1/memory/profile/{}/ask",
        st.base,
        urlencoding_query(&st.user_id)
    );
    let req = st.client.post(&url).json(&body);
    let req = with_retain_headers(req, &st.token);
    let resp = req.send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("user synthesis {}", resp.status()));
    }
    let value: Value = resp.json().map_err(|e| e.to_string())?;
    Ok(value
        .get("answer")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_string())
}

fn retaindb_agent_self_model(st: &RetainState) -> Result<String, String> {
    let url = format!(
        "{}/v1/memory/agent/{}/model?project={}",
        st.base,
        urlencoding_query(&st.agent_id),
        urlencoding_query(&st.project)
    );
    let req = st.client.get(&url);
    let req = with_retain_headers(req, &st.token);
    let resp = req.send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("agent self-model {}", resp.status()));
    }
    let value: Value = resp.json().map_err(|e| e.to_string())?;
    Ok(retaindb_agent_self_model_from_value(&value))
}

fn retaindb_agent_self_model_from_value(value: &Value) -> String {
    if value
        .get("memory_count")
        .and_then(Value::as_u64)
        .unwrap_or_default()
        == 0
    {
        return String::new();
    }
    let mut lines = Vec::new();
    if let Some(persona) = value.get("persona").and_then(Value::as_str) {
        if !persona.trim().is_empty() {
            lines.push(format!("Persona: {}", persona.trim()));
        }
    }
    if let Some(instructions) = value
        .get("persistent_instructions")
        .and_then(Value::as_array)
    {
        let rendered = instructions
            .iter()
            .filter_map(Value::as_str)
            .filter(|item| !item.trim().is_empty())
            .map(|item| format!("- {}", item.trim()))
            .collect::<Vec<_>>();
        if !rendered.is_empty() {
            lines.push(format!("Instructions:\n{}", rendered.join("\n")));
        }
    }
    if let Some(style) = value.get("working_style").and_then(Value::as_str) {
        if !style.trim().is_empty() {
            lines.push(format!("Working style: {}", style.trim()));
        }
    }
    if lines.is_empty() {
        String::new()
    } else {
        format!("[RetainDB Agent Self-Model]\n{}", lines.join("\n"))
    }
}

fn reasoning_level_for_query(query: &str) -> &'static str {
    match query.chars().count() {
        0..=119 => "low",
        120..=399 => "medium",
        _ => "high",
    }
}

fn ensure_retaindb_queue(path: &Path) {
    if let Some(parent) = path.parent() {
        if let Err(err) = std::fs::create_dir_all(parent) {
            tracing::warn!("RetainDB queue directory setup failed: {}", err);
            return;
        }
    }
    match Connection::open(path) {
        Ok(conn) => {
            if let Err(err) = conn.execute(
                "CREATE TABLE IF NOT EXISTS pending (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    payload_json TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    last_error TEXT
                )",
                [],
            ) {
                tracing::warn!("RetainDB queue schema setup failed: {}", err);
            }
        }
        Err(err) => tracing::warn!("RetainDB queue open failed: {}", err),
    }
}

fn enqueue_retaindb_turn(path: &Path, payload: &Value) -> Option<i64> {
    ensure_retaindb_queue(path);
    let conn = match Connection::open(path) {
        Ok(conn) => conn,
        Err(err) => {
            tracing::warn!("RetainDB queue open failed: {}", err);
            return None;
        }
    };
    let now = chrono::Utc::now().to_rfc3339();
    let payload_json = payload.to_string();
    match conn.execute(
        "INSERT INTO pending (payload_json, created_at) VALUES (?1, ?2)",
        params![payload_json, now],
    ) {
        Ok(_) => Some(conn.last_insert_rowid()),
        Err(err) => {
            tracing::warn!("RetainDB queue insert failed: {}", err);
            None
        }
    }
}

fn queued_payload_for_ingest(st: &RetainState, payload: &Value) -> Value {
    json!({
        "project": st.project,
        "session_id": payload.get("session_id").cloned().unwrap_or_else(|| json!(st.session_id)),
        "user_id": payload.get("user_id").cloned().unwrap_or_else(|| json!(st.user_id)),
        "messages": payload.get("messages").cloned().unwrap_or_else(|| json!([])),
        "write_mode": "sync",
    })
}

fn flush_retaindb_queue_row(st: &RetainState, row_id: i64, payload: Value) -> Result<(), String> {
    let body = queued_payload_for_ingest(st, &payload);
    let url = format!("{}/v1/memory/ingest/session", st.base);
    let req = st.client.post(&url).json(&body);
    let req = with_retain_headers(req, &st.token);
    let resp = req.send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        let err = format!("ingest {}", resp.status());
        mark_retaindb_queue_error(&st.queue_db_path, row_id, &err);
        return Err(err);
    }
    delete_retaindb_queue_row(&st.queue_db_path, row_id);
    Ok(())
}

fn drain_retaindb_queue_async(st: RetainState) {
    std::thread::spawn(move || drain_retaindb_queue(&st));
}

fn drain_retaindb_queue(st: &RetainState) {
    ensure_retaindb_queue(&st.queue_db_path);
    let rows = match pending_retaindb_rows(&st.queue_db_path) {
        Ok(rows) => rows,
        Err(err) => {
            tracing::warn!("RetainDB queue read failed: {}", err);
            return;
        }
    };
    for (row_id, payload) in rows {
        if let Err(err) = flush_retaindb_queue_row(st, row_id, payload) {
            tracing::debug!("RetainDB queued ingest still pending: {}", err);
            break;
        }
    }
}

fn pending_retaindb_rows(path: &Path) -> Result<Vec<(i64, Value)>, String> {
    let conn = Connection::open(path).map_err(|e| e.to_string())?;
    let mut stmt = conn
        .prepare("SELECT id, payload_json FROM pending ORDER BY id ASC LIMIT 200")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let payload_json: String = row.get(1)?;
            let payload = serde_json::from_str(&payload_json).unwrap_or_else(|_| json!({}));
            Ok((id, payload))
        })
        .map_err(|e| e.to_string())?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(|e| e.to_string())?);
    }
    Ok(out)
}

fn delete_retaindb_queue_row(path: &Path, row_id: i64) {
    if let Ok(conn) = Connection::open(path) {
        let _ = conn.execute("DELETE FROM pending WHERE id = ?1", params![row_id]);
    }
}

fn mark_retaindb_queue_error(path: &Path, row_id: i64, error: &str) {
    if let Ok(conn) = Connection::open(path) {
        let _ = conn.execute(
            "UPDATE pending SET last_error = ?1 WHERE id = ?2",
            params![error, row_id],
        );
    }
}

fn seed_retaindb_soul(st: &RetainState, hermes_home: &Path) {
    let soul_path = hermes_home.join("SOUL.md");
    let Ok(content) = std::fs::read_to_string(soul_path) else {
        return;
    };
    let content = content.trim().to_string();
    if content.is_empty() {
        return;
    }
    let st = st.clone();
    std::thread::spawn(move || {
        let body = json!({
            "project": st.project,
            "content": content,
            "source": "soul_md",
        });
        let url = format!(
            "{}/v1/memory/agent/{}/seed",
            st.base,
            urlencoding_query(&st.agent_id)
        );
        let req = st.client.post(&url).json(&body);
        let req = with_retain_headers(req, &st.token);
        let _ = req.send();
    });
}

fn retain_file_mime_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "txt" | "text" => "text/plain",
        "md" | "markdown" => "text/markdown",
        "json" => "application/json",
        "csv" => "text/csv",
        "yaml" | "yml" => "application/yaml",
        "xml" => "application/xml",
        "html" | "htm" => "text/html",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

fn retain_file_is_text(file_info: &Value) -> bool {
    let mime = file_info
        .get("mime_type")
        .or_else(|| file_info.get("mimeType"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    if mime.starts_with("text/")
        || matches!(
            mime.as_str(),
            "application/json" | "application/xml" | "application/yaml" | "application/x-yaml"
        )
    {
        return true;
    }
    let name = file_info
        .get("name")
        .or_else(|| file_info.get("filename"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    [
        ".txt", ".md", ".json", ".csv", ".yaml", ".yml", ".xml", ".html",
    ]
    .iter()
    .any(|suffix| name.ends_with(suffix))
}

fn valid_retaindb_file_scope(scope: &str) -> bool {
    matches!(scope, "USER" | "PROJECT" | "ORG")
}

fn extract_retaindb_file_id(upload_result: &Value) -> Option<String> {
    upload_result
        .pointer("/file/id")
        .or_else(|| upload_result.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(ToString::to_string)
}

fn retaindb_ingest_file(st: &RetainState, file_id: &str) -> Result<Value, String> {
    let body = json!({
        "user_id": st.user_id,
        "agent_id": st.agent_id,
    });
    let url = format!("{}/v1/files/{}/ingest", st.base, urlencoding_query(file_id));
    let req = st.client.post(&url).json(&body);
    let req = with_retain_headers(req, &st.token);
    let resp = req.send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("ingest file {}", resp.status()));
    }
    Ok(resp.json().unwrap_or_else(|_| json!({"ok": true})))
}

fn tool_dispatch(st: &RetainState, tool_name: &str, args: &Value) -> Result<Value, String> {
    match tool_name {
        "retaindb_profile" => {
            let url = format!(
                "{}/v1/memory/profile/{}?project={}&include_pending=true",
                st.base,
                urlencoding_query(&st.user_id),
                urlencoding_query(&st.project)
            );
            let req = st.client.get(&url);
            let req = with_retain_headers(req, &st.token);
            let resp = req.send().map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("profile {}", resp.status()));
            }
            let v: Value = resp.json().map_err(|e| e.to_string())?;
            Ok(v)
        }
        "retaindb_search" => {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            if query.is_empty() {
                return Err("query is required".into());
            }
            let top_k = args
                .get("top_k")
                .and_then(|v| v.as_u64())
                .unwrap_or(8)
                .min(20) as u32;
            let body = json!({
                "project": st.project,
                "query": query,
                "user_id": st.user_id,
                "session_id": st.session_id,
                "top_k": top_k,
                "include_pending": true,
            });
            let url = format!("{}/v1/memory/search", st.base);
            let req = st.client.post(&url).json(&body);
            let req = with_retain_headers(req, &st.token);
            let resp = req.send().map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("search {}", resp.status()));
            }
            Ok(resp.json().map_err(|e| e.to_string())?)
        }
        "retaindb_context" => {
            let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
            if query.is_empty() {
                return Err("query is required".into());
            }
            let qbody = json!({
                "project": st.project,
                "query": query,
                "user_id": st.user_id,
                "session_id": st.session_id,
                "include_memories": true,
                "max_tokens": 1200u32,
            });
            let url = format!("{}/v1/context/query", st.base);
            let req = st.client.post(&url).json(&qbody);
            let req = with_retain_headers(req, &st.token);
            let resp = req.send().map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("context {}", resp.status()));
            }
            let raw: Value = resp.json().map_err(|e| e.to_string())?;
            let profile = tool_dispatch(st, "retaindb_profile", &json!({}))?;
            let overlay = build_overlay(&profile, &raw);
            Ok(json!({"context": overlay, "raw": raw}))
        }
        "retaindb_remember" => {
            let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
            if content.is_empty() {
                return Err("content is required".into());
            }
            let memory_type = args
                .get("memory_type")
                .and_then(|v| v.as_str())
                .unwrap_or("factual");
            let importance = args
                .get("importance")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.7);
            let body = json!({
                "project": st.project,
                "content": content,
                "memory_type": memory_type,
                "user_id": st.user_id,
                "session_id": st.session_id,
                "importance": importance,
                "write_mode": "sync",
            });
            let url = format!("{}/v1/memory", st.base);
            let req = st.client.post(&url).json(&body);
            let req = with_retain_headers(req, &st.token);
            let resp = req.send().map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("remember {}", resp.status()));
            }
            Ok(resp.json().map_err(|e| e.to_string())?)
        }
        "retaindb_forget" => {
            let memory_id = args.get("memory_id").and_then(|v| v.as_str()).unwrap_or("");
            if memory_id.is_empty() {
                return Err("memory_id is required".into());
            }
            let mid = urlencoding_query(memory_id);
            let url = format!("{}/v1/memory/{}", st.base, mid);
            let req = st.client.delete(&url);
            let req = with_retain_headers(req, &st.token);
            let resp = req.send().map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("forget {}", resp.status()));
            }
            Ok(resp.json().unwrap_or(json!({"ok": true})))
        }
        "retaindb_upload_file" => {
            let local_path = args
                .get("local_path")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if local_path.is_empty() {
                return Err("local_path is required".into());
            }
            let path = Path::new(local_path);
            if !path.exists() {
                return Err(format!("File not found: {local_path}"));
            }
            let metadata = std::fs::metadata(path).map_err(|e| e.to_string())?;
            if !metadata.is_file() {
                return Err(format!("Not a regular file: {local_path}"));
            }
            let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
            let filename = path
                .file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !name.trim().is_empty())
                .unwrap_or("upload")
                .to_string();
            let remote_path = args
                .get("remote_path")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToString::to_string)
                .unwrap_or_else(|| format!("/{filename}"));
            let scope = args
                .get("scope")
                .and_then(Value::as_str)
                .unwrap_or("PROJECT")
                .trim()
                .to_ascii_uppercase();
            if !valid_retaindb_file_scope(&scope) {
                return Err("scope must be USER, PROJECT, or ORG".into());
            }
            let mime = retain_file_mime_type(path);
            let part = Part::bytes(bytes)
                .file_name(filename)
                .mime_str(mime)
                .map_err(|e| e.to_string())?;
            let form = Form::new()
                .part("file", part)
                .text("path", remote_path)
                .text("scope", scope);
            let url = format!("{}/v1/files", st.base);
            let req = st.client.post(&url).multipart(form);
            let req = with_retain_auth_headers(req, &st.token);
            let resp = req.send().map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("upload file {}", resp.status()));
            }
            let mut result: Value = resp.json().map_err(|e| e.to_string())?;
            if args.get("ingest").and_then(Value::as_bool).unwrap_or(false) {
                if let Some(file_id) = extract_retaindb_file_id(&result) {
                    let ingest = retaindb_ingest_file(st, &file_id)?;
                    if let Value::Object(ref mut map) = result {
                        map.insert("ingest".to_string(), ingest);
                    }
                }
            }
            Ok(result)
        }
        "retaindb_list_files" => {
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(50)
                .clamp(1, 200)
                .to_string();
            let mut query = vec![("limit", limit.as_str())];
            if let Some(prefix) = args
                .get("prefix")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                query.push(("prefix", prefix));
            }
            let url = format!("{}/v1/files", st.base);
            let req = st.client.get(&url).query(&query);
            let req = with_retain_headers(req, &st.token);
            let resp = req.send().map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("list files {}", resp.status()));
            }
            Ok(resp.json().map_err(|e| e.to_string())?)
        }
        "retaindb_read_file" => {
            let file_id = args.get("file_id").and_then(Value::as_str).unwrap_or("");
            if file_id.is_empty() {
                return Err("file_id is required".into());
            }
            let fid = urlencoding_query(file_id);
            let meta_url = format!("{}/v1/files/{}", st.base, fid);
            let meta_req = with_retain_headers(st.client.get(&meta_url), &st.token);
            let meta_resp = meta_req.send().map_err(|e| e.to_string())?;
            if !meta_resp.status().is_success() {
                return Err(format!("read file metadata {}", meta_resp.status()));
            }
            let meta: Value = meta_resp.json().map_err(|e| e.to_string())?;
            let file_info = meta.get("file").unwrap_or(&meta);
            let content_url = format!("{}/v1/files/{}/content", st.base, fid);
            let content_req = with_retain_auth_headers(st.client.get(&content_url), &st.token);
            let content_resp = content_req.send().map_err(|e| e.to_string())?;
            if !content_resp.status().is_success() {
                return Err(format!("read file content {}", content_resp.status()));
            }
            let bytes = content_resp.bytes().map_err(|e| e.to_string())?;
            let name = file_info
                .get("name")
                .or_else(|| file_info.get("filename"))
                .cloned()
                .unwrap_or(Value::Null);
            let rdb_uri = file_info.get("rdb_uri").cloned().unwrap_or(Value::Null);
            if !retain_file_is_text(file_info) {
                return Ok(json!({
                    "file_id": file_id,
                    "rdb_uri": rdb_uri,
                    "name": name,
                    "content": Value::Null,
                    "note": "Binary file - use retaindb_ingest_file to extract text into memory."
                }));
            }
            let text = String::from_utf8_lossy(&bytes);
            let truncated = text.chars().count() > 32_000;
            let content = text.chars().take(32_000).collect::<String>();
            Ok(json!({
                "file_id": file_id,
                "rdb_uri": rdb_uri,
                "name": name,
                "content": content,
                "truncated": truncated
            }))
        }
        "retaindb_ingest_file" => {
            let file_id = args.get("file_id").and_then(Value::as_str).unwrap_or("");
            if file_id.is_empty() {
                return Err("file_id is required".into());
            }
            retaindb_ingest_file(st, file_id)
        }
        "retaindb_delete_file" => {
            let file_id = args.get("file_id").and_then(Value::as_str).unwrap_or("");
            if file_id.is_empty() {
                return Err("file_id is required".into());
            }
            let url = format!("{}/v1/files/{}", st.base, urlencoding_query(file_id));
            let req = st.client.delete(&url);
            let req = with_retain_headers(req, &st.token);
            let resp = req.send().map_err(|e| e.to_string())?;
            if !resp.status().is_success() {
                return Err(format!("delete file {}", resp.status()));
            }
            Ok(resp.json().unwrap_or_else(|_| json!({"ok": true})))
        }
        _ => Err(format!("Unknown tool: {}", tool_name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::time::Duration as StdDuration;

    fn read_http_request(stream: &mut std::net::TcpStream) -> String {
        stream
            .set_read_timeout(Some(StdDuration::from_secs(2)))
            .expect("timeout");
        let mut buf = Vec::new();
        let mut chunk = [0u8; 4096];
        let mut header_end = None;
        let mut content_len = 0usize;
        loop {
            let n = stream.read(&mut chunk).expect("read request");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if header_end.is_none() {
                if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                    header_end = Some(pos + 4);
                    let headers = String::from_utf8_lossy(&buf[..pos]);
                    for line in headers.lines() {
                        if let Some(value) = line.strip_prefix("Content-Length:") {
                            content_len = value.trim().parse().unwrap_or(0);
                        } else if let Some(value) = line.strip_prefix("content-length:") {
                            content_len = value.trim().parse().unwrap_or(0);
                        }
                    }
                }
            }
            if let Some(end) = header_end {
                if buf.len() >= end + content_len {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&buf).to_string()
    }

    fn retain_server(
        responses: Vec<(u16, &'static str, &'static str)>,
    ) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            for (status, content_type, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept");
                let request = read_http_request(&mut stream);
                tx.send(request).expect("send request");
                let reason = if (200..300).contains(&status) {
                    "OK"
                } else {
                    "Error"
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    body.len(),
                    body
                );
                stream
                    .write_all(response.as_bytes())
                    .expect("write response");
            }
        });
        (format!("http://{addr}"), rx)
    }

    fn state_for(base: String, queue_db_path: PathBuf) -> RetainState {
        RetainState {
            client: Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .expect("client"),
            base,
            token: "test-token".to_string(),
            project: "proj".to_string(),
            user_id: "user-1".to_string(),
            agent_id: "agent-1".to_string(),
            session_id: "session-1".to_string(),
            queue_db_path,
        }
    }

    #[test]
    fn name_is_retaindb() {
        let p = RetainDbMemoryPlugin::new();
        assert_eq!(p.name(), "retaindb");
    }

    #[test]
    fn retaindb_tool_schemas_include_memory_and_shared_file_store() {
        let p = RetainDbMemoryPlugin::new();
        let schemas = p.get_tool_schemas();
        let names = schemas
            .iter()
            .filter_map(|schema| schema.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            vec![
                "retaindb_profile",
                "retaindb_search",
                "retaindb_context",
                "retaindb_remember",
                "retaindb_forget",
                "retaindb_upload_file",
                "retaindb_list_files",
                "retaindb_read_file",
                "retaindb_ingest_file",
                "retaindb_delete_file",
            ]
        );
    }

    #[test]
    fn retaindb_file_tool_validation_is_local_and_precise() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let st = state_for(
            "http://127.0.0.1:9".to_string(),
            tmp.path().join("retaindb_queue.db"),
        );
        assert_eq!(
            tool_dispatch(&st, "retaindb_upload_file", &json!({})).unwrap_err(),
            "local_path is required"
        );
        assert!(tool_dispatch(
            &st,
            "retaindb_upload_file",
            &json!({"local_path": tmp.path().join("missing.md").display().to_string()})
        )
        .unwrap_err()
        .contains("File not found"));
        assert_eq!(
            tool_dispatch(&st, "retaindb_ingest_file", &json!({})).unwrap_err(),
            "file_id is required"
        );
        assert_eq!(
            tool_dispatch(&st, "retaindb_delete_file", &json!({})).unwrap_err(),
            "file_id is required"
        );
    }

    #[test]
    fn retaindb_upload_file_posts_multipart_and_optional_ingest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let file = tmp.path().join("note.md");
        std::fs::write(&file, "hello retain").expect("write file");
        let (base, rx) = retain_server(vec![
            (
                200,
                "application/json",
                r#"{"file":{"id":"file-1","rdb_uri":"rdb://file-1","name":"note.md"}}"#,
            ),
            (200, "application/json", r#"{"ingested":true}"#),
        ]);
        let st = state_for(base, tmp.path().join("retaindb_queue.db"));

        let result = tool_dispatch(
            &st,
            "retaindb_upload_file",
            &json!({
                "local_path": file.display().to_string(),
                "remote_path": "/reports/note.md",
                "scope": "project",
                "ingest": true,
            }),
        )
        .expect("upload dispatch");

        assert_eq!(result["file"]["id"], "file-1");
        assert_eq!(result["ingest"]["ingested"], true);
        let upload = rx
            .recv_timeout(StdDuration::from_secs(2))
            .expect("upload request");
        let upload_lower = upload.to_ascii_lowercase();
        assert!(upload.starts_with("POST /v1/files HTTP/1.1"));
        assert!(upload_lower.contains("authorization: bearer test-token"));
        assert!(upload.contains("name=\"path\""));
        assert!(upload.contains("/reports/note.md"));
        assert!(upload.contains("name=\"scope\""));
        assert!(upload.contains("PROJECT"));
        assert!(upload.contains("hello retain"));

        let ingest = rx
            .recv_timeout(StdDuration::from_secs(2))
            .expect("ingest request");
        assert!(ingest.starts_with("POST /v1/files/file-1/ingest HTTP/1.1"));
        assert!(ingest.contains("\"user_id\":\"user-1\""));
        assert!(ingest.contains("\"agent_id\":\"agent-1\""));
    }

    #[test]
    fn retaindb_list_read_and_delete_files_use_expected_routes() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (base, rx) = retain_server(vec![
            (200, "application/json", r#"{"files":[]}"#),
            (
                200,
                "application/json",
                r#"{"file":{"id":"file-1","rdb_uri":"rdb://file-1","name":"note.md","mime_type":"text/markdown"}}"#,
            ),
            (200, "text/plain", "body text"),
            (200, "application/json", r#"{"deleted":true}"#),
        ]);
        let st = state_for(base, tmp.path().join("retaindb_queue.db"));

        let listed = tool_dispatch(
            &st,
            "retaindb_list_files",
            &json!({"prefix": "/reports", "limit": 999}),
        )
        .expect("list");
        assert_eq!(listed["files"], json!([]));
        let list_req = rx
            .recv_timeout(StdDuration::from_secs(2))
            .expect("list req");
        assert!(list_req.starts_with("GET /v1/files?limit=200&prefix=%2Freports HTTP/1.1"));

        let read =
            tool_dispatch(&st, "retaindb_read_file", &json!({"file_id": "file-1"})).expect("read");
        assert_eq!(read["content"], "body text");
        assert_eq!(read["truncated"], false);
        let meta_req = rx
            .recv_timeout(StdDuration::from_secs(2))
            .expect("meta req");
        assert!(meta_req.starts_with("GET /v1/files/file-1 HTTP/1.1"));
        let content_req = rx
            .recv_timeout(StdDuration::from_secs(2))
            .expect("content req");
        assert!(content_req.starts_with("GET /v1/files/file-1/content HTTP/1.1"));

        let deleted = tool_dispatch(&st, "retaindb_delete_file", &json!({"file_id": "file-1"}))
            .expect("delete");
        assert_eq!(deleted["deleted"], true);
        let delete_req = rx
            .recv_timeout(StdDuration::from_secs(2))
            .expect("delete req");
        assert!(delete_req.starts_with("DELETE /v1/files/file-1 HTTP/1.1"));
    }

    #[test]
    fn retaindb_durable_queue_flush_deletes_successful_rows() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let (base, rx) = retain_server(vec![(200, "application/json", r#"{"ok":true}"#)]);
        let st = state_for(base, tmp.path().join("retaindb_queue.db"));
        let payload = json!({
            "session_id": "session-2",
            "user_id": "user-1",
            "messages": [{"role":"user","content":"hi"}],
        });
        let row_id = enqueue_retaindb_turn(&st.queue_db_path, &payload).expect("row");

        flush_retaindb_queue_row(&st, row_id, payload).expect("flush");

        let req = rx.recv_timeout(StdDuration::from_secs(2)).expect("request");
        assert!(req.starts_with("POST /v1/memory/ingest/session HTTP/1.1"));
        assert!(req.contains("\"project\":\"proj\""));
        assert!(req.contains("\"session_id\":\"session-2\""));
        assert!(pending_retaindb_rows(&st.queue_db_path)
            .expect("pending rows")
            .is_empty());
    }

    #[test]
    fn retaindb_prefetch_helpers_render_synthesis_and_self_model() {
        assert_eq!(reasoning_level_for_query("short"), "low");
        assert_eq!(reasoning_level_for_query(&"x".repeat(200)), "medium");
        assert_eq!(reasoning_level_for_query(&"x".repeat(500)), "high");

        let rendered = retaindb_agent_self_model_from_value(&json!({
            "memory_count": 3,
            "persona": "direct",
            "persistent_instructions": ["verify evidence"],
            "working_style": "surgical"
        }));
        assert!(rendered.contains("[RetainDB Agent Self-Model]"));
        assert!(rendered.contains("Persona: direct"));
        assert!(rendered.contains("- verify evidence"));
    }
}
