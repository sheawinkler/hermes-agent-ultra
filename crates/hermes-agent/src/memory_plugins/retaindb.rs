//! RetainDB memory provider — cloud long-term memory API.
//!
//! Mirrors Python `plugins/memory/retaindb/__init__.py` (core memory + ingest).

use std::sync::Mutex;
use std::time::Duration;

use reqwest::blocking::{Client, RequestBuilder};
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

#[derive(Clone)]
struct RetainState {
    client: Client,
    base: String,
    token: String,
    project: String,
    user_id: String,
    session_id: String,
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
        !std::env::var("RETAINDB_API_KEY").unwrap_or_default().is_empty()
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
        let client = match Client::builder()
            .timeout(Duration::from_secs(45))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("RetainDB client build failed: {}", e);
                return;
            }
        };
        *self.state.lock().unwrap() = Some(RetainState {
            client,
            base,
            token: api_key,
            project,
            user_id: "default".into(),
            session_id: session_id.to_string(),
        });
        tracing::info!("RetainDB memory plugin initialized");
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
             Use retaindb_search, retaindb_remember, retaindb_profile, retaindb_context.",
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
            "project": st.project,
            "session_id": sid,
            "user_id": st.user_id,
            "messages": [
                {"role": "user", "content": user_content, "timestamp": now},
                {"role": "assistant", "content": assistant_content, "timestamp": now}
            ],
            "write_mode": "sync"
        });
        std::thread::spawn(move || {
            let url = format!("{}/v1/memory/ingest/session", st.base);
            let req = st.client.post(&url).json(&body);
            let req = with_retain_headers(req, &st.token);
            let _ = req.send();
        });
    }

    fn get_tool_schemas(&self) -> Vec<Value> {
        vec![
            profile_schema(),
            search_schema(),
            context_schema(),
            remember_schema(),
            forget_schema(),
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

    Ok(build_overlay(&profile, &query_result))
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
        _ => Err(format!("Unknown tool: {}", tool_name)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_is_retaindb() {
        let p = RetainDbMemoryPlugin::new();
        assert_eq!(p.name(), "retaindb");
    }
}
