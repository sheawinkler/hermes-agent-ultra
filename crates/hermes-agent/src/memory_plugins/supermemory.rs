//! Supermemory provider with authenticated, opaque subject containers.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

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

const DEFAULT_MAX_RECALL: usize = 10;
const DEFAULT_BASE_URL: &str = "https://api.supermemory.ai";
const SUPERMEMORY_API_KEY_URL: &str = "https://app.supermemory.ai/integrations?connect=hermes";
const SUPERMEMORY_SOURCE: &str = "hermes";
const MAX_REMOTE_RECORDS: usize = 200;
const EXPORT_PAGE_SIZE: usize = 100;

enum SupermemoryExportError {
    Unsupported,
    Failed(&'static str),
}

struct SupermemoryStablePage {
    snapshot: String,
    revision: String,
    next_cursor: Option<String>,
    records: Vec<Value>,
}

fn store_schema() -> Value {
    json!({
        "name": "supermemory-save",
        "description": "Store memory only in the authenticated subject container.",
        "parameters": {"type":"object","properties":{"content":{"type":"string"},"metadata":{"type":"object"}},"required":["content"]}
    })
}

fn search_schema() -> Value {
    json!({
        "name": "supermemory-search",
        "description": "Search only the authenticated subject container.",
        "parameters": {"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer"}},"required":["query"]}
    })
}

fn forget_schema() -> Value {
    json!({
        "name": "supermemory-forget",
        "description": "Forget an exact id or all query matches only in the authenticated subject container.",
        "parameters": {"type":"object","properties":{"id":{"type":"string"},"query":{"type":"string"}},"required":[]}
    })
}

fn profile_schema() -> Value {
    json!({
        "name": "supermemory-profile",
        "description": "Get profile data only for the authenticated subject container.",
        "parameters": {"type":"object","properties":{"query":{"type":"string"}},"required":[]}
    })
}

#[derive(Debug, Clone)]
struct SupermemoryConfig {
    api_key: String,
    base_url: String,
    auto_recall: bool,
    auto_capture: bool,
    max_recall_results: usize,
    search_mode: String,
    api_timeout: f64,
}

impl SupermemoryConfig {
    fn load(hermes_home: &str) -> Self {
        let mut config = Self {
            api_key: std::env::var("SUPERMEMORY_API_KEY").unwrap_or_default(),
            base_url: std::env::var("SUPERMEMORY_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string()),
            auto_recall: true,
            auto_capture: true,
            max_recall_results: DEFAULT_MAX_RECALL,
            search_mode: "hybrid".to_string(),
            api_timeout: 6.0,
        };
        let path = std::path::Path::new(hermes_home).join("supermemory.json");
        if let Ok(content) = std::fs::read_to_string(path) {
            if let Ok(raw) = serde_json::from_str::<Value>(&content) {
                if let Some(value) = raw.get("api_key").and_then(Value::as_str).filter(|value| !value.trim().is_empty()) {
                    config.api_key = value.trim().to_string();
                }
                if let Some(value) = raw.get("base_url").and_then(Value::as_str).filter(|value| !value.trim().is_empty()) {
                    config.base_url = value.trim().to_string();
                }
                if let Some(value) = raw.get("auto_recall").and_then(Value::as_bool) {
                    config.auto_recall = value;
                }
                if let Some(value) = raw.get("auto_capture").and_then(Value::as_bool) {
                    config.auto_capture = value;
                }
                if let Some(value) = raw.get("max_recall_results").and_then(Value::as_u64) {
                    config.max_recall_results = value.clamp(1, 20) as usize;
                }
                if let Some(value) = raw.get("search_mode").and_then(Value::as_str) {
                    if matches!(value, "hybrid" | "memories" | "documents") {
                        config.search_mode = value.to_string();
                    }
                }
                if let Some(value) = raw.get("api_timeout").or_else(|| raw.get("timeout")).and_then(Value::as_f64) {
                    config.api_timeout = value.clamp(1.0, 60.0);
                }
            }
        }
        config.base_url = config.base_url.trim_end_matches('/').to_string();
        config
    }
}

fn visibility_label(visibility: MemoryVisibility) -> &'static str {
    match visibility {
        MemoryVisibility::Private => "private",
        MemoryVisibility::Team => "team",
    }
}

fn subject_metadata(subject: &MemorySubject, filter: &MemorySubjectStorageKey) -> Value {
    let key = filter.as_str();
    json!({
        "sm_source": SUPERMEMORY_SOURCE,
        "subject_key": key,
        "tenant_filter": key,
        "principal_filter": key,
        "team_filter": subject.team_id().map(|_| key),
        "session_filter": key,
        "surface": subject.surface().as_str(),
        "visibility": visibility_label(subject.visibility()),
    })
}

fn scope_query(subject: &MemorySubject, filter: &MemorySubjectStorageKey) -> Vec<(&'static str, String)> {
    let key = filter.as_str().to_string();
    vec![
        ("subject_key", key.clone()),
        ("container_tag", key.clone()),
        ("tenant_id", key.clone()),
        ("principal_id", key.clone()),
        ("team_id", subject.team_id().map(|_| key.clone()).unwrap_or_default()),
        ("session_id", key.clone()),
        ("surface", subject.surface().as_str().to_string()),
        ("visibility", visibility_label(subject.visibility()).to_string()),
    ]
}

fn scoped_payload(
    subject: &MemorySubject,
    filter: &MemorySubjectStorageKey,
    mut payload: Value,
) -> Result<Value, String> {
    let map = payload.as_object_mut().ok_or_else(|| "Supermemory request body must be an object".to_string())?;
    let key = filter.as_str();
    let metadata = subject_metadata(subject, filter);
    map.insert("container_tag".to_string(), json!(key));
    map.insert("container_tags".to_string(), json!([key]));
    map.insert("subject_key".to_string(), json!(key));
    map.insert("tenant_id".to_string(), json!(key));
    map.insert("principal_id".to_string(), json!(key));
    map.insert("team_id".to_string(), subject.team_id().map(|_| json!(key)).unwrap_or(Value::Null));
    map.insert("session_id".to_string(), json!(key));
    match map.remove("metadata") {
        Some(Value::Object(mut existing)) => {
            if let Value::Object(scope) = metadata {
                for (key, value) in scope {
                    existing.insert(key, value);
                }
            }
            map.insert("metadata".to_string(), Value::Object(existing));
        }
        _ => {
            map.insert("metadata".to_string(), metadata);
        }
    }
    Ok(payload)
}

fn reject_subject_override(args: &Value) -> Result<(), String> {
    const FORBIDDEN: &[&str] = &[
        "container_tag", "containerTag", "container_tags", "containerTags", "user_id", "userId",
        "tenant_id", "tenantId", "principal_id", "principalId", "team_id", "teamId",
        "session_id", "sessionId", "subject_key", "subjectKey", "runtime_user_id", "runtimeUserId",
    ];
    if FORBIDDEN.iter().any(|key| args.get(*key).is_some()) {
        return Err("authenticated memory subject cannot be overridden by tool arguments".into());
    }
    Ok(())
}

fn unwrap_results(value: Value) -> Vec<Value> {
    if let Some(results) = value.as_array() {
        return results.clone();
    }
    for key in ["results", "memories", "items", "data"] {
        if let Some(results) = value.get(key).and_then(Value::as_array) {
            return results.clone();
        }
    }
    Vec::new()
}

fn record_id(record: &Value) -> Option<&str> {
    record
        .get("id")
        .or_else(|| record.get("memory_id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
}

fn extract_profile_lists(profile: &Value) -> (Vec<String>, Vec<String>, Vec<Value>) {
    let static_facts = profile
        .pointer("/profile/static")
        .or_else(|| profile.get("static"))
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).map(ToString::to_string).collect())
        .unwrap_or_default();
    let dynamic_facts = profile
        .pointer("/profile/dynamic")
        .or_else(|| profile.get("dynamic"))
        .and_then(Value::as_array)
        .map(|items| items.iter().filter_map(Value::as_str).map(ToString::to_string).collect())
        .unwrap_or_default();
    let results = profile
        .get("search_results")
        .or_else(|| profile.get("searchResults"))
        .map(|value| unwrap_results(value.clone()))
        .unwrap_or_default();
    (static_facts, dynamic_facts, results)
}

fn format_prefetch_context(static_facts: &[String], dynamic_facts: &[String], results: &[Value]) -> String {
    let mut parts = Vec::new();
    if !static_facts.is_empty() {
        parts.push(format!("## User Profile (Persistent)\n{}", static_facts.iter().map(|fact| format!("- {fact}")).collect::<Vec<_>>().join("\n")));
    }
    if !dynamic_facts.is_empty() {
        parts.push(format!("## Recent Context\n{}", dynamic_facts.iter().map(|fact| format!("- {fact}")).collect::<Vec<_>>().join("\n")));
    }
    let memories = results
        .iter()
        .filter_map(|record| record.get("memory").or_else(|| record.get("content")).or_else(|| record.get("text")).and_then(Value::as_str))
        .map(str::trim)
        .filter(|memory| !memory.is_empty())
        .map(ToString::to_string)
        .collect::<Vec<_>>();
    if !memories.is_empty() {
        parts.push(format!("## Relevant Memories\n{}", memories.iter().map(|memory| format!("- {memory}")).collect::<Vec<_>>().join("\n")));
    }
    if parts.is_empty() { String::new() } else { format!("<supermemory-context>\n{}\n</supermemory-context>", parts.join("\n\n")) }
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

pub struct SupermemoryMemoryPlugin {
    config: Mutex<Option<SupermemoryConfig>>,
    bound_subject: Mutex<Option<MemorySubject>>,
    bound_filter: Mutex<Option<MemorySubjectStorageKey>>,
    active: Mutex<bool>,
    prefetch_result: Arc<Mutex<String>>,
}

impl SupermemoryMemoryPlugin {
    pub fn new() -> Self {
        Self {
            config: Mutex::new(None),
            bound_subject: Mutex::new(None),
            bound_filter: Mutex::new(None),
            active: Mutex::new(false),
            prefetch_result: Arc::new(Mutex::new(String::new())),
        }
    }

    fn config_snapshot(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
    ) -> Result<SupermemoryConfig, String> {
        if self.bound_subject.lock().as_ref() != Some(subject)
            || self.bound_filter.lock().as_ref() != Some(filter)
        {
            return Err("Supermemory request subject does not match initialized subject".into());
        }
        self.config.lock().clone().ok_or_else(|| "Supermemory is not configured".to_string())
    }

    fn client(config: &SupermemoryConfig) -> Result<reqwest::blocking::Client, String> {
        reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs_f64(config.api_timeout))
            .build()
            .map_err(|error| format!("Supermemory HTTP client build failed: {error}"))
    }

    fn send_json(
        config: &SupermemoryConfig,
        method: Method,
        path: &str,
        payload: Option<&Value>,
        query: Option<&[(&str, String)]>,
    ) -> Result<Value, String> {
        let url = format!("{}/{}", config.base_url.trim_end_matches('/'), path.trim_start_matches('/'));
        let mut request = Self::client(config)?
            .request(method.clone(), &url)
            .header("Authorization", format!("Bearer {}", config.api_key))
            .header("X-API-Key", &config.api_key)
            .header("x-sm-source", SUPERMEMORY_SOURCE)
            .header("Content-Type", "application/json");
        if let Some(payload) = payload {
            request = request.json(payload);
        }
        if let Some(query) = query {
            request = request.query(query);
        }
        let response = request.send().map_err(|error| format!("Supermemory request {method} {url} failed: {error}"))?;
        let status = response.status();
        let body = response.text().unwrap_or_default();
        if !status.is_success() {
            return Err(format!("Supermemory request {method} {url} returned {status}: {body}"));
        }
        if body.trim().is_empty() { Ok(json!({})) } else { serde_json::from_str(&body).map_err(|error| format!("Supermemory response parse error: {error}")) }
    }

    fn add_memory(
        config: &SupermemoryConfig,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        content: &str,
        metadata: Option<Value>,
    ) -> Result<Value, String> {
        let payload = scoped_payload(subject, filter, json!({"content": redact_sensitive_text(content), "metadata": metadata.unwrap_or_else(|| json!({}))}))?;
        let mut last_error = String::new();
        for path in ["/v3/memories", "/v3/documents", "/v1/memories"] {
            match Self::send_json(config, Method::POST, path, Some(&payload), None) {
                Ok(value) => return Ok(value),
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }

    fn search_memories(
        config: &SupermemoryConfig,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        query: &str,
        limit: usize,
    ) -> Result<Vec<Value>, String> {
        let payload = scoped_payload(
            subject,
            filter,
            json!({"q": query, "query": query, "limit": limit.clamp(1, MAX_REMOTE_RECORDS), "search_mode": config.search_mode}),
        )?;
        let mut last_error = String::new();
        for path in ["/v3/search/memories", "/v3/memories/search", "/v1/memory/search"] {
            match Self::send_json(config, Method::POST, path, Some(&payload), None) {
                Ok(value) => return Ok(unwrap_results(value)),
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
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
        config: &SupermemoryConfig,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        snapshot: Option<&str>,
        revision: Option<&str>,
        cursor: Option<&str>,
    ) -> Result<SupermemoryStablePage, SupermemoryExportError> {
        let mut payload = scoped_payload(
            subject,
            filter,
            json!({
                "q": "*",
                "query": "*",
                "limit": EXPORT_PAGE_SIZE,
                "search_mode": config.search_mode,
                "export_snapshot": true,
                "consistent_snapshot": true,
            }),
        )
        .map_err(|_| SupermemoryExportError::Failed("unable to build stable export request"))?;
        if let Some(snapshot) = snapshot {
            payload["snapshot"] = json!(snapshot);
            payload["snapshot_id"] = json!(snapshot);
        }
        if let Some(revision) = revision {
            payload["revision"] = json!(revision);
        }
        if let Some(cursor) = cursor {
            payload["cursor"] = json!(cursor);
        }
        for path in ["/v3/search/memories", "/v3/memories/search", "/v1/memory/search"] {
            match Self::send_json(config, Method::POST, path, Some(&payload), None) {
                Ok(response) => {
                    let snapshot = Self::export_marker(
                        &response,
                        &["snapshot_id", "snapshotId", "snapshot"],
                    )
                    .ok_or(SupermemoryExportError::Unsupported)?;
                    let revision = Self::export_marker(
                        &response,
                        &["revision_id", "revisionId", "revision", "version"],
                    )
                    .ok_or(SupermemoryExportError::Unsupported)?;
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
                        return Err(SupermemoryExportError::Failed(
                            "stable export page declared more records without a cursor",
                        ));
                    }
                    return Ok(SupermemoryStablePage {
                        snapshot,
                        revision,
                        next_cursor,
                        records: unwrap_results(response),
                    });
                }
                Err(_) => continue,
            }
        }
        Err(SupermemoryExportError::Failed(
            "stable export listing is unavailable",
        ))
    }

    fn stream_stable_export(
        config: &SupermemoryConfig,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        sink: &mut dyn MemoryExportSink,
    ) -> Result<(MemoryExportSnapshot, u64, u64), SupermemoryExportError> {
        let result = (|| {
            let first = Self::stable_export_page(config, subject, filter, None, None, None)?;
            let snapshot_raw = first.snapshot.clone();
            let revision_raw = first.revision.clone();
            let snapshot = MemoryExportSnapshot::new(
                MemoryExportEvidenceDigest::sha256_bytes(snapshot_raw.as_bytes()),
                MemoryExportEvidenceDigest::sha256_bytes(revision_raw.as_bytes()),
                None,
            );
            sink.begin_provider(snapshot.clone()).map_err(|_| {
                SupermemoryExportError::Failed("export sink rejected provider snapshot")
            })?;

            let mut current_cursor: Option<String> = None;
            let mut page = first;
            let mut seen_next_cursors = HashSet::new();
            let mut exported_records = 0_u64;
            let mut page_count = 0_u64;
            loop {
                let SupermemoryStablePage {
                    snapshot: received_snapshot,
                    revision: received_revision,
                    next_cursor,
                    records,
                } = page;
                if received_snapshot != snapshot_raw || received_revision != revision_raw {
                    return Err(SupermemoryExportError::Failed(
                        "stable export snapshot changed while paging",
                    ));
                }
                if let Some(next_cursor) = next_cursor.as_ref() {
                    if !seen_next_cursors.insert(next_cursor.clone()) {
                        return Err(SupermemoryExportError::Failed(
                            "stable export cursor repeated while paging",
                        ));
                    }
                }
                let record_ids = records
                    .iter()
                    .map(|record| {
                        record_id(record)
                            .map(|id| MemoryExportEvidenceDigest::sha256_bytes(id.as_bytes()))
                            .ok_or(SupermemoryExportError::Failed(
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
                    .map_err(|_| SupermemoryExportError::Failed("export page evidence is invalid"))?,
                )
                .map_err(|_| SupermemoryExportError::Failed("export sink rejected provider page"))?;
                exported_records = exported_records.checked_add(record_count).ok_or(
                    SupermemoryExportError::Failed("stable export record count overflow"),
                )?;
                page_count = page_count.checked_add(1).ok_or(
                    SupermemoryExportError::Failed("stable export page count overflow"),
                )?;

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
            .map_err(|_| SupermemoryExportError::Failed("export sink rejected provider completion"))?;
            Ok((snapshot, exported_records, page_count))
        })();
        if result.is_err() {
            sink.abort();
        }
        result
    }

    fn get_profile(
        config: &SupermemoryConfig,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        query: Option<&str>,
    ) -> Result<Value, String> {
        let payload = scoped_payload(subject, filter, json!({"q": query.unwrap_or("")}))?;
        let mut last_error = String::new();
        for path in ["/v3/profile", "/v1/profile"] {
            match Self::send_json(config, Method::POST, path, Some(&payload), None) {
                Ok(value) => return Ok(value),
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }

    fn forget_memory(
        config: &SupermemoryConfig,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        memory_id: &str,
    ) -> Result<(), String> {
        let id = memory_id.trim();
        if id.is_empty() || id.contains('/') || id.contains('?') || id.contains('#') {
            return Err("id must be an exact Supermemory memory id".into());
        }
        let body = scoped_payload(subject, filter, json!({"id": id}))?;
        let query = scope_query(subject, filter);
        let mut last_error = String::new();
        for path in [format!("/v3/memories/{id}"), format!("/v1/memories/{id}")] {
            match Self::send_json(config, Method::DELETE, &path, Some(&body), Some(&query)) {
                Ok(_) => return Ok(()),
                Err(error) => last_error = error,
            }
        }
        Err(last_error)
    }

    fn ingest_conversation(
        config: &SupermemoryConfig,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        messages: &[Value],
    ) -> Result<(), String> {
        let messages = messages
            .iter()
            .filter_map(|message| {
                let role = message.get("role").and_then(Value::as_str)?;
                (role == "user" || role == "assistant").then(|| {
                    json!({"role":role,"content":redact_sensitive_text(message.get("content").and_then(Value::as_str).unwrap_or(""))})
                })
            })
            .collect::<Vec<_>>();
        if messages.is_empty() { return Ok(()); }
        let payload = scoped_payload(subject, filter, json!({"conversationId": filter.as_str(), "messages":messages}))?;
        Self::send_json(config, Method::POST, "/v4/conversations", Some(&payload), None).map(|_| ())
    }
}

fn governance_failure(provider: &str, operation: MemoryGovernanceOperation, filter: &MemorySubjectStorageKey, actor: Option<&MemoryAuditActor>, error: &str) -> MemoryProviderGovernanceError {
    if let Some(actor) = actor {
        tracing::warn!(provider, operation=operation.as_str(), subject_key=filter.as_str(), actor=actor.opaque_id().as_str(), error=%redact_sensitive_text(error), "subject-scoped memory governance failed");
    } else {
        tracing::warn!(provider, operation=operation.as_str(), subject_key=filter.as_str(), error=%redact_sensitive_text(error), "subject-scoped memory governance failed");
    }
    MemoryProviderGovernanceError::failed(provider, operation)
}

impl MemoryProviderPlugin for SupermemoryMemoryPlugin {
    fn name(&self) -> &str { "supermemory" }

    fn is_available(&self) -> bool {
        !std::env::var("SUPERMEMORY_API_KEY").unwrap_or_default().trim().is_empty()
            || config_io::json_file_has_nonempty_string(&config_io::default_hermes_home().join("supermemory.json"), &["api_key"])
    }

    fn initialize(&self, subject: &MemorySubject, filter: &MemorySubjectStorageKey, hermes_home: &str) -> Result<(), String> {
        let config = SupermemoryConfig::load(hermes_home);
        *self.active.lock() = !config.api_key.trim().is_empty();
        *self.config.lock() = Some(config);
        *self.bound_subject.lock() = Some(subject.clone());
        *self.bound_filter.lock() = Some(filter.clone());
        *self.prefetch_result.lock() = String::new();
        Ok(())
    }

    fn system_prompt_block(&self) -> String {
        if !*self.active.lock() { return String::new(); }
        "# Supermemory\nActive only in the authenticated memory subject container. Use supermemory-search, supermemory-save, supermemory-forget, and supermemory-profile.".to_string()
    }

    fn prefetch(&self, subject: &MemorySubject, filter: &MemorySubjectStorageKey, query: &str) -> String {
        if !*self.active.lock() { return String::new(); }
        let queued = std::mem::take(&mut *self.prefetch_result.lock());
        if !queued.is_empty() { return queued; }
        let config = match self.config_snapshot(subject, filter) { Ok(config) => config, Err(_) => return String::new() };
        if !config.auto_recall || query.trim().is_empty() { return String::new(); }
        match Self::get_profile(&config, subject, filter, Some(query.trim())) {
            Ok(profile) => {
                let (static_facts, dynamic_facts, results) = extract_profile_lists(&profile);
                format_prefetch_context(&static_facts, &dynamic_facts, &results)
            }
            Err(_) => String::new(),
        }
    }

    fn queue_prefetch(&self, subject: &MemorySubject, filter: &MemorySubjectStorageKey, query: &str) {
        if !*self.active.lock() || query.trim().is_empty() { return; }
        let config = match self.config_snapshot(subject, filter) { Ok(config) => config, Err(_) => return };
        if !config.auto_recall { return; }
        let subject = subject.clone();
        let filter = filter.clone();
        let query = query.trim().to_string();
        let out = Arc::clone(&self.prefetch_result);
        std::thread::spawn(move || {
            if let Ok(profile) = Self::get_profile(&config, &subject, &filter, Some(&query)) {
                let (static_facts, dynamic_facts, results) = extract_profile_lists(&profile);
                let context = format_prefetch_context(&static_facts, &dynamic_facts, &results);
                if !context.is_empty() { *out.lock() = context; }
            }
        });
    }

    fn sync_turn(&self, subject: &MemorySubject, filter: &MemorySubjectStorageKey, user_content: &str, assistant_content: &str) {
        if !*self.active.lock() { return; }
        let config = match self.config_snapshot(subject, filter) { Ok(config) => config, Err(_) => return };
        if !config.auto_capture || user_content.trim().is_empty() || assistant_content.trim().is_empty() { return; }
        let subject = subject.clone();
        let filter = filter.clone();
        let content = format!("[role: user]\n{}\n[user:end]\n\n[role: assistant]\n{}\n[assistant:end]", redact_sensitive_text(&user_content.chars().take(2500).collect::<String>()), redact_sensitive_text(&assistant_content.chars().take(2500).collect::<String>()));
        std::thread::spawn(move || {
            let _ = Self::add_memory(&config, &subject, &filter, &content, Some(json!({"type":"conversation_turn"})));
        });
    }

    fn get_tool_schemas(&self) -> Vec<Value> { vec![store_schema(), search_schema(), forget_schema(), profile_schema()] }

    fn handle_tool_call(&self, subject: &MemorySubject, filter: &MemorySubjectStorageKey, tool_name: &str, args: &Value) -> String {
        if let Err(error) = reject_subject_override(args) { return json!({"error":error}).to_string(); }
        if !*self.active.lock() { return json!({"error":"Supermemory is not configured"}).to_string(); }
        let config = match self.config_snapshot(subject, filter) { Ok(config) => config, Err(error) => return json!({"error":error}).to_string() };
        let result = match tool_name {
            "supermemory-save" | "supermemory_store" => {
                let content = args.get("content").and_then(Value::as_str).unwrap_or("").trim();
                if content.is_empty() { Err("content is required".to_string()) }
                else { Self::add_memory(&config, subject, filter, content, args.get("metadata").cloned().filter(Value::is_object)).map(|value| json!({"saved":true,"raw":value})) }
            }
            "supermemory-search" | "supermemory_search" => {
                let query = args.get("query").and_then(Value::as_str).unwrap_or("").trim();
                if query.is_empty() { Err("query is required".to_string()) }
                else { Self::search_memories(&config, subject, filter, query, args.get("limit").and_then(Value::as_u64).unwrap_or(config.max_recall_results as u64) as usize).map(|records| json!({"results":records,"count":records.len()})) }
            }
            "supermemory-forget" | "supermemory_forget" => {
                let id = args.get("id").and_then(Value::as_str).unwrap_or("").trim();
                let query = args.get("query").and_then(Value::as_str).unwrap_or("").trim();
                if id.is_empty() && query.is_empty() { Err("provide id or query".to_string()) }
                else if !id.is_empty() { Self::forget_memory(&config, subject, filter, id).map(|_| json!({"forgotten":true,"id":id})) }
                else {
                    // Query-forget deliberately deletes every scoped match; a best match is never an erasure claim.
                    (|| -> Result<Value, String> {
                        let records =
                            Self::search_memories(&config, subject, filter, query, MAX_REMOTE_RECORDS)?;
                        let mut deleted = 0_u64;
                        for record in records {
                            let id = record_id(&record)
                                .ok_or_else(|| "matching memory lacks an exact id".to_string())?;
                            Self::forget_memory(&config, subject, filter, id)?;
                            deleted += 1;
                        }
                        let residual =
                            Self::search_memories(&config, subject, filter, query, MAX_REMOTE_RECORDS)?;
                        if !residual.is_empty() {
                            Err("subject-scoped query forget left residue".to_string())
                        } else {
                            Ok(json!({"forgotten":true,"deleted":deleted}))
                        }
                    })()
                }
            }
            "supermemory-profile" | "supermemory_profile" => Self::get_profile(&config, subject, filter, args.get("query").and_then(Value::as_str)).map(|profile| json!({"profile":profile})),
            _ => Err(format!("Unknown tool: {tool_name}")),
        };
        match result { Ok(value) => value.to_string(), Err(error) => json!({"error":error}).to_string() }
    }

    fn on_session_end(&self, subject: &MemorySubject, filter: &MemorySubjectStorageKey, messages: &[Value]) {
        if !*self.active.lock() { return; }
        let config = match self.config_snapshot(subject, filter) { Ok(config) => config, Err(_) => return };
        let subject = subject.clone();
        let filter = filter.clone();
        let messages = messages.to_vec();
        std::thread::spawn(move || { let _ = Self::ingest_conversation(&config, &subject, &filter, &messages); });
    }

    fn on_memory_write(&self, subject: &MemorySubject, filter: &MemorySubjectStorageKey, action: &str, target: &str, content: &str) {
        if action != "add" || target != "user" || content.trim().is_empty() || !*self.active.lock() { return; }
        let config = match self.config_snapshot(subject, filter) { Ok(config) => config, Err(_) => return };
        let subject = subject.clone();
        let filter = filter.clone();
        let content = content.trim().to_string();
        std::thread::spawn(move || { let _ = Self::add_memory(&config, &subject, &filter, &content, Some(json!({"type":"explicit_memory"}))); });
    }

    fn shutdown(&self) {
        *self.active.lock() = false;
        *self.config.lock() = None;
        *self.bound_subject.lock() = None;
        *self.bound_filter.lock() = None;
        *self.prefetch_result.lock() = String::new();
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key":"api_key","description":"Supermemory API key","secret":true,"required":true,"env_var":"SUPERMEMORY_API_KEY","url":SUPERMEMORY_API_KEY_URL},
            {"key":"base_url","description":"Supermemory API base URL","default":DEFAULT_BASE_URL},
            {"key":"search_mode","description":"hybrid|memories|documents","default":"hybrid"},
            {"key":"auto_recall","description":"Automatically recall subject-scoped memory","default":true},
            {"key":"auto_capture","description":"Automatically capture subject-scoped turns","default":true}
        ]))
    }

    fn save_config(&self, config: &Value) -> Result<(), String> {
        reject_subject_override(config)?;
        config_io::merge_and_write_owner_only(&config_io::default_hermes_home().join("supermemory.json"), config)
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
            Err(SupermemoryExportError::Unsupported) => {
                sink.abort();
                Err(MemoryProviderGovernanceError::unsupported(
                    self.name(),
                    MemoryGovernanceOperation::Export,
                ))
            }
            Err(SupermemoryExportError::Failed(error)) => Err(governance_failure(
                self.name(),
                MemoryGovernanceOperation::Export,
                filter,
                Some(actor),
                error,
            )),
        }
    }

    // Supermemory exposes semantic search, not a stable subject inventory
    // cursor. A query result cannot establish deletion coverage or readback.
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

    // Search is capped and has neither immutable ordering nor policy-wide age
    // metadata, so retention must not make a partial deletion claim.
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

    // A search miss is not an exhaustive absence proof.
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
        MemorySubjectDerivationKey, MemorySurface, MemoryTenantId,
    };
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::mpsc;

    #[derive(Default)]
    struct TestExportSink {
        pages: Vec<MemoryExportPage>,
        completion: Option<MemoryExportCompletion>,
    }

    impl MemoryExportSink for TestExportSink {
        fn begin_provider(
            &mut self,
            _snapshot: MemoryExportSnapshot,
        ) -> Result<(), MemoryExportSinkError> {
            Ok(())
        }

        fn append_page(&mut self, page: MemoryExportPage) -> Result<(), MemoryExportSinkError> {
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

        fn abort(&mut self) {}

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
        ).unwrap();
        let filter = subject.storage_key(&MemorySubjectDerivationKey::from_bytes([6; 32]));
        (subject, filter)
    }

    fn config(base_url: String) -> SupermemoryConfig {
        SupermemoryConfig { api_key: "test-key".into(), base_url, auto_recall: true, auto_capture: true, max_recall_results: 10, search_mode: "hybrid".into(), api_timeout: 2.0 }
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
                let content_length = headers.lines().find_map(|line| { let (name, value) = line.split_once(':')?; name.eq_ignore_ascii_case("content-length").then(|| value.trim().parse::<usize>().ok()).flatten() }).unwrap_or(0);
                if bytes.len() >= end + 4 + content_length { break; }
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
        config: SupermemoryConfig,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
    ) -> SupermemoryMemoryPlugin {
        let plugin = SupermemoryMemoryPlugin::new();
        *plugin.config.lock() = Some(config);
        *plugin.bound_subject.lock() = Some(subject.clone());
        *plugin.bound_filter.lock() = Some(filter.clone());
        *plugin.active.lock() = true;
        plugin
    }

    #[test]
    fn subject_container_is_opaque_and_victim_container_override_is_rejected() {
        let (alice, filter) = subject("alice");
        let (_bob, bob_filter) = subject("bob");
        assert_ne!(filter, bob_filter);
        let (base, requests) = server(vec![(200, r#"{"results":[]}"#)]);
        let plugin = plugin(config(base), &alice, &filter);
        assert!(plugin.handle_tool_call(&alice, &filter, "supermemory-search", &json!({"query":"x","container_tag":"victim"})).contains("cannot be overridden"));
        assert!(plugin.handle_tool_call(&alice, &filter, "supermemory-search", &json!({"query":"x"})).contains("results"));
        let request = requests.recv_timeout(Duration::from_secs(2)).unwrap();
        assert!(request.contains(filter.as_str()));
        assert!(!request.contains("alice"));
    }

    #[test]
    fn bound_subject_rejects_foreign_subject_with_reused_filter() {
        let (alice, filter) = subject("alice");
        let (bob, _bob_filter) = subject("bob");
        let plugin = plugin(config("http://127.0.0.1:9".to_string()), &alice, &filter);
        assert!(plugin
            .handle_tool_call(&bob, &filter, "supermemory-search", &json!({"query":"x"}))
            .contains("does not match initialized subject"));
    }

    #[test]
    fn query_forget_deletes_all_matches_not_a_best_match() {
        let (subject, filter) = subject("alice");
        let (base, requests) = server(vec![
            (200, r#"{"results":[{"id":"a"},{"id":"b"}]}"#),
            (200, r#"{}"#),
            (200, r#"{}"#),
            (200, r#"{"results":[]}"#),
        ]);
        let plugin = plugin(config(base), &subject, &filter);
        let value = plugin.handle_tool_call(&subject, &filter, "supermemory-forget", &json!({"query":"remove"}));
        assert!(value.contains("\"deleted\":2"));
        for _ in 0..4 { assert!(requests.recv_timeout(Duration::from_secs(2)).unwrap().contains(filter.as_str())); }
    }

    #[test]
    fn governance_rejects_over_cap_semantic_search_as_an_inventory() {
        let (subject, filter) = subject("alice");
        let semantic_result = json!({
            "results": (0..=MAX_REMOTE_RECORDS)
                .map(|index| json!({"id": format!("nonmatching-{index}")}))
                .collect::<Vec<_>>()
        })
        .to_string();
        let (base, requests) = owned_server(vec![(200, semantic_result)]);
        let plugin = plugin(config(base), &subject, &filter);
        let actor = MemorySubjectDerivationKey::from_bytes([7; 32]).actor_for_subject(&subject);
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
        let actor = MemorySubjectDerivationKey::from_bytes([7; 32]).actor_for_subject(&subject);
        let mut sink = TestExportSink::default();
        assert_eq!(
            plugin
                .export_subject(&subject, &filter, &actor, &mut sink)
                .unwrap()
                .exported_records,
            3
        );
        assert_eq!(sink.pages.len(), 1);
        assert!(sink.completion.as_ref().unwrap().exhausted);
        assert!(requests
            .recv_timeout(Duration::from_secs(2))
            .unwrap()
            .contains(filter.as_str()));
    }
}
