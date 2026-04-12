//! OpenViking memory provider — REST context database.
//!
//! Mirrors Python `plugins/memory/openviking/__init__.py` (HTTP subset).

use std::sync::Mutex;
use std::time::Duration;

use reqwest::blocking::Client;
use serde_json::{json, Value};

use crate::memory_manager::MemoryProviderPlugin;

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:1933";

fn search_schema() -> Value {
    json!({
        "name": "viking_search",
        "description": "Semantic search over the OpenViking knowledge base.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type": "string"},
                "mode": {"type": "string", "description": "auto|fast|deep"},
                "scope": {"type": "string"},
                "limit": {"type": "integer"}
            },
            "required": ["query"]
        }
    })
}

fn read_schema() -> Value {
    json!({
        "name": "viking_read",
        "description": "Read content at a viking:// URI (abstract|overview|full).",
        "parameters": {
            "type": "object",
            "properties": {
                "uri": {"type": "string"},
                "level": {"type": "string"}
            },
            "required": ["uri"]
        }
    })
}

fn browse_schema() -> Value {
    json!({
        "name": "viking_browse",
        "description": "Browse OpenViking store (tree|list|stat).",
        "parameters": {
            "type": "object",
            "properties": {
                "action": {"type": "string"},
                "path": {"type": "string"}
            },
            "required": ["action"]
        }
    })
}

fn remember_schema() -> Value {
    json!({
        "name": "viking_remember",
        "description": "Store a fact as a session message for later extraction.",
        "parameters": {
            "type": "object",
            "properties": {
                "content": {"type": "string"},
                "category": {"type": "string"}
            },
            "required": ["content"]
        }
    })
}

fn add_resource_schema() -> Value {
    json!({
        "name": "viking_add_resource",
        "description": "Add a URL or document path to the knowledge base.",
        "parameters": {
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "reason": {"type": "string"}
            },
            "required": ["url"]
        }
    })
}

#[derive(Clone)]
struct VikingState {
    client: Client,
    endpoint: String,
    api_key: String,
    account: String,
    user: String,
    session_id: String,
    turn_count: u32,
}

pub struct OpenVikingMemoryPlugin {
    state: Mutex<Option<VikingState>>,
    prefetch: std::sync::Arc<Mutex<String>>,
}

fn viking_headers(st: &VikingState) -> reqwest::header::HeaderMap {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert(
        "Content-Type",
        "application/json".parse().expect("mime"),
    );
    h.insert(
        "X-OpenViking-Account",
        st.account.parse().expect("account"),
    );
    h.insert("X-OpenViking-User", st.user.parse().expect("user"));
    if !st.api_key.is_empty() {
        h.insert("X-API-Key", st.api_key.parse().expect("key"));
    }
    h
}

impl OpenVikingMemoryPlugin {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(None),
            prefetch: std::sync::Arc::new(Mutex::new(String::new())),
        }
    }
}

impl MemoryProviderPlugin for OpenVikingMemoryPlugin {
    fn name(&self) -> &str {
        "openviking"
    }

    fn is_available(&self) -> bool {
        std::env::var("OPENVIKING_ENDPOINT")
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
    }

    fn initialize(&self, session_id: &str, _hermes_home: &str) {
        let endpoint = std::env::var("OPENVIKING_ENDPOINT")
            .unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string())
            .trim_end_matches('/')
            .to_string();
        let api_key = std::env::var("OPENVIKING_API_KEY").unwrap_or_default();
        let account = std::env::var("OPENVIKING_ACCOUNT").unwrap_or_else(|_| "root".into());
        let user = std::env::var("OPENVIKING_USER").unwrap_or_else(|_| "default".into());
        let client = match Client::builder()
            .timeout(Duration::from_secs(45))
            .build()
        {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("OpenViking client: {}", e);
                return;
            }
        };
        let st = VikingState {
            client,
            endpoint,
            api_key,
            account,
            user,
            session_id: session_id.to_string(),
            turn_count: 0,
        };
        let health_url = format!("{}/health", st.endpoint);
        let h = viking_headers(&st);
        if !st
            .client
            .get(&health_url)
            .headers(h)
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            tracing::warn!(
                "OpenViking health check failed for {} — tools may still work if the server is warming up",
                st.endpoint
            );
        }
        *self.state.lock().unwrap() = Some(st);
        tracing::info!("OpenViking memory plugin initialized");
    }

    fn system_prompt_block(&self) -> String {
        let guard = self.state.lock().unwrap();
        let ep = guard
            .as_ref()
            .map(|s| s.endpoint.as_str())
            .unwrap_or("");
        if ep.is_empty() {
            return String::new();
        }
        format!(
            "# OpenViking Knowledge Base\n\
             Active. Endpoint: {}.\n\
             Use viking_search, viking_read, viking_browse, viking_remember, viking_add_resource.",
            ep
        )
    }

    fn prefetch(&self, _query: &str, _session_id: &str) -> String {
        let mut g = self.prefetch.lock().unwrap();
        let r = g.clone();
        g.clear();
        if r.is_empty() {
            return String::new();
        }
        format!("## OpenViking Context\n{}", r)
    }

    fn queue_prefetch(&self, query: &str, _session_id: &str) {
        let st = match self.state.lock().unwrap().clone() {
            Some(s) => s,
            None => return,
        };
        if query.trim().is_empty() {
            return;
        }
        let q = query.to_string();
        let out = std::sync::Arc::clone(&self.prefetch);
        let plugin = OpenVikingPrefetch { st, q };
        std::thread::spawn(move || {
            if let Ok(s) = plugin.run() {
                if !s.is_empty() {
                    *out.lock().unwrap() = s;
                }
            }
        });
    }

    fn sync_turn(&self, user_content: &str, assistant_content: &str, _session_id: &str) {
        let mut lock = self.state.lock().unwrap();
        let st = match lock.as_mut() {
            Some(s) => s,
            None => return,
        };
        st.turn_count = st.turn_count.saturating_add(1);
        let stc = st.clone();
        let u = user_content.chars().take(4000).collect::<String>();
        let a = assistant_content.chars().take(4000).collect::<String>();
        let h = viking_headers(&stc);
        std::thread::spawn(move || {
            let url = format!(
                "{}/api/v1/sessions/{}/messages",
                stc.endpoint, stc.session_id
            );
            let _ = stc
                .client
                .post(&url)
                .headers(h.clone())
                .json(&json!({"role": "user", "content": u}))
                .send();
            let _ = stc
                .client
                .post(&url)
                .headers(h)
                .json(&json!({"role": "assistant", "content": a}))
                .send();
        });
    }

    fn on_session_end(&self, _messages: &[Value]) {
        let st = match self.state.lock().unwrap().clone() {
            Some(s) => s,
            None => return,
        };
        if st.turn_count == 0 {
            return;
        }
        let h = viking_headers(&st);
        let url = format!(
            "{}/api/v1/sessions/{}/commit",
            st.endpoint, st.session_id
        );
        let _ = st.client.post(&url).headers(h).send();
    }

    fn get_tool_schemas(&self) -> Vec<Value> {
        vec![
            search_schema(),
            read_schema(),
            browse_schema(),
            remember_schema(),
            add_resource_schema(),
        ]
    }

    fn handle_tool_call(&self, tool_name: &str, args: &Value) -> String {
        let st = match self.state.lock().unwrap().clone() {
            Some(s) => s,
            None => return json!({"error": "OpenViking not initialized"}).to_string(),
        };
        let h = viking_headers(&st);
        match tool_name {
            "viking_search" => {
                let query = args.get("query").and_then(|v| v.as_str()).unwrap_or("");
                if query.is_empty() {
                    return json!({"error": "query is required"}).to_string();
                }
                let mut body = json!({"query": query, "top_k": args.get("limit").and_then(|v| v.as_u64()).unwrap_or(10)});
                if let Some(m) = args.get("mode").and_then(|v| v.as_str()) {
                    if m != "auto" {
                        body["mode"] = json!(m);
                    }
                }
                if let Some(s) = args.get("scope").and_then(|v| v.as_str()) {
                    body["target_uri"] = json!(s);
                }
                let url = format!("{}/api/v1/search/find", st.endpoint);
                match st.client.post(&url).headers(h).json(&body).send() {
                    Ok(r) if r.status().is_success() => {
                        r.json::<Value>().unwrap_or(json!({})).to_string()
                    }
                    Ok(r) => json!({"error": format!("HTTP {}", r.status())}).to_string(),
                    Err(e) => json!({"error": e.to_string()}).to_string(),
                }
            }
            "viking_read" => {
                let uri = args.get("uri").and_then(|v| v.as_str()).unwrap_or("");
                if uri.is_empty() {
                    return json!({"error": "uri is required"}).to_string();
                }
                let level = args.get("level").and_then(|v| v.as_str()).unwrap_or("overview");
                let path = match level {
                    "abstract" => "/api/v1/content/abstract",
                    "full" => "/api/v1/content/read",
                    _ => "/api/v1/content/overview",
                };
                let url = format!("{}{}", st.endpoint, path);
                match st
                    .client
                    .get(&url)
                    .headers(h)
                    .query(&[("uri", uri)])
                    .send()
                {
                    Ok(r) if r.status().is_success() => match r.json::<Value>() {
                        Ok(v) => v.to_string(),
                        Err(e) => json!({"error": e.to_string()}).to_string(),
                    },
                    Ok(r) => json!({"error": format!("HTTP {}", r.status())}).to_string(),
                    Err(e) => json!({"error": e.to_string()}).to_string(),
                }
            }
            "viking_browse" => {
                let action = args.get("action").and_then(|v| v.as_str()).unwrap_or("list");
                let path_uri = args
                    .get("path")
                    .and_then(|v| v.as_str())
                    .unwrap_or("viking://");
                let ep = match action {
                    "tree" => "/api/v1/fs/tree",
                    "stat" => "/api/v1/fs/stat",
                    _ => "/api/v1/fs/ls",
                };
                let url = format!("{}{}", st.endpoint, ep);
                match st
                    .client
                    .get(&url)
                    .headers(h)
                    .query(&[("uri", path_uri)])
                    .send()
                {
                    Ok(r) if r.status().is_success() => r.json().unwrap_or(json!({})).to_string(),
                    Ok(r) => json!({"error": format!("HTTP {}", r.status())}).to_string(),
                    Err(e) => json!({"error": e.to_string()}).to_string(),
                }
            }
            "viking_remember" => {
                let content = args.get("content").and_then(|v| v.as_str()).unwrap_or("");
                if content.is_empty() {
                    return json!({"error": "content is required"}).to_string();
                }
                let cat = args.get("category").and_then(|v| v.as_str()).unwrap_or("");
                let text = if cat.is_empty() {
                    format!("[Remember] {}", content)
                } else {
                    format!("[Remember — {}] {}", cat, content)
                };
                let url = format!(
                    "{}/api/v1/sessions/{}/messages",
                    st.endpoint, st.session_id
                );
                let body = json!({"role": "user", "parts": [{"type": "text", "text": text}]});
                match st.client.post(&url).headers(h).json(&body).send() {
                    Ok(r) if r.status().is_success() => {
                        json!({"status": "stored"}).to_string()
                    }
                    Ok(r) => json!({"error": format!("HTTP {}", r.status())}).to_string(),
                    Err(e) => json!({"error": e.to_string()}).to_string(),
                }
            }
            "viking_add_resource" => {
                let url_arg = args.get("url").and_then(|v| v.as_str()).unwrap_or("");
                if url_arg.is_empty() {
                    return json!({"error": "url is required"}).to_string();
                }
                let mut body = json!({"path": url_arg});
                if let Some(r) = args.get("reason").and_then(|v| v.as_str()) {
                    body["reason"] = json!(r);
                }
                let url = format!("{}/api/v1/resources", st.endpoint);
                match st.client.post(&url).headers(h).json(&body).send() {
                    Ok(resp) if resp.status().is_success() => {
                        resp.json().unwrap_or(json!({"status": "added"})).to_string()
                    }
                    Ok(r) => json!({"error": format!("HTTP {}", r.status())}).to_string(),
                    Err(e) => json!({"error": e.to_string()}).to_string(),
                }
            }
            _ => json!({"error": format!("Unknown tool: {}", tool_name)}).to_string(),
        }
    }

    fn shutdown(&self) {
        *self.state.lock().unwrap() = None;
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key": "endpoint", "description": "OpenViking server URL", "env_var": "OPENVIKING_ENDPOINT", "default": DEFAULT_ENDPOINT},
            {"key": "api_key", "description": "API key", "secret": true, "env_var": "OPENVIKING_API_KEY"}
        ]))
    }
}

struct OpenVikingPrefetch {
    st: VikingState,
    q: String,
}

impl OpenVikingPrefetch {
    fn run(self) -> Result<String, ()> {
        let h = {
            let mut hm = reqwest::header::HeaderMap::new();
            hm.insert("Content-Type", "application/json".parse().unwrap());
            hm.insert(
                "X-OpenViking-Account",
                self.st.account.parse().unwrap(),
            );
            hm.insert("X-OpenViking-User", self.st.user.parse().unwrap());
            if !self.st.api_key.is_empty() {
                hm.insert("X-API-Key", self.st.api_key.parse().unwrap());
            }
            hm
        };
        let url = format!("{}/api/v1/search/find", self.st.endpoint);
        let body = json!({"query": self.q, "top_k": 5u64});
        let resp = self
            .st
            .client
            .post(&url)
            .headers(h)
            .json(&body)
            .send()
            .map_err(|_| ())?;
        if !resp.status().is_success() {
            return Err(());
        }
        let v: Value = resp.json().map_err(|_| ())?;
        let result = v.get("result").cloned().unwrap_or(json!({}));
        let mut parts = Vec::new();
        for key in ["memories", "resources"] {
            if let Some(arr) = result.get(key).and_then(|a| a.as_array()) {
                for item in arr.iter().take(3) {
                    let uri = item.get("uri").and_then(|u| u.as_str()).unwrap_or("");
                    let ab = item.get("abstract").and_then(|u| u.as_str()).unwrap_or("");
                    let score = item.get("score").and_then(|s| s.as_f64()).unwrap_or(0.0);
                    if !ab.is_empty() {
                        parts.push(format!("- [{:.2}] {} ({})", score, ab, uri));
                    }
                }
            }
        }
        if parts.is_empty() {
            Err(())
        } else {
            Ok(parts.join("\n"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name() {
        let p = OpenVikingMemoryPlugin::new();
        assert_eq!(p.name(), "openviking");
    }
}
