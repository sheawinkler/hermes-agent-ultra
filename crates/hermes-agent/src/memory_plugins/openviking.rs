//! OpenViking memory provider — REST context database.
//!
//! Mirrors Python `plugins/memory/openviking/__init__.py` (HTTP subset).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use reqwest::blocking::multipart::{Form, Part};
use reqwest::blocking::Client;
use serde_json::{json, Value};

use crate::memory_manager::MemoryProviderPlugin;

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:1933";
const DEFAULT_AGENT: &str = "hermes";
const DEFAULT_MEMORY_SUBDIR: &str = "preferences";
const REMOTE_RESOURCE_PREFIXES: &[&str] = &["http://", "https://", "git@", "ssh://", "git://"];

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
        "description": "Store a fact directly in the OpenViking memory tree.",
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
        "description": "Add a remote URL or local file/directory to the knowledge base.",
        "parameters": {
            "type": "object",
            "properties": {
                "url": {"type": "string"},
                "reason": {"type": "string"},
                "to": {"type": "string"},
                "parent": {"type": "string"},
                "instruction": {"type": "string"},
                "wait": {"type": "boolean"},
                "timeout": {"type": "number"}
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
    agent: String,
    session_id: String,
    turn_count: u32,
}

pub struct OpenVikingMemoryPlugin {
    state: Mutex<Option<VikingState>>,
    prefetch: std::sync::Arc<Mutex<String>>,
}

fn viking_headers(st: &VikingState) -> reqwest::header::HeaderMap {
    let mut h = reqwest::header::HeaderMap::new();
    h.insert("Content-Type", "application/json".parse().expect("mime"));
    append_viking_tenant_headers(&mut h, st);
    h
}

fn viking_multipart_headers(st: &VikingState) -> reqwest::header::HeaderMap {
    let mut h = reqwest::header::HeaderMap::new();
    append_viking_tenant_headers(&mut h, st);
    h
}

fn append_viking_tenant_headers(h: &mut reqwest::header::HeaderMap, st: &VikingState) {
    h.insert("X-OpenViking-Account", st.account.parse().expect("account"));
    h.insert("X-OpenViking-User", st.user.parse().expect("user"));
    h.insert("X-OpenViking-Agent", st.agent.parse().expect("agent"));
    if !st.api_key.is_empty() {
        h.insert("X-API-Key", st.api_key.parse().expect("key"));
        h.insert(
            "Authorization",
            format!("Bearer {}", st.api_key).parse().expect("bearer"),
        );
    }
}

fn viking_uri_segment(raw: &str) -> String {
    let sanitized = raw
        .trim()
        .chars()
        .map(|ch| match ch {
            'A'..='Z' | 'a'..='z' | '0'..='9' | '.' | '_' | '-' => ch,
            _ => '_',
        })
        .collect::<String>();
    if sanitized.is_empty() {
        "default".to_string()
    } else {
        sanitized
    }
}

fn memory_subdir_for_category(category: &str) -> &'static str {
    match category.trim().to_ascii_lowercase().as_str() {
        "preference" | "preferences" => "preferences",
        "entity" | "entities" => "entities",
        "event" | "events" => "events",
        "case" | "cases" => "cases",
        "pattern" | "patterns" => "patterns",
        _ => DEFAULT_MEMORY_SUBDIR,
    }
}

fn memory_subdir_for_target(target: &str) -> &'static str {
    match target.trim().to_ascii_lowercase().as_str() {
        "memory" | "memories" => "patterns",
        "user" | "preferences" => "preferences",
        _ => DEFAULT_MEMORY_SUBDIR,
    }
}

fn build_memory_uri(user: &str, agent: &str, subdir: &str) -> String {
    let slug = uuid::Uuid::new_v4().simple().to_string();
    format!(
        "viking://user/{}/agent/{}/memories/{}/mem_{}.md",
        viking_uri_segment(user),
        viking_uri_segment(agent),
        viking_uri_segment(subdir),
        &slug[..12]
    )
}

fn content_write_body(st: &VikingState, subdir: &str, content: &str) -> Value {
    json!({
        "uri": build_memory_uri(&st.user, &st.agent, subdir),
        "content": content,
        "mode": "create",
    })
}

fn is_remote_resource_source(value: &str) -> bool {
    REMOTE_RESOURCE_PREFIXES
        .iter()
        .any(|prefix| value.starts_with(prefix))
}

fn is_windows_absolute_path(value: &str) -> bool {
    let bytes = value.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

fn is_local_path_reference(value: &str) -> bool {
    if value.is_empty() || value.contains('\n') || value.contains('\r') {
        return false;
    }
    if is_remote_resource_source(value) {
        return false;
    }
    is_windows_absolute_path(value)
        || value.starts_with('/')
        || value.starts_with("./")
        || value.starts_with("../")
        || value.starts_with("~/")
        || value.starts_with(".\\")
        || value.starts_with("..\\")
        || value.starts_with("~\\")
        || value.contains('/')
        || value.contains('\\')
}

fn file_uri_to_path(uri: &str) -> Result<PathBuf, String> {
    let Some(rest) = uri.strip_prefix("file://") else {
        return Err(format!("Unsupported file URI: {uri}"));
    };
    let path = if let Some(path) = rest.strip_prefix("localhost/") {
        format!("/{path}")
    } else if rest.starts_with('/') {
        rest.to_string()
    } else {
        return Err(format!("Unsupported non-local file URI: {uri}"));
    };
    percent_decode_path(&path).map(PathBuf::from)
}

fn percent_decode_path(raw: &str) -> Result<String, String> {
    let mut out = Vec::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut idx = 0;
    while idx < bytes.len() {
        if bytes[idx] == b'%' {
            if idx + 2 >= bytes.len() {
                return Err(format!("Invalid percent escape in path: {raw}"));
            }
            let hex = std::str::from_utf8(&bytes[idx + 1..idx + 3])
                .map_err(|_| format!("Invalid percent escape in path: {raw}"))?;
            let value = u8::from_str_radix(hex, 16)
                .map_err(|_| format!("Invalid percent escape in path: {raw}"))?;
            out.push(value);
            idx += 3;
        } else {
            out.push(bytes[idx]);
            idx += 1;
        }
    }
    String::from_utf8(out).map_err(|_| format!("Invalid UTF-8 in path URI: {raw}"))
}

fn zip_directory(dir_path: &Path) -> Result<PathBuf, String> {
    let zip_path = std::env::temp_dir().join(format!(
        "openviking_upload_{}.zip",
        uuid::Uuid::new_v4().simple()
    ));
    let file = std::fs::File::create(&zip_path)
        .map_err(|e| format!("create {}: {e}", zip_path.display()))?;
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default()
        .compression_method(zip::CompressionMethod::Deflated);
    add_directory_to_zip(dir_path, dir_path, &mut zip, options)?;
    zip.finish()
        .map_err(|e| format!("finish {}: {e}", zip_path.display()))?;
    Ok(zip_path)
}

fn add_directory_to_zip(
    root: &Path,
    current: &Path,
    zip: &mut zip::ZipWriter<std::fs::File>,
    options: zip::write::SimpleFileOptions,
) -> Result<(), String> {
    for entry in
        std::fs::read_dir(current).map_err(|e| format!("read_dir {}: {e}", current.display()))?
    {
        let entry = entry.map_err(|e| format!("read_dir entry {}: {e}", current.display()))?;
        let path = entry.path();
        let file_type = entry
            .file_type()
            .map_err(|e| format!("file_type {}: {e}", path.display()))?;
        if file_type.is_symlink() {
            continue;
        }
        let metadata = entry
            .metadata()
            .map_err(|e| format!("metadata {}: {e}", path.display()))?;
        if metadata.is_dir() {
            add_directory_to_zip(root, &path, zip, options)?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }
        let rel = path
            .strip_prefix(root)
            .map_err(|e| format!("strip_prefix {}: {e}", path.display()))?
            .to_string_lossy()
            .replace('\\', "/");
        zip.start_file(rel, options)
            .map_err(|e| format!("zip start_file {}: {e}", path.display()))?;
        let mut file =
            std::fs::File::open(&path).map_err(|e| format!("open {}: {e}", path.display()))?;
        let mut buffer = Vec::new();
        file.read_to_end(&mut buffer)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        zip.write_all(&buffer)
            .map_err(|e| format!("zip write {}: {e}", path.display()))?;
    }
    Ok(())
}

fn upload_temp_file(st: &VikingState, file_path: &Path) -> Result<String, String> {
    let file_name = file_path
        .file_name()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .unwrap_or("upload.bin")
        .to_string();
    let bytes =
        std::fs::read(file_path).map_err(|e| format!("read {}: {e}", file_path.display()))?;
    let part = Part::bytes(bytes).file_name(file_name);
    let form = Form::new().part("file", part);
    let url = format!("{}/api/v1/resources/temp_upload", st.endpoint);
    let resp = st
        .client
        .post(&url)
        .headers(viking_multipart_headers(st))
        .multipart(form)
        .send()
        .map_err(|e| format!("OpenViking temp_upload failed: {e}"))?;
    let status = resp.status();
    let text = resp.text().unwrap_or_default();
    if !status.is_success() {
        return Err(format!("OpenViking temp_upload HTTP {status}: {text}"));
    }
    let value: Value =
        serde_json::from_str(&text).map_err(|e| format!("OpenViking temp_upload JSON: {e}"))?;
    value
        .get("result")
        .and_then(|result| result.get("temp_file_id"))
        .or_else(|| value.get("temp_file_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToString::to_string)
        .ok_or_else(|| "OpenViking temp_upload did not return temp_file_id".to_string())
}

fn add_resource_payload_for_source(
    source: &str,
    args: &Value,
) -> Result<(Value, Option<PathBuf>), String> {
    if args
        .get("to")
        .and_then(Value::as_str)
        .is_some_and(|value| !value.trim().is_empty())
        && args
            .get("parent")
            .and_then(Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    {
        return Err("Cannot specify both 'to' and 'parent'".to_string());
    }

    let mut body = json!({});
    for key in ["reason", "to", "parent", "instruction"] {
        if let Some(value) = args.get(key).and_then(Value::as_str) {
            if !value.trim().is_empty() {
                body[key] = json!(value);
            }
        }
    }
    for key in ["wait", "timeout"] {
        if let Some(value) = args.get(key) {
            if !value.is_null() {
                body[key] = value.clone();
            }
        }
    }

    let source = source.trim();
    if is_remote_resource_source(source) {
        body["path"] = json!(source);
        return Ok((body, None));
    }

    let path = if source.starts_with("file://") {
        file_uri_to_path(source)?
    } else if source.contains("://") && !is_windows_absolute_path(source) {
        body["path"] = json!(source);
        return Ok((body, None));
    } else {
        PathBuf::from(source).expanduser()
    };

    if !path.exists() {
        if is_local_path_reference(source) {
            return Err(format!("Local resource path does not exist: {source}"));
        }
        body["path"] = json!(source);
        return Ok((body, None));
    }

    if path
        .symlink_metadata()
        .map_err(|e| format!("metadata {}: {e}", path.display()))?
        .file_type()
        .is_symlink()
    {
        return Err(format!(
            "Local resource path is a symlink and will not be uploaded: {source}"
        ));
    }

    if path.is_file() {
        body["source_name"] = json!(path.file_name().and_then(|v| v.to_str()).unwrap_or("file"));
        Ok((body, Some(path)))
    } else if path.is_dir() {
        body["source_name"] = json!(path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or("directory"));
        Ok((body, Some(zip_directory(&path)?)))
    } else {
        Err(format!("Unsupported local resource path: {source}"))
    }
}

trait ExpandUserPath {
    fn expanduser(self) -> PathBuf;
}

impl ExpandUserPath for PathBuf {
    fn expanduser(self) -> PathBuf {
        let raw = self.to_string_lossy();
        if raw == "~" {
            if let Some(home) = dirs::home_dir() {
                return home;
            }
        }
        if let Some(rest) = raw.strip_prefix("~/").or_else(|| raw.strip_prefix("~\\")) {
            if let Some(home) = dirs::home_dir() {
                return home.join(rest);
            }
        }
        self
    }
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
        let agent = std::env::var("OPENVIKING_AGENT").unwrap_or_else(|_| DEFAULT_AGENT.into());
        let client = match Client::builder().timeout(Duration::from_secs(45)).build() {
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
            agent,
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
        let ep = guard.as_ref().map(|s| s.endpoint.as_str()).unwrap_or("");
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
        let url = format!("{}/api/v1/sessions/{}/commit", st.endpoint, st.session_id);
        let _ = st.client.post(&url).headers(h).send();
    }

    fn on_session_switch(&self, new_session_id: &str, _parent_session_id: &str, _reset: bool) {
        let new_session_id = new_session_id.trim();
        if new_session_id.is_empty() {
            return;
        }
        if let Some(st) = self.state.lock().unwrap().as_mut() {
            st.session_id = new_session_id.to_string();
            st.turn_count = 0;
        }
        *self.prefetch.lock().unwrap() = String::new();
    }

    fn on_memory_write(&self, action: &str, target: &str, content: &str) {
        if !action.trim().eq_ignore_ascii_case("add") || content.trim().is_empty() {
            return;
        }
        let st = match self.state.lock().unwrap().clone() {
            Some(s) => s,
            None => return,
        };
        let h = viking_headers(&st);
        let url = format!("{}/api/v1/content/write", st.endpoint);
        let body = content_write_body(&st, memory_subdir_for_target(target), content);
        std::thread::spawn(move || {
            let _ = st.client.post(&url).headers(h).json(&body).send();
        });
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
                let level = args
                    .get("level")
                    .and_then(|v| v.as_str())
                    .unwrap_or("overview");
                let path = match level {
                    "abstract" => "/api/v1/content/abstract",
                    "full" => "/api/v1/content/read",
                    _ => "/api/v1/content/overview",
                };
                let url = format!("{}{}", st.endpoint, path);
                match st.client.get(&url).headers(h).query(&[("uri", uri)]).send() {
                    Ok(r) if r.status().is_success() => match r.json::<Value>() {
                        Ok(v) => v.to_string(),
                        Err(e) => json!({"error": e.to_string()}).to_string(),
                    },
                    Ok(r) => json!({"error": format!("HTTP {}", r.status())}).to_string(),
                    Err(e) => json!({"error": e.to_string()}).to_string(),
                }
            }
            "viking_browse" => {
                let action = args
                    .get("action")
                    .and_then(|v| v.as_str())
                    .unwrap_or("list");
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
                let body = content_write_body(&st, memory_subdir_for_category(cat), content);
                let uri = body.get("uri").cloned().unwrap_or(Value::Null);
                let url = format!("{}/api/v1/content/write", st.endpoint);
                match st.client.post(&url).headers(h).json(&body).send() {
                    Ok(r) if r.status().is_success() => {
                        json!({"status": "stored", "uri": uri}).to_string()
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
                let (mut body, upload_path) = match add_resource_payload_for_source(url_arg, args) {
                    Ok(value) => value,
                    Err(e) => return json!({"error": e}).to_string(),
                };
                let cleanup_path = upload_path
                    .as_ref()
                    .filter(|path| {
                        path.file_name()
                            .and_then(|name| name.to_str())
                            .is_some_and(|name| name.starts_with("openviking_upload_"))
                    })
                    .cloned();
                if let Some(path) = upload_path.as_deref() {
                    match upload_temp_file(&st, path) {
                        Ok(temp_file_id) => body["temp_file_id"] = json!(temp_file_id),
                        Err(e) => {
                            if let Some(cleanup_path) = cleanup_path {
                                let _ = std::fs::remove_file(cleanup_path);
                            }
                            return json!({"error": e}).to_string();
                        }
                    }
                }
                let url = format!("{}/api/v1/resources", st.endpoint);
                let result = match st.client.post(&url).headers(h).json(&body).send() {
                    Ok(resp) if resp.status().is_success() => resp
                        .json()
                        .unwrap_or(json!({"status": "added"}))
                        .to_string(),
                    Ok(r) => json!({"error": format!("HTTP {}", r.status())}).to_string(),
                    Err(e) => json!({"error": e.to_string()}).to_string(),
                };
                if let Some(cleanup_path) = cleanup_path {
                    let _ = std::fs::remove_file(cleanup_path);
                }
                result
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
            {"key": "api_key", "description": "API key", "secret": true, "env_var": "OPENVIKING_API_KEY"},
            {"key": "agent", "description": "OpenViking agent namespace", "env_var": "OPENVIKING_AGENT", "default": DEFAULT_AGENT}
        ]))
    }
}

struct OpenVikingPrefetch {
    st: VikingState,
    q: String,
}

impl OpenVikingPrefetch {
    fn run(self) -> Result<String, ()> {
        let h = viking_headers(&self.st);
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

    #[test]
    fn memory_uri_includes_agent_and_sanitizes_tenant_segments() {
        let uri = build_memory_uri("user/name", "agent one", "patterns");
        assert!(uri.starts_with("viking://user/user_name/agent/agent_one/memories/patterns/mem_"));
        assert!(uri.ends_with(".md"));
        assert!(!uri.contains("user/name"));
        assert!(!uri.contains("agent one"));
    }

    #[test]
    fn memory_subdir_mapping_matches_write_targets_and_categories() {
        assert_eq!(memory_subdir_for_category("entity"), "entities");
        assert_eq!(memory_subdir_for_category("event"), "events");
        assert_eq!(memory_subdir_for_category("case"), "cases");
        assert_eq!(memory_subdir_for_category("pattern"), "patterns");
        assert_eq!(memory_subdir_for_category("unknown"), "preferences");
        assert_eq!(memory_subdir_for_target("memory"), "patterns");
        assert_eq!(memory_subdir_for_target("user"), "preferences");
    }

    #[test]
    fn headers_include_agent_and_bearer_key() {
        let st = VikingState {
            client: Client::new(),
            endpoint: DEFAULT_ENDPOINT.to_string(),
            api_key: "secret".to_string(),
            account: "acct".to_string(),
            user: "user".to_string(),
            agent: "agent".to_string(),
            session_id: "session".to_string(),
            turn_count: 0,
        };
        let headers = viking_headers(&st);
        assert_eq!(headers["X-OpenViking-Agent"], "agent");
        assert_eq!(headers["X-API-Key"], "secret");
        assert_eq!(headers["Authorization"], "Bearer secret");
    }

    #[test]
    fn content_write_body_uses_agent_scoped_create_uri() {
        let st = VikingState {
            client: Client::new(),
            endpoint: DEFAULT_ENDPOINT.to_string(),
            api_key: String::new(),
            account: "acct".to_string(),
            user: "she/a".to_string(),
            agent: "hermes ultra".to_string(),
            session_id: "session".to_string(),
            turn_count: 0,
        };
        let body = content_write_body(&st, "patterns", "fact");
        let uri = body["uri"].as_str().expect("uri");
        assert!(uri.starts_with("viking://user/she_a/agent/hermes_ultra/memories/patterns/"));
        assert_eq!(body["content"], "fact");
        assert_eq!(body["mode"], "create");
    }

    #[test]
    fn session_switch_updates_session_and_clears_prefetch() {
        let plugin = OpenVikingMemoryPlugin::new();
        *plugin.state.lock().unwrap() = Some(VikingState {
            client: Client::new(),
            endpoint: DEFAULT_ENDPOINT.to_string(),
            api_key: String::new(),
            account: "acct".to_string(),
            user: "user".to_string(),
            agent: "agent".to_string(),
            session_id: "old".to_string(),
            turn_count: 7,
        });
        *plugin.prefetch.lock().unwrap() = "stale".to_string();

        plugin.on_session_switch("new", "old", false);

        let state = plugin.state.lock().unwrap().clone().expect("state");
        assert_eq!(state.session_id, "new");
        assert_eq!(state.turn_count, 0);
        assert!(plugin.prefetch.lock().unwrap().is_empty());
    }

    #[test]
    fn add_resource_payload_routes_remote_url_as_path() {
        let (body, upload) = add_resource_payload_for_source(
            "https://example.com/doc.md",
            &json!({"reason": "docs", "wait": true}),
        )
        .expect("payload");

        assert_eq!(body["path"], "https://example.com/doc.md");
        assert_eq!(body["reason"], "docs");
        assert_eq!(body["wait"], true);
        assert!(upload.is_none());
    }

    #[test]
    fn add_resource_payload_uploads_existing_local_file_and_file_uri() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let sample = tmp.path().join("sample file.md");
        std::fs::write(&sample, "# Local\n").expect("write sample");

        let (body, upload) =
            add_resource_payload_for_source(sample.to_str().expect("sample path"), &json!({}))
                .expect("payload");
        assert_eq!(body["source_name"], "sample file.md");
        assert_eq!(upload.as_deref(), Some(sample.as_path()));

        let uri = format!("file://{}", sample.to_string_lossy().replace(' ', "%20"));
        let (body, upload) = add_resource_payload_for_source(&uri, &json!({"reason": "file uri"}))
            .expect("file uri payload");
        assert_eq!(body["source_name"], "sample file.md");
        assert_eq!(body["reason"], "file uri");
        assert_eq!(upload.as_deref(), Some(sample.as_path()));
    }

    #[test]
    fn add_resource_payload_rejects_missing_local_path_and_to_parent_conflict() {
        let err = add_resource_payload_for_source("./definitely-missing-openviking.md", &json!({}))
            .expect_err("missing local path");
        assert!(err.contains("does not exist"));

        let err = add_resource_payload_for_source(
            "https://example.com/doc.md",
            &json!({"to": "viking://a", "parent": "viking://b"}),
        )
        .expect_err("to parent conflict");
        assert!(err.contains("Cannot specify both"));
    }

    #[test]
    fn add_resource_payload_zips_directory_and_skips_symlinks() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let docs = tmp.path().join("docs");
        std::fs::create_dir_all(docs.join("nested")).expect("mkdir");
        std::fs::write(docs.join("guide.md"), "# Guide\n").expect("write guide");
        std::fs::write(docs.join("nested").join("api.md"), "# API\n").expect("write api");
        #[cfg(unix)]
        std::os::unix::fs::symlink(docs.join("guide.md"), docs.join("guide-link.md"))
            .expect("symlink");

        let (body, upload) =
            add_resource_payload_for_source(docs.to_str().expect("docs path"), &json!({}))
                .expect("payload");
        let zip_path = upload.expect("zip path");
        assert_eq!(body["source_name"], "docs");
        assert!(zip_path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("openviking_upload_")));

        let zip_file = std::fs::File::open(&zip_path).expect("open zip");
        let mut archive = zip::ZipArchive::new(zip_file).expect("zip archive");
        let mut names = Vec::new();
        for idx in 0..archive.len() {
            names.push(archive.by_index(idx).expect("zip entry").name().to_string());
        }
        assert!(names.contains(&"guide.md".to_string()));
        assert!(names.contains(&"nested/api.md".to_string()));
        assert!(!names.contains(&"guide-link.md".to_string()));

        std::fs::remove_file(zip_path).expect("cleanup zip");
    }
}
