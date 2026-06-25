//! OpenViking memory provider — REST context database.
//!
//! Mirrors Python `plugins/memory/openviking/__init__.py` (HTTP subset).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use reqwest::blocking::multipart::{Form, Part};
use reqwest::blocking::Client;
use serde_json::{json, Value};

use crate::memory_manager::MemoryProviderPlugin;
use crate::memory_plugins::config_io;

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:1933";
const DEFAULT_AGENT: &str = "hermes";
const DEFAULT_MEMORY_SUBDIR: &str = "preferences";
const DEFAULT_SESSION_DRAIN_TIMEOUT: Duration = Duration::from_secs(10);
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

#[derive(Debug, Clone)]
struct OpenVikingConfig {
    endpoint: String,
    api_key: String,
    api_key_type: String,
    account: String,
    user: String,
    agent: String,
}

impl OpenVikingConfig {
    fn config_path(hermes_home: &str) -> PathBuf {
        Path::new(hermes_home).join("openviking.json")
    }

    fn default_config_path() -> PathBuf {
        config_io::default_hermes_home().join("openviking.json")
    }

    fn configured_at(path: &Path) -> bool {
        let object = config_io::read_json_object(path);
        if object
            .get("enabled")
            .and_then(Value::as_bool)
            .is_some_and(|enabled| enabled)
        {
            return true;
        }
        ["endpoint", "api_key", "root_api_key"].iter().any(|key| {
            object
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(|value| !value.trim().is_empty())
        })
    }

    fn load(hermes_home: &str) -> Self {
        let mut config = Self {
            endpoint: std::env::var("OPENVIKING_ENDPOINT")
                .unwrap_or_else(|_| DEFAULT_ENDPOINT.to_string()),
            api_key: std::env::var("OPENVIKING_API_KEY").unwrap_or_default(),
            api_key_type: std::env::var("OPENVIKING_API_KEY_TYPE")
                .unwrap_or_else(|_| "user".to_string()),
            account: std::env::var("OPENVIKING_ACCOUNT").unwrap_or_else(|_| "default".into()),
            user: std::env::var("OPENVIKING_USER").unwrap_or_else(|_| "default".into()),
            agent: std::env::var("OPENVIKING_AGENT").unwrap_or_else(|_| DEFAULT_AGENT.into()),
        };

        let path = Self::config_path(hermes_home);
        let raw = config_io::read_json_object(&path);
        apply_openviking_config_map(&mut config, &raw);

        config.endpoint = normalize_openviking_endpoint(&config.endpoint);
        config.api_key_type = normalize_openviking_key_type(&config.api_key_type);
        config.account = nonempty_or(&config.account, "default");
        config.user = nonempty_or(&config.user, "default");
        config.agent = nonempty_or(&config.agent, DEFAULT_AGENT);
        config
    }
}

fn apply_openviking_config_map(
    config: &mut OpenVikingConfig,
    raw: &serde_json::Map<String, Value>,
) {
    if let Some(endpoint) = raw
        .get("endpoint")
        .or(raw.get("base_url"))
        .or(raw.get("baseUrl"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        config.endpoint = endpoint.to_string();
    }
    if let Some(api_key) = raw
        .get("api_key")
        .or(raw.get("apiKey"))
        .or(raw.get("root_api_key"))
        .or(raw.get("rootApiKey"))
        .and_then(Value::as_str)
    {
        config.api_key = api_key.to_string();
    }
    if let Some(key_type) = raw
        .get("api_key_type")
        .or(raw.get("apiKeyType"))
        .and_then(Value::as_str)
    {
        config.api_key_type = key_type.to_string();
    }
    if let Some(account) = raw.get("account").and_then(Value::as_str) {
        config.account = account.to_string();
    }
    if let Some(user) = raw.get("user").and_then(Value::as_str) {
        config.user = user.to_string();
    }
    if let Some(agent) = raw.get("agent").and_then(Value::as_str) {
        config.agent = agent.to_string();
    }
}

fn normalize_openviking_endpoint(raw: &str) -> String {
    let value = raw.trim();
    let with_scheme = if value.is_empty() {
        DEFAULT_ENDPOINT.to_string()
    } else if value.contains("://") {
        value.to_string()
    } else {
        format!("http://{value}")
    };
    with_scheme.trim_end_matches('/').to_string()
}

fn normalize_openviking_key_type(raw: &str) -> String {
    match raw.trim().to_ascii_lowercase().as_str() {
        "root" | "root_api_key" | "root-api-key" => "root".to_string(),
        "none" | "dev" | "local" | "no_api_key" | "no-api-key" => "none".to_string(),
        _ => "user".to_string(),
    }
}

fn nonempty_or(raw: &str, default: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
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
    prefetch: Arc<Mutex<String>>,
    inflight_writers: Arc<Mutex<HashMap<String, Vec<JoinHandle<()>>>>>,
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

fn build_memory_uri(user: &str, _agent: &str, subdir: &str) -> String {
    let slug = uuid::Uuid::new_v4().simple().to_string();
    format!(
        "viking://user/{}/memories/{}/mem_{}.md",
        viking_uri_segment(user),
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

type InflightWriters = Arc<Mutex<HashMap<String, Vec<JoinHandle<()>>>>>;

fn openviking_session_drain_timeout() -> Duration {
    std::env::var("OPENVIKING_SESSION_DRAIN_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_SESSION_DRAIN_TIMEOUT)
}

fn drain_writers_for_session(
    writers: &InflightWriters,
    session_id: &str,
    timeout: Duration,
) -> bool {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return true;
    }
    let deadline = Instant::now() + timeout;
    loop {
        let finished = {
            let mut guard = writers.lock().unwrap();
            let mut finished = Vec::new();
            let mut pending = Vec::new();
            let remove_session = {
                let Some(handles) = guard.get_mut(session_id) else {
                    return true;
                };
                for handle in handles.drain(..) {
                    if handle.is_finished() {
                        finished.push(handle);
                    } else {
                        pending.push(handle);
                    }
                }
                if pending.is_empty() {
                    true
                } else {
                    *handles = pending;
                    false
                }
            };
            if remove_session {
                guard.remove(session_id);
            }
            finished
        };
        for handle in finished {
            if handle.join().is_err() {
                tracing::warn!("OpenViking writer for {session_id} panicked");
            }
        }
        if writers
            .lock()
            .unwrap()
            .get(session_id)
            .is_none_or(|handles| handles.is_empty())
        {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn commit_openviking_session(st: &VikingState) -> bool {
    if st.session_id.trim().is_empty() {
        return false;
    }
    let h = viking_headers(st);
    let url = format!("{}/api/v1/sessions/{}/commit", st.endpoint, st.session_id);
    match st.client.post(&url).headers(h).send() {
        Ok(resp) if resp.status().is_success() => true,
        Ok(resp) => {
            tracing::warn!(
                "OpenViking session commit for {} returned HTTP {}",
                st.session_id,
                resp.status()
            );
            false
        }
        Err(err) => {
            tracing::warn!(
                "OpenViking session commit for {} failed: {err}",
                st.session_id
            );
            false
        }
    }
}

fn spawn_deferred_commit(st: VikingState, writers: InflightWriters, context: &'static str) {
    std::thread::spawn(move || {
        if !drain_writers_for_session(&writers, &st.session_id, openviking_session_drain_timeout())
        {
            tracing::warn!(
                "OpenViking writer for {} still alive after drain during {context}; leaving session uncommitted",
                st.session_id
            );
            return;
        }
        let _ = commit_openviking_session(&st);
    });
}

impl OpenVikingMemoryPlugin {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(None),
            prefetch: Arc::new(Mutex::new(String::new())),
            inflight_writers: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn spawn_session_writer<F>(&self, session_id: String, job: F)
    where
        F: FnOnce() + Send + 'static,
    {
        if session_id.trim().is_empty() {
            return;
        }
        let handle = std::thread::spawn(job);
        self.inflight_writers
            .lock()
            .unwrap()
            .entry(session_id)
            .or_default()
            .push(handle);
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
            || OpenVikingConfig::configured_at(&OpenVikingConfig::default_config_path())
    }

    fn initialize(&self, session_id: &str, hermes_home: &str) {
        let config = OpenVikingConfig::load(hermes_home);
        let api_key_type = config.api_key_type.clone();
        let client = match Client::builder().timeout(Duration::from_secs(45)).build() {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("OpenViking client: {}", e);
                return;
            }
        };
        let st = VikingState {
            client,
            endpoint: config.endpoint,
            api_key: config.api_key,
            account: config.account,
            user: config.user,
            agent: config.agent,
            session_id: session_id.to_string(),
            turn_count: 0,
        };
        let health_url = format!("{}/health", st.endpoint);
        let h = viking_headers(&st);
        if st
            .client
            .get(&health_url)
            .headers(h)
            .send()
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            *self.state.lock().unwrap() = Some(st);
            tracing::info!("OpenViking memory plugin initialized ({api_key_type} credential mode)");
        } else {
            tracing::warn!(
                "OpenViking health check failed for {}; OpenViking memory disabled for this session",
                st.endpoint
            );
            *self.state.lock().unwrap() = None;
        }
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

    fn sync_turn(&self, user_content: &str, assistant_content: &str, session_id: &str) {
        let (sid, stc) = {
            let mut lock = self.state.lock().unwrap();
            let st = match lock.as_mut() {
                Some(s) => s,
                None => return,
            };
            let sid = if session_id.trim().is_empty() {
                st.session_id.clone()
            } else {
                session_id.trim().to_string()
            };
            if sid.is_empty() {
                return;
            }
            st.turn_count = st.turn_count.saturating_add(1);
            let mut stc = st.clone();
            stc.session_id = sid.clone();
            (sid, stc)
        };
        let u = user_content.chars().take(4000).collect::<String>();
        let a = assistant_content.chars().take(4000).collect::<String>();
        let h = viking_headers(&stc);
        self.spawn_session_writer(sid, move || {
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
        if !drain_writers_for_session(
            &self.inflight_writers,
            &st.session_id,
            openviking_session_drain_timeout(),
        ) {
            tracing::warn!(
                "OpenViking writer for {} still alive after drain; skipping session commit",
                st.session_id
            );
            return;
        }
        if commit_openviking_session(&st) {
            if let Some(current) = self.state.lock().unwrap().as_mut() {
                if current.session_id == st.session_id {
                    current.turn_count = 0;
                }
            }
        }
    }

    fn on_session_switch(&self, new_session_id: &str, _parent_session_id: &str, _reset: bool) {
        let new_session_id = new_session_id.trim();
        if new_session_id.is_empty() {
            return;
        }
        *self.prefetch.lock().unwrap() = String::new();
        let old_state = {
            let mut guard = self.state.lock().unwrap();
            let Some(st) = guard.as_mut() else {
                return;
            };
            if st.session_id == new_session_id {
                return;
            }
            let old = st.clone();
            st.session_id = new_session_id.to_string();
            st.turn_count = 0;
            old
        };
        if old_state.turn_count > 0 {
            spawn_deferred_commit(
                old_state,
                Arc::clone(&self.inflight_writers),
                "session switch",
            );
        }
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
        self.spawn_session_writer(st.session_id.clone(), move || {
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
        let writers = Arc::clone(&self.inflight_writers);
        let session_ids = writers.lock().unwrap().keys().cloned().collect::<Vec<_>>();
        for session_id in session_ids {
            let _ = drain_writers_for_session(
                &writers,
                &session_id,
                openviking_session_drain_timeout(),
            );
        }
        *self.state.lock().unwrap() = None;
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key": "endpoint", "description": "OpenViking server URL", "env_var": "OPENVIKING_ENDPOINT", "default": DEFAULT_ENDPOINT},
            {"key": "api_key", "description": "API key", "secret": true, "env_var": "OPENVIKING_API_KEY"},
            {"key": "api_key_type", "description": "Credential type: none|user|root", "default": "user", "env_var": "OPENVIKING_API_KEY_TYPE"},
            {"key": "account", "description": "Tenant account for root/local trusted mode", "env_var": "OPENVIKING_ACCOUNT", "default": "default"},
            {"key": "user", "description": "Tenant user for root/local trusted mode", "env_var": "OPENVIKING_USER", "default": "default"},
            {"key": "agent", "description": "OpenViking agent namespace", "env_var": "OPENVIKING_AGENT", "default": DEFAULT_AGENT}
        ]))
    }

    fn save_config(&self, config: &Value) -> Result<(), String> {
        let path = OpenVikingConfig::default_config_path();
        config_io::merge_and_write_owner_only(&path, config)
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
    fn name() {
        let p = OpenVikingMemoryPlugin::new();
        assert_eq!(p.name(), "openviking");
    }

    #[test]
    fn config_file_activates_provider_and_loads_values() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let _endpoint = EnvGuard::remove("OPENVIKING_ENDPOINT");
        let _api_key = EnvGuard::remove("OPENVIKING_API_KEY");
        let _account = EnvGuard::remove("OPENVIKING_ACCOUNT");
        let _user = EnvGuard::remove("OPENVIKING_USER");
        let _agent = EnvGuard::remove("OPENVIKING_AGENT");
        std::fs::write(
            tmp.path().join("openviking.json"),
            r#"{
                "enabled": true,
                "endpoint": "localhost:1934/",
                "api_key": "ov-secret",
                "api_key_type": "root",
                "account": "acct",
                "user": "operator",
                "agent": "ultra"
            }"#,
        )
        .expect("write config");

        assert!(OpenVikingMemoryPlugin::new().is_available());
        let config = OpenVikingConfig::load(tmp.path().to_str().expect("home"));
        assert_eq!(config.endpoint, "http://localhost:1934");
        assert_eq!(config.api_key, "ov-secret");
        assert_eq!(config.api_key_type, "root");
        assert_eq!(config.account, "acct");
        assert_eq!(config.user, "operator");
        assert_eq!(config.agent, "ultra");
    }

    #[test]
    fn save_config_merges_and_writes_owner_only() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let tmp = tempfile::tempdir().expect("tempdir");
        let _home = EnvGuard::set("HERMES_HOME", tmp.path());
        let path = tmp.path().join("openviking.json");
        std::fs::write(&path, r#"{"agent":"existing"}"#).expect("write existing");

        OpenVikingMemoryPlugin::new()
            .save_config(&json!({
                "enabled": true,
                "endpoint": "https://openviking.example",
                "api_key": "ov-secret"
            }))
            .expect("save config");

        let parsed: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read config"))
                .expect("json");
        assert_eq!(parsed["agent"], "existing");
        assert_eq!(parsed["enabled"], true);
        assert_eq!(parsed["endpoint"], "https://openviking.example");
        assert_eq!(parsed["api_key"], "ov-secret");

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
    fn memory_uri_sanitizes_tenant_segments_without_agent_scope() {
        let uri = build_memory_uri("user/name", "agent one", "patterns");
        assert!(uri.starts_with("viking://user/user_name/memories/patterns/mem_"));
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
    fn content_write_body_uses_user_scoped_create_uri() {
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
        assert!(uri.starts_with("viking://user/she_a/memories/patterns/"));
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
            turn_count: 0,
        });
        *plugin.prefetch.lock().unwrap() = "stale".to_string();

        plugin.on_session_switch("new", "old", false);

        let state = plugin.state.lock().unwrap().clone().expect("state");
        assert_eq!(state.session_id, "new");
        assert_eq!(state.turn_count, 0);
        assert!(plugin.prefetch.lock().unwrap().is_empty());
    }

    #[test]
    fn drain_writers_waits_for_all_finished_session_writers() {
        let plugin = OpenVikingMemoryPlugin::new();
        plugin.spawn_session_writer("sid".to_string(), || {});
        plugin.spawn_session_writer("sid".to_string(), || {});

        assert!(drain_writers_for_session(
            &plugin.inflight_writers,
            "sid",
            Duration::from_secs(1)
        ));
        assert!(plugin.inflight_writers.lock().unwrap().get("sid").is_none());
    }

    #[test]
    fn session_end_skips_commit_when_writer_outlives_drain() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let _timeout = EnvGuard::set("OPENVIKING_SESSION_DRAIN_TIMEOUT_MS", "1");
        let plugin = OpenVikingMemoryPlugin::new();
        *plugin.state.lock().unwrap() = Some(VikingState {
            client: Client::new(),
            endpoint: "http://127.0.0.1:9".to_string(),
            api_key: String::new(),
            account: "acct".to_string(),
            user: "user".to_string(),
            agent: "agent".to_string(),
            session_id: "old".to_string(),
            turn_count: 2,
        });
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        plugin.spawn_session_writer("old".to_string(), move || {
            let _ = release_rx.recv();
        });

        plugin.on_session_end(&[]);

        assert_eq!(
            plugin
                .state
                .lock()
                .unwrap()
                .as_ref()
                .expect("state")
                .turn_count,
            2
        );
        release_tx.send(()).expect("release writer");
        assert!(drain_writers_for_session(
            &plugin.inflight_writers,
            "old",
            Duration::from_secs(1)
        ));
    }

    #[test]
    fn session_switch_rotates_without_waiting_for_old_writer() {
        let _guard = config_io::TEST_ENV_LOCK.lock().expect("env lock");
        let _timeout = EnvGuard::set("OPENVIKING_SESSION_DRAIN_TIMEOUT_MS", "1");
        let plugin = OpenVikingMemoryPlugin::new();
        *plugin.state.lock().unwrap() = Some(VikingState {
            client: Client::new(),
            endpoint: "http://127.0.0.1:9".to_string(),
            api_key: String::new(),
            account: "acct".to_string(),
            user: "user".to_string(),
            agent: "agent".to_string(),
            session_id: "old".to_string(),
            turn_count: 2,
        });
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        plugin.spawn_session_writer("old".to_string(), move || {
            let _ = release_rx.recv();
        });
        let start = Instant::now();

        plugin.on_session_switch("new", "old", false);

        assert!(start.elapsed() < Duration::from_millis(100));
        let state = plugin.state.lock().unwrap().clone().expect("state");
        assert_eq!(state.session_id, "new");
        assert_eq!(state.turn_count, 0);
        release_tx.send(()).expect("release writer");
        assert!(drain_writers_for_session(
            &plugin.inflight_writers,
            "old",
            Duration::from_secs(1)
        ));
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
