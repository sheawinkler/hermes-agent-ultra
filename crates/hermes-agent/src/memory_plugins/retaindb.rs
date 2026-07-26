//! RetainDB memory provider — subject-partitioned cloud long-term memory.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use aes_gcm::aead::{Aead, AeadCore, KeyInit, OsRng, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::prelude::{Engine as _, BASE64_STANDARD};
use hermes_core::{
    MemoryAuditActor, MemoryExportCompletion, MemoryExportEvidenceDigest, MemoryExportPage,
    MemoryExportReceipt, MemoryExportSink, MemoryExportSnapshot, MemoryForgetReceipt,
    MemoryGovernanceOperation, MemoryProviderGovernanceError, MemoryReadbackReceipt,
    MemoryRetentionPolicy, MemoryRetentionReceipt, MemorySubject, MemorySubjectStorageKey,
    MemoryVisibility,
};
use hermes_intelligence::redact_sensitive_text;
use parking_lot::Mutex;
use reqwest::blocking::{
    multipart::{Form, Part},
    Client, RequestBuilder,
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::memory_manager::MemoryProviderPlugin;

const DEFAULT_BASE: &str = "https://api.retaindb.com";
const RETAINDB_QUEUE_AAD_PREFIX: &str = "hermes-retaindb-queue-v2";
const RETAINDB_QUEUE_NONCE_BYTES: usize = 12;
const MAX_REMOTE_RECORDS: usize = 200;
const RETAINDB_QUEUE_TEXT_LIMIT: usize = 2_048;
const EXPORT_PAGE_SIZE: u32 = 100;

enum RetainExportError {
    Unsupported,
    Failed(&'static str),
}

struct RetainStablePage {
    snapshot: String,
    revision: String,
    next_cursor: Option<String>,
    records: Vec<Value>,
}

// Schema checks contain DDL; serialize only that setup phase so simultaneous
// subject writers never race a migration or drop each other's queue table.
static RETAINDB_QUEUE_SCHEMA_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

fn profile_schema() -> Value {
    json!({
        "name": "retaindb_profile",
        "description": "Get the authenticated subject's stable RetainDB profile.",
        "parameters": {"type": "object", "properties": {}, "required": []}
    })
}

fn search_schema() -> Value {
    json!({
        "name": "retaindb_search",
        "description": "Semantic search only within the authenticated subject's RetainDB memory.",
        "parameters": {
            "type": "object",
            "properties": {"query": {"type": "string"}, "top_k": {"type": "integer"}},
            "required": ["query"]
        }
    })
}

fn context_schema() -> Value {
    json!({
        "name": "retaindb_context",
        "description": "Subject-scoped RetainDB context for the current task.",
        "parameters": {
            "type": "object",
            "properties": {"query": {"type": "string"}},
            "required": ["query"]
        }
    })
}

fn remember_schema() -> Value {
    json!({
        "name": "retaindb_remember",
        "description": "Persist a fact only for the authenticated memory subject.",
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
        "description": "Delete an exact RetainDB memory only within the authenticated subject.",
        "parameters": {
            "type": "object",
            "properties": {"memory_id": {"type": "string"}},
            "required": ["memory_id"]
        }
    })
}

fn upload_file_schema() -> Value {
    json!({
        "name": "retaindb_upload_file",
        "description": "Upload a file only to the authenticated subject's RetainDB partition.",
        "parameters": {
            "type": "object",
            "properties": {
                "local_path": {"type": "string"},
                "remote_path": {"type": "string"},
                "ingest": {"type": "boolean"}
            },
            "required": ["local_path"]
        }
    })
}

fn list_files_schema() -> Value {
    json!({
        "name": "retaindb_list_files",
        "description": "List files only in the authenticated subject's RetainDB partition.",
        "parameters": {
            "type": "object",
            "properties": {"prefix": {"type": "string"}, "limit": {"type": "integer"}},
            "required": []
        }
    })
}

fn read_file_schema() -> Value {
    json!({
        "name": "retaindb_read_file",
        "description": "Read a file only through the authenticated subject partition.",
        "parameters": {
            "type": "object",
            "properties": {"file_id": {"type": "string"}},
            "required": ["file_id"]
        }
    })
}

fn ingest_file_schema() -> Value {
    json!({
        "name": "retaindb_ingest_file",
        "description": "Ingest a file only for the authenticated subject partition.",
        "parameters": {
            "type": "object",
            "properties": {"file_id": {"type": "string"}},
            "required": ["file_id"]
        }
    })
}

fn delete_file_schema() -> Value {
    json!({
        "name": "retaindb_delete_file",
        "description": "Delete a file only through the authenticated subject partition.",
        "parameters": {
            "type": "object",
            "properties": {"file_id": {"type": "string"}},
            "required": ["file_id"]
        }
    })
}

#[derive(Clone)]
struct RetainState {
    client: Client,
    base: String,
    token: String,
    subject: MemorySubject,
    filter: MemorySubjectStorageKey,
    queue_db_path: PathBuf,
}

#[derive(Debug, Clone)]
struct RetainQueueRow {
    id: i64,
    payload: Value,
}

fn bearer_token(raw: &str) -> String {
    raw.trim()
        .strip_prefix("Bearer ")
        .unwrap_or(raw)
        .to_string()
}

fn with_retain_headers(req: RequestBuilder, token: &str) -> RequestBuilder {
    let token = bearer_token(token);
    req.header("Authorization", format!("Bearer {token}"))
        .header("X-API-Key", token)
        .header("Content-Type", "application/json")
        .header("x-sdk-runtime", "hermes-agent-rust")
}

fn with_retain_auth_headers(req: RequestBuilder, token: &str) -> RequestBuilder {
    let token = bearer_token(token);
    req.header("Authorization", format!("Bearer {token}"))
        .header("x-sdk-runtime", "hermes-agent-rust")
}

fn visibility_label(visibility: MemoryVisibility) -> &'static str {
    match visibility {
        MemoryVisibility::Private => "private",
        MemoryVisibility::Team => "team",
    }
}

/// Never send raw authenticated identifiers to a remote provider.  The same
/// opaque, keyed subject filter is carried under each provider-supported scope
/// field so a provider cannot fall back to a shared/default partition.
fn subject_scope(subject: &MemorySubject, filter: &MemorySubjectStorageKey) -> Value {
    let key = filter.as_str();
    json!({
        "opaque_key": key,
        "tenant_filter": key,
        "principal_filter": key,
        "team_filter": subject.team_id().map(|_| key),
        "session_filter": key,
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
        .ok_or_else(|| "RetainDB request body must be an object".to_string())?;
    let key = filter.as_str();
    map.insert("subject".to_string(), subject_scope(subject, filter));
    map.insert("subject_key".to_string(), json!(key));
    map.insert("tenant_id".to_string(), json!(key));
    map.insert("principal_id".to_string(), json!(key));
    map.insert(
        "team_id".to_string(),
        subject.team_id().map(|_| json!(key)).unwrap_or(Value::Null),
    );
    map.insert("session_id".to_string(), json!(key));
    Ok(body)
}

fn scope_query(
    subject: &MemorySubject,
    filter: &MemorySubjectStorageKey,
) -> Vec<(&'static str, String)> {
    let key = filter.as_str().to_string();
    vec![
        ("subject_key", key.clone()),
        ("tenant_id", key.clone()),
        ("principal_id", key.clone()),
        (
            "team_id",
            subject.team_id().map(|_| key.clone()).unwrap_or_default(),
        ),
        ("session_id", key.clone()),
        ("surface", subject.surface().as_str().to_string()),
        (
            "visibility",
            visibility_label(subject.visibility()).to_string(),
        ),
    ]
}

fn reject_subject_override(args: &Value) -> Result<(), String> {
    const FORBIDDEN: &[&str] = &[
        "user_id",
        "userId",
        "tenant_id",
        "tenantId",
        "principal_id",
        "principalId",
        "team_id",
        "teamId",
        "session_id",
        "sessionId",
        "subject_key",
        "subjectKey",
        "runtime_user_id",
        "runtimeUserId",
    ];
    if FORBIDDEN.iter().any(|key| args.get(*key).is_some()) {
        return Err("authenticated memory subject cannot be overridden by tool arguments".into());
    }
    Ok(())
}

fn state_matches(
    st: &RetainState,
    subject: &MemorySubject,
    filter: &MemorySubjectStorageKey,
) -> bool {
    st.subject == *subject && st.filter == *filter
}

fn parse_results(value: Value) -> Vec<Value> {
    if let Some(items) = value.as_array() {
        return items.clone();
    }
    for key in ["results", "memories", "items", "data"] {
        if let Some(items) = value.get(key).and_then(Value::as_array) {
            return items.clone();
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

fn urlencoding_query(value: &str) -> String {
    value
        .bytes()
        .map(|byte| match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (byte as char).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

fn remote_memories(
    st: &RetainState,
    subject: &MemorySubject,
    filter: &MemorySubjectStorageKey,
    query: &str,
    top_k: u32,
) -> Result<Vec<Value>, String> {
    let body = scoped_body(
        subject,
        filter,
        json!({
            "project": filter.as_str(),
            "query": query,
            "top_k": top_k.clamp(1, MAX_REMOTE_RECORDS as u32),
            "include_pending": true,
        }),
    )?;
    let response = with_retain_headers(
        st.client
            .post(format!("{}/v1/memory/search", st.base))
            .json(&body),
        &st.token,
    )
    .send()
    .map_err(|err| err.to_string())?;
    if !response.status().is_success() {
        return Err(format!("search {}", response.status()));
    }
    let value: Value = response.json().map_err(|err| err.to_string())?;
    Ok(parse_results(value))
}

fn retain_export_marker(response: &Value, keys: &[&str]) -> Option<String> {
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

fn retain_export_has_more(response: &Value) -> Option<bool> {
    [
        Some(response),
        response.get("meta"),
        response.get("pagination"),
    ]
    .into_iter()
    .flatten()
    .find_map(|scope| {
        scope
            .get("has_more")
            .or_else(|| scope.get("hasMore"))
            .and_then(Value::as_bool)
    })
}

fn stable_retain_export_page(
    st: &RetainState,
    subject: &MemorySubject,
    filter: &MemorySubjectStorageKey,
    snapshot: Option<&str>,
    revision: Option<&str>,
    cursor: Option<&str>,
) -> Result<RetainStablePage, RetainExportError> {
    let mut body = scoped_body(
        subject,
        filter,
        json!({
            "project": filter.as_str(),
            "query": "*",
            "top_k": EXPORT_PAGE_SIZE,
            "include_pending": true,
            "export_snapshot": true,
            "consistent_snapshot": true,
        }),
    )
    .map_err(|_| RetainExportError::Failed("unable to build stable export request"))?;
    if let Some(snapshot) = snapshot {
        body["snapshot"] = json!(snapshot);
        body["snapshot_id"] = json!(snapshot);
    }
    if let Some(revision) = revision {
        body["revision"] = json!(revision);
    }
    if let Some(cursor) = cursor {
        body["cursor"] = json!(cursor);
    }
    let response = with_retain_headers(
        st.client
            .post(format!("{}/v1/memory/search", st.base))
            .json(&body),
        &st.token,
    )
    .send()
    .map_err(|_| RetainExportError::Failed("stable export listing is unavailable"))?;
    if !response.status().is_success() {
        return Err(RetainExportError::Failed(
            "stable export listing is unavailable",
        ));
    }
    let response: Value = response
        .json()
        .map_err(|_| RetainExportError::Failed("stable export response is invalid"))?;
    let snapshot = retain_export_marker(&response, &["snapshot_id", "snapshotId", "snapshot"])
        .ok_or(RetainExportError::Unsupported)?;
    let revision = retain_export_marker(
        &response,
        &["revision_id", "revisionId", "revision", "version"],
    )
    .ok_or(RetainExportError::Unsupported)?;
    let next_cursor = retain_export_marker(
        &response,
        &[
            "next_cursor",
            "nextCursor",
            "next_page_token",
            "nextPageToken",
        ],
    );
    if retain_export_has_more(&response) == Some(true) && next_cursor.is_none() {
        return Err(RetainExportError::Failed(
            "stable export page declared more records without a cursor",
        ));
    }
    Ok(RetainStablePage {
        snapshot,
        revision,
        next_cursor,
        records: parse_results(response),
    })
}

fn stream_retain_export(
    st: &RetainState,
    subject: &MemorySubject,
    filter: &MemorySubjectStorageKey,
    sink: &mut dyn MemoryExportSink,
) -> Result<(MemoryExportSnapshot, u64, u64), RetainExportError> {
    let result = (|| {
        let first = stable_retain_export_page(st, subject, filter, None, None, None)?;
        let snapshot_raw = first.snapshot.clone();
        let revision_raw = first.revision.clone();
        let snapshot = MemoryExportSnapshot::new(
            MemoryExportEvidenceDigest::sha256_bytes(snapshot_raw.as_bytes()),
            MemoryExportEvidenceDigest::sha256_bytes(revision_raw.as_bytes()),
            None,
        );
        sink.begin_provider(snapshot.clone())
            .map_err(|_| RetainExportError::Failed("export sink rejected provider snapshot"))?;

        let mut current_cursor: Option<String> = None;
        let mut page = first;
        let mut seen_next_cursors = HashSet::new();
        let mut exported_records = 0_u64;
        let mut page_count = 0_u64;
        loop {
            let RetainStablePage {
                snapshot: received_snapshot,
                revision: received_revision,
                next_cursor,
                records,
            } = page;
            if received_snapshot != snapshot_raw || received_revision != revision_raw {
                return Err(RetainExportError::Failed(
                    "stable export snapshot changed while paging",
                ));
            }
            if let Some(next_cursor) = next_cursor.as_ref() {
                if !seen_next_cursors.insert(next_cursor.clone()) {
                    return Err(RetainExportError::Failed(
                        "stable export cursor repeated while paging",
                    ));
                }
            }
            let record_ids = records
                .iter()
                .map(|record| {
                    record_id(record)
                        .map(|id| MemoryExportEvidenceDigest::sha256_bytes(id.as_bytes()))
                        .ok_or(RetainExportError::Failed(
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
                .map_err(|_| RetainExportError::Failed("export page evidence is invalid"))?,
            )
            .map_err(|_| RetainExportError::Failed("export sink rejected provider page"))?;
            exported_records =
                exported_records
                    .checked_add(record_count)
                    .ok_or(RetainExportError::Failed(
                        "stable export record count overflow",
                    ))?;
            page_count = page_count.checked_add(1).ok_or(RetainExportError::Failed(
                "stable export page count overflow",
            ))?;

            let Some(next_cursor) = next_cursor else {
                break;
            };
            current_cursor = Some(next_cursor);
            page = stable_retain_export_page(
                st,
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
        .map_err(|_| RetainExportError::Failed("export sink rejected provider completion"))?;
        Ok((snapshot, exported_records, page_count))
    })();
    if result.is_err() {
        sink.abort();
    }
    result
}

fn delete_remote_memory(
    st: &RetainState,
    subject: &MemorySubject,
    filter: &MemorySubjectStorageKey,
    memory_id: &str,
) -> Result<(), String> {
    let memory_id = memory_id.trim();
    if memory_id.is_empty()
        || memory_id.contains('/')
        || memory_id.contains('?')
        || memory_id.contains('#')
    {
        return Err("memory_id must be an exact RetainDB memory id".into());
    }
    let response = with_retain_headers(
        st.client
            .delete(format!(
                "{}/v1/memory/{}",
                st.base,
                urlencoding_query(memory_id)
            ))
            .query(&scope_query(subject, filter)),
        &st.token,
    )
    .send()
    .map_err(|err| err.to_string())?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("forget {}", response.status()))
    }
}

fn extract_retaindb_file_id(value: &Value) -> Option<String> {
    value
        .pointer("/file/id")
        .or_else(|| value.get("id"))
        .and_then(Value::as_str)
        .filter(|id| !id.trim().is_empty())
        .map(ToString::to_string)
}

fn retain_file_mime_type(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
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
    mime.starts_with("text/")
        || matches!(
            mime.as_str(),
            "application/json" | "application/xml" | "application/yaml" | "application/x-yaml"
        )
}

fn retaindb_ingest_file(
    st: &RetainState,
    subject: &MemorySubject,
    filter: &MemorySubjectStorageKey,
    file_id: &str,
) -> Result<Value, String> {
    let body = scoped_body(subject, filter, json!({}))?;
    let response = with_retain_headers(
        st.client
            .post(format!(
                "{}/v1/files/{}/ingest",
                st.base,
                urlencoding_query(file_id)
            ))
            .json(&body),
        &st.token,
    )
    .send()
    .map_err(|err| err.to_string())?;
    if !response.status().is_success() {
        return Err(format!("ingest file {}", response.status()));
    }
    Ok(response.json().unwrap_or_else(|_| json!({"ok": true})))
}

fn tool_dispatch(
    st: &RetainState,
    subject: &MemorySubject,
    filter: &MemorySubjectStorageKey,
    tool_name: &str,
    args: &Value,
) -> Result<Value, String> {
    if !state_matches(st, subject, filter) {
        return Err("RetainDB request subject does not match initialized subject".into());
    }
    reject_subject_override(args)?;
    match tool_name {
        "retaindb_profile" => {
            let response = with_retain_headers(
                st.client
                    .get(format!(
                        "{}/v1/memory/profile/{}",
                        st.base,
                        urlencoding_query(filter.as_str())
                    ))
                    .query(&scope_query(subject, filter)),
                &st.token,
            )
            .send()
            .map_err(|err| err.to_string())?;
            if !response.status().is_success() {
                return Err(format!("profile {}", response.status()));
            }
            response.json().map_err(|err| err.to_string())
        }
        "retaindb_search" => {
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if query.is_empty() {
                return Err("query is required".into());
            }
            let top_k = args.get("top_k").and_then(Value::as_u64).unwrap_or(8) as u32;
            Ok(json!({"results": remote_memories(st, subject, filter, query, top_k)?}))
        }
        "retaindb_context" => {
            let query = args
                .get("query")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if query.is_empty() {
                return Err("query is required".into());
            }
            let body = scoped_body(
                subject,
                filter,
                json!({
                    "project": filter.as_str(),
                    "query": query,
                    "include_memories": true,
                    "max_tokens": 1200u32,
                }),
            )?;
            let response = with_retain_headers(
                st.client
                    .post(format!("{}/v1/context/query", st.base))
                    .json(&body),
                &st.token,
            )
            .send()
            .map_err(|err| err.to_string())?;
            if !response.status().is_success() {
                return Err(format!("context {}", response.status()));
            }
            response.json().map_err(|err| err.to_string())
        }
        "retaindb_remember" => {
            let content = args
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if content.is_empty() {
                return Err("content is required".into());
            }
            let body = scoped_body(
                subject,
                filter,
                json!({
                    "project": filter.as_str(),
                    "content": redact_sensitive_text(content),
                    "memory_type": args.get("memory_type").and_then(Value::as_str).unwrap_or("factual"),
                    "importance": args.get("importance").and_then(Value::as_f64).unwrap_or(0.7),
                    "write_mode": "sync",
                }),
            )?;
            let response = with_retain_headers(
                st.client.post(format!("{}/v1/memory", st.base)).json(&body),
                &st.token,
            )
            .send()
            .map_err(|err| err.to_string())?;
            if !response.status().is_success() {
                return Err(format!("remember {}", response.status()));
            }
            response.json().map_err(|err| err.to_string())
        }
        "retaindb_forget" => {
            let memory_id = args.get("memory_id").and_then(Value::as_str).unwrap_or("");
            delete_remote_memory(st, subject, filter, memory_id)?;
            Ok(json!({"forgotten": true, "memory_id": memory_id}))
        }
        "retaindb_upload_file" => {
            let local_path = args
                .get("local_path")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .ok_or_else(|| "local_path is required".to_string())?;
            let path = Path::new(local_path);
            let metadata = std::fs::metadata(path).map_err(|err| err.to_string())?;
            if !metadata.is_file() {
                return Err("local_path must be a regular file".into());
            }
            let bytes = std::fs::read(path).map_err(|err| err.to_string())?;
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
                .filter(|path| !path.is_empty())
                .unwrap_or(&filename)
                .to_string();
            let part = Part::bytes(bytes)
                .file_name(filename)
                .mime_str(retain_file_mime_type(path))
                .map_err(|err| err.to_string())?;
            let team_filter = subject.team_id().map(|_| filter.as_str()).unwrap_or("");
            let form = Form::new()
                .part("file", part)
                .text("path", remote_path)
                .text("scope", "USER")
                .text("subject_key", filter.as_str().to_string())
                .text("tenant_id", filter.as_str().to_string())
                .text("principal_id", filter.as_str().to_string())
                .text("team_id", team_filter.to_string())
                .text("session_id", filter.as_str().to_string());
            let response = with_retain_auth_headers(
                st.client
                    .post(format!("{}/v1/files", st.base))
                    .multipart(form),
                &st.token,
            )
            .send()
            .map_err(|err| err.to_string())?;
            if !response.status().is_success() {
                return Err(format!("upload file {}", response.status()));
            }
            let mut value: Value = response.json().map_err(|err| err.to_string())?;
            if args.get("ingest").and_then(Value::as_bool).unwrap_or(false) {
                if let Some(file_id) = extract_retaindb_file_id(&value) {
                    let ingest = retaindb_ingest_file(st, subject, filter, &file_id)?;
                    if let Some(map) = value.as_object_mut() {
                        map.insert("ingest".to_string(), ingest);
                    }
                }
            }
            Ok(value)
        }
        "retaindb_list_files" => {
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .unwrap_or(50)
                .clamp(1, 200);
            let mut query = scope_query(subject, filter);
            query.push(("limit", limit.to_string()));
            if let Some(prefix) = args
                .get("prefix")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|prefix| !prefix.is_empty())
            {
                query.push(("prefix", prefix.to_string()));
            }
            let response = with_retain_headers(
                st.client.get(format!("{}/v1/files", st.base)).query(&query),
                &st.token,
            )
            .send()
            .map_err(|err| err.to_string())?;
            if !response.status().is_success() {
                return Err(format!("list files {}", response.status()));
            }
            response.json().map_err(|err| err.to_string())
        }
        "retaindb_read_file" => {
            let file_id = args
                .get("file_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if file_id.is_empty() {
                return Err("file_id is required".into());
            }
            let id = urlencoding_query(file_id);
            let metadata = with_retain_headers(
                st.client
                    .get(format!("{}/v1/files/{id}", st.base))
                    .query(&scope_query(subject, filter)),
                &st.token,
            )
            .send()
            .map_err(|err| err.to_string())?;
            if !metadata.status().is_success() {
                return Err(format!("read file metadata {}", metadata.status()));
            }
            let metadata: Value = metadata.json().map_err(|err| err.to_string())?;
            let file_info = metadata.get("file").unwrap_or(&metadata);
            let content = with_retain_auth_headers(
                st.client
                    .get(format!("{}/v1/files/{id}/content", st.base))
                    .query(&scope_query(subject, filter)),
                &st.token,
            )
            .send()
            .map_err(|err| err.to_string())?;
            if !content.status().is_success() {
                return Err(format!("read file content {}", content.status()));
            }
            if !retain_file_is_text(file_info) {
                return Ok(
                    json!({"file_id": file_id, "content": Value::Null, "note": "binary file"}),
                );
            }
            let content_bytes = content.bytes().map_err(|err| err.to_string())?;
            let content = String::from_utf8_lossy(&content_bytes);
            let truncated = content.chars().count() > 32_000;
            Ok(json!({
                "file_id": file_id,
                "content": content.chars().take(32_000).collect::<String>(),
                "truncated": truncated,
            }))
        }
        "retaindb_ingest_file" => {
            let file_id = args
                .get("file_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if file_id.is_empty() {
                return Err("file_id is required".into());
            }
            retaindb_ingest_file(st, subject, filter, file_id)
        }
        "retaindb_delete_file" => {
            let file_id = args
                .get("file_id")
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if file_id.is_empty() {
                return Err("file_id is required".into());
            }
            let response = with_retain_headers(
                st.client
                    .delete(format!(
                        "{}/v1/files/{}",
                        st.base,
                        urlencoding_query(file_id)
                    ))
                    .query(&scope_query(subject, filter)),
                &st.token,
            )
            .send()
            .map_err(|err| err.to_string())?;
            if !response.status().is_success() {
                return Err(format!("delete file {}", response.status()));
            }
            Ok(response.json().unwrap_or_else(|_| json!({"deleted": true})))
        }
        _ => Err(format!("Unknown tool: {tool_name}")),
    }
}

fn open_retaindb_queue(path: &Path) -> Result<Connection, String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|err| err.to_string())?;
    }
    let connection = Connection::open(path).map_err(|err| err.to_string())?;
    connection
        .busy_timeout(Duration::from_secs(5))
        .map_err(|err| err.to_string())?;
    Ok(connection)
}

fn ensure_retaindb_queue(path: &Path) -> Result<(), String> {
    let _schema_guard = RETAINDB_QUEUE_SCHEMA_LOCK.lock();
    let connection = open_retaindb_queue(path)?;
    connection
        .execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA busy_timeout=5000;",
        )
        .map_err(|err| err.to_string())?;
    let mut columns = Vec::new();
    {
        let mut statement = connection
            .prepare("PRAGMA table_info(pending)")
            .map_err(|err| err.to_string())?;
        let rows = statement
            .query_map([], |row| row.get::<_, String>(1))
            .map_err(|err| err.to_string())?;
        for row in rows {
            columns.push(row.map_err(|err| err.to_string())?);
        }
    }
    if !columns.is_empty()
        && (!columns.iter().any(|column| column == "payload_ciphertext")
            || columns.iter().any(|column| column == "payload_json")
            || !columns.iter().any(|column| column == "subject_key"))
    {
        // Plaintext, unpartitioned queue records cannot be safely migrated.
        connection
            .execute_batch("DROP TABLE pending")
            .map_err(|err| err.to_string())?;
    }
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS pending (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                subject_key TEXT NOT NULL,
                tenant_filter TEXT NOT NULL,
                principal_filter TEXT NOT NULL,
                team_filter TEXT,
                session_filter TEXT NOT NULL,
                payload_ciphertext TEXT NOT NULL,
                idempotency_key TEXT NOT NULL UNIQUE,
                created_at TEXT NOT NULL,
                last_error TEXT,
                lease_id TEXT,
                lease_expires_at TEXT
            );
            CREATE INDEX IF NOT EXISTS pending_subject_id ON pending(subject_key, id);
            CREATE INDEX IF NOT EXISTS pending_lease ON pending(subject_key, lease_expires_at);",
        )
        .map_err(|err| err.to_string())?;
    Ok(())
}

fn redact_queue_text(text: &str) -> String {
    let redacted = redact_sensitive_text(text);
    let mut characters = redacted.chars();
    let excerpt = characters
        .by_ref()
        .take(RETAINDB_QUEUE_TEXT_LIMIT)
        .collect::<String>();
    if characters.next().is_some() {
        format!("{excerpt} [truncated]")
    } else {
        excerpt
    }
}

fn redact_queue_value(value: &Value) -> Value {
    match value {
        Value::String(text) => Value::String(redact_queue_text(text)),
        Value::Array(values) => Value::Array(values.iter().map(redact_queue_value).collect()),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), redact_queue_value(value)))
                .collect(),
        ),
        value => value.clone(),
    }
}

fn idempotency_material(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(idempotency_material).collect()),
        Value::Object(values) => Value::Object(
            values
                .iter()
                .filter(|(key, _)| key.as_str() != "timestamp")
                .map(|(key, value)| (key.clone(), idempotency_material(value)))
                .collect(),
        ),
        value => value.clone(),
    }
}

fn queue_key(token: &str, subject_key: &str) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(b"hermes-retaindb-queue-key-v2");
    digest.update([0]);
    digest.update(token.as_bytes());
    digest.update([0]);
    digest.update(subject_key.as_bytes());
    digest.finalize().into()
}

fn encrypt_queue_payload(
    token: &str,
    subject_key: &str,
    payload: &Value,
) -> Result<String, String> {
    let plaintext =
        serde_json::to_vec(&redact_queue_value(payload)).map_err(|err| err.to_string())?;
    let cipher = Aes256Gcm::new_from_slice(&queue_key(token, subject_key))
        .map_err(|_| "invalid RetainDB queue key".to_string())?;
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
    let aad = format!("{RETAINDB_QUEUE_AAD_PREFIX}:{subject_key}");
    let ciphertext = cipher
        .encrypt(
            &nonce,
            Payload {
                msg: &plaintext,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| "RetainDB queue encryption failed".to_string())?;
    let mut envelope = nonce.to_vec();
    envelope.extend_from_slice(&ciphertext);
    Ok(BASE64_STANDARD.encode(envelope))
}

fn decrypt_queue_payload(
    token: &str,
    subject_key: &str,
    ciphertext: &str,
) -> Result<Value, String> {
    let envelope = BASE64_STANDARD
        .decode(ciphertext)
        .map_err(|_| "RetainDB queue ciphertext is invalid".to_string())?;
    if envelope.len() <= RETAINDB_QUEUE_NONCE_BYTES {
        return Err("RetainDB queue ciphertext is truncated".into());
    }
    let (nonce, ciphertext) = envelope.split_at(RETAINDB_QUEUE_NONCE_BYTES);
    let cipher = Aes256Gcm::new_from_slice(&queue_key(token, subject_key))
        .map_err(|_| "invalid RetainDB queue key".to_string())?;
    let aad = format!("{RETAINDB_QUEUE_AAD_PREFIX}:{subject_key}");
    let plaintext = cipher
        .decrypt(
            Nonce::from_slice(nonce),
            Payload {
                msg: ciphertext,
                aad: aad.as_bytes(),
            },
        )
        .map_err(|_| "RetainDB queue decrypt failed".to_string())?;
    serde_json::from_slice(&plaintext).map_err(|_| "RetainDB queue payload is invalid".to_string())
}

fn queue_idempotency_key(subject_key: &str, payload: &Value) -> Result<String, String> {
    let mut digest = Sha256::new();
    digest.update(b"hermes-retaindb-ingest-v2");
    digest.update([0]);
    digest.update(subject_key.as_bytes());
    digest.update([0]);
    digest.update(
        serde_json::to_vec(&idempotency_material(&redact_queue_value(payload)))
            .map_err(|err| err.to_string())?,
    );
    Ok(format!("{:x}", digest.finalize()))
}

fn enqueue_retaindb_turn(st: &RetainState, payload: &Value) -> Result<i64, String> {
    ensure_retaindb_queue(&st.queue_db_path)?;
    let key = st.filter.as_str();
    let encrypted = encrypt_queue_payload(&st.token, key, payload)?;
    let idempotency_key = queue_idempotency_key(key, payload)?;
    let connection = open_retaindb_queue(&st.queue_db_path)?;
    let now = chrono::Utc::now().to_rfc3339();
    let team_filter = payload
        .get("team_id")
        .and_then(Value::as_str)
        .map(ToString::to_string);
    let changed = connection
        .execute(
            "INSERT INTO pending (
                subject_key, tenant_filter, principal_filter, team_filter, session_filter,
                payload_ciphertext, idempotency_key, created_at
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ON CONFLICT(idempotency_key) DO NOTHING",
            params![
                key,
                key,
                key,
                team_filter,
                key,
                encrypted,
                idempotency_key,
                now,
            ],
        )
        .map_err(|err| err.to_string())?;
    if changed == 1 {
        return Ok(connection.last_insert_rowid());
    }
    connection
        .query_row(
            "SELECT id FROM pending WHERE subject_key = ?1 AND idempotency_key = ?2",
            params![key, idempotency_key],
            |row| row.get(0),
        )
        .map_err(|err| err.to_string())
}

fn pending_retaindb_rows(st: &RetainState) -> Result<Vec<RetainQueueRow>, String> {
    let connection = open_retaindb_queue(&st.queue_db_path)?;
    let mut statement = connection
        .prepare(
            "SELECT id, payload_ciphertext
             FROM pending
             WHERE subject_key = ?1
             ORDER BY id ASC
             LIMIT 200",
        )
        .map_err(|err| err.to_string())?;
    let rows = statement
        .query_map(params![st.filter.as_str()], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(|err| err.to_string())?;
    let mut pending = Vec::new();
    for row in rows {
        let (id, ciphertext) = row.map_err(|err| err.to_string())?;
        pending.push(RetainQueueRow {
            id,
            payload: decrypt_queue_payload(&st.token, st.filter.as_str(), &ciphertext)?,
        });
    }
    Ok(pending)
}

fn count_retaindb_queue_rows(st: &RetainState) -> Result<u64, String> {
    let connection = open_retaindb_queue(&st.queue_db_path)?;
    connection
        .query_row(
            "SELECT COUNT(*) FROM pending WHERE subject_key = ?1",
            params![st.filter.as_str()],
            |row| row.get(0),
        )
        .map_err(|err| err.to_string())
}

fn claim_queue_row(st: &RetainState, row_id: i64, lease_id: &str) -> Result<bool, String> {
    let connection = open_retaindb_queue(&st.queue_db_path)?;
    let now = chrono::Utc::now();
    let expires = (now + chrono::Duration::minutes(2)).to_rfc3339();
    let changed = connection
        .execute(
            "UPDATE pending
             SET lease_id = ?1, lease_expires_at = ?2
             WHERE id = ?3 AND subject_key = ?4
               AND (lease_id IS NULL OR lease_expires_at IS NULL OR lease_expires_at < ?5)",
            params![
                lease_id,
                expires,
                row_id,
                st.filter.as_str(),
                now.to_rfc3339(),
            ],
        )
        .map_err(|err| err.to_string())?;
    Ok(changed == 1)
}

fn queued_payload_for_ingest(st: &RetainState, payload: &Value) -> Result<Value, String> {
    if payload.get("subject_key").and_then(Value::as_str) != Some(st.filter.as_str()) {
        return Err("RetainDB queue subject mismatch".into());
    }
    let mut body = json!({
        "project": st.filter.as_str(),
        "messages": payload.get("messages").cloned().unwrap_or_else(|| json!([])),
        "write_mode": "sync",
    });
    let body_map = body.as_object_mut().expect("fixed queue body is object");
    for key in [
        "subject",
        "subject_key",
        "tenant_id",
        "principal_id",
        "team_id",
        "session_id",
    ] {
        if let Some(value) = payload.get(key) {
            body_map.insert(key.to_string(), value.clone());
        }
    }
    Ok(body)
}

fn delete_claimed_queue_row(st: &RetainState, row_id: i64, lease_id: &str) -> Result<(), String> {
    let connection = open_retaindb_queue(&st.queue_db_path)?;
    connection
        .execute(
            "DELETE FROM pending WHERE id = ?1 AND subject_key = ?2 AND lease_id = ?3",
            params![row_id, st.filter.as_str(), lease_id],
        )
        .map_err(|err| err.to_string())?;
    Ok(())
}

fn release_queue_row(st: &RetainState, row_id: i64, lease_id: &str, error: &str) {
    if let Ok(connection) = open_retaindb_queue(&st.queue_db_path) {
        let _ = connection.execute(
            "UPDATE pending
             SET last_error = ?1, lease_id = NULL, lease_expires_at = NULL
             WHERE id = ?2 AND subject_key = ?3 AND lease_id = ?4",
            params![
                redact_sensitive_text(error),
                row_id,
                st.filter.as_str(),
                lease_id,
            ],
        );
    }
}

fn flush_retaindb_queue_row(st: &RetainState, row: RetainQueueRow) -> Result<(), String> {
    let lease_id = uuid::Uuid::new_v4().to_string();
    if !claim_queue_row(st, row.id, &lease_id)? {
        return Ok(());
    }
    let mut body = queued_payload_for_ingest(st, &row.payload)?;
    body["idempotency_key"] = json!(queue_idempotency_key(st.filter.as_str(), &row.payload)?);
    let response = with_retain_headers(
        st.client
            .post(format!("{}/v1/memory/ingest/session", st.base))
            .json(&body),
        &st.token,
    )
    .send()
    .map_err(|err| err.to_string())?;
    if !response.status().is_success() {
        let error = format!("ingest {}", response.status());
        release_queue_row(st, row.id, &lease_id, &error);
        return Err(error);
    }
    delete_claimed_queue_row(st, row.id, &lease_id)
}

fn drain_retaindb_queue(st: &RetainState) {
    let rows = match pending_retaindb_rows(st) {
        Ok(rows) => rows,
        Err(error) => {
            tracing::warn!(provider = "retaindb", error = %error, "unable to read subject queue");
            return;
        }
    };
    for row in rows {
        if let Err(error) = flush_retaindb_queue_row(st, row) {
            tracing::debug!(provider = "retaindb", error = %error, "subject queue row remains pending");
            break;
        }
    }
}

fn delete_retaindb_queue_subject(st: &RetainState) -> Result<u64, String> {
    ensure_retaindb_queue(&st.queue_db_path)?;
    let connection = open_retaindb_queue(&st.queue_db_path)?;
    let deleted = connection
        .execute(
            "DELETE FROM pending WHERE subject_key = ?1",
            params![st.filter.as_str()],
        )
        .map_err(|err| err.to_string())?;
    Ok(deleted as u64)
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

fn governance_failure(
    provider: &str,
    operation: MemoryGovernanceOperation,
    filter: &MemorySubjectStorageKey,
    actor: Option<&MemoryAuditActor>,
    error: &str,
) -> MemoryProviderGovernanceError {
    if let Some(actor) = actor {
        tracing::warn!(
            provider,
            operation = operation.as_str(),
            subject_key = filter.as_str(),
            actor = actor.opaque_id().as_str(),
            error = %redact_sensitive_text(error),
            "subject-scoped memory governance failed"
        );
    } else {
        tracing::warn!(
            provider,
            operation = operation.as_str(),
            subject_key = filter.as_str(),
            error = %redact_sensitive_text(error),
            "subject-scoped memory governance failed"
        );
    }
    MemoryProviderGovernanceError::failed(provider, operation)
}

pub struct RetainDbMemoryPlugin {
    state: Mutex<Option<RetainState>>,
    prefetch_ctx: Arc<Mutex<String>>,
}

impl RetainDbMemoryPlugin {
    pub fn new() -> Self {
        Self {
            state: Mutex::new(None),
            prefetch_ctx: Arc::new(Mutex::new(String::new())),
        }
    }

    fn state_for(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
    ) -> Result<RetainState, String> {
        let state = self
            .state
            .lock()
            .clone()
            .ok_or_else(|| "RetainDB not initialized".to_string())?;
        if state_matches(&state, subject, filter) {
            Ok(state)
        } else {
            Err("RetainDB request subject does not match initialized subject".into())
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
            .trim()
            .is_empty()
    }

    fn initialize(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        hermes_home: &str,
    ) -> Result<(), String> {
        let token = std::env::var("RETAINDB_API_KEY").unwrap_or_default();
        if token.trim().is_empty() {
            *self.state.lock() = None;
            return Ok(());
        }
        let client = Client::builder()
            .timeout(Duration::from_secs(45))
            .build()
            .map_err(|error| format!("RetainDB client build failed: {error}"))?;
        let state = RetainState {
            client,
            base: std::env::var("RETAINDB_BASE_URL")
                .unwrap_or_else(|_| DEFAULT_BASE.to_string())
                .trim_end_matches('/')
                .to_string(),
            token,
            subject: subject.clone(),
            filter: filter.clone(),
            queue_db_path: Path::new(hermes_home).join("retaindb_queue.db"),
        };
        ensure_retaindb_queue(&state.queue_db_path)?;
        *self.state.lock() = Some(state.clone());
        *self.prefetch_ctx.lock() = String::new();
        std::thread::spawn(move || drain_retaindb_queue(&state));
        Ok(())
    }

    fn system_prompt_block(&self) -> String {
        if self.state.lock().is_none() {
            return String::new();
        }
        "# RetainDB Memory\nActive in the authenticated memory subject only. Use retaindb_search, retaindb_remember, retaindb_profile, retaindb_context, and subject-scoped file tools.".to_string()
    }

    fn queue_prefetch(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        query: &str,
    ) {
        let Ok(state) = self.state_for(subject, filter) else {
            return;
        };
        let query = query.trim();
        if query.is_empty() {
            return;
        }
        let query = query.to_string();
        let subject = subject.clone();
        let filter = filter.clone();
        let out = Arc::clone(&self.prefetch_ctx);
        std::thread::spawn(move || {
            if let Ok(records) = remote_memories(&state, &subject, &filter, &query, 8) {
                let lines = records
                    .iter()
                    .filter_map(|record| {
                        record
                            .get("content")
                            .or_else(|| record.get("memory"))
                            .or_else(|| record.get("text"))
                            .and_then(Value::as_str)
                            .map(str::trim)
                            .filter(|text| !text.is_empty())
                            .map(|text| format!("- {text}"))
                    })
                    .collect::<Vec<_>>();
                if !lines.is_empty() {
                    *out.lock() = format!("[RetainDB Context]\n{}", lines.join("\n"));
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
        if self.state_for(subject, filter).is_err() {
            return String::new();
        }
        let mut value = self.prefetch_ctx.lock();
        std::mem::take(&mut *value)
    }

    fn sync_turn(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        user_content: &str,
        assistant_content: &str,
    ) {
        let Ok(state) = self.state_for(subject, filter) else {
            return;
        };
        let user_content = user_content.trim();
        let assistant_content = assistant_content.trim();
        if user_content.is_empty() || assistant_content.is_empty() {
            return;
        }
        let now = chrono::Utc::now().to_rfc3339();
        let payload = match scoped_body(
            subject,
            filter,
            json!({
                "messages": [
                    {"role": "user", "content": redact_sensitive_text(user_content), "timestamp": now},
                    {"role": "assistant", "content": redact_sensitive_text(assistant_content), "timestamp": now}
                ]
            }),
        ) {
            Ok(payload) => payload,
            Err(error) => {
                tracing::warn!(provider = "retaindb", error = %error, "unable to scope queued turn");
                return;
            }
        };
        let state_for_flush = state.clone();
        std::thread::spawn(move || {
            if let Ok(row_id) = enqueue_retaindb_turn(&state_for_flush, &payload) {
                let row = pending_retaindb_rows(&state_for_flush)
                    .ok()
                    .and_then(|rows| rows.into_iter().find(|row| row.id == row_id));
                if let Some(row) = row {
                    let _ = flush_retaindb_queue_row(&state_for_flush, row);
                }
                drain_retaindb_queue(&state_for_flush);
            }
        });
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

    fn handle_tool_call(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        tool_name: &str,
        args: &Value,
    ) -> String {
        let state = match self.state_for(subject, filter) {
            Ok(state) => state,
            Err(error) => return json!({"error": error}).to_string(),
        };
        match tool_dispatch(&state, subject, filter, tool_name, args) {
            Ok(value) => value.to_string(),
            Err(error) => json!({"error": error}).to_string(),
        }
    }

    fn shutdown(&self) {
        *self.state.lock() = None;
        *self.prefetch_ctx.lock() = String::new();
    }

    fn get_config_schema(&self) -> Option<Value> {
        Some(json!([
            {"key": "api_key", "description": "RetainDB API key", "secret": true, "env_var": "RETAINDB_API_KEY", "url": "https://retaindb.com"},
            {"key": "base_url", "description": "API base URL", "default": DEFAULT_BASE}
        ]))
    }

    fn export_subject(
        &self,
        subject: &MemorySubject,
        filter: &MemorySubjectStorageKey,
        actor: &MemoryAuditActor,
        sink: &mut dyn MemoryExportSink,
    ) -> Result<MemoryExportReceipt, MemoryProviderGovernanceError> {
        let state = match self.state_for(subject, filter) {
            Ok(state) => state,
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
        match stream_retain_export(&state, subject, filter, sink) {
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
            Err(RetainExportError::Unsupported) => {
                sink.abort();
                Err(MemoryProviderGovernanceError::unsupported(
                    self.name(),
                    MemoryGovernanceOperation::Export,
                ))
            }
            Err(RetainExportError::Failed(error)) => Err(governance_failure(
                self.name(),
                MemoryGovernanceOperation::Export,
                filter,
                Some(actor),
                error,
            )),
        }
    }

    // RetainDB's configured endpoint is semantic `/v1/memory/search`, not a
    // stable subject inventory. Deleting the local queue before a remote
    // exhaustive delete/readback proof would make an incomplete erase look done.
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

    // The semantic search response is capped and lacks stable ordering and
    // complete creation metadata, so it cannot support retention enforcement.
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

    // Queue rows plus a bounded semantic result set are insufficient evidence
    // that the remote partition is absent.
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
    use std::thread;

    #[derive(Default)]
    struct TestExportSink {
        pages: Vec<MemoryExportPage>,
        completion: Option<MemoryExportCompletion>,
        aborted: bool,
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

        fn abort(&mut self) {
            self.aborted = true;
        }

        fn commit(self: Box<Self>) -> Result<MemoryExportArtifact, MemoryExportSinkError> {
            Err(MemoryExportSinkError::Incomplete)
        }
    }

    fn subject(principal: &str, session: &str) -> (MemorySubject, MemorySubjectStorageKey) {
        let subject = MemorySubject::new(
            MemoryTenantId::new("tenant-a").expect("tenant"),
            MemoryPrincipalId::new(principal).expect("principal"),
            None,
            MemorySessionId::new(session).expect("session"),
            MemorySurface::Gateway,
            MemoryVisibility::Private,
        )
        .expect("subject");
        let filter = subject.storage_key(&MemorySubjectDerivationKey::from_bytes([9; 32]));
        (subject, filter)
    }

    fn state(
        base: String,
        subject: &MemorySubject,
        filter: MemorySubjectStorageKey,
        queue: PathBuf,
    ) -> RetainState {
        RetainState {
            client: Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .expect("client"),
            base,
            token: "test-token".to_string(),
            subject: subject.clone(),
            filter,
            queue_db_path: queue,
        }
    }

    fn read_request(stream: &mut std::net::TcpStream) -> String {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .expect("timeout");
        let mut bytes = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let read = stream.read(&mut chunk).expect("read");
            if read == 0 {
                break;
            }
            bytes.extend_from_slice(&chunk[..read]);
            if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                let headers = String::from_utf8_lossy(&bytes[..end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if bytes.len() >= end + 4 + content_length {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&bytes).to_string()
    }

    fn mock_server(responses: Vec<(u16, &'static str)>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept");
                sender.send(read_request(&mut stream)).expect("request");
                let reason = if (200..300).contains(&status) {
                    "OK"
                } else {
                    "Error"
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).expect("response");
            }
        });
        (format!("http://{address}"), receiver)
    }

    fn owned_mock_server(responses: Vec<(u16, String)>) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let address = listener.local_addr().expect("address");
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            for (status, body) in responses {
                let (mut stream, _) = listener.accept().expect("accept");
                sender.send(read_request(&mut stream)).expect("request");
                let reason = if (200..300).contains(&status) {
                    "OK"
                } else {
                    "Error"
                };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                stream.write_all(response.as_bytes()).expect("response");
            }
        });
        (format!("http://{address}"), receiver)
    }

    #[test]
    fn queue_is_subject_partitioned_encrypted_and_redacted() {
        let temp = tempfile::tempdir().expect("temp");
        let (alice, alice_filter) = subject("alice", "session-a");
        let (bob, bob_filter) = subject("bob", "session-b");
        let queue = temp.path().join("queue.sqlite");
        let alice_state = state(
            "http://127.0.0.1:9".to_string(),
            &alice,
            alice_filter.clone(),
            queue.clone(),
        );
        let bob_state = state(
            "http://127.0.0.1:9".to_string(),
            &bob,
            bob_filter.clone(),
            queue.clone(),
        );
        let secret = "api_key=sk-retaindb-secret-123456";
        let alice_payload = scoped_body(
            &alice,
            &alice_filter,
            json!({"messages":[{"content":secret}]}),
        )
        .expect("scope");
        let bob_payload = scoped_body(
            &bob,
            &bob_filter,
            json!({"messages":[{"content":"bob memory"}]}),
        )
        .expect("scope");
        enqueue_retaindb_turn(&alice_state, &alice_payload).expect("alice row");
        enqueue_retaindb_turn(&bob_state, &bob_payload).expect("bob row");
        assert_eq!(
            pending_retaindb_rows(&alice_state)
                .expect("alice rows")
                .len(),
            1
        );
        assert_eq!(
            pending_retaindb_rows(&bob_state).expect("bob rows").len(),
            1
        );
        let database = std::fs::read(&queue).expect("database");
        let raw = String::from_utf8_lossy(&database);
        assert!(!raw.contains(secret));
        assert!(!raw.contains("bob memory"));
    }

    #[test]
    fn queue_concurrent_writers_keep_all_subject_rows() {
        let temp = tempfile::tempdir().expect("temp");
        let (subject, filter) = subject("alice", "session-a");
        let state = Arc::new(state(
            "http://127.0.0.1:9".to_string(),
            &subject,
            filter.clone(),
            temp.path().join("queue.sqlite"),
        ));
        let mut writers = Vec::new();
        for index in 0..12 {
            let state = Arc::clone(&state);
            let subject = subject.clone();
            let filter = filter.clone();
            writers.push(thread::spawn(move || {
                let payload = scoped_body(
                    &subject,
                    &filter,
                    json!({"messages":[{"content":format!("turn-{index}")}]}),
                )
                .expect("scope");
                enqueue_retaindb_turn(&state, &payload).expect("insert");
            }));
        }
        for writer in writers {
            writer.join().expect("writer");
        }
        assert_eq!(pending_retaindb_rows(&state).expect("rows").len(), 12);
    }

    #[test]
    fn scoped_tool_rejects_victim_identity_override_and_carries_opaque_filter() {
        let temp = tempfile::tempdir().expect("temp");
        let (subject, filter) = subject("alice", "session-a");
        let (base, requests) = mock_server(vec![(200, r#"{"results":[]}"#)]);
        let state = state(
            base,
            &subject,
            filter.clone(),
            temp.path().join("queue.sqlite"),
        );
        let rejected = tool_dispatch(
            &state,
            &subject,
            &filter,
            "retaindb_search",
            &json!({"query":"x", "user_id":"victim"}),
        )
        .expect_err("override rejected");
        assert!(rejected.contains("cannot be overridden"));
        let value = tool_dispatch(
            &state,
            &subject,
            &filter,
            "retaindb_search",
            &json!({"query":"x"}),
        )
        .expect("search");
        assert_eq!(value["results"], json!([]));
        let request = requests
            .recv_timeout(Duration::from_secs(2))
            .expect("request");
        assert!(request.contains(filter.as_str()));
        assert!(!request.contains("alice"));
    }

    #[test]
    fn bound_state_rejects_a_foreign_subject_with_the_bound_filter() {
        let temp = tempfile::tempdir().expect("temp");
        let (alice, filter) = subject("alice", "session-a");
        let (bob, _bob_filter) = subject("bob", "session-b");
        let state = state(
            "http://127.0.0.1:9".to_string(),
            &alice,
            filter.clone(),
            temp.path().join("queue.sqlite"),
        );
        let error = tool_dispatch(
            &state,
            &bob,
            &filter,
            "retaindb_search",
            &json!({"query":"x"}),
        )
        .expect_err("foreign subject");
        assert!(error.contains("does not match initialized subject"));
    }

    #[test]
    fn export_streams_only_bound_subject_records() {
        let temp = tempfile::tempdir().expect("temp");
        let (subject, filter) = subject("alice", "session-a");
        let (base, requests) = mock_server(vec![(
            200,
            r#"{"snapshot":"snapshot-a","revision":"revision-a","results":[{"id":"a"},{"id":"b"}]}"#,
        )]);
        let plugin = RetainDbMemoryPlugin::new();
        *plugin.state.lock() = Some(state(
            base,
            &subject,
            filter.clone(),
            temp.path().join("queue.sqlite"),
        ));
        let actor = MemorySubjectDerivationKey::from_bytes([5; 32]).actor_for_subject(&subject);
        let mut sink = TestExportSink::default();
        let receipt = plugin
            .export_subject(&subject, &filter, &actor, &mut sink)
            .expect("export");
        assert_eq!(receipt.exported_records, 2);
        assert_eq!(sink.pages.len(), 1);
        assert_eq!(sink.pages[0].records.len(), 2);
        assert!(sink.completion.as_ref().unwrap().exhausted);
        assert!(requests
            .recv_timeout(Duration::from_secs(2))
            .expect("request")
            .contains(filter.as_str()));
    }

    #[test]
    fn governance_rejects_over_cap_semantic_inventory_without_erasing_the_queue() {
        let temp = tempfile::tempdir().expect("temp");
        let (subject, filter) = subject("alice", "session-a");
        let semantic_result = json!({
            "results": (0..=MAX_REMOTE_RECORDS)
                .map(|index| json!({"id": format!("nonmatching-{index}")}))
                .collect::<Vec<_>>()
        })
        .to_string();
        let (base, requests) = owned_mock_server(vec![(200, semantic_result)]);
        let plugin = RetainDbMemoryPlugin::new();
        let state = state(
            base,
            &subject,
            filter.clone(),
            temp.path().join("queue.sqlite"),
        );
        enqueue_retaindb_turn(
            &state,
            &scoped_body(
                &subject,
                &filter,
                json!({"messages":[{"content":"pending"}]}),
            )
            .expect("payload"),
        )
        .expect("queue row");
        *plugin.state.lock() = Some(state.clone());
        let actor = MemorySubjectDerivationKey::from_bytes([5; 32]).actor_for_subject(&subject);
        assert!(matches!(
            plugin.forget_subject(&subject, &filter, &actor),
            Err(MemoryProviderGovernanceError::Unsupported {
                operation: MemoryGovernanceOperation::Forget,
                ..
            })
        ));
        assert_eq!(pending_retaindb_rows(&state).expect("rows").len(), 1);
        let policy = MemoryRetentionPolicy::new(None, Some(1)).expect("policy");
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
}
