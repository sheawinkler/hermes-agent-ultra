//! Mem0 memory provider plugin with authenticated subject partitioning.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use hermes_core::{
    MemoryAuditActor, MemoryExportCompletion, MemoryExportEvidenceDigest, MemoryExportPage,
    MemoryExportReceipt, MemoryExportSink, MemoryExportSnapshot, MemoryForgetReceipt,
    MemoryGovernanceOperation, MemoryProviderGovernanceError, MemoryReadbackReceipt,
    MemoryRetentionPolicy, MemoryRetentionReceipt, MemorySubject, MemorySubjectStorageKey,
    MemoryVisibility,
};
use hermes_intelligence::redact_sensitive_text;
use parking_lot::Mutex;
use reqwest::Method;
use serde_json::{json, Value};

use crate::memory_manager::MemoryProviderPlugin;
use crate::memory_plugins::config_io;

const BREAKER_THRESHOLD: u32 = 5;
const BREAKER_COOLDOWN_SECS: u64 = 120;
const MEM0_CLOUD_BASE_URL: &str = "https://api.mem0.ai/v1";
const MAX_REMOTE_RECORDS: usize = 200;
const EXPORT_PAGE_SIZE: u64 = 100;

enum Mem0ExportError {
    Unsupported,
    Failed(&'static str),
}

struct Mem0StablePage {
    snapshot: String,
    revision: String,
    next_cursor: Option<String>,
    records: Vec<Value>,
}

fn list_schema() -> Value {
    json!({
        "name": "mem0_list",
        "description": "List only memories in the authenticated subject partition.",
        "parameters": {"type":"object","properties":{"page":{"type":"integer"},"page_size":{"type":"integer"}},"required":[]}
    })
}

fn search_schema() -> Value {
    json!({
        "name": "mem0_search",
        "description": "Search only memories in the authenticated subject partition.",
        "parameters": {"type":"object","properties":{"query":{"type":"string"},"rerank":{"type":"boolean"},"top_k":{"type":"integer"}},"required":["query"]}
    })
}

fn add_schema() -> Value {
    json!({
        "name": "mem0_add",
        "description": "Store a fact only in the authenticated subject partition.",
        "parameters": {"type":"object","properties":{"content":{"type":"string"}},"required":["content"]}
    })
}

fn update_schema() -> Value {
    json!({
        "name": "mem0_update",
        "description": "Update an exact memory only in the authenticated subject partition.",
        "parameters": {"type":"object","properties":{"memory_id":{"type":"string"},"text":{"type":"string"}},"required":["memory_id","text"]}
    })
}

fn delete_schema() -> Value {
    json!({
        "name": "mem0_delete",
        "description": "Delete an exact memory only in the authenticated subject partition.",
        "parameters": {"type":"object","properties":{"memory_id":{"type":"string"}},"required":["memory_id"]}
    })
}

#[derive(Debug, Clone)]
struct Mem0Config {
    mode: String,
    api_key: String,
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
                if env_host.trim().is_empty() { "platform".into() } else { "oss".into() }
            }),
            api_key: std::env::var("MEM0_API_KEY").unwrap_or_default(),
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
        if let Ok(content) = std::fs::read_to_string(Self::config_path(hermes_home)) {
            if let Ok(raw) = serde_json::from_str::<Value>(&content) {
                if let Some(api_key) = raw.get("api_key").and_then(Value::as_str).filter(|key| !key.trim().is_empty()) {
                    config.api_key = api_key.to_string();
                }
                if let Some(mode) = raw.get("mode").and_then(Value::as_str).filter(|mode| !mode.trim().is_empty()) {
                    config.mode = mode.to_string();
                }
                if let Some(base_url) = raw
                    .get("base_url")
                    .or_else(|| raw.get("host"))
                    .and_then(Value::as_str)
                    .filter(|url| !url.trim().is_empty())
                {
                    config.base_url = base_url.to_string();
                }
                if let Some(timeout) = raw
                    .get("api_timeout_secs")
                    .or_else(|| raw.get("timeout"))
                    .and_then(Value::as_f64)
                {
                    config.api_timeout_secs = timeout.clamp(1.0, 60.0);
                }
                if let Some(rerank) = raw.get("rerank").and_then(Value::as_bool) {
                    config.rerank = rerank;
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

fn mem0_mode_label(config: &Mem0Config) -> String {
    if config.mode == "oss" {
        "OSS/self-hosted".to_string()
    } else if !config.base_url.eq_ignore_ascii_case(MEM0_CLOUD_BASE_URL) {
        format!("self-hosted HTTP at {}", config.base_url)
    } else {
        "platform".to_string()
    }
}

fn visibility_label(visibility: MemoryVisibility) -> &'static str {
    match visibility {
        MemoryVisibility::Private => "private",
        MemoryVisibility::Team => "team",
    }
}

fn subject_filters(subject: &MemorySubject, filter: &MemorySubjectStorageKey) -> Value {
    let key = filter.as_str();
    json!({
        "subject_key": key,
        "user_id": key,
        "tenant_id": key,
        "principal_id": key,
        "team_id": subject.team_id().map(|_| key),
        "session_id": key,
        "surface": subject.surface().as_str(),
        "visibility": visibility_label(subject.visibility()),
    })
}

fn scoped_body(
    subject: &MemorySubject,
    filter: &MemorySubjectStorageKey,
    mut body: Value,
) -> Result<Value, String> {
    let map = body
        .as_object_mut()
        .ok_or_else(|| "Mem0 request body must be an object".to_string())?;
    let key = filter.as_str();
    let scope = subject_filters(subject, filter);
    map.insert("filters".to_string(), scope.clone());
    map.insert("subject".to_string(), scope);
    map.insert("subject_key".to_string(), json!(key));
    map.insert("user_id".to_string(), json!(key));
    map.insert("tenant_id".to_string(), json!(key));
    map.insert("principal_id".to_string(), json!(key));
    map.insert("team_id".to_string(), subject.team_id().map(|_| json!(key)).unwrap_or(Value::Null));
    map.insert("session_id".to_string(), json!(key));
    Ok(body)
}

fn scope_query(subject: &MemorySubject, filter: &MemorySubjectStorageKey) -> Vec<(&'static str, String)> {
    let key = filter.as_str().to_string();
    vec![
        ("subject_key", key.clone()),
        ("user_id", key.clone()),
        ("tenant_id", key.clone()),
        ("principal_id", key.clone()),
        ("team_id", subject.team_id().map(|_| key.clone()).unwrap_or_default()),
        ("session_id", key.clone()),
        ("surface", subject.surface().as_str().to_string()),
        ("visibility", visibility_label(subject.visibility()).to_string()),
    ]
}

fn reject_subject_override(args: &Value) -> Result<(), String> {
    const FORBIDDEN: &[&str] = &[
        "user_id", "userId", "agent_id", "agentId", "tenant_id", "tenantId",
        "principal_id", "principalId", "team_id", "teamId", "session_id", "sessionId",
        "subject_key", "subjectKey", "runtime_user_id", "runtimeUserId",
    ];
    if FORBIDDEN.iter().any(|key| args.get(*key).is_some()) {
        return Err("authenticated memory subject cannot be overridden by tool arguments".into());
    }
    Ok(())
}

fn validate_memory_id(raw: &str) -> Result<String, String> {
    let id = raw.trim();
    if id.is_empty() {
        return Err("memory_id is required".into());
    }
    if id.contains('/') || id.contains('?') || id.contains('#') {
        return Err("memory_id must be an exact Mem0 memory id".into());
    }
    Ok(id.to_string())
}

fn record_id(record: &Value) -> Option<&str> {
    record
        .get("id")
        .or_else(|| record.get("memory_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
}

fn extract_memory_text(record: &Value) -> Option<String> {
    record
        .get("memory")
        .or_else(|| record.get("content"))
        .or_else(|| record.pointer("/message/content"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(ToString::to_string)
}

fn unwrap_results(response: Value) -> Vec<Value> {
    if let Some(records) = response.as_array() {
        return records.clone();
    }
    for key in ["results", "memories", "items", "data"] {
        if let Some(records) = response.get(key).and_then(Value::as_array) {
            return records.clone();
        }
    }
    Vec::new()
}

fn retention_candidates(records: &[Value], policy: &MemoryRetentionPolicy) -> Vec<String> {
    let mut candidates = HashSet::new();
    if let Some(max_records) = policy.max_records {
        for record in records.iter().skip(max_records as usize) {
            if let Some(id) = record_id(record) {
                candidates.insert(id.to_string());
            }
        }
    }
    if let Some(max_age_days) = policy.max_age_days {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(max_age_days as i64);
        for record in records {
            let created = record
                .get("created_at")
                .or_else(|| record.get("createdAt"))
                .and_then(Value::as_str)
                .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
                .map(|value| value.with_timezone(&chrono::Utc));
            if created.is_some_and(|created| created < cutoff) {
                if let Some(id) = record_id(record) {
                    candidates.insert(id.to_string());
                }
            }
        }
    }
    candidates.into_iter().collect()
}

pub struct Mem0MemoryPlugin {
    config: Mutex<Option<Mem0Config>>,
    bound_subject: Mutex<Option<MemorySubject>>,
    bound_filter: Mutex<Option<MemorySubjectStorageKey>>,
    prefetch_result: Arc<Mutex<String>>,
    consecutive_failures: Arc<AtomicU32>,
    breaker_open_until: Arc<Mutex<Option<Instant>>>,
}

impl Mem0MemoryPlugin {
    pub fn new() -> Self {
        Self {
            config: Mutex::new(None),
            bound_subject: Mutex::new(None),
            bound_filter: Mutex::new(None),
            prefetch_result: Arc::new(Mutex::new(String::new())),
            consecutive_failures: Arc::new(AtomicU32::new(0)),
            breaker_open_until: Arc::new(Mutex::new(None)),
        }
    }

    fn config_snapshot(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
    ) -> Result<Mem0Config, String> {
        if self.bound_subject.lock().as_ref() != Some(subject)
            || self.bound_filter.lock().as_ref() != Some(filter)
        {
            return Err("Mem0 request subject does not match initialized subject".into());
        }
        self.config
            .lock()
            .clone()
            .ok_or_else(|| "Mem0 plugin is not initialized".to_string())
    }

    fn client(config: &Mem0Config) -> Result<reqwest::blocking::Client, String> {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs_f64(config.api_timeout_secs))
            .build()
            .map_err(|error| format!("Mem0 HTTP client build failed: {error}"))
    }

    fn send_json(
        config: &Mem0Config,
        method: Method,
        path: &str,
        body: Option<&Value>,
        query: Option<&[(&str, String)]>,
    ) -> Result<Value, String> {
        let url = format!("{}/{}", config.base_url.trim_end_matches('/'), path.trim_start_matches('/'));
        let mut request = Self::client(config)?
            .request(method.clone(), &url)
            .header("Content-Type", "application/json");
        if !config.api_key.trim().is_empty() {
            request = request
                .header("Authorization", format!("Bearer {}", config.api_key))
                .header("X-API-Key", &config.api_key);
        }
        if let Some(query) = query {
            request = request.query(query);
        }
        if let Some(body) = body {
            request = request.json(body);
        }
        let response = request
            .send()
            .map_err(|error| format!("Mem0 request {method} {url} failed: {error}"))?;
        let status = response.status();
        let text = response.text().unwrap_or_default();
        if !status.is_success() {
            return Err(format!("Mem0 request {method} {url} returned {status}: {text}"));
        }
        if text.trim().is_empty() {
            return Ok(json!({}));
        }
        serde_json::from_str(&text).map_err(|error| format!("Mem0 response parse error: {error}"))
    }

    fn search_memories(
        config: &Mem0Config,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        query: &str,
        rerank: bool,
        top_k: u64,
    ) -> Result<Vec<Value>, String> {
        let payload = scoped_body(
            subject,
            filter,
            json!({"query": query, "rerank": rerank, "top_k": top_k.clamp(1, 50)}),
        )?;
        let mut last_error = String::new();
        for path in ["memories/search", "memory/search"] {
            match Self::send_json(config, Method::POST, path, Some(&payload), None) {
                Ok(value) => return Ok(unwrap_results(value)),
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }

    fn list_memories(
        config: &Mem0Config,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        page: u64,
        page_size: u64,
    ) -> Result<Vec<Value>, String> {
        let mut query = scope_query(subject, filter);
        query.push(("page", page.max(1).to_string()));
        query.push(("page_size", page_size.clamp(1, MAX_REMOTE_RECORDS as u64).to_string()));
        let mut last_error = String::new();
        for path in ["memories", "memory"] {
            match Self::send_json(config, Method::GET, path, None, Some(&query)) {
                Ok(value) => return Ok(unwrap_results(value)),
                Err(error) => last_error = error,
            }
        }
        Self::search_memories(config, subject, filter, "*", config.rerank, MAX_REMOTE_RECORDS as u64)
            .map_err(|fallback| format!("{last_error}; fallback search failed: {fallback}"))
    }

    fn export_marker(response: &Value, keys: &[&str]) -> Option<String> {
        [
            Some(response),
            response.get("meta"),
            response.get("metadata"),
            response.get("pagination"),
            response.get("data"),
        ]
        .into_iter()
        .flatten()
        .find_map(|scope| {
            keys.iter().find_map(|key| {
                let value = scope.get(*key)?;
                value
                    .as_str()
                    .or_else(|| value.get("id").and_then(Value::as_str))
                    .or_else(|| value.get("token").and_then(Value::as_str))
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToString::to_string)
            })
        })
    }

    fn export_has_more(response: &Value) -> Option<bool> {
        [Some(response), response.get("meta"), response.get("pagination")]
            .into_iter()
            .flatten()
            .find_map(|scope| {
                scope
                    .get("has_more")
                    .or_else(|| scope.get("hasMore"))
                    .and_then(Value::as_bool)
            })
    }

    fn stable_export_page(
        config: &Mem0Config,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        snapshot: Option<&str>,
        revision: Option<&str>,
        cursor: Option<&str>,
    ) -> Result<Mem0StablePage, Mem0ExportError> {
        let mut query = scope_query(subject, filter);
        query.push(("page_size", EXPORT_PAGE_SIZE.to_string()));
        query.push(("export_snapshot", "true".to_string()));
        query.push(("consistent_snapshot", "true".to_string()));
        if let Some(snapshot) = snapshot {
            query.push(("snapshot", snapshot.to_string()));
            query.push(("snapshot_id", snapshot.to_string()));
        }
        if let Some(revision) = revision {
            query.push(("revision", revision.to_string()));
        }
        if let Some(cursor) = cursor {
            query.push(("cursor", cursor.to_string()));
        }

        let mut last_error = None;
        for path in ["memories", "memory"] {
            match Self::send_json(config, Method::GET, path, None, Some(&query)) {
                Ok(response) => {
                    let snapshot = Self::export_marker(
                        &response,
                        &["snapshot_id", "snapshotId", "snapshot"],
                    )
                    .ok_or(Mem0ExportError::Unsupported)?;
                    let revision = Self::export_marker(
                        &response,
                        &["revision_id", "revisionId", "revision", "version"],
                    )
                    .ok_or(Mem0ExportError::Unsupported)?;
                    let next_cursor = Self::export_marker(
                        &response,
                        &[
                            "next_cursor",
                            "nextCursor",
                            "next_page_token",
                            "nextPageToken",
                        ],
                    );
                    if Self::export_has_more(&response) == Some(true) && next_cursor.is_none() {
                        return Err(Mem0ExportError::Failed(
                            "stable export page declared more records without a cursor",
                        ));
                    }
                    return Ok(Mem0StablePage {
                        snapshot,
                        revision,
                        next_cursor,
                        records: unwrap_results(response),
                    });
                }
                Err(error) => last_error = Some(error),
            }
        }
        let _ = last_error;
        Err(Mem0ExportError::Failed(
            "stable export listing is unavailable",
        ))
    }

    fn stream_stable_export(
        config: &Mem0Config,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        sink: &mut dyn MemoryExportSink,
    ) -> Result<(MemoryExportSnapshot, u64, u64), Mem0ExportError> {
        let result = (|| {
            let first = Self::stable_export_page(config, subject, filter, None, None, None)?;
            let snapshot_raw = first.snapshot.clone();
            let revision_raw = first.revision.clone();
            let snapshot = MemoryExportSnapshot::new(
                MemoryExportEvidenceDigest::sha256_bytes(snapshot_raw.as_bytes()),
                MemoryExportEvidenceDigest::sha256_bytes(revision_raw.as_bytes()),
                None,
            );
            sink.begin_provider(snapshot.clone())
                .map_err(|_| Mem0ExportError::Failed("export sink rejected provider snapshot"))?;

            let mut current_cursor: Option<String> = None;
            let mut page = first;
            let mut seen_next_cursors = HashSet::new();
            let mut exported_records = 0_u64;
            let mut page_count = 0_u64;
            loop {
                let Mem0StablePage {
                    snapshot: received_snapshot,
                    revision: received_revision,
                    next_cursor,
                    records,
                } = page;
                if received_snapshot != snapshot_raw || received_revision != revision_raw {
                    return Err(Mem0ExportError::Failed(
                        "stable export snapshot changed while paging",
                    ));
                }
                if let Some(next_cursor) = next_cursor.as_ref() {
                    if !seen_next_cursors.insert(next_cursor.clone()) {
                        return Err(Mem0ExportError::Failed(
                            "stable export cursor repeated while paging",
                        ));
                    }
                }

                let record_ids = records
                    .iter()
                    .map(|record| {
                        record_id(record)
                            .map(|id| MemoryExportEvidenceDigest::sha256_bytes(id.as_bytes()))
                            .ok_or(Mem0ExportError::Failed(
                                "stable export record has no stable identifier",
                            ))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let record_count = records.len() as u64;
                sink.append_page(
                    MemoryExportPage::new(
                        current_cursor
                            .as_deref()
                            .map(|cursor| MemoryExportEvidenceDigest::sha256_bytes(cursor.as_bytes())),
                        next_cursor
                            .as_deref()
                            .map(|cursor| MemoryExportEvidenceDigest::sha256_bytes(cursor.as_bytes())),
                        record_ids,
                        records,
                    )
                    .map_err(|_| Mem0ExportError::Failed("export page evidence is invalid"))?,
                )
                .map_err(|_| Mem0ExportError::Failed("export sink rejected provider page"))?;
                exported_records = exported_records
                    .checked_add(record_count)
                    .ok_or(Mem0ExportError::Failed("stable export record count overflow"))?;
                page_count = page_count
                    .checked_add(1)
                    .ok_or(Mem0ExportError::Failed("stable export page count overflow"))?;

                let Some(next_cursor) = next_cursor else {
                    break;
                };
                current_cursor = Some(next_cursor);
                page = Self::stable_export_page(
                    config,
                    subject,
                    filter,
                    Some(&snapshot_raw),
                    Some(&revision_raw),
                    current_cursor.as_deref(),
                )?;
            }

            sink.finish_provider(MemoryExportCompletion::exhausted(
                exported_records,
                page_count,
            ))
            .map_err(|_| Mem0ExportError::Failed("export sink rejected provider completion"))?;
            Ok((snapshot, exported_records, page_count))
        })();
        if result.is_err() {
            sink.abort();
        }
        result
    }

    fn add_messages(
        config: &Mem0Config,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        messages: Vec<Value>,
        infer: Option<bool>,
    ) -> Result<(), String> {
        let mut payload = scoped_body(subject, filter, json!({"messages": messages}))?;
        if let Some(infer) = infer {
            payload["infer"] = json!(infer);
        }
        let mut last_error = String::new();
        for path in ["memories", "memories/add", "memory"] {
            match Self::send_json(config, Method::POST, path, Some(&payload), None) {
                Ok(_) => return Ok(()),
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }

    fn update_memory(
        config: &Mem0Config,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        memory_id: &str,
        text: &str,
    ) -> Result<Value, String> {
        let memory_id = validate_memory_id(memory_id)?;
        let text = text.trim();
        if text.is_empty() {
            return Err("text is required".into());
        }
        let payload = scoped_body(
            subject,
            filter,
            json!({"memory_id": memory_id, "text": redact_sensitive_text(text), "memory": redact_sensitive_text(text)}),
        )?;
        let mut last_error = String::new();
        for (method, path) in [
            (Method::PATCH, format!("memories/{memory_id}")),
            (Method::PUT, format!("memories/{memory_id}")),
            (Method::POST, "memories/update".to_string()),
        ] {
            match Self::send_json(config, method, &path, Some(&payload), Some(&scope_query(subject, filter))) {
                Ok(value) => return Ok(value),
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }

    fn delete_memory(
        config: &Mem0Config,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        memory_id: &str,
    ) -> Result<(), String> {
        let memory_id = validate_memory_id(memory_id)?;
        let payload = scoped_body(subject, filter, json!({"memory_id": memory_id}))?;
        let mut last_error = String::new();
        for (method, path, body) in [
            (Method::DELETE, format!("memories/{memory_id}"), Some(&payload)),
            (Method::DELETE, format!("memory/{memory_id}"), Some(&payload)),
            (Method::POST, "memories/delete".to_string(), Some(&payload)),
        ] {
            match Self::send_json(config, method, &path, body, Some(&scope_query(subject, filter))) {
                Ok(_) => return Ok(()),
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }

    fn is_breaker_open(&self) -> bool {
        if self.consecutive_failures.load(Ordering::Relaxed) < BREAKER_THRESHOLD {
            return false;
        }
        let mut until = self.breaker_open_until.lock();
        if let Some(deadline) = *until {
            if Instant::now() >= deadline {
                self.consecutive_failures.store(0, Ordering::Relaxed);
                *until = None;
                return false;
            }
            return true;
        }
        false
    }

    fn record_success(&self) {
        self.consecutive_failures.store(0, Ordering::Relaxed);
        *self.breaker_open_until.lock() = None;
    }

    fn record_failure(&self) {
        let previous = self.consecutive_failures.fetch_add(1, Ordering::Relaxed);
        if previous + 1 >= BREAKER_THRESHOLD {
            *self.breaker_open_until.lock() = Some(Instant::now() + Duration::from_secs(BREAKER_COOLDOWN_SECS));
        }
    }
}

fn governance_failure(
    provider: &str,
    operation: MemoryGovernanceOperation,
    filter: &MemorySubjectStorageKey,
    actor: Option<&MemoryAuditActor>,
    error: &str,
) -> MemoryProviderGovernanceError {
    if let Some(actor) = actor {
        tracing::warn!(provider, operation = operation.as_str(), subject_key = filter.as_str(), actor = actor.opaque_id().as_str(), error = %redact_sensitive_text(error), "subject-scoped memory governance failed");
    } else {
        tracing::warn!(provider, operation = operation.as_str(), subject_key = filter.as_str(), error = %redact_sensitive_text(error), "subject-scoped memory governance failed");
    }
    MemoryProviderGovernanceError::failed(provider, operation)
}

impl MemoryProviderPlugin for Mem0MemoryPlugin {
    fn name(&self) -> &str {
        "mem0"
    }

    fn is_available(&self) -> bool {
        !std::env::var("MEM0_API_KEY").unwrap_or_default().trim().is_empty()
            || !std::env::var("MEM0_HOST").unwrap_or_default().trim().is_empty()
            || Mem0Config::configured_at(&Mem0Config::default_config_path())
    }

    fn initialize(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        hermes_home: &str,
    ) -> Result<(), String> {
        *self.config.lock() = Some(Mem0Config::load(hermes_home));
        *self.bound_subject.lock() = Some(subject.clone());
        *self.bound_filter.lock() = Some(filter.clone());
        *self.prefetch_result.lock() = String::new();
        self.record_success();
        Ok(())
    }

    fn system_prompt_block(&self) -> String {
        self.config
            .lock()
            .as_ref()
            .map(|config| format!("# Mem0 Memory\nActive. Mode: {}. All tools are limited to the authenticated memory subject.", mem0_mode_label(config)))
            .unwrap_or_default()
    }

    fn queue_prefetch(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        query: &str,
    ) {
        if self.is_breaker_open() || query.trim().is_empty() {
            return;
        }
        let config = match self.config_snapshot(subject, filter) {
            Ok(config) => config,
            Err(_) => return,
        };
        let subject = subject.clone();
        let filter = filter.clone();
        let query = query.trim().to_string();
        let out = Arc::clone(&self.prefetch_result);
        let failures = Arc::clone(&self.consecutive_failures);
        let breaker = Arc::clone(&self.breaker_open_until);
        std::thread::spawn(move || match Self::search_memories(&config, &subject, &filter, &query, config.rerank, 5) {
            Ok(records) => {
                let lines = records
                    .iter()
                    .filter_map(extract_memory_text)
                    .map(|memory| format!("- {memory}"))
                    .collect::<Vec<_>>();
                if !lines.is_empty() {
                    *out.lock() = lines.join("\n");
                }
                failures.store(0, Ordering::Relaxed);
            }
            Err(_) => {
                let previous = failures.fetch_add(1, Ordering::Relaxed);
                if previous + 1 >= BREAKER_THRESHOLD {
                    *breaker.lock() = Some(Instant::now() + Duration::from_secs(BREAKER_COOLDOWN_SECS));
                }
            }
        });
    }

    fn prefetch(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        _query: &str,
    ) -> String {
        if self.config_snapshot(subject, filter).is_err() {
            return String::new();
        }
        let mut result = self.prefetch_result.lock();
        let result = std::mem::take(&mut *result);
        if result.is_empty() { String::new() } else { format!("## Mem0 Memory\n{result}") }
    }

    fn sync_turn(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        user_content: &str,
        assistant_content: &str,
    ) {
        if self.is_breaker_open() {
            return;
        }
        let user = user_content.trim();
        let assistant = assistant_content.trim();
        if user.is_empty() || assistant.is_empty() {
            return;
        }
        let config = match self.config_snapshot(subject, filter) {
            Ok(config) => config,
            Err(_) => return,
        };
        let subject = subject.clone();
        let filter = filter.clone();
        let user = redact_sensitive_text(&user.chars().take(4000).collect::<String>());
        let assistant = redact_sensitive_text(&assistant.chars().take(4000).collect::<String>());
        let failures = Arc::clone(&self.consecutive_failures);
        let breaker = Arc::clone(&self.breaker_open_until);
        std::thread::spawn(move || {
            let messages = vec![json!({"role":"user","content":user}), json!({"role":"assistant","content":assistant})];
            if Self::add_messages(&config, &subject, &filter, messages, None).is_ok() {
                failures.store(0, Ordering::Relaxed);
            } else {
                let previous = failures.fetch_add(1, Ordering::Relaxed);
                if previous + 1 >= BREAKER_THRESHOLD {
                    *breaker.lock() = Some(Instant::now() + Duration::from_secs(BREAKER_COOLDOWN_SECS));
                }
            }
        });
    }

    fn get_tool_schemas(&self) -> Vec<Value> {
        vec![list_schema(), search_schema(), add_schema(), update_schema(), delete_schema()]
    }

    fn handle_tool_call(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        tool_name: &str,
        args: &Value,
    ) -> String {
        if let Err(error) = reject_subject_override(args) {
            return json!({"error": error}).to_string();
        }
        if self.is_breaker_open() {
            return json!({"error":"Mem0 API temporarily unavailable"}).to_string();
        }
        let config = match self.config_snapshot(subject, filter) {
            Ok(config) => config,
            Err(error) => return json!({"error":error}).to_string(),
        };
        let result = match tool_name {
            "mem0_list" | "mem0_profile" => {
                let page = args.get("page").and_then(Value::as_u64).unwrap_or(1);
                let size = args.get("page_size").and_then(Value::as_u64).unwrap_or(100);
                Self::list_memories(&config, subject, filter, page, size).map(|records| json!({"results":records,"count":records.len()}))
            }
            "mem0_search" => {
                let query = args.get("query").and_then(Value::as_str).unwrap_or("").trim();
                if query.is_empty() {
                    Err("query is required".to_string())
                } else {
                    let rerank = args.get("rerank").and_then(Value::as_bool).unwrap_or(config.rerank);
                    let top_k = args.get("top_k").and_then(Value::as_u64).unwrap_or(10);
                    Self::search_memories(&config, subject, filter, query, rerank, top_k).map(|records| json!({"results":records,"count":records.len()}))
                }
            }
            "mem0_add" | "mem0_conclude" => {
                let content = args.get("content").or_else(|| args.get("conclusion")).and_then(Value::as_str).unwrap_or("").trim();
                if content.is_empty() {
                    Err("content is required".to_string())
                } else {
                    Self::add_messages(&config, subject, filter, vec![json!({"role":"user","content":redact_sensitive_text(content)})], Some(false)).map(|_| json!({"result":"Fact stored."}))
                }
            }
            "mem0_update" => Self::update_memory(
                &config,
                subject,
                filter,
                args.get("memory_id").and_then(Value::as_str).unwrap_or(""),
                args.get("text").and_then(Value::as_str).unwrap_or(""),
            )
            .map(|response| json!({"result":"Memory updated.","response":response})),
            "mem0_delete" => Self::delete_memory(
                &config,
                subject,
                filter,
                args.get("memory_id").and_then(Value::as_str).unwrap_or(""),
            )
            .map(|_| json!({"result":"Memory deleted."})),
            _ => Err(format!("Unknown tool: {tool_name}")),
        };
        match result {
            Ok(value) => {
                self.record_success();
                value.to_string()
            }
            Err(error) => {
                self.record_failure();
                json!({"error":error}).to_string()
            }
        }
    }

    fn shutdown(&self) {
        *self.config.lock() = None;
        *self.bound_subject.lock() = None;
        *self.bound_filter.lock() = None;
        *self.prefetch_result.lock() = String::new();
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key":"mode","description":"Mem0 mode","default":"platform","choices":["platform","selfhosted","oss"],"env_var":"MEM0_MODE"},
            {"key":"api_key","description":"Mem0 API key","secret":true,"env_var":"MEM0_API_KEY"},
            {"key":"host","description":"Self-hosted Mem0 URL","env_var":"MEM0_HOST"},
            {"key":"base_url","description":"Mem0 API base URL","default":MEM0_CLOUD_BASE_URL,"env_var":"MEM0_BASE_URL"},
            {"key":"api_timeout_secs","description":"HTTP timeout seconds","default":"10"},
            {"key":"rerank","description":"Enable reranking","default":"false"}
        ]))
    }

    fn save_config(&self, config: &Value) -> Result<(), String> {
        reject_subject_override(config)?;
        config_io::merge_and_write_owner_only(&Mem0Config::default_config_path(), config)
    }

    fn export_subject(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        actor: &MemoryAuditActor,
        sink: &mut dyn MemoryExportSink,
    ) -> Result<MemoryExportReceipt, MemoryProviderGovernanceError> {
        let config = match self.config_snapshot(subject, filter) {
            Ok(config) => config,
            Err(error) => {
                sink.abort();
                return Err(governance_failure(
                    self.name(),
                    MemoryGovernanceOperation::Export,
                    filter,
                    Some(actor),
                    &error,
                ));
            }
        };
        match Self::stream_stable_export(&config, subject, filter, sink) {
            Ok((snapshot, exported_records, page_count)) => {
                tracing::info!(
                    provider = self.name(),
                    operation = MemoryGovernanceOperation::Export.as_str(),
                    subject_key = filter.as_str(),
                    actor = actor.opaque_id().as_str(),
                    exported_records,
                    page_count,
                    "subject-scoped memory export exhausted"
                );
                Ok(MemoryExportReceipt::exhausted(
                    self.name(),
                    filter.clone(),
                    snapshot,
                    exported_records,
                    page_count,
                ))
            }
            Err(Mem0ExportError::Unsupported) => {
                sink.abort();
                Err(MemoryProviderGovernanceError::unsupported(
                    self.name(),
                    MemoryGovernanceOperation::Export,
                ))
            }
            Err(Mem0ExportError::Failed(error)) => Err(governance_failure(
                self.name(),
                MemoryGovernanceOperation::Export,
                filter,
                Some(actor),
                error,
            )),
        }
    }

    // Mem0's exposed list path is offset-based and its fallback is semantic
    // search. Neither supplies a stable subject inventory cursor, so deletion
    // cannot certify that every record was enumerated before mutation.
    fn forget_subject(
        &self,
        _subject: &MemorySubject,
        _filter: &MemorySubjectStorageKey,
        _actor: &MemoryAuditActor,
    ) -> Result<MemoryForgetReceipt, MemoryProviderGovernanceError> {
        Err(MemoryProviderGovernanceError::unsupported(
            self.name(),
            MemoryGovernanceOperation::Forget,
        ))
    }

    // Retention requires a stable subject inventory with ordering and durable
    // creation metadata; Mem0's bounded list/search responses do not provide it.
    fn retention_sweep(
        &self,
        _subject: &MemorySubject,
        _filter: &MemorySubjectStorageKey,
        _policy: &MemoryRetentionPolicy,
        _actor: &MemoryAuditActor,
    ) -> Result<MemoryRetentionReceipt, MemoryProviderGovernanceError> {
        Err(MemoryProviderGovernanceError::unsupported(
            self.name(),
            MemoryGovernanceOperation::RetentionSweep,
        ))
    }

    // A bounded or semantic query cannot prove a subject partition is empty.
    fn verify_subject_absent(
        &self,
        _subject: &MemorySubject,
        _filter: &MemorySubjectStorageKey,
    ) -> Result<MemoryReadbackReceipt, MemoryProviderGovernanceError> {
        Err(MemoryProviderGovernanceError::unsupported(
            self.name(),
            MemoryGovernanceOperation::Readback,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::{
        MemoryExportArtifact, MemoryExportCompletion, MemoryExportPage, MemoryExportSink,
        MemoryExportSinkError, MemoryExportSnapshot, MemoryPrincipalId, MemorySessionId,
        MemorySubjectDerivationKey, MemorySurface, MemoryTenantId, MemoryVisibility,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    #[derive(Default)]
    struct TestExportSink {
        snapshot: Option<MemoryExportSnapshot>,
        pages: Vec<MemoryExportPage>,
        completion: Option<MemoryExportCompletion>,
        aborted: bool,
        reject_pages: bool,
    }

    impl MemoryExportSink for TestExportSink {
        fn begin_provider(
            &mut self,
            snapshot: MemoryExportSnapshot,
        ) -> Result<(), MemoryExportSinkError> {
            self.snapshot = Some(snapshot);
            Ok(())
        }

        fn append_page(&mut self, page: MemoryExportPage) -> Result<(), MemoryExportSinkError> {
            if self.reject_pages {
                return Err(MemoryExportSinkError::Rejected);
            }
            self.pages.push(page);
            Ok(())
        }

        fn finish_provider(
            &mut self,
            completion: MemoryExportCompletion,
        ) -> Result<(), MemoryExportSinkError> {
            self.completion = Some(completion);
            Ok(())
        }

        fn abort(&mut self) {
            self.aborted = true;
        }

        fn commit(self: Box<Self>) -> Result<MemoryExportArtifact, MemoryExportSinkError> {
            Err(MemoryExportSinkError::Incomplete)
        }
    }
    fn subject(principal: &str) -> (MemorySubject, MemorySubjectStorageKey) {
        let subject = MemorySubject::new(
            MemoryTenantId::new("tenant-a").unwrap(),
            MemoryPrincipalId::new(principal).unwrap(),
            None,
            MemorySessionId::new("session-a").unwrap(),
            MemorySurface::Gateway,
            MemoryVisibility::Private,
        )
        .unwrap();
        let filter = subject.storage_key(&MemorySubjectDerivationKey::from_bytes([4; 32]));
        (subject, filter)
    }

    fn config(base_url: String) -> Mem0Config {
        Mem0Config { mode: "oss".into(), api_key: String::new(), base_url, api_timeout_secs: 2.0, rerank: false }
    }

    fn read_request(stream: &mut std::net::TcpStream) -> String {
        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let read = stream.read(&mut chunk).unwrap();
            if read == 0 { break; }
            bytes.extend_from_slice(&chunk[..read]);
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let length = headers.lines().find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().ok()).flatten()
                }).unwrap_or(0);
                if bytes.len() >= end + 4 + length { break; }
            }
        }
        String::from_utf8_lossy(&bytes).to_string()
    }

    fn server(responses: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                sender.send(read_request(&mut stream)).unwrap();
                let reason = if (200..300).contains(&status) { "OK" } else { "Error" };
                stream.write_all(format!("HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).unwrap();
            }
        });
        (format!("http://{address}"), receiver)
    }

    fn owned_server(responses: Vec<(u16, String)>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().unwrap();
                sender.send(read_request(&mut stream)).unwrap();
                let reason = if (200..300).contains(&status) { "OK" } else { "Error" };
                stream
                    .write_all(
                        format!(
                            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        )
                        .as_bytes(),
                    )
                    .unwrap();
            }
        });
        (format!("http://{address}"), receiver)
    }

    fn plugin(
        config: Mem0Config,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
    ) -> Mem0MemoryPlugin {
        let plugin = Mem0MemoryPlugin::new();
        *plugin.config.lock() = Some(config);
        *plugin.bound_subject.lock() = Some(subject.clone());
        *plugin.bound_filter.lock() = Some(filter.clone());
        plugin
    }

    #[test]
    fn two_subject_filters_are_opaque_and_victim_override_is_rejected() {
        let (alice, alice_filter) = subject("alice");
        let (_bob, bob_filter) = subject("bob");
        assert_ne!(alice_filter, bob_filter);
        let (base, requests) = server(vec![(200, r#"{"results":[]}"#)]);
        let plugin = plugin(config(base), &alice, &alice_filter);
        let rejected = plugin.handle_tool_call(&alice, &alice_filter, "mem0_search", &json!({"query":"x","user_id":"victim"}));
        assert!(rejected.contains("cannot be overridden"));
        let result = plugin.handle_tool_call(&alice, &alice_filter, "mem0_search", &json!({"query":"x"}));
        assert!(result.contains("results"));
        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(request.contains(alice_filter.as_str()));
        assert!(!request.contains("alice"));
    }

    #[test]
    fn bound_subject_rejects_a_foreign_subject_even_with_the_bound_filter() {
        let (alice, filter) = subject("alice");
        let (bob, _bob_filter) = subject("bob");
        let plugin = plugin(config("http://127.0.0.1:9".to_string()), &alice, &filter);
        let result = plugin.handle_tool_call(&bob, &filter, "mem0_search", &json!({"query":"x"}));
        assert!(result.contains("does not match initialized subject"));
    }

    #[test]
    fn governance_rejects_over_cap_semantic_fallback_without_claiming_absence() {
        let (subject, filter) = subject("alice");
        let semantic_result = json!({
            "results": (0..=MAX_REMOTE_RECORDS)
                .map(|index| json!({"id": format!("nonmatching-{index}")}))
                .collect::<Vec<_>>()
        })
        .to_string();
        let (base, requests) = owned_server(vec![(200, semantic_result)]);
        let plugin = plugin(config(base), &subject, &filter);
        let actor = MemorySubjectDerivationKey::from_bytes([3; 32]).actor_for_subject(&subject);
        assert!(matches!(
            plugin.forget_subject(&subject, &filter, &actor),
            Err(MemoryProviderGovernanceError::Unsupported {
                operation: MemoryGovernanceOperation::Forget,
                ..
            })
        ));
        let policy = MemoryRetentionPolicy::new(None, Some(1)).unwrap();
        assert!(matches!(
            plugin.retention_sweep(&subject, &filter, &policy, &actor),
            Err(MemoryProviderGovernanceError::Unsupported {
                operation: MemoryGovernanceOperation::RetentionSweep,
                ..
            })
        ));
        assert!(matches!(
            plugin.verify_subject_absent(&subject, &filter),
            Err(MemoryProviderGovernanceError::Unsupported {
                operation: MemoryGovernanceOperation::Readback,
                ..
            })
        ));
        assert!(requests.recv_timeout(Duration::from_millis(100)).is_err());
    }

    #[test]
    fn export_streams_a_stable_subject_snapshot() {
        let (subject, filter) = subject("alice");
        let (base, requests) = server(vec![(
            200,
            r#"{"snapshot":"snapshot-a","revision":"revision-a","results":[{"id":"a"},{"id":"b"},{"id":"c"}]}"#,
        )]);
        let plugin = plugin(config(base), &subject, &filter);
        let actor = MemorySubjectDerivationKey::from_bytes([3; 32]).actor_for_subject(&subject);
        let mut sink = TestExportSink::default();
        assert_eq!(
            plugin
                .export_subject(&subject, &filter, &actor, &mut sink)
                .unwrap()
                .exported_records,
            3
        );
        assert_eq!(sink.pages.len(), 1);
        assert_eq!(sink.pages[0].records.len(), 3);
        assert!(sink.completion.as_ref().unwrap().exhausted);
        assert!(requests
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .contains(filter.as_str()));
    }
}
