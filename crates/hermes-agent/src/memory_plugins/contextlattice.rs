//! ContextLattice memory provider plugin.
//!
//! Every ContextLattice selector is derived from the authenticated
//! [`MemorySubjectStorageKey`].  Configuration can tune transport behavior but
//! cannot select a project, topic, agent, or user partition.

use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use hermes_core::{
    MemoryAuditActor, MemoryExportReceipt, MemoryExportSink, MemoryForgetReceipt,
    MemoryGovernanceOperation, MemoryProviderGovernanceError, MemoryReadbackReceipt,
    MemoryRetentionPolicy, MemoryRetentionReceipt, MemorySubject, MemorySubjectStorageKey,
    MemorySurface,
};
use reqwest::Method;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::memory_manager::MemoryProviderPlugin;

const DEFAULT_ORCHESTRATOR_URL: &str = "http://127.0.0.1:8075";
const CONTEXTLATTICE_OUTBOX_DIR: &str = "contextlattice-outbox";
const CONTEXTLATTICE_OUTBOX_SCHEMA: &str = "hermes.contextlattice-outbox/v1";
const CONTEXTLATTICE_MAX_IMMEDIATE_ATTEMPTS: u32 = 3;
const CONTEXTLATTICE_RETRY_DELAY: Duration = Duration::from_millis(20);

fn search_schema() -> Value {
    json!({
        "name": "contextlattice_search",
        "description": "Search authenticated ContextLattice memory for relevant context with source grounding.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type":"string", "description":"Search query."},
                "topic_path": {"type":"string", "description":"Optional relative topic suffix inside the authenticated subject partition."},
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
        "description": "Fetch authenticated ContextLattice context for broad multi-file grounding.",
        "parameters": {
            "type": "object",
            "properties": {
                "query": {"type":"string"},
                "topic_path": {"type":"string", "description":"Optional relative topic suffix inside the authenticated subject partition."},
                "retrieval_mode": {"type":"string", "description":"fast|balanced|deep"}
            },
            "required": ["query"]
        }
    })
}

fn write_schema() -> Value {
    json!({
        "name": "contextlattice_write",
        "description": "Durably write an authenticated ContextLattice checkpoint.",
        "parameters": {
            "type": "object",
            "properties": {
                "content": {"type":"string"},
                "file_name": {"type":"string", "description":"Relative logical file suffix inside the authenticated subject partition."},
                "topic_path": {"type":"string", "description":"Relative topic suffix inside the authenticated subject partition."}
            },
            "required": ["content"]
        }
    })
}

/// Every remote selector begins with this canonical opaque storage key.  Raw
/// tenant, principal, team, and session identifiers are never used as paths,
/// remote selectors, or durable filenames.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ContextLatticeScope {
    opaque_subject_key: String,
}

impl ContextLatticeScope {
    fn new(filter: &MemorySubjectStorageKey) -> Result<Self, String> {
        let opaque_subject_key = filter.as_str();
        let valid_key = opaque_subject_key
            .strip_prefix("msp1_")
            .is_some_and(|digest| {
                digest.len() == 64 && digest.as_bytes().iter().all(u8::is_ascii_hexdigit)
            });
        if !valid_key {
            return Err(
                "ContextLattice requires a canonical authenticated subject key".to_string(),
            );
        }
        Ok(Self {
            opaque_subject_key: opaque_subject_key.to_string(),
        })
    }

    fn project(&self) -> String {
        format!("{}/project", self.opaque_subject_key)
    }

    fn agent_id(&self) -> String {
        format!("{}/agent", self.opaque_subject_key)
    }

    fn topic_root(&self) -> String {
        format!("{}/topics", self.opaque_subject_key)
    }

    fn topic_path(&self, suffix: &str) -> Result<String, String> {
        Ok(format!(
            "{}/topics/{}",
            self.opaque_subject_key,
            relative_scope_suffix(suffix, "memory")?
        ))
    }

    fn file_name(&self, file_name: &str) -> Result<String, String> {
        Ok(format!(
            "{}/files/{}",
            self.opaque_subject_key,
            relative_scope_suffix(file_name, "checkpoints/current.md")?
        ))
    }

    fn route(&self, endpoint: &str) -> String {
        format!("{endpoint}?subject_key={}", self.opaque_subject_key)
    }

    fn outbox_directory_name(&self) -> String {
        let mut digest = Sha256::new();
        digest.update(b"hermes.contextlattice.outbox.directory/v1\0");
        digest.update(self.opaque_subject_key.as_bytes());
        hex::encode(digest.finalize())
    }
}

fn relative_scope_suffix(raw: &str, fallback: &str) -> Result<String, String> {
    let value = if raw.trim().is_empty() {
        fallback
    } else {
        raw.trim()
    };
    if value.starts_with('/') || value.starts_with('\\') || value.contains('\\') {
        return Err(
            "ContextLattice scope suffix must be a relative slash-delimited path".to_string(),
        );
    }
    let mut normalized = Vec::new();
    for segment in value.split('/') {
        if segment.is_empty() || segment == "." || segment == ".." {
            return Err("ContextLattice scope suffix contains an invalid path segment".to_string());
        }
        if segment
            .chars()
            .any(|character| character.is_control() || character == ':')
        {
            return Err("ContextLattice scope suffix contains an unsafe character".to_string());
        }
        normalized.push(segment);
    }
    Ok(normalized.join("/"))
}

#[derive(Debug, Clone)]
struct ContextLatticeConfig {
    orchestrator_url: String,
    timeout_secs: f64,
    include_grounding: bool,
    include_retrieval_debug: bool,
    default_retrieval_mode: String,
    default_topic_suffix: String,
    default_file_name: String,
    pressure_aware: bool,
    pressure_pending_threshold: u64,
    pressure_cooldown_threshold: u64,
    pressure_status_ttl_secs: f64,
    pressure_fast_sources: Vec<String>,
    pressure_force_fast_mode: bool,
    pressure_force_fast_prefetch: bool,
    api_key: Option<String>,
    outbox_root: PathBuf,
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
    fn load(hermes_home: &str) -> Result<Self, String> {
        let mut config = Self {
            orchestrator_url: std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL")
                .or_else(|_| std::env::var("MEMMCP_ORCHESTRATOR_URL"))
                .unwrap_or_else(|_| DEFAULT_ORCHESTRATOR_URL.to_string()),
            timeout_secs: std::env::var("CONTEXTLATTICE_TIMEOUT_SECS")
                .ok()
                .and_then(|value| value.parse::<f64>().ok())
                .unwrap_or(10.0)
                .clamp(1.0, 60.0),
            include_grounding: true,
            include_retrieval_debug: true,
            default_retrieval_mode: "balanced".to_string(),
            default_topic_suffix: "memory".to_string(),
            default_file_name: "checkpoints/current.md".to_string(),
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
            outbox_root: PathBuf::from(hermes_home),
        };

        let path = Path::new(hermes_home).join("contextlattice.json");
        if let Ok(content) = fs::read_to_string(path) {
            let raw: Value = serde_json::from_str(&content)
                .map_err(|_| "ContextLattice configuration is not valid JSON".to_string())?;
            let object = raw
                .as_object()
                .ok_or_else(|| "ContextLattice configuration must be a JSON object".to_string())?;
            if object.contains_key("project") || object.contains_key("agent_id") {
                return Err(
                    "ContextLattice project and agent configuration are prohibited; use the authenticated subject partition"
                        .to_string(),
                );
            }
            if let Some(url) = object.get("orchestrator_url").and_then(Value::as_str) {
                if !url.trim().is_empty() {
                    config.orchestrator_url = url.trim().to_string();
                }
            }
            if let Some(timeout) = object.get("timeout_secs").and_then(Value::as_f64) {
                config.timeout_secs = timeout.clamp(1.0, 60.0);
            }
            if let Some(enabled) = object.get("include_grounding").and_then(Value::as_bool) {
                config.include_grounding = enabled;
            }
            if let Some(enabled) = object
                .get("include_retrieval_debug")
                .and_then(Value::as_bool)
            {
                config.include_retrieval_debug = enabled;
            }
            if let Some(mode) = object.get("retrieval_mode").and_then(Value::as_str) {
                if !mode.trim().is_empty() {
                    config.default_retrieval_mode = mode.trim().to_string();
                }
            }
            if let Some(topic) = object
                .get("topic_suffix")
                .or_else(|| object.get("topic_path"))
                .and_then(Value::as_str)
            {
                config.default_topic_suffix = relative_scope_suffix(topic, "memory")?;
            }
            if let Some(file_name) = object.get("file_name").and_then(Value::as_str) {
                config.default_file_name =
                    relative_scope_suffix(file_name, "checkpoints/current.md")?;
            }
            if let Some(enabled) = object.get("pressure_aware").and_then(Value::as_bool) {
                config.pressure_aware = enabled;
            }
            if let Some(value) = object
                .get("pressure_pending_threshold")
                .and_then(Value::as_u64)
            {
                config.pressure_pending_threshold = value;
            }
            if let Some(value) = object
                .get("pressure_cooldown_threshold")
                .and_then(Value::as_u64)
            {
                config.pressure_cooldown_threshold = value;
            }
            if let Some(value) = object
                .get("pressure_status_ttl_secs")
                .and_then(Value::as_f64)
            {
                config.pressure_status_ttl_secs = value.clamp(0.2, 30.0);
            }
            if let Some(values) = object
                .get("pressure_fast_sources")
                .and_then(Value::as_array)
            {
                let parsed = values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string)
                    .collect::<Vec<_>>();
                if !parsed.is_empty() {
                    config.pressure_fast_sources = parsed;
                }
            }
            if let Some(value) = object
                .get("pressure_force_fast_mode")
                .and_then(Value::as_bool)
            {
                config.pressure_force_fast_mode = value;
            }
            if let Some(value) = object
                .get("pressure_force_fast_prefetch")
                .and_then(Value::as_bool)
            {
                config.pressure_force_fast_prefetch = value;
            }
            if let Some(api_key) = object.get("api_key").and_then(Value::as_str) {
                if !api_key.trim().is_empty() {
                    config.api_key = Some(api_key.to_string());
                }
            }
        }

        if let Ok(raw) = std::env::var("CONTEXTLATTICE_PRESSURE_AWARE") {
            if let Some(value) = parse_bool_flag(&raw) {
                config.pressure_aware = value;
            }
        }
        if let Ok(raw) = std::env::var("CONTEXTLATTICE_PRESSURE_PENDING_THRESHOLD") {
            if let Ok(value) = raw.trim().parse::<u64>() {
                config.pressure_pending_threshold = value;
            }
        }
        if let Ok(raw) = std::env::var("CONTEXTLATTICE_PRESSURE_COOLDOWN_THRESHOLD") {
            if let Ok(value) = raw.trim().parse::<u64>() {
                config.pressure_cooldown_threshold = value;
            }
        }
        if let Ok(raw) = std::env::var("CONTEXTLATTICE_PRESSURE_STATUS_TTL_SECS") {
            if let Ok(value) = raw.trim().parse::<f64>() {
                config.pressure_status_ttl_secs = value.clamp(0.2, 30.0);
            }
        }
        if let Ok(raw) = std::env::var("CONTEXTLATTICE_PRESSURE_FAST_SOURCES") {
            let parsed = parse_csv_sources(&raw);
            if !parsed.is_empty() {
                config.pressure_fast_sources = parsed;
            }
        }
        if let Ok(raw) = std::env::var("CONTEXTLATTICE_PRESSURE_FORCE_FAST_MODE") {
            if let Some(value) = parse_bool_flag(&raw) {
                config.pressure_force_fast_mode = value;
            }
        }
        if let Ok(raw) = std::env::var("CONTEXTLATTICE_PRESSURE_FORCE_FAST_PREFETCH") {
            if let Some(value) = parse_bool_flag(&raw) {
                config.pressure_force_fast_prefetch = value;
            }
        }

        config.orchestrator_url = config.orchestrator_url.trim_end_matches('/').to_string();
        if config.orchestrator_url.trim().is_empty() {
            return Err("ContextLattice orchestrator URL is required".to_string());
        }
        Ok(config)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ContextLatticeRequestFailure {
    Client,
    Transport,
    Status(u16),
    Decode,
}

impl ContextLatticeRequestFailure {
    fn code(self) -> &'static str {
        match self {
            Self::Client => "contextlattice_client_unavailable",
            Self::Transport => "contextlattice_transport_failed",
            Self::Status(_) => "contextlattice_http_failure",
            Self::Decode => "contextlattice_response_invalid",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DurableContextLatticeWrite {
    schema: String,
    idempotency_key: String,
    subject_storage_key: String,
    payload: Value,
    attempts: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_failure_code: Option<String>,
}

impl DurableContextLatticeWrite {
    fn new(scope: &ContextLatticeScope, payload: Value) -> Self {
        Self {
            schema: CONTEXTLATTICE_OUTBOX_SCHEMA.to_string(),
            idempotency_key: uuid::Uuid::new_v4().to_string(),
            subject_storage_key: scope.opaque_subject_key.clone(),
            payload,
            attempts: 0,
            last_failure_code: None,
        }
    }
}

fn durable_outbox_directory(home: &Path, scope: &ContextLatticeScope) -> PathBuf {
    home.join(CONTEXTLATTICE_OUTBOX_DIR)
        .join(scope.outbox_directory_name())
}

fn ensure_private_directory(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path)
        .map_err(|_| "ContextLattice durable outbox directory unavailable".to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|_| "ContextLattice durable outbox permissions unavailable".to_string())?;
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| "ContextLattice durable outbox directory sync failed".to_string())?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

fn write_private_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "ContextLattice durable outbox path is invalid".to_string())?;
    ensure_private_directory(parent)?;
    let bytes = serde_json::to_vec(value)
        .map_err(|_| "ContextLattice durable outbox serialization failed".to_string())?;
    let temporary = parent.join(format!(
        ".{}.{}.tmp",
        uuid::Uuid::new_v4(),
        std::process::id()
    ));
    let result = (|| -> Result<(), String> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options
            .open(&temporary)
            .map_err(|_| "ContextLattice durable outbox write failed".to_string())?;
        file.write_all(&bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "ContextLattice durable outbox write failed".to_string())?;
        drop(file);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&temporary, fs::Permissions::from_mode(0o600))
                .map_err(|_| "ContextLattice durable outbox permissions unavailable".to_string())?;
        }
        fs::rename(&temporary, path)
            .map_err(|_| "ContextLattice durable outbox commit failed".to_string())?;
        sync_directory(parent)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn persist_outbox_write(
    home: &Path,
    scope: &ContextLatticeScope,
    entry: &DurableContextLatticeWrite,
) -> Result<PathBuf, String> {
    let directory = durable_outbox_directory(home, scope);
    ensure_private_directory(&directory)?;
    let path = directory.join(format!("{}.json", entry.idempotency_key));
    write_private_json(&path, entry)?;
    Ok(path)
}

fn read_outbox_write(path: &Path) -> Result<DurableContextLatticeWrite, String> {
    let mut file = File::open(path)
        .map_err(|_| "ContextLattice durable outbox entry unavailable".to_string())?;
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|_| "ContextLattice durable outbox entry unreadable".to_string())?;
    let entry: DurableContextLatticeWrite = serde_json::from_slice(&bytes)
        .map_err(|_| "ContextLattice durable outbox entry unreadable".to_string())?;
    if entry.schema != CONTEXTLATTICE_OUTBOX_SCHEMA
        || entry.idempotency_key.trim().is_empty()
        || entry.subject_storage_key.trim().is_empty()
        || !entry.payload.is_object()
    {
        return Err("ContextLattice durable outbox entry is invalid".to_string());
    }
    Ok(entry)
}

fn pending_outbox_paths(home: &Path, scope: &ContextLatticeScope) -> Result<Vec<PathBuf>, String> {
    let directory = durable_outbox_directory(home, scope);
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let entries = fs::read_dir(directory)
        .map_err(|_| "ContextLattice durable outbox unavailable".to_string())?;
    let mut paths = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|extension| extension.to_str()) == Some("json"))
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

fn subject_outbox_residue_count(home: &Path, scope: &ContextLatticeScope) -> Result<u64, String> {
    let directory = durable_outbox_directory(home, scope);
    if !directory.exists() {
        return Ok(0);
    }
    let entries = fs::read_dir(directory)
        .map_err(|_| "ContextLattice durable outbox unavailable".to_string())?;
    let mut count = 0_u64;
    for entry in entries {
        let _entry = entry.map_err(|_| "ContextLattice durable outbox unavailable".to_string())?;
        count = count.saturating_add(1);
    }
    Ok(count)
}

fn remove_outbox_write(path: &Path) -> Result<(), String> {
    fs::remove_file(path)
        .map_err(|_| "ContextLattice durable outbox acknowledgement cleanup failed".to_string())?;
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

#[derive(Debug, Clone)]
enum WriteDisposition {
    Acknowledged {
        idempotency_key: String,
    },
    PendingRetry {
        idempotency_key: String,
        failure_code: &'static str,
    },
}

pub struct ContextLatticeMemoryPlugin {
    config: Mutex<Option<ContextLatticeConfig>>,
    prefetch_results: Arc<Mutex<HashMap<String, String>>>,
    pressure_cache: Arc<Mutex<Option<QueuePressureSnapshot>>>,
}

impl ContextLatticeMemoryPlugin {
    pub fn new() -> Self {
        Self {
            config: Mutex::new(None),
            prefetch_results: Arc::new(Mutex::new(HashMap::new())),
            pressure_cache: Arc::new(Mutex::new(None)),
        }
    }

    fn config_snapshot(&self) -> Result<ContextLatticeConfig, String> {
        self.config
            .lock()
            .map_err(|_| "ContextLattice configuration lock unavailable".to_string())?
            .clone()
            .ok_or_else(|| "ContextLattice plugin not initialized".to_string())
    }

    fn scope_for(
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
    ) -> Result<ContextLatticeScope, String> {
        // Construction is explicit for every surface.  The match makes the
        // enforced multi-user boundary visible here rather than falling back to
        // a session/config-derived owner.
        match subject.surface() {
            MemorySurface::Gateway
            | MemorySurface::Http
            | MemorySurface::Acp
            | MemorySurface::Cron => ContextLatticeScope::new(filter),
            MemorySurface::Cli | MemorySurface::Local => ContextLatticeScope::new(filter),
        }
    }

    fn client(
        config: &ContextLatticeConfig,
    ) -> Result<reqwest::blocking::Client, ContextLatticeRequestFailure> {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs_f64(config.timeout_secs))
            .build()
            .map_err(|_| ContextLatticeRequestFailure::Client)
    }

    fn request_json(
        config: &ContextLatticeConfig,
        method: Method,
        route: &str,
        payload: &Value,
        idempotency_key: Option<&str>,
    ) -> Result<Value, ContextLatticeRequestFailure> {
        let client = Self::client(config)?;
        let url = format!(
            "{}/{}",
            config.orchestrator_url.trim_end_matches('/'),
            route.trim_start_matches('/')
        );
        let mut request = client
            .request(method, url)
            .header("Content-Type", "application/json")
            .json(payload);
        if let Some(idempotency_key) = idempotency_key {
            request = request.header("Idempotency-Key", idempotency_key);
        }
        if let Some(api_key) = &config.api_key {
            request = request
                .header("Authorization", format!("Bearer {api_key}"))
                .header("X-API-Key", api_key);
        }
        let response = request
            .send()
            .map_err(|_| ContextLatticeRequestFailure::Transport)?;
        let status = response.status();
        if !status.is_success() {
            return Err(ContextLatticeRequestFailure::Status(status.as_u16()));
        }
        let body = response
            .bytes()
            .map_err(|_| ContextLatticeRequestFailure::Transport)?;
        if body.is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_slice(&body).map_err(|_| ContextLatticeRequestFailure::Decode)
    }

    fn request_no_body(
        config: &ContextLatticeConfig,
        method: Method,
        route: &str,
    ) -> Result<Value, ContextLatticeRequestFailure> {
        let client = Self::client(config)?;
        let url = format!(
            "{}/{}",
            config.orchestrator_url.trim_end_matches('/'),
            route.trim_start_matches('/')
        );
        let mut request = client.request(method, url);
        if let Some(api_key) = &config.api_key {
            request = request
                .header("Authorization", format!("Bearer {api_key}"))
                .header("X-API-Key", api_key);
        }
        let response = request
            .send()
            .map_err(|_| ContextLatticeRequestFailure::Transport)?;
        let status = response.status();
        if !status.is_success() {
            return Err(ContextLatticeRequestFailure::Status(status.as_u16()));
        }
        let body = response
            .bytes()
            .map_err(|_| ContextLatticeRequestFailure::Transport)?;
        if body.is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_slice(&body).map_err(|_| ContextLatticeRequestFailure::Decode)
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

        // `/status` is global operational telemetry, not subject memory. It
        // must never receive an authenticated memory selector.
        let status = match Self::request_no_body(config, Method::GET, "/status") {
            Ok(value) => value,
            Err(failure) => {
                tracing::debug!(
                    provider = "contextlattice",
                    status = failure.code(),
                    "ContextLattice pressure status unavailable"
                );
                return None;
            }
        };
        let queue = status.get("queue").and_then(Value::as_object)?;
        let depth = queue.get("memoryWriteQueueDepth").and_then(Value::as_u64)?;
        let maximum = queue.get("memoryWriteQueueMax").and_then(Value::as_u64)?;
        let ratio = queue.get("memoryWriteQueueRatio").and_then(Value::as_f64)?;
        let pending = queue.get("pending").and_then(Value::as_u64)?;
        let pending_total = queue.get("pendingTotal").and_then(Value::as_u64)?;
        let cooldown_active = queue.get("cooldownActive").and_then(Value::as_u64)?;
        if maximum == 0
            || depth > maximum
            || !ratio.is_finite()
            || !(0.0..=1.0).contains(&ratio)
            || (ratio - (depth as f64 / maximum as f64)).abs() > 0.001
        {
            return None;
        }

        let pending_threshold = config.pressure_pending_threshold.max(1);
        let cooldown_threshold = config.pressure_cooldown_threshold.max(1);
        let ratio_threshold = (pending_threshold as f64 / maximum as f64).min(1.0);
        let snapshot = QueuePressureSnapshot {
            pending_total,
            cooldown_active,
            pressure_active: ratio >= ratio_threshold
                || pending >= pending_threshold
                || pending_total >= pending_threshold
                || cooldown_active >= cooldown_threshold,
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
            provider = "contextlattice",
            pending_total = snapshot.pending_total,
            cooldown_active = snapshot.cooldown_active,
            "ContextLattice pressure-aware retrieval tuning active"
        );
    }

    fn extract_search_lines(value: &Value, limit: usize) -> Vec<String> {
        search_results(value)
            .iter()
            .take(limit.max(1))
            .filter_map(|item| {
                let summary = item
                    .get("summary")
                    .or_else(|| item.get("content"))
                    .or_else(|| item.get("text"))
                    .and_then(Value::as_str)?;
                let trimmed = summary.trim();
                if trimmed.is_empty() {
                    return None;
                }
                let file = item.get("file").and_then(Value::as_str).unwrap_or("memory");
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
        scope: &ContextLatticeScope,
        query: &str,
        topic_suffix: Option<&str>,
        retrieval_mode: Option<&str>,
        limit: Option<u64>,
    ) -> Result<Value, String> {
        let topic_suffix = topic_suffix.unwrap_or(&config.default_topic_suffix);
        let mut payload = json!({
            "project": scope.project(),
            "query": query,
            "agent_id": scope.agent_id(),
            "topic_path": scope.topic_path(topic_suffix)?,
            "subject_key": scope.opaque_subject_key,
            "include_grounding": config.include_grounding,
            "include_retrieval_debug": config.include_retrieval_debug,
            "retrieval_mode": retrieval_mode.unwrap_or(&config.default_retrieval_mode),
        });
        if let Some(limit) = limit {
            payload["limit"] = json!(limit);
        }
        Ok(payload)
    }

    fn build_subject_wide_search_payload(
        config: &ContextLatticeConfig,
        scope: &ContextLatticeScope,
        query: &str,
        retrieval_mode: Option<&str>,
        limit: Option<u64>,
    ) -> Value {
        let mut payload = json!({
            "project": scope.project(),
            "query": query,
            "agent_id": scope.agent_id(),
            "topic_path": scope.topic_root(),
            "subject_key": scope.opaque_subject_key,
            "include_grounding": config.include_grounding,
            "include_retrieval_debug": config.include_retrieval_debug,
            "retrieval_mode": retrieval_mode.unwrap_or(&config.default_retrieval_mode),
        });
        if let Some(limit) = limit {
            payload["limit"] = json!(limit);
        }
        payload
    }

    fn build_write_payload(
        scope: &ContextLatticeScope,
        content: &str,
        file_name: &str,
        topic_suffix: &str,
    ) -> Result<Value, String> {
        Ok(json!({
            "projectName": scope.project(),
            "fileName": scope.file_name(file_name)?,
            "topicPath": scope.topic_path(topic_suffix)?,
            "agentId": scope.agent_id(),
            "subjectKey": scope.opaque_subject_key,
            "content": content,
        }))
    }

    fn deliver_write(
        config: &ContextLatticeConfig,
        scope: &ContextLatticeScope,
        entry: &DurableContextLatticeWrite,
    ) -> Result<u32, (ContextLatticeRequestFailure, u32)> {
        let mut payload = entry.payload.clone();
        payload["idempotency_key"] = json!(entry.idempotency_key);
        let route = scope.route("/memory/write");
        let mut last_failure = ContextLatticeRequestFailure::Transport;
        for attempt in 1..=CONTEXTLATTICE_MAX_IMMEDIATE_ATTEMPTS {
            match Self::request_json(
                config,
                Method::POST,
                &route,
                &payload,
                Some(&entry.idempotency_key),
            ) {
                Ok(_) => return Ok(attempt),
                Err(failure) => last_failure = failure,
            }
            if attempt < CONTEXTLATTICE_MAX_IMMEDIATE_ATTEMPTS {
                thread::sleep(CONTEXTLATTICE_RETRY_DELAY.saturating_mul(attempt));
            }
        }
        Err((last_failure, CONTEXTLATTICE_MAX_IMMEDIATE_ATTEMPTS))
    }

    fn record_pending_failure(
        path: &Path,
        entry: &mut DurableContextLatticeWrite,
        failure: ContextLatticeRequestFailure,
        attempts: u32,
    ) -> Result<(), String> {
        entry.attempts = entry.attempts.saturating_add(attempts);
        entry.last_failure_code = Some(failure.code().to_string());
        write_private_json(path, entry)
    }

    fn retry_pending_writes(
        config: &ContextLatticeConfig,
        scope: &ContextLatticeScope,
    ) -> Result<u64, String> {
        let mut delivered = 0_u64;
        let mut pending = false;
        for path in pending_outbox_paths(&config.outbox_root, scope)? {
            let mut entry = read_outbox_write(&path)?;
            if entry.subject_storage_key != scope.opaque_subject_key {
                return Err("ContextLattice durable outbox subject mismatch".to_string());
            }
            match Self::deliver_write(config, scope, &entry) {
                Ok(_) => {
                    remove_outbox_write(&path)?;
                    delivered = delivered.saturating_add(1);
                }
                Err((failure, attempts)) => {
                    Self::record_pending_failure(&path, &mut entry, failure, attempts)?;
                    pending = true;
                }
            }
        }
        if pending {
            Err("ContextLattice durable write remains queued for retry".to_string())
        } else {
            Ok(delivered)
        }
    }

    fn submit_durable_write(
        config: &ContextLatticeConfig,
        scope: &ContextLatticeScope,
        payload: Value,
    ) -> Result<WriteDisposition, String> {
        // A prior outage never makes a newer write a reason to discard the old
        // durable entry. Attempt it first, then persist the new entry before any
        // new network request.
        let _ = Self::retry_pending_writes(config, scope);
        let mut entry = DurableContextLatticeWrite::new(scope, payload);
        let path = persist_outbox_write(&config.outbox_root, scope, &entry)?;
        let idempotency_key = entry.idempotency_key.clone();
        match Self::deliver_write(config, scope, &entry) {
            Ok(_) => {
                remove_outbox_write(&path)?;
                Ok(WriteDisposition::Acknowledged { idempotency_key })
            }
            Err((failure, attempts)) => {
                Self::record_pending_failure(&path, &mut entry, failure, attempts)?;
                Ok(WriteDisposition::PendingRetry {
                    idempotency_key,
                    failure_code: failure.code(),
                })
            }
        }
    }

    fn search_subject(
        config: &ContextLatticeConfig,
        scope: &ContextLatticeScope,
        query: &str,
        topic_suffix: Option<&str>,
        retrieval_mode: Option<&str>,
        limit: Option<u64>,
    ) -> Result<Value, ContextLatticeRequestFailure> {
        let payload =
            Self::build_search_payload(config, scope, query, topic_suffix, retrieval_mode, limit)
                .map_err(|_| ContextLatticeRequestFailure::Decode)?;
        Self::request_json(
            config,
            Method::POST,
            &scope.route("/memory/search"),
            &payload,
            None,
        )
    }

    fn search_subject_wide(
        config: &ContextLatticeConfig,
        scope: &ContextLatticeScope,
        query: &str,
        retrieval_mode: Option<&str>,
        limit: Option<u64>,
    ) -> Result<Value, ContextLatticeRequestFailure> {
        let payload =
            Self::build_subject_wide_search_payload(config, scope, query, retrieval_mode, limit);
        Self::request_json(
            config,
            Method::POST,
            &scope.route("/memory/search"),
            &payload,
            None,
        )
    }

    fn remote_readback_count(
        config: &ContextLatticeConfig,
        scope: &ContextLatticeScope,
    ) -> Result<u64, ContextLatticeRequestFailure> {
        let response = Self::search_subject_wide(config, scope, "*", Some("fast"), Some(100))?;
        Ok(search_results(&response).len() as u64)
    }

    fn readback_count(
        config: &ContextLatticeConfig,
        scope: &ContextLatticeScope,
    ) -> Result<u64, ContextLatticeRequestFailure> {
        let remote = Self::remote_readback_count(config, scope)?;
        let local = subject_outbox_residue_count(&config.outbox_root, scope)
            .map_err(|_| ContextLatticeRequestFailure::Decode)?;
        Ok(remote.saturating_add(local))
    }
}

impl MemoryProviderPlugin for ContextLatticeMemoryPlugin {
    fn name(&self) -> &str {
        "contextlattice"
    }

    fn is_available(&self) -> bool {
        std::env::var("HERMES_ENABLE_CONTEXTLATTICE")
            .map(|value| value == "1" || value.eq_ignore_ascii_case("true"))
            .unwrap_or(false)
            || std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL").is_ok()
            || std::env::var("MEMMCP_ORCHESTRATOR_URL").is_ok()
    }

    fn initialize(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        hermes_home: &str,
    ) -> Result<(), String> {
        let scope = Self::scope_for(subject, filter)?;
        let config = ContextLatticeConfig::load(hermes_home)?;
        *self
            .config
            .lock()
            .map_err(|_| "ContextLattice configuration lock unavailable".to_string())? =
            Some(config.clone());
        self.prefetch_results
            .lock()
            .map_err(|_| "ContextLattice prefetch lock unavailable".to_string())?
            .remove(filter.as_str());
        *self
            .pressure_cache
            .lock()
            .map_err(|_| "ContextLattice pressure cache unavailable".to_string())? = None;
        if let Err(error) = Self::retry_pending_writes(&config, &scope) {
            tracing::warn!(
                provider = "contextlattice",
                subject = %filter,
                status = %error,
                "ContextLattice durable outbox remains retryable"
            );
        }
        Ok(())
    }

    fn system_prompt_block(&self) -> String {
        "# ContextLattice Memory\nAuthenticated subject scope is active. Use contextlattice_search for recall, contextlattice_context_pack for broad grounding, and contextlattice_write for durable checkpoints.".to_string()
    }

    fn prefetch(
        &self,
        _subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        _query: &str,
    ) -> String {
        let result = self
            .prefetch_results
            .lock()
            .ok()
            .and_then(|mut results| results.remove(filter.as_str()))
            .unwrap_or_default();
        if result.is_empty() {
            String::new()
        } else {
            format!("## ContextLattice Memory\n{result}")
        }
    }

    fn queue_prefetch(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        query: &str,
    ) {
        let query = query.trim();
        if query.is_empty() {
            return;
        }
        let config = match self.config_snapshot() {
            Ok(config) => config,
            Err(error) => {
                tracing::debug!(provider = "contextlattice", status = %error, "ContextLattice prefetch skipped");
                return;
            }
        };
        let scope = match Self::scope_for(subject, filter) {
            Ok(scope) => scope,
            Err(error) => {
                tracing::warn!(provider = "contextlattice", subject = %filter, status = %error, "ContextLattice prefetch rejected");
                return;
            }
        };
        let results = Arc::clone(&self.prefetch_results);
        let pressure_cache = Arc::clone(&self.pressure_cache);
        let query = query.to_string();
        let filter_key = filter.as_str().to_string();
        thread::spawn(move || {
            let mut payload = match Self::build_search_payload(
                &config,
                &scope,
                &query,
                None,
                Some(if config.pressure_force_fast_prefetch {
                    "fast"
                } else {
                    "balanced"
                }),
                Some(4),
            ) {
                Ok(payload) => payload,
                Err(_) => return,
            };
            let pressure = Self::queue_pressure_snapshot(&config, &pressure_cache);
            Self::apply_pressure_tuning_to_payload(
                &config,
                &mut payload,
                pressure.as_ref(),
                config.pressure_force_fast_prefetch,
            );
            match Self::request_json(
                &config,
                Method::POST,
                &scope.route("/memory/search"),
                &payload,
                None,
            ) {
                Ok(response) => {
                    let lines = Self::extract_search_lines(&response, 5);
                    if !lines.is_empty() {
                        if let Ok(mut cache) = results.lock() {
                            cache.insert(filter_key, lines.join("\n"));
                        }
                    }
                }
                Err(failure) => tracing::debug!(
                    provider = "contextlattice",
                    status = failure.code(),
                    "ContextLattice prefetch unavailable"
                ),
            }
        });
    }

    fn sync_turn(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        user_content: &str,
        assistant_content: &str,
    ) {
        let user = user_content.trim();
        let assistant = assistant_content.trim();
        if user.is_empty() || assistant.is_empty() {
            return;
        }
        let config = match self.config_snapshot() {
            Ok(config) => config,
            Err(error) => {
                tracing::warn!(provider = "contextlattice", subject = %filter, status = %error, "ContextLattice turn write not initialized");
                return;
            }
        };
        let scope = match Self::scope_for(subject, filter) {
            Ok(scope) => scope,
            Err(error) => {
                tracing::warn!(provider = "contextlattice", subject = %filter, status = %error, "ContextLattice turn write rejected");
                return;
            }
        };
        let content = format!(
            "### memory turn\nuser: {}\nassistant: {}",
            user.chars().take(4000).collect::<String>(),
            assistant.chars().take(4000).collect::<String>(),
        );
        let payload = match Self::build_write_payload(
            &scope,
            &content,
            "turns/current.md",
            "memory",
        ) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(provider = "contextlattice", subject = %filter, status = %error, "ContextLattice turn write rejected");
                return;
            }
        };
        match Self::submit_durable_write(&config, &scope, payload) {
            Ok(WriteDisposition::Acknowledged { .. }) => {}
            Ok(WriteDisposition::PendingRetry {
                idempotency_key,
                failure_code,
            }) => tracing::warn!(
                provider = "contextlattice",
                subject = %filter,
                idempotency_key = %idempotency_key,
                status = failure_code,
                "ContextLattice turn write durably queued for retry"
            ),
            Err(error) => tracing::error!(
                provider = "contextlattice",
                subject = %filter,
                status = %error,
                "ContextLattice turn write could not enter durable outbox"
            ),
        }
    }

    fn get_tool_schemas(&self) -> Vec<Value> {
        vec![search_schema(), context_pack_schema(), write_schema()]
    }

    fn handle_tool_call(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        tool_name: &str,
        args: &Value,
    ) -> String {
        let config = match self.config_snapshot() {
            Ok(config) => config,
            Err(error) => return json!({"error": error}).to_string(),
        };
        let scope = match Self::scope_for(subject, filter) {
            Ok(scope) => scope,
            Err(error) => return json!({"error": error}).to_string(),
        };
        match tool_name {
            "contextlattice_search" => {
                let query = args
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                if query.is_empty() {
                    return json!({"error":"query is required"}).to_string();
                }
                let topic_suffix = args.get("topic_path").and_then(Value::as_str);
                let retrieval_mode = args.get("retrieval_mode").and_then(Value::as_str);
                let limit = args.get("limit").and_then(Value::as_u64);
                let mut payload = match Self::build_search_payload(
                    &config,
                    &scope,
                    query,
                    topic_suffix,
                    retrieval_mode,
                    limit,
                ) {
                    Ok(payload) => payload,
                    Err(error) => return json!({"error": error}).to_string(),
                };
                let pressure = Self::queue_pressure_snapshot(&config, &self.pressure_cache);
                if retrieval_mode.is_none() {
                    Self::apply_pressure_tuning_to_payload(
                        &config,
                        &mut payload,
                        pressure.as_ref(),
                        true,
                    );
                }
                match Self::request_json(
                    &config,
                    Method::POST,
                    &scope.route("/memory/search"),
                    &payload,
                    None,
                ) {
                    Ok(response) => response.to_string(),
                    Err(failure) => json!({"error": failure.code()}).to_string(),
                }
            }
            "contextlattice_context_pack" => {
                let query = args
                    .get("query")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                if query.is_empty() {
                    return json!({"error":"query is required"}).to_string();
                }
                let topic_suffix = args.get("topic_path").and_then(Value::as_str);
                let retrieval_mode = args.get("retrieval_mode").and_then(Value::as_str);
                let mut payload = match Self::build_search_payload(
                    &config,
                    &scope,
                    query,
                    topic_suffix,
                    retrieval_mode,
                    None,
                ) {
                    Ok(payload) => payload,
                    Err(error) => return json!({"error": error}).to_string(),
                };
                let pressure = Self::queue_pressure_snapshot(&config, &self.pressure_cache);
                if retrieval_mode.is_none() {
                    Self::apply_pressure_tuning_to_payload(
                        &config,
                        &mut payload,
                        pressure.as_ref(),
                        true,
                    );
                }
                match Self::request_json(
                    &config,
                    Method::POST,
                    &scope.route("/memory/context-pack"),
                    &payload,
                    None,
                ) {
                    Ok(response) => response.to_string(),
                    Err(failure) => json!({"error": failure.code()}).to_string(),
                }
            }
            "contextlattice_write" => {
                let content = args
                    .get("content")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .trim();
                if content.is_empty() {
                    return json!({"error":"content is required"}).to_string();
                }
                let file_name = args
                    .get("file_name")
                    .and_then(Value::as_str)
                    .unwrap_or(&config.default_file_name);
                let topic_suffix = args
                    .get("topic_path")
                    .and_then(Value::as_str)
                    .unwrap_or(&config.default_topic_suffix);
                let payload =
                    match Self::build_write_payload(&scope, content, file_name, topic_suffix) {
                        Ok(payload) => payload,
                        Err(error) => return json!({"error": error}).to_string(),
                    };
                match Self::submit_durable_write(&config, &scope, payload) {
                    Ok(WriteDisposition::Acknowledged { idempotency_key }) => {
                        json!({"result":"acknowledged", "idempotency_key": idempotency_key})
                            .to_string()
                    }
                    Ok(WriteDisposition::PendingRetry {
                        idempotency_key,
                        failure_code,
                    }) => json!({
                        "error":"contextlattice_write_pending_retry",
                        "idempotency_key": idempotency_key,
                        "status": failure_code,
                    })
                    .to_string(),
                    Err(error) => json!({"error": error}).to_string(),
                }
            }
            _ => json!({"error": format!("Unknown tool: {tool_name}")}).to_string(),
        }
    }

    fn export_subject(
        &self,
        _subject: &MemorySubject,
        _filter: &MemorySubjectStorageKey,
        _actor: &MemoryAuditActor,
        _sink: &mut dyn MemoryExportSink,
    ) -> Result<MemoryExportReceipt, MemoryProviderGovernanceError> {
        // The installed ContextLattice search API has no immutable snapshot,
        // revision, or stable cursor contract. A count-only search would be an
        // inventory, not an export, so do not issue a remote request or claim
        // a partial artifact.
        Err(MemoryProviderGovernanceError::unsupported(
            self.name(),
            MemoryGovernanceOperation::Export,
        ))
    }

    fn forget_subject(
        &self,
        _subject: &MemorySubject,
        _filter: &MemorySubjectStorageKey,
        _actor: &MemoryAuditActor,
    ) -> Result<MemoryForgetReceipt, MemoryProviderGovernanceError> {
        // ContextLattice exposes no installed delete capability. Do not use a
        // guessed endpoint or discard a durable outbox entry while remote
        // memory may still exist.
        Err(MemoryProviderGovernanceError::unsupported(
            self.name(),
            MemoryGovernanceOperation::Forget,
        ))
    }

    fn retention_sweep(
        &self,
        _subject: &MemorySubject,
        _filter: &MemorySubjectStorageKey,
        _policy: &MemoryRetentionPolicy,
        _actor: &MemoryAuditActor,
    ) -> Result<MemoryRetentionReceipt, MemoryProviderGovernanceError> {
        // Retention requires the same verified deletion capability as forget.
        Err(MemoryProviderGovernanceError::unsupported(
            self.name(),
            MemoryGovernanceOperation::RetentionSweep,
        ))
    }

    fn verify_subject_absent(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
    ) -> Result<MemoryReadbackReceipt, MemoryProviderGovernanceError> {
        let config = self.config_snapshot().map_err(|_| {
            MemoryProviderGovernanceError::failed(self.name(), MemoryGovernanceOperation::Readback)
        })?;
        let scope = Self::scope_for(subject, filter).map_err(|_| {
            MemoryProviderGovernanceError::failed(self.name(), MemoryGovernanceOperation::Readback)
        })?;
        let residual = Self::readback_count(&config, &scope).map_err(|_| {
            MemoryProviderGovernanceError::failed(self.name(), MemoryGovernanceOperation::Readback)
        })?;
        if residual == 0 {
            Ok(MemoryReadbackReceipt::absent(self.name(), filter.clone()))
        } else {
            Ok(MemoryReadbackReceipt::residual(
                self.name(),
                filter.clone(),
                residual,
            ))
        }
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key":"orchestrator_url", "description":"ContextLattice orchestrator URL", "default": DEFAULT_ORCHESTRATOR_URL, "env_var":"CONTEXTLATTICE_ORCHESTRATOR_URL"},
            {"key":"topic_suffix", "description":"Relative topic suffix appended inside the authenticated subject partition", "default":"memory"},
            {"key":"file_name", "description":"Relative file suffix appended inside the authenticated subject partition", "default":"checkpoints/current.md"},
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
        let object = config
            .as_object()
            .ok_or_else(|| "config must be a JSON object".to_string())?;
        if object.contains_key("project") || object.contains_key("agent_id") {
            return Err(
                "ContextLattice project and agent configuration are prohibited; use the authenticated subject partition"
                    .to_string(),
            );
        }
        if let Some(topic) = object
            .get("topic_suffix")
            .or_else(|| object.get("topic_path"))
            .and_then(Value::as_str)
        {
            relative_scope_suffix(topic, "memory")?;
        }
        if let Some(file_name) = object.get("file_name").and_then(Value::as_str) {
            relative_scope_suffix(file_name, "checkpoints/current.md")?;
        }
        Ok(())
    }
}

/// Submit a compaction checkpoint through the same subject-scoped durable
/// outbox used by normal ContextLattice turn and tool writes.
pub(crate) fn write_subject_scoped_checkpoint(
    subject: &MemorySubject,
    filter: &MemorySubjectStorageKey,
    hermes_home: &str,
    topic_suffix: &str,
    file_name: &str,
    content: &str,
) -> Result<(), String> {
    if content.trim().is_empty() {
        return Err("ContextLattice checkpoint content is required".to_string());
    }
    let config = ContextLatticeConfig::load(hermes_home)?;
    let scope = ContextLatticeMemoryPlugin::scope_for(subject, filter)?;
    let payload =
        ContextLatticeMemoryPlugin::build_write_payload(&scope, content, file_name, topic_suffix)?;
    match ContextLatticeMemoryPlugin::submit_durable_write(&config, &scope, payload)? {
        WriteDisposition::Acknowledged { .. } => Ok(()),
        WriteDisposition::PendingRetry { .. } => {
            Err("ContextLattice checkpoint is durably queued for retry".to_string())
        }
    }
}

fn search_results(value: &Value) -> Vec<Value> {
    value
        .get("results")
        .and_then(Value::as_array)
        .cloned()
        .or_else(|| {
            value
                .get("context_pack")
                .and_then(|pack| pack.get("results"))
                .and_then(Value::as_array)
                .cloned()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::{
        MemoryExportArtifact, MemoryExportCompletion, MemoryExportPage, MemoryExportSink,
        MemoryExportSinkError, MemoryExportSnapshot, MemoryPrincipalId, MemorySubjectDerivationKey,
        MemorySurface, MemoryTenantId, MemoryVisibility,
    };
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::time::Duration as StdDuration;

    #[derive(Clone, Copy)]
    struct MockResponse {
        status: u16,
        body: &'static str,
    }

    /// Governance must reject ContextLattice export before it mutates or
    /// starts a sink transaction, because the installed API cannot prove a
    /// stable complete export.
    struct UnusedExportSink;

    impl MemoryExportSink for UnusedExportSink {
        fn begin_provider(
            &mut self,
            _snapshot: MemoryExportSnapshot,
        ) -> Result<(), MemoryExportSinkError> {
            panic!("unsupported ContextLattice export must not start a sink")
        }

        fn append_page(&mut self, _page: MemoryExportPage) -> Result<(), MemoryExportSinkError> {
            panic!("unsupported ContextLattice export must not append a page")
        }

        fn finish_provider(
            &mut self,
            _completion: MemoryExportCompletion,
        ) -> Result<(), MemoryExportSinkError> {
            panic!("unsupported ContextLattice export must not finish a sink")
        }

        fn abort(&mut self) {
            panic!("unsupported ContextLattice export must not abort a sink")
        }

        fn commit(self: Box<Self>) -> Result<MemoryExportArtifact, MemoryExportSinkError> {
            panic!("unsupported ContextLattice export must not commit a sink")
        }
    }

    fn subject_and_filter(
        principal: &str,
        session: &str,
    ) -> (MemorySubject, MemorySubjectStorageKey) {
        let subject = MemorySubject::new(
            MemoryTenantId::new("tenant-test").expect("tenant"),
            MemoryPrincipalId::new(principal).expect("principal"),
            None,
            hermes_core::MemorySessionId::new(session).expect("session"),
            MemorySurface::Gateway,
            MemoryVisibility::Private,
        )
        .expect("subject");
        let derivation_key = MemorySubjectDerivationKey::from_bytes([41; 32]);
        let filter = subject.storage_key(&derivation_key);
        (subject, filter)
    }

    fn actor_for(subject: &MemorySubject) -> MemoryAuditActor {
        MemorySubjectDerivationKey::from_bytes([42; 32]).actor_for_subject(subject)
    }

    fn test_config(base_url: String, root: &Path) -> ContextLatticeConfig {
        ContextLatticeConfig {
            orchestrator_url: base_url,
            timeout_secs: 1.0,
            include_grounding: true,
            include_retrieval_debug: true,
            default_retrieval_mode: "balanced".to_string(),
            default_topic_suffix: "memory".to_string(),
            default_file_name: "checkpoints/current.md".to_string(),
            pressure_aware: false,
            pressure_pending_threshold: 12,
            pressure_cooldown_threshold: 3,
            pressure_status_ttl_secs: 2.5,
            pressure_fast_sources: vec!["topic_rollups".to_string()],
            pressure_force_fast_mode: true,
            pressure_force_fast_prefetch: true,
            api_key: None,
            outbox_root: root.to_path_buf(),
        }
    }

    fn install_config(plugin: &ContextLatticeMemoryPlugin, config: ContextLatticeConfig) {
        *plugin.config.lock().expect("config lock") = Some(config);
    }

    fn scripted_server(responses: Vec<MockResponse>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
        let address = listener.local_addr().expect("mock address");
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            for response in responses {
                let (mut stream, _) = listener.accept().expect("accept mock request");
                stream
                    .set_read_timeout(Some(StdDuration::from_secs(2)))
                    .expect("read timeout");
                let request = read_http_request(&mut stream);
                sender.send(request).expect("send mock request");
                let reason = if response.status == 200 {
                    "OK"
                } else {
                    "ERROR"
                };
                let reply = format!(
                    "HTTP/1.1 {} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                    response.status,
                    reason,
                    response.body.len(),
                    response.body,
                );
                stream
                    .write_all(reply.as_bytes())
                    .expect("write mock response");
            }
        });
        (format!("http://{address}"), receiver)
    }

    fn read_http_request(stream: &mut TcpStream) -> String {
        let mut bytes = Vec::new();
        let mut buffer = [0_u8; 2048];
        loop {
            let read = stream.read(&mut buffer).expect("read request");
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..read]);
            let Some(header_end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") else {
                continue;
            };
            let headers = String::from_utf8_lossy(&bytes[..header_end]);
            let content_length = headers
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if bytes.len() >= header_end + 4 + content_length {
                break;
            }
        }
        String::from_utf8(bytes).expect("utf8 request")
    }

    fn receive_requests(receiver: &mpsc::Receiver<String>, count: usize) -> Vec<String> {
        (0..count)
            .map(|_| {
                receiver
                    .recv_timeout(StdDuration::from_secs(3))
                    .expect("expected mock request")
            })
            .collect()
    }

    fn request_target(request: &str) -> &str {
        request
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .expect("request target")
    }

    fn request_body(request: &str) -> Value {
        let (_, body) = request.split_once("\r\n\r\n").expect("request body");
        serde_json::from_str(body).expect("json body")
    }

    fn request_header<'a>(request: &'a str, name: &str) -> Option<&'a str> {
        request.lines().find_map(|line| {
            let (header, value) = line.split_once(':')?;
            header.eq_ignore_ascii_case(name).then_some(value.trim())
        })
    }

    #[test]
    fn search_payloads_and_routes_are_partitioned_per_subject() {
        let (base_url, requests) = scripted_server(vec![
            MockResponse {
                status: 200,
                body: r#"{"results":[]}"#,
            },
            MockResponse {
                status: 200,
                body: r#"{"results":[]}"#,
            },
        ]);
        let home = tempfile::tempdir().expect("home");
        let plugin = ContextLatticeMemoryPlugin::new();
        install_config(&plugin, test_config(base_url, home.path()));
        let (alice, alice_filter) = subject_and_filter("alice", "s1");
        let (bob, bob_filter) = subject_and_filter("bob", "s1");

        assert!(plugin
            .handle_tool_call(
                &alice,
                &alice_filter,
                "contextlattice_search",
                &json!({
                    "query":"status",
                    "topic_path":"ops",
                    "project":"session-default",
                    "agent_id":"hermes-user",
                    "subject_key":bob_filter.as_str()
                }),
            )
            .contains("results"));
        assert!(plugin
            .handle_tool_call(
                &bob,
                &bob_filter,
                "contextlattice_search",
                &json!({"query":"status", "topic_path":"ops"}),
            )
            .contains("results"));

        let captured = receive_requests(&requests, 2);
        let alice_request = &captured[0];
        let bob_request = &captured[1];
        assert_ne!(request_target(alice_request), request_target(bob_request));
        assert!(request_target(alice_request).contains(alice_filter.as_str()));
        assert!(request_target(bob_request).contains(bob_filter.as_str()));
        let alice_payload = request_body(alice_request);
        let bob_payload = request_body(bob_request);
        for (payload, filter, other) in [
            (&alice_payload, &alice_filter, &bob_filter),
            (&bob_payload, &bob_filter, &alice_filter),
        ] {
            assert_eq!(payload["subject_key"], filter.as_str());
            assert!(payload["project"]
                .as_str()
                .unwrap()
                .starts_with(filter.as_str()));
            assert!(payload["topic_path"]
                .as_str()
                .unwrap()
                .starts_with(filter.as_str()));
            assert!(payload["agent_id"]
                .as_str()
                .unwrap()
                .starts_with(filter.as_str()));
            assert!(!payload.to_string().contains(other.as_str()));
            assert!(!payload.to_string().contains("session-default"));
            assert!(!payload.to_string().contains("hermes-user"));
        }
    }

    #[test]
    fn unsupported_governance_never_issues_remote_or_local_mutation() {
        let (base_url, requests) = scripted_server(vec![MockResponse {
            status: 200,
            body: r#"{"unexpected":true}"#,
        }]);
        let home = tempfile::tempdir().expect("home");
        let plugin = ContextLatticeMemoryPlugin::new();
        install_config(&plugin, test_config(base_url, home.path()));
        let (subject, filter) = subject_and_filter("alice", "s1");
        let scope = ContextLatticeMemoryPlugin::scope_for(&subject, &filter).expect("scope");
        let payload = ContextLatticeMemoryPlugin::build_write_payload(
            &scope,
            "queued private memory",
            "checkpoints/current.md",
            "memory",
        )
        .expect("payload");
        let entry = DurableContextLatticeWrite::new(&scope, payload);
        let path = persist_outbox_write(home.path(), &scope, &entry).expect("persist outbox");
        let policy = MemoryRetentionPolicy::new(Some(30), Some(100)).expect("policy");
        let mut sink = UnusedExportSink;

        assert!(matches!(
            plugin.export_subject(&subject, &filter, &actor_for(&subject), &mut sink),
            Err(MemoryProviderGovernanceError::Unsupported {
                operation: MemoryGovernanceOperation::Export,
                ..
            })
        ));
        assert!(matches!(
            plugin.forget_subject(&subject, &filter, &actor_for(&subject)),
            Err(MemoryProviderGovernanceError::Unsupported {
                operation: MemoryGovernanceOperation::Forget,
                ..
            })
        ));
        assert!(matches!(
            plugin.retention_sweep(&subject, &filter, &policy, &actor_for(&subject)),
            Err(MemoryProviderGovernanceError::Unsupported {
                operation: MemoryGovernanceOperation::RetentionSweep,
                ..
            })
        ));
        assert!(path.exists());
        assert_eq!(
            subject_outbox_residue_count(home.path(), &scope).expect("outbox residue"),
            1
        );
        assert!(requests
            .recv_timeout(StdDuration::from_millis(100))
            .is_err());
    }

    #[test]
    fn failed_write_is_durable_and_retries_with_one_idempotency_key() {
        let (base_url, requests) = scripted_server(vec![
            MockResponse {
                status: 500,
                body: r#"{"error":"down"}"#,
            },
            MockResponse {
                status: 500,
                body: r#"{"error":"down"}"#,
            },
            MockResponse {
                status: 500,
                body: r#"{"error":"down"}"#,
            },
            MockResponse {
                status: 200,
                body: r#"{"ok":true}"#,
            },
        ]);
        let home = tempfile::tempdir().expect("home");
        let config = test_config(base_url, home.path());
        let plugin = ContextLatticeMemoryPlugin::new();
        install_config(&plugin, config.clone());
        let (subject, filter) = subject_and_filter("alice", "s1");
        let scope = ContextLatticeMemoryPlugin::scope_for(&subject, &filter).expect("scope");

        let response = plugin.handle_tool_call(
            &subject,
            &filter,
            "contextlattice_write",
            &json!({"content":"durable note"}),
        );
        assert!(response.contains("contextlattice_write_pending_retry"));
        let pending = pending_outbox_paths(home.path(), &scope).expect("pending entries");
        assert_eq!(pending.len(), 1);
        assert!(read_outbox_write(&pending[0])
            .expect("entry")
            .last_failure_code
            .as_deref()
            .is_some_and(|code| code.starts_with("contextlattice_")));

        assert_eq!(
            ContextLatticeMemoryPlugin::retry_pending_writes(&config, &scope)
                .expect("retry succeeds"),
            1
        );
        assert!(pending_outbox_paths(home.path(), &scope)
            .expect("pending after retry")
            .is_empty());
        let captured = receive_requests(&requests, 4);
        let keys = captured
            .iter()
            .map(|request| request_header(request, "idempotency-key").expect("idempotency key"))
            .collect::<Vec<_>>();
        assert!(keys.windows(2).all(|pair| pair[0] == pair[1]));
    }

    #[test]
    fn queued_write_is_readback_residue_and_forget_preserves_outbox() {
        let (base_url, requests) = scripted_server(vec![
            MockResponse {
                status: 500,
                body: r#"{"error":"down"}"#,
            },
            MockResponse {
                status: 500,
                body: r#"{"error":"down"}"#,
            },
            MockResponse {
                status: 500,
                body: r#"{"error":"down"}"#,
            },
            MockResponse {
                status: 200,
                body: r#"{"results":[]}"#,
            },
        ]);
        let home = tempfile::tempdir().expect("home");
        let plugin = ContextLatticeMemoryPlugin::new();
        install_config(&plugin, test_config(base_url, home.path()));
        let (subject, filter) = subject_and_filter("alice", "s1");
        let scope = ContextLatticeMemoryPlugin::scope_for(&subject, &filter).expect("scope");

        assert!(plugin
            .handle_tool_call(
                &subject,
                &filter,
                "contextlattice_write",
                &json!({"content":"queued private memory"}),
            )
            .contains("pending_retry"));
        assert_eq!(
            plugin
                .verify_subject_absent(&subject, &filter)
                .expect("readback receipt")
                .residual_records,
            1
        );
        assert!(matches!(
            plugin.forget_subject(&subject, &filter, &actor_for(&subject)),
            Err(MemoryProviderGovernanceError::Unsupported {
                operation: MemoryGovernanceOperation::Forget,
                ..
            })
        ));
        assert_eq!(
            subject_outbox_residue_count(home.path(), &scope).expect("outbox residue"),
            1
        );
        let captured = receive_requests(&requests, 4);
        assert!(captured
            .iter()
            .all(|request| request_target(request).contains(filter.as_str())));
    }

    #[test]
    fn canonical_key_is_the_only_accepted_remote_prefix() {
        for invalid in [
            "",
            "default",
            "hermes-user",
            "hermes",
            "session-default",
            "msp1_bad",
        ] {
            assert!(serde_json::from_value::<MemorySubjectStorageKey>(json!(invalid)).is_err());
        }
        let (_, filter) = subject_and_filter("alice", "s1");
        let scope = ContextLatticeScope::new(&filter).expect("canonical scope");
        assert!(scope.project().starts_with(filter.as_str()));
        assert!(scope
            .topic_path("ops")
            .unwrap()
            .starts_with(filter.as_str()));
        assert!(scope.agent_id().starts_with(filter.as_str()));
        assert!(scope
            .file_name("notes.md")
            .unwrap()
            .starts_with(filter.as_str()));
        assert!(relative_scope_suffix("../../other", "memory").is_err());
    }

    #[test]
    fn transport_errors_redact_server_content_and_api_key() {
        let secret_api_key = "api-secret-do-not-log";
        let secret_server_body = "server-secret-do-not-log";
        let (base_url, requests) = scripted_server(vec![
            MockResponse {
                status: 500,
                body: secret_server_body,
            },
            MockResponse {
                status: 500,
                body: secret_server_body,
            },
            MockResponse {
                status: 500,
                body: secret_server_body,
            },
        ]);
        let home = tempfile::tempdir().expect("home");
        let mut config = test_config(base_url, home.path());
        config.api_key = Some(secret_api_key.to_string());
        let plugin = ContextLatticeMemoryPlugin::new();
        install_config(&plugin, config);
        let (subject, filter) = subject_and_filter("alice", "s1");

        let response = plugin.handle_tool_call(
            &subject,
            &filter,
            "contextlattice_write",
            &json!({"content":"private checkpoint"}),
        );
        assert!(!response.contains(secret_api_key));
        assert!(!response.contains(secret_server_body));
        assert!(response.contains("contextlattice_http_failure"));
        let captured = receive_requests(&requests, 3);
        assert!(captured
            .iter()
            .all(|request| request_header(request, "authorization").is_some()));
    }

    #[test]
    fn compaction_checkpoint_uses_the_shared_subject_outbox() {
        let (base_url, requests) = scripted_server(vec![MockResponse {
            status: 200,
            body: r#"{"ok":true}"#,
        }]);
        let home = tempfile::tempdir().expect("home");
        fs::write(
            home.path().join("contextlattice.json"),
            json!({"orchestrator_url": base_url}).to_string(),
        )
        .expect("write config");
        let (subject, filter) = subject_and_filter("alice", "s1");

        write_subject_scoped_checkpoint(
            &subject,
            &filter,
            home.path().to_str().expect("utf8 home"),
            "compaction",
            "checkpoints/compaction.md",
            "compaction metadata only",
        )
        .expect("checkpoint acknowledged");
        let scope = ContextLatticeScope::new(&filter).expect("scope");
        assert!(pending_outbox_paths(home.path(), &scope)
            .expect("outbox paths")
            .is_empty());
        let request = receive_requests(&requests, 1).pop().expect("request");
        let payload = request_body(&request);
        assert!(request_target(&request).contains(filter.as_str()));
        assert!(payload["projectName"]
            .as_str()
            .unwrap()
            .starts_with(filter.as_str()));
        assert!(payload["topicPath"]
            .as_str()
            .unwrap()
            .starts_with(filter.as_str()));
        assert!(payload["agentId"]
            .as_str()
            .unwrap()
            .starts_with(filter.as_str()));
        assert!(!payload.to_string().contains("alice"));
        assert!(!payload.to_string().contains("s1"));
    }

    #[test]
    fn unscoped_project_or_agent_config_is_rejected() {
        let home = tempfile::tempdir().expect("home");
        fs::write(
            home.path().join("contextlattice.json"),
            r#"{"project":"shared-project","agent_id":"shared-agent"}"#,
        )
        .expect("write config");
        assert!(ContextLatticeConfig::load(home.path().to_str().expect("utf8 home")).is_err());
    }

    #[test]
    fn extract_search_lines_prefers_summary() {
        let value = json!({
            "results": [
                {"file":"a.md", "summary":"alpha"},
                {"file":"b.md", "content":"beta"}
            ]
        });
        let lines = ContextLatticeMemoryPlugin::extract_search_lines(&value, 10);
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("alpha"));
        assert!(lines[1].contains("beta"));
    }

    #[test]
    fn pressure_status_uses_live_queue_shape_without_subject_query() {
        let (base_url, requests) = scripted_server(vec![
            MockResponse {
                status: 200,
                body: r#"{"queue":{"memoryWriteQueueDepth":1,"memoryWriteQueueMax":8,"memoryWriteQueueRatio":0.125,"pending":1,"pendingTotal":1,"retrying":0,"cooldownActive":0,"oldestAgeSecs":50.873,"syncLane":{}}}"#,
            },
            MockResponse {
                status: 200,
                body: r#"{"results":[]}"#,
            },
        ]);
        let home = tempfile::tempdir().expect("home");
        let mut config = test_config(base_url, home.path());
        config.pressure_aware = true;
        config.pressure_pending_threshold = 1;
        let plugin = ContextLatticeMemoryPlugin::new();
        install_config(&plugin, config);
        let (subject, filter) = subject_and_filter("alice", "s1");

        let response = plugin.handle_tool_call(
            &subject,
            &filter,
            "contextlattice_search",
            &json!({"query":"status"}),
        );
        assert!(response.contains("results"));
        let captured = receive_requests(&requests, 2);
        assert_eq!(request_target(&captured[0]), "/status");
        assert!(!request_target(&captured[0]).contains(filter.as_str()));
        assert!(request_target(&captured[1]).contains(filter.as_str()));
        let search_payload = request_body(&captured[1]);
        assert_eq!(search_payload["retrieval_mode"], "fast");
        assert_eq!(search_payload["sources"], json!(["topic_rollups"]));
    }

    #[test]
    fn malformed_status_does_not_apply_pressure_tuning() {
        let (base_url, requests) = scripted_server(vec![
            MockResponse {
                status: 200,
                body: r#"{"queue":{"memoryWriteQueueDepth":1,"memoryWriteQueueMax":8,"pending":1,"pendingTotal":1,"cooldownActive":0}}"#,
            },
            MockResponse {
                status: 200,
                body: r#"{"results":[]}"#,
            },
        ]);
        let home = tempfile::tempdir().expect("home");
        let mut config = test_config(base_url, home.path());
        config.pressure_aware = true;
        config.pressure_pending_threshold = 1;
        let plugin = ContextLatticeMemoryPlugin::new();
        install_config(&plugin, config);
        let (subject, filter) = subject_and_filter("alice", "s1");

        assert!(plugin
            .handle_tool_call(
                &subject,
                &filter,
                "contextlattice_search",
                &json!({"query":"status"}),
            )
            .contains("results"));
        let captured = receive_requests(&requests, 2);
        assert_eq!(request_target(&captured[0]), "/status");
        let search_payload = request_body(&captured[1]);
        assert_eq!(search_payload["retrieval_mode"], "balanced");
        assert!(search_payload.get("sources").is_none());
    }

    #[test]
    fn pressure_tuning_preserves_subject_scoped_payload() {
        let home = tempfile::tempdir().expect("home");
        let (_, filter) = subject_and_filter("alice", "s1");
        let scope = ContextLatticeScope::new(&filter).expect("scope");
        let config = test_config(DEFAULT_ORCHESTRATOR_URL.to_string(), home.path());
        let mut payload = ContextLatticeMemoryPlugin::build_search_payload(
            &config,
            &scope,
            "hello",
            None,
            Some("balanced"),
            Some(5),
        )
        .expect("payload");
        let pressure = QueuePressureSnapshot {
            pending_total: 42,
            cooldown_active: 6,
            pressure_active: true,
            captured_at: Instant::now(),
        };
        ContextLatticeMemoryPlugin::apply_pressure_tuning_to_payload(
            &config,
            &mut payload,
            Some(&pressure),
            true,
        );
        assert_eq!(payload["retrieval_mode"], "fast");
        assert_eq!(payload["sources"], json!(["topic_rollups"]));
        assert!(payload["project"]
            .as_str()
            .unwrap()
            .starts_with(filter.as_str()));
    }
}
