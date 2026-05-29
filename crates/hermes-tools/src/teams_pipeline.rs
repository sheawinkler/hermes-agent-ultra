//! Rust-native Microsoft Teams meeting summary pipeline.
//!
//! This module ports the Teams pipeline runtime into Rust. The Python plugin
//! files are reference material only; job state, Graph access, transcript-first
//! orchestration, recording/STT fallback, summaries, and sink bookkeeping live
//! here.

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use reqwest::{Client, Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::process::Command;
use uuid::Uuid;

use hermes_config::hermes_home;
use hermes_core::{ToolError, ToolHandler};

pub const DEFAULT_TEAMS_PIPELINE_STORE_FILENAME: &str = "teams_pipeline_store.json";

pub const TERMINAL_PIPELINE_STATES: &[&str] = &["completed", "failed", "retry_scheduled"];
pub const ACTIVE_PIPELINE_STATES: &[&str] = &[
    "received",
    "resolving_meeting",
    "fetching_transcript",
    "downloading_recording",
    "transcribing_audio",
    "summarizing",
    "writing_notion",
    "writing_linear",
    "sending_teams",
];

#[derive(Debug, thiserror::Error)]
pub enum TeamsPipelineError {
    #[error("Teams pipeline config error: {0}")]
    Config(String),
    #[error("Teams pipeline invalid data: {0}")]
    Invalid(String),
    #[error("Teams pipeline store error: {0}")]
    Store(String),
    #[error("Teams pipeline IO error: {0}")]
    Io(String),
    #[error("Teams pipeline JSON error: {0}")]
    Json(String),
    #[error("Teams pipeline retryable error: {0}")]
    Retryable(String),
    #[error("Teams pipeline artifact unavailable: {0}")]
    ArtifactNotFound(String),
    #[error("Teams pipeline sink error: {0}")]
    Sink(String),
    #[error("Microsoft Graph HTTP {status}: {message}")]
    Graph { status: u16, message: String },
}

impl TeamsPipelineError {
    fn is_retryable(&self) -> bool {
        matches!(self, Self::Retryable(_) | Self::ArtifactNotFound(_))
    }
}

impl From<std::io::Error> for TeamsPipelineError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value.to_string())
    }
}

impl From<serde_json::Error> for TeamsPipelineError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value.to_string())
    }
}

impl From<ToolError> for TeamsPipelineError {
    fn from(value: ToolError) -> Self {
        Self::Retryable(value.to_string())
    }
}

pub type TeamsPipelineResult<T> = Result<T, TeamsPipelineError>;

fn skip_map(map: &Map<String, Value>) -> bool {
    map.is_empty()
}

fn utc_now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

fn env_nonempty(key: &str) -> Option<String> {
    std::env::var(key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn parse_bool(value: Option<&Value>, default: bool) -> bool {
    match value {
        Some(Value::Bool(value)) => *value,
        Some(Value::String(value)) => match value.trim().to_ascii_lowercase().as_str() {
            "1" | "true" | "yes" | "on" => true,
            "0" | "false" | "no" | "off" => false,
            _ => default,
        },
        Some(Value::Number(value)) => value.as_i64().map(|n| n != 0).unwrap_or(default),
        _ => default,
    }
}

fn parse_usize(value: Option<&Value>, default: usize) -> usize {
    match value {
        Some(Value::Number(value)) => value.as_u64().map(|n| n as usize).unwrap_or(default),
        Some(Value::String(value)) => value.trim().parse::<usize>().unwrap_or(default),
        _ => default,
    }
}

fn value_string(value: Option<&Value>) -> Option<String> {
    value.and_then(value_to_string)
}

fn value_to_string(value: &Value) -> Option<String> {
    match value {
        Value::String(value) => Some(value.trim().to_string()).filter(|v| !v.is_empty()),
        Value::Number(value) => Some(value.to_string()),
        Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn object_field<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    value.as_object().and_then(|object| object.get(key))
}

fn parse_datetime_utc(value: &str) -> Option<DateTime<Utc>> {
    let text = value.trim();
    if text.is_empty() {
        return None;
    }
    DateTime::parse_from_rfc3339(text)
        .map(|dt| dt.with_timezone(&Utc))
        .ok()
}

fn canonical_json(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(value) => value.to_string(),
        Value::Number(value) => value.to_string(),
        Value::String(value) => serde_json::to_string(value).unwrap_or_else(|_| "\"\"".into()),
        Value::Array(items) => {
            let body = items
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",");
            format!("[{body}]")
        }
        Value::Object(object) => {
            let mut keys = object.keys().collect::<Vec<_>>();
            keys.sort();
            let body = keys
                .into_iter()
                .map(|key| {
                    let key_json = serde_json::to_string(key).unwrap_or_else(|_| "\"\"".into());
                    let value_json = object.get(key).map(canonical_json).unwrap_or_default();
                    format!("{key_json}:{value_json}")
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{body}}}")
        }
    }
}

fn strip_empty_object_keys(mut value: Value) -> Value {
    if let Value::Object(object) = &mut value {
        let keys = object
            .iter()
            .filter_map(|(key, value)| {
                if value.is_null()
                    || value.as_array().map(Vec::is_empty).unwrap_or(false)
                    || value.as_object().map(Map::is_empty).unwrap_or(false)
                {
                    Some(key.clone())
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for key in keys {
            object.remove(&key);
        }
    }
    value
}

fn path_percent_encode(segment: &str) -> String {
    let mut output = String::new();
    for byte in segment.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(*byte, b'-' | b'_' | b'.' | b'~') {
            output.push(*byte as char);
        } else {
            output.push_str(&format!("%{:02X}", byte));
        }
    }
    output
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GraphSubscription {
    #[serde(default, alias = "id")]
    pub subscription_id: String,
    #[serde(default)]
    pub resource: String,
    #[serde(default, alias = "changeType")]
    pub change_type: String,
    #[serde(default, alias = "notificationUrl")]
    pub notification_url: String,
    #[serde(default, alias = "expirationDateTime")]
    pub expiration_datetime: String,
    #[serde(
        default,
        alias = "clientState",
        skip_serializing_if = "Option::is_none"
    )]
    pub client_state: Option<String>,
    #[serde(
        default,
        alias = "latestRenewalAt",
        skip_serializing_if = "Option::is_none"
    )]
    pub latest_renewal_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
}

impl GraphSubscription {
    pub fn from_value(value: Value) -> TeamsPipelineResult<Self> {
        let record: Self = serde_json::from_value(value)?;
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> TeamsPipelineResult<()> {
        if self.subscription_id.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "GraphSubscription.subscription_id is required.".into(),
            ));
        }
        if self.resource.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "GraphSubscription.resource is required.".into(),
            ));
        }
        if self.change_type.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "GraphSubscription.change_type is required.".into(),
            ));
        }
        if self.notification_url.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "GraphSubscription.notification_url is required.".into(),
            ));
        }
        if self.expiration_datetime.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "GraphSubscription.expiration_datetime is required.".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamsMeetingRef {
    #[serde(default, alias = "id")]
    pub meeting_id: String,
    #[serde(
        default,
        alias = "organizerUserId",
        skip_serializing_if = "Option::is_none"
    )]
    pub organizer_user_id: Option<String>,
    #[serde(default, alias = "joinWebUrl", skip_serializing_if = "Option::is_none")]
    pub join_web_url: Option<String>,
    #[serde(
        default,
        alias = "calendarEventId",
        skip_serializing_if = "Option::is_none"
    )]
    pub calendar_event_id: Option<String>,
    #[serde(default, alias = "threadId", skip_serializing_if = "Option::is_none")]
    pub thread_id: Option<String>,
    #[serde(default, alias = "tenantId", skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<String>,
    #[serde(default, skip_serializing_if = "skip_map")]
    pub metadata: Map<String, Value>,
}

impl TeamsMeetingRef {
    pub fn from_value(value: Value) -> TeamsPipelineResult<Self> {
        let record: Self = serde_json::from_value(value)?;
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> TeamsPipelineResult<()> {
        if self.meeting_id.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "TeamsMeetingRef.meeting_id is required.".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MeetingArtifact {
    #[serde(default, alias = "artifactType")]
    pub artifact_type: String,
    #[serde(default, alias = "id")]
    pub artifact_id: String,
    #[serde(
        default,
        alias = "displayName",
        alias = "name",
        skip_serializing_if = "Option::is_none"
    )]
    pub display_name: Option<String>,
    #[serde(
        default,
        alias = "contentType",
        skip_serializing_if = "Option::is_none"
    )]
    pub content_type: Option<String>,
    #[serde(
        default,
        alias = "sourceUrl",
        alias = "webUrl",
        skip_serializing_if = "Option::is_none"
    )]
    pub source_url: Option<String>,
    #[serde(
        default,
        alias = "downloadUrl",
        alias = "@microsoft.graph.downloadUrl",
        skip_serializing_if = "Option::is_none"
    )]
    pub download_url: Option<String>,
    #[serde(
        default,
        alias = "createdDateTime",
        skip_serializing_if = "Option::is_none"
    )]
    pub created_at: Option<String>,
    #[serde(
        default,
        alias = "availableDateTime",
        alias = "lastModifiedDateTime",
        skip_serializing_if = "Option::is_none"
    )]
    pub available_at: Option<String>,
    #[serde(default, alias = "size", skip_serializing_if = "Option::is_none")]
    pub size_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "skip_map")]
    pub metadata: Map<String, Value>,
}

impl MeetingArtifact {
    pub fn from_value(value: Value) -> TeamsPipelineResult<Self> {
        let record: Self = serde_json::from_value(value)?;
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> TeamsPipelineResult<()> {
        if !matches!(
            self.artifact_type.as_str(),
            "transcript" | "recording" | "call_record"
        ) {
            return Err(TeamsPipelineError::Invalid(
                "MeetingArtifact.artifact_type must be transcript, recording, or call_record."
                    .into(),
            ));
        }
        if self.artifact_id.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "MeetingArtifact.artifact_id is required.".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamsMeetingSummaryPayload {
    pub meeting_ref: TeamsMeetingRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, alias = "startTime", skip_serializing_if = "Option::is_none")]
    pub start_time: Option<String>,
    #[serde(default, alias = "endTime", skip_serializing_if = "Option::is_none")]
    pub end_time: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub participants: Vec<String>,
    #[serde(
        default,
        alias = "transcriptText",
        skip_serializing_if = "Option::is_none"
    )]
    pub transcript_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, alias = "keyDecisions", skip_serializing_if = "Vec::is_empty")]
    pub key_decisions: Vec<String>,
    #[serde(default, alias = "actionItems", skip_serializing_if = "Vec::is_empty")]
    pub action_items: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub risks: Vec<String>,
    #[serde(default, alias = "callMetrics", skip_serializing_if = "skip_map")]
    pub call_metrics: Map<String, Value>,
    #[serde(
        default,
        alias = "sourceArtifacts",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub source_artifacts: Vec<MeetingArtifact>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub confidence: Option<String>,
    #[serde(
        default,
        alias = "confidenceNotes",
        skip_serializing_if = "Option::is_none"
    )]
    pub confidence_notes: Option<String>,
    #[serde(
        default,
        alias = "notionTarget",
        skip_serializing_if = "Option::is_none"
    )]
    pub notion_target: Option<String>,
    #[serde(
        default,
        alias = "linearTarget",
        skip_serializing_if = "Option::is_none"
    )]
    pub linear_target: Option<String>,
    #[serde(
        default,
        alias = "teamsTarget",
        skip_serializing_if = "Option::is_none"
    )]
    pub teams_target: Option<String>,
}

impl TeamsMeetingSummaryPayload {
    pub fn from_value(value: Value) -> TeamsPipelineResult<Self> {
        Ok(serde_json::from_value(value)?)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TeamsMeetingPipelineJob {
    #[serde(default, alias = "jobId")]
    pub job_id: String,
    #[serde(default, alias = "eventId")]
    pub event_id: String,
    #[serde(default, alias = "sourceEventType")]
    pub source_event_type: String,
    #[serde(default, alias = "dedupeKey")]
    pub dedupe_key: String,
    #[serde(default)]
    pub status: String,
    #[serde(default, alias = "retryCount")]
    pub retry_count: u32,
    #[serde(default, alias = "createdAt", skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(default, alias = "updatedAt", skip_serializing_if = "Option::is_none")]
    pub updated_at: Option<String>,
    #[serde(default, alias = "meetingRef", skip_serializing_if = "Option::is_none")]
    pub meeting_ref: Option<TeamsMeetingRef>,
    #[serde(
        default,
        alias = "selectedArtifactStrategy",
        skip_serializing_if = "Option::is_none"
    )]
    pub selected_artifact_strategy: Option<String>,
    #[serde(
        default,
        alias = "summaryPayload",
        skip_serializing_if = "Option::is_none"
    )]
    pub summary_payload: Option<TeamsMeetingSummaryPayload>,
    #[serde(default, alias = "errorInfo", skip_serializing_if = "skip_map")]
    pub error_info: Map<String, Value>,
}

impl TeamsMeetingPipelineJob {
    pub fn from_value(value: Value) -> TeamsPipelineResult<Self> {
        let record: Self = serde_json::from_value(value)?;
        record.validate()?;
        Ok(record)
    }

    pub fn validate(&self) -> TeamsPipelineResult<()> {
        if self.job_id.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "TeamsMeetingPipelineJob.job_id is required.".into(),
            ));
        }
        if self.event_id.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "TeamsMeetingPipelineJob.event_id is required.".into(),
            ));
        }
        if self.source_event_type.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "TeamsMeetingPipelineJob.source_event_type is required.".into(),
            ));
        }
        if self.dedupe_key.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "TeamsMeetingPipelineJob.dedupe_key is required.".into(),
            ));
        }
        if self.status.trim().is_empty() {
            return Err(TeamsPipelineError::Invalid(
                "TeamsMeetingPipelineJob.status is required.".into(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TeamsPipelineState {
    #[serde(default)]
    subscriptions: BTreeMap<String, Value>,
    #[serde(default)]
    notification_receipts: BTreeMap<String, Value>,
    #[serde(default)]
    event_timestamps: BTreeMap<String, Value>,
    #[serde(default)]
    jobs: BTreeMap<String, Value>,
    #[serde(default)]
    sink_records: BTreeMap<String, Value>,
}

#[derive(Debug)]
pub struct TeamsPipelineStore {
    path: PathBuf,
    state: Mutex<TeamsPipelineState>,
}

impl TeamsPipelineStore {
    pub fn new(path: impl Into<PathBuf>) -> TeamsPipelineResult<Self> {
        let path = path.into();
        let state = if path.exists() {
            let raw = std::fs::read_to_string(&path)?;
            if raw.trim().is_empty() {
                TeamsPipelineState::default()
            } else {
                serde_json::from_str(&raw).unwrap_or_default()
            }
        } else {
            TeamsPipelineState::default()
        };
        Ok(Self {
            path,
            state: Mutex::new(state),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn build_notification_receipt_key(notification: &Value) -> String {
        if let Some(id) = object_field(notification, "id").and_then(value_to_string) {
            return format!("id:{id}");
        }
        let canonical = canonical_json(notification);
        let digest = Sha256::digest(canonical.as_bytes());
        format!("sha256:{}", hex::encode(digest))
    }

    pub fn stats(&self) -> TeamsPipelineResult<BTreeMap<String, usize>> {
        let state = self.lock_state()?;
        Ok(BTreeMap::from([
            ("subscriptions".into(), state.subscriptions.len()),
            (
                "notification_receipts".into(),
                state.notification_receipts.len(),
            ),
            ("event_timestamps".into(), state.event_timestamps.len()),
            ("jobs".into(), state.jobs.len()),
            ("sink_records".into(), state.sink_records.len()),
        ]))
    }

    pub fn list_subscriptions(&self) -> TeamsPipelineResult<BTreeMap<String, Value>> {
        Ok(self.lock_state()?.subscriptions.clone())
    }

    pub fn get_subscription(&self, subscription_id: &str) -> TeamsPipelineResult<Option<Value>> {
        Ok(self
            .lock_state()?
            .subscriptions
            .get(subscription_id)
            .cloned())
    }

    pub fn upsert_subscription(
        &self,
        subscription_id: &str,
        payload: Value,
    ) -> TeamsPipelineResult<Value> {
        self.upsert_record("subscriptions", subscription_id, payload)
    }

    pub fn delete_subscription(&self, subscription_id: &str) -> TeamsPipelineResult<bool> {
        let removed = {
            let mut state = self.lock_state()?;
            state.subscriptions.remove(subscription_id).is_some()
        };
        if removed {
            self.persist()?;
        }
        Ok(removed)
    }

    pub fn has_notification_receipt(&self, receipt_key: &str) -> TeamsPipelineResult<bool> {
        Ok(self
            .lock_state()?
            .notification_receipts
            .contains_key(receipt_key))
    }

    pub fn record_notification_receipt(
        &self,
        receipt_key: &str,
        payload: Option<Value>,
        received_at: Option<String>,
    ) -> TeamsPipelineResult<bool> {
        let inserted = {
            let mut state = self.lock_state()?;
            if state.notification_receipts.contains_key(receipt_key) {
                return Ok(false);
            }
            state.notification_receipts.insert(
                receipt_key.to_string(),
                json!({
                    "received_at": received_at.unwrap_or_else(utc_now_iso),
                    "payload": payload
                }),
            );
            true
        };
        self.persist()?;
        Ok(inserted)
    }

    pub fn record_event_timestamp(
        &self,
        event_key: &str,
        timestamp: Option<String>,
    ) -> TeamsPipelineResult<String> {
        let timestamp = timestamp.unwrap_or_else(utc_now_iso);
        {
            let mut state = self.lock_state()?;
            state
                .event_timestamps
                .insert(event_key.to_string(), Value::String(timestamp.clone()));
        }
        self.persist()?;
        Ok(timestamp)
    }

    pub fn get_event_timestamp(&self, event_key: &str) -> TeamsPipelineResult<Option<String>> {
        Ok(self
            .lock_state()?
            .event_timestamps
            .get(event_key)
            .and_then(value_to_string))
    }

    pub fn upsert_job(&self, job_id: &str, payload: Value) -> TeamsPipelineResult<Value> {
        self.upsert_record("jobs", job_id, payload)
    }

    pub fn get_job(&self, job_id: &str) -> TeamsPipelineResult<Option<Value>> {
        Ok(self.lock_state()?.jobs.get(job_id).cloned())
    }

    pub fn list_jobs(&self) -> TeamsPipelineResult<BTreeMap<String, Value>> {
        Ok(self.lock_state()?.jobs.clone())
    }

    pub fn upsert_sink_record(&self, sink_key: &str, payload: Value) -> TeamsPipelineResult<Value> {
        self.upsert_record("sink_records", sink_key, payload)
    }

    pub fn get_sink_record(&self, sink_key: &str) -> TeamsPipelineResult<Option<Value>> {
        Ok(self.lock_state()?.sink_records.get(sink_key).cloned())
    }

    fn lock_state(&self) -> TeamsPipelineResult<std::sync::MutexGuard<'_, TeamsPipelineState>> {
        self.state
            .lock()
            .map_err(|_| TeamsPipelineError::Store("teams pipeline store lock poisoned".into()))
    }

    fn upsert_record(&self, bucket: &str, key: &str, payload: Value) -> TeamsPipelineResult<Value> {
        let stored = {
            let mut state = self.lock_state()?;
            let records = match bucket {
                "subscriptions" => &mut state.subscriptions,
                "jobs" => &mut state.jobs,
                "sink_records" => &mut state.sink_records,
                _ => {
                    return Err(TeamsPipelineError::Store(format!(
                        "unknown store bucket: {bucket}"
                    )))
                }
            };
            let now = utc_now_iso();
            let existing = records
                .get(key)
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            let mut merged = existing.clone();
            if let Some(object) = payload.as_object() {
                for (field, value) in object {
                    merged.insert(field.clone(), value.clone());
                }
            }
            let id_field = match bucket {
                "subscriptions" => "subscription_id",
                "jobs" => "job_id",
                "sink_records" => "sink_key",
                _ => "id",
            };
            merged.insert(id_field.into(), Value::String(key.to_string()));
            if !merged.contains_key("created_at") {
                let created = existing
                    .get("created_at")
                    .cloned()
                    .unwrap_or_else(|| Value::String(now.clone()));
                merged.insert("created_at".into(), created);
            }
            merged.insert("updated_at".into(), Value::String(now));
            let value = Value::Object(merged);
            records.insert(key.to_string(), value.clone());
            value
        };
        self.persist()?;
        Ok(stored)
    }

    fn persist(&self) -> TeamsPipelineResult<()> {
        let state = self.lock_state()?.clone();
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let body = serde_json::to_vec_pretty(&state)?;
        let tmp = self
            .path
            .with_extension(format!("json.tmp-{}", Uuid::new_v4().simple()));
        std::fs::write(&tmp, body)?;
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

pub fn resolve_teams_pipeline_store_path(path: Option<&Path>) -> PathBuf {
    if let Some(path) = path {
        if !path.as_os_str().is_empty() {
            return path.to_path_buf();
        }
    }
    if let Some(path) = env_nonempty("MSGRAPH_WEBHOOK_STORE_PATH") {
        return PathBuf::from(path);
    }
    hermes_home().join(DEFAULT_TEAMS_PIPELINE_STORE_FILENAME)
}

#[derive(Debug, Clone, PartialEq)]
pub struct TeamsPipelineConfig {
    pub transcript_preferred: bool,
    pub transcript_required: bool,
    pub transcription_fallback: bool,
    pub stt_model: Option<String>,
    pub ffmpeg_extract_audio: bool,
    pub transcript_min_chars: usize,
    pub tmp_dir: Option<PathBuf>,
    pub notion: Option<Value>,
    pub linear: Option<Value>,
    pub teams_delivery: Option<Value>,
}

impl Default for TeamsPipelineConfig {
    fn default() -> Self {
        Self {
            transcript_preferred: true,
            transcript_required: false,
            transcription_fallback: true,
            stt_model: None,
            ffmpeg_extract_audio: true,
            transcript_min_chars: 80,
            tmp_dir: None,
            notion: None,
            linear: None,
            teams_delivery: None,
        }
    }
}

impl TeamsPipelineConfig {
    pub fn from_value(value: Option<Value>) -> Self {
        let Some(Value::Object(object)) = value else {
            return Self::default();
        };
        let default = Self::default();
        Self {
            transcript_preferred: parse_bool(
                object.get("transcript_preferred"),
                default.transcript_preferred,
            ),
            transcript_required: parse_bool(
                object.get("transcript_required"),
                default.transcript_required,
            ),
            transcription_fallback: parse_bool(
                object.get("transcription_fallback"),
                default.transcription_fallback,
            ),
            stt_model: value_string(object.get("stt_model")),
            ffmpeg_extract_audio: parse_bool(
                object.get("ffmpeg_extract_audio"),
                default.ffmpeg_extract_audio,
            ),
            transcript_min_chars: parse_usize(
                object.get("transcript_min_chars"),
                default.transcript_min_chars,
            ),
            tmp_dir: value_string(object.get("tmp_dir")).map(PathBuf::from),
            notion: object
                .get("notion")
                .cloned()
                .filter(|value| value.is_object()),
            linear: object
                .get("linear")
                .cloned()
                .filter(|value| value.is_object()),
            teams_delivery: object
                .get("teams_delivery")
                .cloned()
                .filter(|value| value.is_object()),
        }
    }
}

#[async_trait]
pub trait TeamsGraphBackend: Send + Sync {
    async fn resolve_meeting_reference(
        &self,
        meeting_id: Option<&str>,
        join_web_url: Option<&str>,
        tenant_id: Option<&str>,
    ) -> TeamsPipelineResult<TeamsMeetingRef>;

    async fn fetch_preferred_transcript_text(
        &self,
        meeting_ref: &TeamsMeetingRef,
    ) -> TeamsPipelineResult<Option<(MeetingArtifact, String)>>;

    async fn list_recording_artifacts(
        &self,
        meeting_ref: &TeamsMeetingRef,
    ) -> TeamsPipelineResult<Vec<MeetingArtifact>>;

    async fn download_recording_artifact(
        &self,
        meeting_ref: &TeamsMeetingRef,
        recording: &MeetingArtifact,
    ) -> TeamsPipelineResult<Vec<u8>>;

    async fn enrich_meeting_with_call_record(
        &self,
        meeting_ref: &TeamsMeetingRef,
        call_record_id: Option<&str>,
    ) -> TeamsPipelineResult<Option<MeetingArtifact>>;
}

#[async_trait]
pub trait TeamsTranscriber: Send + Sync {
    async fn transcribe_recording(
        &self,
        recording: &MeetingArtifact,
        bytes: &[u8],
        config: &TeamsPipelineConfig,
    ) -> TeamsPipelineResult<String>;
}

#[async_trait]
pub trait TeamsSummarizer: Send + Sync {
    async fn summarize(
        &self,
        resolved_meeting: &TeamsMeetingRef,
        transcript_text: &str,
        artifacts: &[MeetingArtifact],
        config: &TeamsPipelineConfig,
    ) -> TeamsPipelineResult<TeamsMeetingSummaryPayload>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamsSinkKind {
    Notion,
    Linear,
    Teams,
}

impl TeamsSinkKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Notion => "notion",
            Self::Linear => "linear",
            Self::Teams => "teams",
        }
    }
}

#[async_trait]
pub trait TeamsSinkWriter: Send + Sync {
    async fn write_summary(
        &self,
        sink: TeamsSinkKind,
        payload: &TeamsMeetingSummaryPayload,
        config: &Value,
        existing_record: Option<Value>,
    ) -> TeamsPipelineResult<Value>;
}

pub struct TeamsMeetingPipeline {
    graph_backend: Arc<dyn TeamsGraphBackend>,
    store: Arc<TeamsPipelineStore>,
    config: TeamsPipelineConfig,
    transcriber: Arc<dyn TeamsTranscriber>,
    summarizer: Arc<dyn TeamsSummarizer>,
    notion_writer: Option<Arc<dyn TeamsSinkWriter>>,
    linear_writer: Option<Arc<dyn TeamsSinkWriter>>,
    teams_sender: Option<Arc<dyn TeamsSinkWriter>>,
}

impl TeamsMeetingPipeline {
    pub fn new(
        graph_backend: Arc<dyn TeamsGraphBackend>,
        store: Arc<TeamsPipelineStore>,
        config: TeamsPipelineConfig,
    ) -> Self {
        Self {
            graph_backend,
            store,
            config,
            transcriber: Arc::new(TranscriptionToolTeamsTranscriber),
            summarizer: Arc::new(HeuristicTeamsSummarizer),
            notion_writer: None,
            linear_writer: None,
            teams_sender: None,
        }
    }

    pub fn with_transcriber(mut self, transcriber: Arc<dyn TeamsTranscriber>) -> Self {
        self.transcriber = transcriber;
        self
    }

    pub fn with_summarizer(mut self, summarizer: Arc<dyn TeamsSummarizer>) -> Self {
        self.summarizer = summarizer;
        self
    }

    pub fn with_notion_writer(mut self, writer: Arc<dyn TeamsSinkWriter>) -> Self {
        self.notion_writer = Some(writer);
        self
    }

    pub fn with_linear_writer(mut self, writer: Arc<dyn TeamsSinkWriter>) -> Self {
        self.linear_writer = Some(writer);
        self
    }

    pub fn with_teams_sender(mut self, writer: Arc<dyn TeamsSinkWriter>) -> Self {
        self.teams_sender = Some(writer);
        self
    }

    pub fn store(&self) -> &TeamsPipelineStore {
        &self.store
    }

    pub fn config(&self) -> &TeamsPipelineConfig {
        &self.config
    }

    pub fn create_job_from_notification(
        &self,
        notification: Value,
    ) -> TeamsPipelineResult<TeamsMeetingPipelineJob> {
        let event_id = TeamsPipelineStore::build_notification_receipt_key(&notification);
        self.store
            .record_notification_receipt(&event_id, Some(notification.clone()), None)?;
        if let Some(existing) = self.find_job_by_dedupe_key(&event_id)? {
            return Ok(existing);
        }
        let resource_data = object_field(&notification, "resourceData")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let resource = value_string(object_field(&notification, "resource"));
        let meeting_id = resource_data
            .get("id")
            .and_then(value_to_string)
            .or_else(|| value_string(object_field(&notification, "meetingId")))
            .or_else(|| {
                resource
                    .as_deref()
                    .and_then(extract_meeting_id_from_resource)
            })
            .or(resource)
            .unwrap_or_else(|| event_id.clone());
        let mut metadata = Map::new();
        metadata.insert("notification".into(), notification.clone());
        if let Some(join_web_url) = resource_data.get("joinWebUrl").and_then(value_to_string) {
            metadata.insert("join_web_url".into(), Value::String(join_web_url));
        }
        if let Some(call_record_id) = resource_data
            .get("callRecordId")
            .and_then(value_to_string)
            .or_else(|| value_string(object_field(&notification, "callRecordId")))
        {
            metadata.insert("call_record_id".into(), Value::String(call_record_id));
        }
        let job_suffix = Uuid::new_v4().simple().to_string();
        let job = TeamsMeetingPipelineJob {
            job_id: format!("teams-job-{}", &job_suffix[..12]),
            event_id: event_id.clone(),
            source_event_type: value_string(object_field(&notification, "changeType"))
                .unwrap_or_else(|| "graph.notification".into()),
            dedupe_key: event_id,
            status: "received".into(),
            retry_count: 0,
            created_at: None,
            updated_at: None,
            meeting_ref: Some(TeamsMeetingRef {
                meeting_id,
                organizer_user_id: None,
                join_web_url: None,
                calendar_event_id: None,
                thread_id: None,
                tenant_id: resource_data
                    .get("tenantId")
                    .and_then(value_to_string)
                    .or_else(|| value_string(object_field(&notification, "tenantId"))),
                metadata,
            }),
            selected_artifact_strategy: None,
            summary_payload: None,
            error_info: Map::new(),
        };
        self.persist_job(job)
    }

    pub async fn run_notification(
        &self,
        notification: Value,
    ) -> TeamsPipelineResult<TeamsMeetingPipelineJob> {
        let job = self.create_job_from_notification(notification)?;
        let active: HashSet<&str> = ACTIVE_PIPELINE_STATES.iter().copied().collect();
        if TERMINAL_PIPELINE_STATES.contains(&job.status.as_str())
            || (active.contains(job.status.as_str()) && job.status != "received")
        {
            return Ok(job);
        }
        self.run_job(&job.job_id).await
    }

    pub async fn run_job(&self, job_id: &str) -> TeamsPipelineResult<TeamsMeetingPipelineJob> {
        let payload = self.store.get_job(job_id)?.ok_or_else(|| {
            TeamsPipelineError::Invalid(format!("Unknown Teams pipeline job: {job_id}"))
        })?;
        let job = TeamsMeetingPipelineJob::from_value(payload)?;
        self.run_job_record(job).await
    }

    pub async fn run_job_record(
        &self,
        job: TeamsMeetingPipelineJob,
    ) -> TeamsPipelineResult<TeamsMeetingPipelineJob> {
        match self.run_job_inner(job.clone()).await {
            Ok(job) => Ok(job),
            Err(error) if error.is_retryable() => {
                let mut retry = self.latest_job_or(job)?;
                retry.status = "retry_scheduled".into();
                retry.error_info = Map::from_iter([
                    ("message".into(), Value::String(error.to_string())),
                    ("retryable".into(), Value::Bool(true)),
                ]);
                self.persist_job(retry)
            }
            Err(error) => {
                let mut failed = self.latest_job_or(job)?;
                failed.status = "failed".into();
                failed.error_info = Map::from_iter([
                    ("message".into(), Value::String(error.to_string())),
                    ("type".into(), Value::String(error_type_name(&error).into())),
                ]);
                self.persist_job(failed)
            }
        }
    }

    fn latest_job_or(
        &self,
        fallback: TeamsMeetingPipelineJob,
    ) -> TeamsPipelineResult<TeamsMeetingPipelineJob> {
        self.store
            .get_job(&fallback.job_id)?
            .map(TeamsMeetingPipelineJob::from_value)
            .transpose()
            .map(|job| job.unwrap_or(fallback))
    }

    async fn run_job_inner(
        &self,
        mut job: TeamsMeetingPipelineJob,
    ) -> TeamsPipelineResult<TeamsMeetingPipelineJob> {
        let meeting_ref = job.meeting_ref.clone().ok_or_else(|| {
            TeamsPipelineError::Invalid(format!("Job {} has no meeting_ref.", job.job_id))
        })?;
        let mut artifacts = Vec::new();
        job.status = "resolving_meeting".into();
        job = self.persist_job(job)?;

        let notification = meeting_ref
            .metadata
            .get("notification")
            .cloned()
            .unwrap_or(Value::Null);
        let resolved_meeting = self
            .graph_backend
            .resolve_meeting_reference(
                Some(&meeting_ref.meeting_id),
                meeting_ref.join_web_url.as_deref().or_else(|| {
                    meeting_ref
                        .metadata
                        .get("join_web_url")
                        .and_then(Value::as_str)
                }),
                meeting_ref.tenant_id.as_deref(),
            )
            .await?;
        job.meeting_ref = Some(resolved_meeting.clone());
        job = self.persist_job(job)?;

        let mut transcript_text = None;
        if self.config.transcript_preferred {
            job.status = "fetching_transcript".into();
            job = self.persist_job(job)?;
            if let Some((artifact, text)) = self
                .graph_backend
                .fetch_preferred_transcript_text(&resolved_meeting)
                .await?
            {
                artifacts.push(artifact);
                if text.trim().chars().count() >= self.config.transcript_min_chars {
                    transcript_text = Some(text);
                }
            }
        }

        let transcript_text = if let Some(transcript_text) = transcript_text {
            job.selected_artifact_strategy = Some("transcript_first".into());
            job = self.persist_job(job)?;
            transcript_text
        } else {
            if self.config.transcript_required {
                return Err(TeamsPipelineError::Retryable(format!(
                    "Transcript unavailable for meeting {}.",
                    resolved_meeting.meeting_id
                )));
            }
            if !self.config.transcription_fallback {
                return Err(TeamsPipelineError::ArtifactNotFound(format!(
                    "No transcript available and transcription fallback disabled for {}.",
                    resolved_meeting.meeting_id
                )));
            }
            job.status = "downloading_recording".into();
            job = self.persist_job(job)?;
            let recordings = self
                .graph_backend
                .list_recording_artifacts(&resolved_meeting)
                .await?;
            let recording = recordings.first().cloned().ok_or_else(|| {
                TeamsPipelineError::Retryable(format!(
                    "Recording unavailable for meeting {}.",
                    resolved_meeting.meeting_id
                ))
            })?;
            artifacts.push(recording.clone());
            let bytes = self
                .graph_backend
                .download_recording_artifact(&resolved_meeting, &recording)
                .await?;
            job.status = "transcribing_audio".into();
            job = self.persist_job(job)?;
            let transcript = self
                .transcriber
                .transcribe_recording(&recording, &bytes, &self.config)
                .await?;
            job.selected_artifact_strategy = Some("recording_stt_fallback".into());
            job = self.persist_job(job)?;
            transcript
        };

        let call_record_id =
            value_string(object_field(&notification, "callRecordId")).or_else(|| {
                meeting_ref
                    .metadata
                    .get("call_record_id")
                    .and_then(value_to_string)
            });
        if let Some(call_record) = self
            .graph_backend
            .enrich_meeting_with_call_record(&resolved_meeting, call_record_id.as_deref())
            .await?
        {
            artifacts.push(call_record);
        }

        job.status = "summarizing".into();
        job = self.persist_job(job)?;
        let summary_payload = self
            .summarizer
            .summarize(
                &resolved_meeting,
                &transcript_text,
                &artifacts,
                &self.config,
            )
            .await?;
        job.summary_payload = Some(summary_payload.clone());
        job = self.persist_job(job)?;

        self.write_sinks(&mut job, &summary_payload).await?;
        job.status = "completed".into();
        self.persist_job(job)
    }

    async fn write_sinks(
        &self,
        job: &mut TeamsMeetingPipelineJob,
        payload: &TeamsMeetingSummaryPayload,
    ) -> TeamsPipelineResult<()> {
        if let (Some(config), Some(writer)) = (&self.config.notion, &self.notion_writer) {
            if parse_bool(config.as_object().and_then(|o| o.get("enabled")), false) {
                job.status = "writing_notion".into();
                *job = self.persist_job(job.clone())?;
                let sink_key = format!("notion:{}", payload.meeting_ref.meeting_id);
                let existing = self.store.get_sink_record(&sink_key)?;
                let result = writer
                    .write_summary(TeamsSinkKind::Notion, payload, config, existing)
                    .await?;
                self.store.upsert_sink_record(&sink_key, result)?;
            }
        }
        if let (Some(config), Some(writer)) = (&self.config.linear, &self.linear_writer) {
            if parse_bool(config.as_object().and_then(|o| o.get("enabled")), false) {
                job.status = "writing_linear".into();
                *job = self.persist_job(job.clone())?;
                let sink_key = format!("linear:{}", payload.meeting_ref.meeting_id);
                let existing = self.store.get_sink_record(&sink_key)?;
                let result = writer
                    .write_summary(TeamsSinkKind::Linear, payload, config, existing)
                    .await?;
                self.store.upsert_sink_record(&sink_key, result)?;
            }
        }
        if let (Some(config), Some(writer)) = (&self.config.teams_delivery, &self.teams_sender) {
            if parse_bool(config.as_object().and_then(|o| o.get("enabled")), false) {
                job.status = "sending_teams".into();
                *job = self.persist_job(job.clone())?;
                let sink_key = format!("teams:{}", payload.meeting_ref.meeting_id);
                let existing = self.store.get_sink_record(&sink_key)?;
                let result = writer
                    .write_summary(TeamsSinkKind::Teams, payload, config, existing)
                    .await?;
                self.store.upsert_sink_record(&sink_key, result)?;
            }
        }
        Ok(())
    }

    fn persist_job(
        &self,
        job: TeamsMeetingPipelineJob,
    ) -> TeamsPipelineResult<TeamsMeetingPipelineJob> {
        let value = serde_json::to_value(&job)?;
        let stored = self
            .store
            .upsert_job(&job.job_id, strip_empty_object_keys(value))?;
        TeamsMeetingPipelineJob::from_value(stored)
    }

    fn find_job_by_dedupe_key(
        &self,
        dedupe_key: &str,
    ) -> TeamsPipelineResult<Option<TeamsMeetingPipelineJob>> {
        for payload in self.store.list_jobs()?.into_values() {
            if payload
                .get("dedupe_key")
                .and_then(Value::as_str)
                .map(|value| value == dedupe_key)
                .unwrap_or(false)
            {
                return TeamsMeetingPipelineJob::from_value(payload).map(Some);
            }
        }
        Ok(None)
    }
}

fn error_type_name(error: &TeamsPipelineError) -> &'static str {
    match error {
        TeamsPipelineError::Config(_) => "Config",
        TeamsPipelineError::Invalid(_) => "Invalid",
        TeamsPipelineError::Store(_) => "Store",
        TeamsPipelineError::Io(_) => "Io",
        TeamsPipelineError::Json(_) => "Json",
        TeamsPipelineError::Retryable(_) => "Retryable",
        TeamsPipelineError::ArtifactNotFound(_) => "ArtifactNotFound",
        TeamsPipelineError::Sink(_) => "Sink",
        TeamsPipelineError::Graph { .. } => "Graph",
    }
}

#[derive(Debug)]
pub struct TranscriptionToolTeamsTranscriber;

#[async_trait]
impl TeamsTranscriber for TranscriptionToolTeamsTranscriber {
    async fn transcribe_recording(
        &self,
        recording: &MeetingArtifact,
        bytes: &[u8],
        config: &TeamsPipelineConfig,
    ) -> TeamsPipelineResult<String> {
        let temp_root = config
            .tmp_dir
            .clone()
            .unwrap_or_else(|| hermes_home().join("tmp").join("teams_pipeline"));
        tokio::fs::create_dir_all(&temp_root).await?;
        let run_dir = temp_root.join(format!("teams-recording-{}", Uuid::new_v4().simple()));
        tokio::fs::create_dir_all(&run_dir).await?;
        let recording_name = recording
            .display_name
            .clone()
            .unwrap_or_else(|| format!("{}.mp4", recording.artifact_id));
        let recording_path = run_dir.join(safe_file_name(&recording_name));
        tokio::fs::write(&recording_path, bytes).await?;
        let audio_path = prepare_audio_path(&recording_path, config.ffmpeg_extract_audio).await?;
        let handler = crate::tools::transcription::TranscriptionHandler;
        let response = handler
            .execute(json!({ "audio_path": audio_path.to_string_lossy() }))
            .await?;
        let _ = tokio::fs::remove_dir_all(&run_dir).await;
        let parsed: Value = serde_json::from_str(&response)?;
        let transcript = parsed
            .get("text")
            .and_then(Value::as_str)
            .or_else(|| parsed.get("transcript").and_then(Value::as_str))
            .unwrap_or("")
            .trim()
            .to_string();
        if transcript.is_empty() {
            return Err(TeamsPipelineError::Retryable(
                "STT returned an empty transcript.".into(),
            ));
        }
        Ok(transcript)
    }
}

fn safe_file_name(value: &str) -> String {
    let cleaned = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    if cleaned.is_empty() {
        "recording.mp4".into()
    } else {
        cleaned
    }
}

async fn prepare_audio_path(
    recording_path: &Path,
    ffmpeg_extract_audio: bool,
) -> TeamsPipelineResult<PathBuf> {
    let extension = recording_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if matches!(
        extension.as_str(),
        "wav" | "mp3" | "m4a" | "ogg" | "flac" | "aac" | "webm"
    ) || !ffmpeg_extract_audio
    {
        return Ok(recording_path.to_path_buf());
    }
    let audio_path = recording_path.with_extension("wav");
    let output = Command::new("ffmpeg")
        .arg("-y")
        .arg("-i")
        .arg(recording_path)
        .arg(&audio_path)
        .output()
        .await
        .map_err(|e| {
            TeamsPipelineError::Retryable(format!(
                "Recording fallback requires ffmpeg for audio extraction, but ffmpeg failed to start: {e}"
            ))
        })?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(TeamsPipelineError::Retryable(format!(
            "ffmpeg audio extraction failed: {detail}"
        )));
    }
    Ok(audio_path)
}

#[derive(Debug)]
pub struct HeuristicTeamsSummarizer;

#[async_trait]
impl TeamsSummarizer for HeuristicTeamsSummarizer {
    async fn summarize(
        &self,
        resolved_meeting: &TeamsMeetingRef,
        transcript_text: &str,
        artifacts: &[MeetingArtifact],
        config: &TeamsPipelineConfig,
    ) -> TeamsPipelineResult<TeamsMeetingSummaryPayload> {
        let parsed = heuristic_summary(transcript_text);
        let call_metrics = collect_call_metrics(artifacts);
        Ok(TeamsMeetingSummaryPayload {
            meeting_ref: resolved_meeting.clone(),
            title: resolved_meeting
                .metadata
                .get("subject")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| Some(format!("Meeting {}", resolved_meeting.meeting_id))),
            start_time: resolved_meeting
                .metadata
                .get("startDateTime")
                .and_then(value_to_string),
            end_time: resolved_meeting
                .metadata
                .get("endDateTime")
                .and_then(value_to_string),
            participants: collect_participants(resolved_meeting),
            transcript_text: Some(transcript_text.to_string()),
            summary: Some(parsed.summary),
            key_decisions: parsed.key_decisions,
            action_items: parsed.action_items,
            risks: parsed.risks,
            call_metrics,
            source_artifacts: artifacts.to_vec(),
            confidence: Some(parsed.confidence),
            confidence_notes: Some(parsed.confidence_notes),
            notion_target: config
                .notion
                .as_ref()
                .and_then(|v| object_field(v, "database_id"))
                .and_then(value_to_string),
            linear_target: config
                .linear
                .as_ref()
                .and_then(|v| object_field(v, "team_id"))
                .and_then(value_to_string),
            teams_target: config.teams_delivery.as_ref().and_then(|v| {
                object_field(v, "channel_id")
                    .and_then(value_to_string)
                    .or_else(|| object_field(v, "chat_id").and_then(value_to_string))
            }),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SummaryParts {
    pub summary: String,
    pub key_decisions: Vec<String>,
    pub action_items: Vec<String>,
    pub risks: Vec<String>,
    pub confidence: String,
    pub confidence_notes: String,
}

pub fn parse_summary_json(content: &str) -> TeamsPipelineResult<SummaryParts> {
    let mut text = content.trim().to_string();
    if text.is_empty() {
        return Ok(heuristic_summary(""));
    }
    if let (Some(start), Some(end)) = (text.find('{'), text.rfind('}')) {
        if end > start {
            text = text[start..=end].to_string();
        }
    }
    let payload: Value = serde_json::from_str(&text)?;
    Ok(SummaryParts {
        summary: payload
            .get("summary")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string(),
        key_decisions: string_list(payload.get("key_decisions")),
        action_items: string_list(payload.get("action_items")),
        risks: string_list(payload.get("risks")),
        confidence: payload
            .get("confidence")
            .and_then(Value::as_str)
            .unwrap_or("medium")
            .trim()
            .to_string(),
        confidence_notes: payload
            .get("confidence_notes")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string(),
    })
}

pub fn heuristic_summary(transcript_text: &str) -> SummaryParts {
    let lines = transcript_text
        .lines()
        .map(|line| line.trim_matches(|ch: char| ch == ' ' || ch == '-' || ch == '*' || ch == '\t'))
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let mut summary = lines.iter().take(3).cloned().collect::<Vec<_>>().join(" ");
    if summary.chars().count() > 1200 {
        summary = summary.chars().take(1200).collect();
    }
    if summary.is_empty() {
        summary = "Transcript unavailable or too sparse for a confident summary.".into();
    }
    let action_items = lines
        .iter()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.starts_with("action:")
                || lower.starts_with("todo:")
                || lower.starts_with("next step:")
                || lower.starts_with("follow up:")
        })
        .take(8)
        .cloned()
        .collect();
    let risks = lines
        .iter()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("risk") || lower.contains("blocker")
        })
        .take(6)
        .cloned()
        .collect();
    let key_decisions = lines
        .iter()
        .filter(|line| {
            let lower = line.to_ascii_lowercase();
            lower.contains("decide") || lower.contains("decision")
        })
        .take(6)
        .cloned()
        .collect();
    SummaryParts {
        summary,
        key_decisions,
        action_items,
        risks,
        confidence: if transcript_text.trim().chars().count() < 300 {
            "low".into()
        } else {
            "medium".into()
        },
        confidence_notes:
            "Generated with heuristic fallback because no LLM summary response was available."
                .into(),
    }
}

fn string_list(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(value_to_string)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .collect()
}

pub fn collect_call_metrics(artifacts: &[MeetingArtifact]) -> Map<String, Value> {
    let mut metrics = Map::new();
    for artifact in artifacts {
        if artifact.artifact_type == "call_record" {
            if let Some(object) = artifact.metadata.get("metrics").and_then(Value::as_object) {
                for (key, value) in object {
                    metrics.insert(key.clone(), value.clone());
                }
            }
        }
    }
    metrics.insert("artifact_count".into(), json!(artifacts.len()));
    metrics
}

pub fn collect_participants(meeting_ref: &TeamsMeetingRef) -> Vec<String> {
    meeting_ref
        .metadata
        .get("participants")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            item.get("displayName")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned)
                .or_else(|| {
                    item.get("identity")
                        .and_then(|v| v.get("user"))
                        .and_then(|v| v.get("displayName"))
                        .and_then(Value::as_str)
                        .map(ToOwned::to_owned)
                })
        })
        .collect()
}

pub fn extract_meeting_id_from_resource(resource: &str) -> Option<String> {
    if resource.trim().is_empty() {
        return None;
    }
    let parts = resource
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.is_empty() {
        return None;
    }
    if let Some(index) = parts.iter().position(|part| *part == "onlineMeetings") {
        if let Some(next) = parts.get(index + 1) {
            return Some((*next).to_string());
        }
    }
    parts.last().map(|part| (*part).to_string())
}

pub fn build_summary_prompt(
    meeting_ref: &TeamsMeetingRef,
    transcript_text: &str,
    artifacts: &[MeetingArtifact],
) -> String {
    let artifact_lines = artifacts
        .iter()
        .map(|artifact| {
            format!(
                "- {}:{}:{}",
                artifact.artifact_type,
                artifact.artifact_id,
                artifact.display_name.clone().unwrap_or_default()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let transcript = transcript_text.chars().take(18_000).collect::<String>();
    format!(
        "Meeting ID: {}\nTitle: {}\nArtifacts:\n{}\n\nTranscript:\n{}",
        meeting_ref.meeting_id,
        meeting_ref
            .metadata
            .get("subject")
            .and_then(Value::as_str)
            .unwrap_or("Unknown"),
        if artifact_lines.is_empty() {
            "- none"
        } else {
            artifact_lines.as_str()
        },
        transcript
    )
}

pub fn render_summary_markdown(payload: &TeamsMeetingSummaryPayload) -> String {
    fn list_or_none(items: &[String]) -> Vec<String> {
        if items.is_empty() {
            vec!["- None".into()]
        } else {
            items.iter().map(|item| format!("- {item}")).collect()
        }
    }
    let mut lines = vec![
        format!(
            "# {}",
            payload
                .title
                .clone()
                .unwrap_or_else(|| format!("Meeting {}", payload.meeting_ref.meeting_id))
        ),
        "".into(),
        "## Summary".into(),
        payload
            .summary
            .clone()
            .unwrap_or_else(|| "No summary available.".into()),
        "".into(),
        "## Key Decisions".into(),
    ];
    lines.extend(list_or_none(&payload.key_decisions));
    lines.extend(["".into(), "## Action Items".into()]);
    lines.extend(list_or_none(&payload.action_items));
    lines.extend(["".into(), "## Risks".into()]);
    lines.extend(list_or_none(&payload.risks));
    lines.extend([
        "".into(),
        format!(
            "Confidence: {}",
            payload.confidence.as_deref().unwrap_or("unknown")
        ),
    ]);
    if let Some(notes) = &payload.confidence_notes {
        if !notes.trim().is_empty() {
            lines.push(notes.clone());
        }
    }
    lines.join("\n").trim().to_string()
}

#[derive(Debug, Clone)]
pub struct MicrosoftGraphTokenProvider {
    client: Client,
    config: MicrosoftGraphAuthConfig,
    cache: Arc<tokio::sync::Mutex<Option<CachedGraphToken>>>,
}

#[derive(Debug, Clone)]
struct MicrosoftGraphAuthConfig {
    direct_access_token: Option<String>,
    tenant_id: Option<String>,
    client_id: Option<String>,
    client_secret: Option<String>,
    scope: String,
    authority_url: String,
}

#[derive(Debug, Clone)]
struct CachedGraphToken {
    access_token: String,
    expires_at: DateTime<Utc>,
}

impl MicrosoftGraphTokenProvider {
    pub fn from_env() -> TeamsPipelineResult<Self> {
        let config = MicrosoftGraphAuthConfig {
            direct_access_token: env_nonempty("MSGRAPH_ACCESS_TOKEN")
                .or_else(|| env_nonempty("TEAMS_GRAPH_ACCESS_TOKEN")),
            tenant_id: env_nonempty("MSGRAPH_TENANT_ID"),
            client_id: env_nonempty("MSGRAPH_CLIENT_ID"),
            client_secret: env_nonempty("MSGRAPH_CLIENT_SECRET"),
            scope: env_nonempty("MSGRAPH_SCOPE")
                .unwrap_or_else(|| "https://graph.microsoft.com/.default".into()),
            authority_url: env_nonempty("MSGRAPH_AUTHORITY_URL")
                .unwrap_or_else(|| "https://login.microsoftonline.com".into()),
        };
        if config.direct_access_token.is_none()
            && (config.tenant_id.is_none()
                || config.client_id.is_none()
                || config.client_secret.is_none())
        {
            return Err(TeamsPipelineError::Config(
                "Microsoft Graph is not configured. Set MSGRAPH_TENANT_ID, MSGRAPH_CLIENT_ID, and MSGRAPH_CLIENT_SECRET.".into(),
            ));
        }
        Ok(Self {
            client: graph_http_client(),
            config,
            cache: Arc::new(tokio::sync::Mutex::new(None)),
        })
    }

    pub fn inspect_token_health(&self) -> Value {
        json!({
            "configured": self.config.direct_access_token.is_some()
                || (self.config.tenant_id.is_some()
                    && self.config.client_id.is_some()
                    && self.config.client_secret.is_some()),
            "direct_access_token": self.config.direct_access_token.is_some(),
            "tenant_id": self.config.tenant_id.is_some(),
            "client_id": self.config.client_id.is_some(),
            "client_secret": self.config.client_secret.is_some(),
            "scope": self.config.scope,
            "authority_url": self.config.authority_url,
        })
    }

    pub async fn get_access_token(&self, force_refresh: bool) -> TeamsPipelineResult<String> {
        if let Some(token) = &self.config.direct_access_token {
            return Ok(token.clone());
        }
        if !force_refresh {
            if let Some(cached) = self.cache.lock().await.clone() {
                if cached.expires_at > Utc::now() + ChronoDuration::seconds(60) {
                    return Ok(cached.access_token);
                }
            }
        }
        let tenant =
            self.config.tenant_id.as_deref().ok_or_else(|| {
                TeamsPipelineError::Config("MSGRAPH_TENANT_ID is missing.".into())
            })?;
        let token_url = format!(
            "{}/{}/oauth2/v2.0/token",
            self.config.authority_url.trim_end_matches('/'),
            path_percent_encode(tenant)
        );
        let response = self
            .client
            .post(token_url)
            .form(&[
                ("client_id", self.config.client_id.as_deref().unwrap_or("")),
                (
                    "client_secret",
                    self.config.client_secret.as_deref().unwrap_or(""),
                ),
                ("scope", self.config.scope.as_str()),
                ("grant_type", "client_credentials"),
            ])
            .send()
            .await
            .map_err(|e| TeamsPipelineError::Config(format!("Graph token request failed: {e}")))?;
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(TeamsPipelineError::Config(format!(
                "Graph token request failed with HTTP {status}: {text}"
            )));
        }
        let payload: Value = serde_json::from_str(&text)?;
        let access_token = payload
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| {
                TeamsPipelineError::Config("Graph token response omitted access_token.".into())
            })?
            .to_string();
        let expires_in = payload
            .get("expires_in")
            .and_then(Value::as_i64)
            .unwrap_or(3600)
            .max(120);
        let cached = CachedGraphToken {
            access_token: access_token.clone(),
            expires_at: Utc::now() + ChronoDuration::seconds(expires_in - 60),
        };
        *self.cache.lock().await = Some(cached);
        Ok(access_token)
    }
}

#[derive(Debug, Clone)]
pub struct MicrosoftGraphClient {
    client: Client,
    token_provider: MicrosoftGraphTokenProvider,
    base_url: String,
}

impl MicrosoftGraphClient {
    pub fn from_env() -> TeamsPipelineResult<Self> {
        Ok(Self {
            client: graph_http_client(),
            token_provider: MicrosoftGraphTokenProvider::from_env()?,
            base_url: env_nonempty("MSGRAPH_BASE_URL")
                .unwrap_or_else(|| "https://graph.microsoft.com/v1.0".into()),
        })
    }

    pub fn new(token_provider: MicrosoftGraphTokenProvider, base_url: impl Into<String>) -> Self {
        Self {
            client: graph_http_client(),
            token_provider,
            base_url: base_url.into(),
        }
    }

    pub fn token_provider(&self) -> &MicrosoftGraphTokenProvider {
        &self.token_provider
    }

    pub async fn get_json(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> TeamsPipelineResult<Value> {
        self.request_json(Method::GET, path, params, None).await
    }

    pub async fn post_json(&self, path: &str, body: Value) -> TeamsPipelineResult<Value> {
        self.request_json(Method::POST, path, &[], Some(body)).await
    }

    pub async fn patch_json(&self, path: &str, body: Value) -> TeamsPipelineResult<Value> {
        self.request_json(Method::PATCH, path, &[], Some(body))
            .await
    }

    pub async fn delete_json(&self, path: &str) -> TeamsPipelineResult<Value> {
        self.request_json(Method::DELETE, path, &[], None).await
    }

    pub async fn collect_paginated(&self, path: &str) -> TeamsPipelineResult<Vec<Value>> {
        let mut next = Some(path.to_string());
        let mut values = Vec::new();
        while let Some(url) = next {
            let payload = self.get_json(&url, &[]).await?;
            if let Some(items) = payload.get("value").and_then(Value::as_array) {
                values.extend(items.iter().cloned());
            }
            next = payload
                .get("@odata.nextLink")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
        }
        Ok(values)
    }

    pub async fn download_bytes(&self, path_or_url: &str) -> TeamsPipelineResult<Vec<u8>> {
        let url = self.build_url(path_or_url, &[])?;
        let token = self.token_provider.get_access_token(false).await?;
        let response = self
            .client
            .get(url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| TeamsPipelineError::Graph {
                status: 0,
                message: format!("Graph download failed: {e}"),
            })?;
        let status = response.status();
        if !status.is_success() {
            let text = response.text().await.unwrap_or_default();
            return Err(friendly_graph_error(status, &text));
        }
        Ok(response
            .bytes()
            .await
            .map_err(|e| TeamsPipelineError::Graph {
                status: 0,
                message: format!("Graph download read failed: {e}"),
            })?
            .to_vec())
    }

    async fn request_json(
        &self,
        method: Method,
        path: &str,
        params: &[(&str, String)],
        body: Option<Value>,
    ) -> TeamsPipelineResult<Value> {
        let url = self.build_url(path, params)?;
        let token = self.token_provider.get_access_token(false).await?;
        let mut request = self.client.request(method, url).bearer_auth(token);
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response = request
            .send()
            .await
            .map_err(|e| TeamsPipelineError::Graph {
                status: 0,
                message: format!("Graph request failed: {e}"),
            })?;
        read_graph_json_response(response).await
    }

    fn build_url(
        &self,
        path: &str,
        params: &[(&str, String)],
    ) -> TeamsPipelineResult<reqwest::Url> {
        let mut url = if path.starts_with("http://") || path.starts_with("https://") {
            reqwest::Url::parse(path)
        } else {
            let base = self.base_url.trim_end_matches('/');
            let path = path.trim_start_matches('/');
            reqwest::Url::parse(&format!("{base}/{path}"))
        }
        .map_err(|e| TeamsPipelineError::Config(format!("invalid Graph URL: {e}")))?;
        {
            let mut query = url.query_pairs_mut();
            for (key, value) in params {
                query.append_pair(key, value);
            }
        }
        Ok(url)
    }
}

fn graph_http_client() -> Client {
    Client::builder()
        .timeout(Duration::from_secs(45))
        .build()
        .unwrap_or_else(|_| Client::new())
}

async fn read_graph_json_response(response: reqwest::Response) -> TeamsPipelineResult<Value> {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(friendly_graph_error(status, &text));
    }
    if status == StatusCode::NO_CONTENT || text.trim().is_empty() {
        return Ok(json!({"success": true, "status_code": status.as_u16(), "empty": true}));
    }
    serde_json::from_str(&text).map_err(TeamsPipelineError::from)
}

fn friendly_graph_error(status: StatusCode, text: &str) -> TeamsPipelineError {
    let detail = extract_graph_error_message(text);
    let message = match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            format!("Microsoft Graph permission denied or token expired: {detail}")
        }
        StatusCode::NOT_FOUND => format!("Microsoft Graph resource not found: {detail}"),
        StatusCode::TOO_MANY_REQUESTS => format!("Microsoft Graph rate limited: {detail}"),
        _ => detail,
    };
    TeamsPipelineError::Graph {
        status: status.as_u16(),
        message,
    }
}

fn extract_graph_error_message(text: &str) -> String {
    let fallback = text.trim().to_string();
    let Ok(payload) = serde_json::from_str::<Value>(text) else {
        return fallback;
    };
    payload
        .get("error")
        .and_then(|error| {
            error
                .get("message")
                .and_then(Value::as_str)
                .or_else(|| error.as_str())
        })
        .unwrap_or(&fallback)
        .to_string()
}

#[derive(Debug, Clone)]
pub struct MicrosoftGraphTeamsBackend {
    client: MicrosoftGraphClient,
}

impl MicrosoftGraphTeamsBackend {
    pub fn from_env() -> TeamsPipelineResult<Self> {
        Ok(Self {
            client: MicrosoftGraphClient::from_env()?,
        })
    }

    pub fn new(client: MicrosoftGraphClient) -> Self {
        Self { client }
    }

    pub fn client(&self) -> &MicrosoftGraphClient {
        &self.client
    }
}

#[async_trait]
impl TeamsGraphBackend for MicrosoftGraphTeamsBackend {
    async fn resolve_meeting_reference(
        &self,
        meeting_id: Option<&str>,
        join_web_url: Option<&str>,
        tenant_id: Option<&str>,
    ) -> TeamsPipelineResult<TeamsMeetingRef> {
        if let Some(meeting_id) = meeting_id.filter(|value| !value.trim().is_empty()) {
            let payload = self.client.get_json(&meeting_path(meeting_id), &[]).await?;
            return normalize_meeting_ref(&payload, tenant_id);
        }
        if let Some(join_web_url) = join_web_url.filter(|value| !value.trim().is_empty()) {
            let escaped = join_web_url.replace('\'', "''");
            let payload = self
                .client
                .get_json(
                    "/communications/onlineMeetings",
                    &[("$filter", format!("JoinWebUrl eq '{escaped}'"))],
                )
                .await?;
            let candidate = payload
                .get("value")
                .and_then(Value::as_array)
                .and_then(|items| items.first())
                .ok_or_else(|| {
                    TeamsPipelineError::Invalid(format!(
                        "Teams meeting not found for join URL: {join_web_url}"
                    ))
                })?;
            return normalize_meeting_ref(candidate, tenant_id);
        }
        Err(TeamsPipelineError::Invalid(
            "Either meeting_id or join_web_url is required.".into(),
        ))
    }

    async fn fetch_preferred_transcript_text(
        &self,
        meeting_ref: &TeamsMeetingRef,
    ) -> TeamsPipelineResult<Option<(MeetingArtifact, String)>> {
        let transcripts = self
            .client
            .collect_paginated(&format!(
                "{}/transcripts",
                meeting_path(&meeting_ref.meeting_id)
            ))
            .await?
            .into_iter()
            .filter_map(|payload| normalize_artifact("transcript", &payload, None).ok())
            .collect::<Vec<_>>();
        let Some(transcript) = select_preferred_transcript(&transcripts) else {
            return Ok(None);
        };
        let path = transcript_download_path(meeting_ref, &transcript);
        let bytes = self.client.download_bytes(&path).await?;
        let text = String::from_utf8_lossy(&bytes).trim().to_string();
        if text.is_empty() {
            return Ok(None);
        }
        Ok(Some((transcript, text)))
    }

    async fn list_recording_artifacts(
        &self,
        meeting_ref: &TeamsMeetingRef,
    ) -> TeamsPipelineResult<Vec<MeetingArtifact>> {
        let payloads = self
            .client
            .collect_paginated(&format!(
                "{}/recordings",
                meeting_path(&meeting_ref.meeting_id)
            ))
            .await?;
        Ok(payloads
            .into_iter()
            .filter_map(|payload| normalize_artifact("recording", &payload, None).ok())
            .collect())
    }

    async fn download_recording_artifact(
        &self,
        meeting_ref: &TeamsMeetingRef,
        recording: &MeetingArtifact,
    ) -> TeamsPipelineResult<Vec<u8>> {
        self.client
            .download_bytes(&recording_download_path(meeting_ref, recording))
            .await
    }

    async fn enrich_meeting_with_call_record(
        &self,
        _meeting_ref: &TeamsMeetingRef,
        call_record_id: Option<&str>,
    ) -> TeamsPipelineResult<Option<MeetingArtifact>> {
        let Some(call_record_id) = call_record_id.filter(|value| !value.trim().is_empty()) else {
            return Ok(None);
        };
        let path = format!(
            "/communications/callRecords/{}",
            path_percent_encode(call_record_id)
        );
        match self.client.get_json(&path, &[]).await {
            Ok(payload) => Ok(fetch_call_record_artifact_from_payload(&payload)),
            Err(TeamsPipelineError::Graph {
                status: 401 | 403 | 404,
                ..
            }) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

fn meeting_path(meeting_id: &str) -> String {
    format!(
        "/communications/onlineMeetings/{}",
        path_percent_encode(meeting_id)
    )
}

fn recording_download_path(meeting_ref: &TeamsMeetingRef, artifact: &MeetingArtifact) -> String {
    artifact.download_url.clone().unwrap_or_else(|| {
        format!(
            "{}/recordings/{}/content",
            meeting_path(&meeting_ref.meeting_id),
            path_percent_encode(&artifact.artifact_id)
        )
    })
}

fn transcript_download_path(meeting_ref: &TeamsMeetingRef, artifact: &MeetingArtifact) -> String {
    artifact.download_url.clone().unwrap_or_else(|| {
        format!(
            "{}/transcripts/{}/content",
            meeting_path(&meeting_ref.meeting_id),
            path_percent_encode(&artifact.artifact_id)
        )
    })
}

fn normalize_meeting_ref(
    payload: &Value,
    tenant_id: Option<&str>,
) -> TeamsPipelineResult<TeamsMeetingRef> {
    let object = payload.as_object().ok_or_else(|| {
        TeamsPipelineError::Invalid("Graph meeting payload was not an object.".into())
    })?;
    let mut metadata = Map::new();
    for key in [
        "subject",
        "startDateTime",
        "endDateTime",
        "createdDateTime",
        "participants",
    ] {
        if let Some(value) = object.get(key) {
            metadata.insert(key.into(), value.clone());
        }
    }
    let meeting_ref = TeamsMeetingRef {
        meeting_id: object
            .get("id")
            .and_then(value_to_string)
            .unwrap_or_default(),
        organizer_user_id: parse_organizer_user_id(payload),
        join_web_url: object.get("joinWebUrl").and_then(value_to_string),
        calendar_event_id: object.get("calendarEventId").and_then(value_to_string),
        thread_id: parse_thread_id(payload),
        tenant_id: tenant_id
            .map(ToOwned::to_owned)
            .or_else(|| object.get("tenantId").and_then(value_to_string)),
        metadata,
    };
    meeting_ref.validate()?;
    Ok(meeting_ref)
}

fn parse_organizer_user_id(payload: &Value) -> Option<String> {
    payload
        .get("organizer")
        .and_then(|v| v.get("identity"))
        .and_then(|v| v.get("user"))
        .and_then(|v| v.get("id"))
        .and_then(value_to_string)
}

fn parse_thread_id(payload: &Value) -> Option<String> {
    payload
        .get("chatInfo")
        .and_then(|v| v.get("threadId"))
        .and_then(value_to_string)
        .or_else(|| payload.get("threadId").and_then(value_to_string))
}

fn normalize_artifact(
    artifact_type: &str,
    payload: &Value,
    default_source_url: Option<String>,
) -> TeamsPipelineResult<MeetingArtifact> {
    let object = payload.as_object().cloned().unwrap_or_default();
    let download_url = object
        .get("@microsoft.graph.downloadUrl")
        .and_then(value_to_string)
        .or_else(|| object.get("downloadUrl").and_then(value_to_string))
        .or_else(|| object.get("recordingContentUrl").and_then(value_to_string))
        .or_else(|| object.get("transcriptContentUrl").and_then(value_to_string));
    let source_url = object
        .get("webUrl")
        .and_then(value_to_string)
        .or_else(|| object.get("contentUrl").and_then(value_to_string))
        .or(default_source_url);
    let artifact = MeetingArtifact {
        artifact_type: artifact_type.into(),
        artifact_id: object
            .get("id")
            .and_then(value_to_string)
            .unwrap_or_default(),
        display_name: object
            .get("displayName")
            .and_then(value_to_string)
            .or_else(|| object.get("name").and_then(value_to_string)),
        content_type: object
            .get("contentType")
            .and_then(value_to_string)
            .or_else(|| object.get("fileMimeType").and_then(value_to_string)),
        source_url,
        download_url,
        created_at: object.get("createdDateTime").and_then(value_to_string),
        available_at: object
            .get("lastModifiedDateTime")
            .and_then(value_to_string)
            .or_else(|| object.get("meetingEndDateTime").and_then(value_to_string)),
        size_bytes: object.get("size").and_then(Value::as_u64),
        metadata: object,
    };
    artifact.validate()?;
    Ok(artifact)
}

pub fn select_preferred_transcript(candidates: &[MeetingArtifact]) -> Option<MeetingArtifact> {
    let mut transcripts = candidates
        .iter()
        .filter(|candidate| candidate.artifact_type == "transcript")
        .cloned()
        .collect::<Vec<_>>();
    transcripts.sort_by_key(transcript_sort_key);
    transcripts.pop()
}

fn transcript_sort_key(artifact: &MeetingArtifact) -> (u8, u8, String) {
    let status = artifact
        .metadata
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("")
        .to_ascii_lowercase();
    let is_completed = u8::from(matches!(
        status.as_str(),
        "available" | "completed" | "succeeded"
    ));
    let has_download = u8::from(artifact.download_url.is_some() || artifact.source_url.is_some());
    let timestamp = artifact
        .available_at
        .clone()
        .or_else(|| artifact.created_at.clone())
        .unwrap_or_default();
    (is_completed, has_download, timestamp)
}

fn fetch_call_record_artifact_from_payload(payload: &Value) -> Option<MeetingArtifact> {
    let object = payload.as_object()?;
    let id = object.get("id").and_then(value_to_string)?;
    let sessions = object
        .get("sessions")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let participants = object
        .get("participants")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0);
    let mut metrics = Map::new();
    if let Some(version) = object.get("version") {
        metrics.insert("version".into(), version.clone());
    }
    if let Some(modalities) = object.get("modalities") {
        metrics.insert("modalities".into(), modalities.clone());
    }
    metrics.insert("participant_count".into(), json!(participants));
    if sessions > 0 {
        metrics.insert("session_count".into(), json!(sessions));
    }
    if let Some(organizer) = parse_organizer_user_id(payload) {
        metrics.insert("organizer".into(), Value::String(organizer));
    }
    let mut metadata = Map::new();
    metadata.insert("call_record".into(), payload.clone());
    metadata.insert("metrics".into(), Value::Object(metrics));
    Some(MeetingArtifact {
        artifact_type: "call_record".into(),
        artifact_id: id,
        display_name: object
            .get("type")
            .and_then(value_to_string)
            .or_else(|| Some("call_record".into())),
        content_type: None,
        source_url: object.get("webUrl").and_then(value_to_string),
        download_url: None,
        created_at: object.get("startDateTime").and_then(value_to_string),
        available_at: object.get("endDateTime").and_then(value_to_string),
        size_bytes: None,
        metadata,
    })
}

pub fn default_change_type_for_resource(resource: &str) -> &'static str {
    let normalized = resource.trim().to_ascii_lowercase();
    if normalized.starts_with("communications/onlinemeetings/getalltranscripts")
        || normalized.starts_with("communications/onlinemeetings/getallrecordings")
        || normalized.starts_with("communications/callrecords")
    {
        "created"
    } else {
        "updated"
    }
}

pub fn expected_client_state(raw: Option<&str>) -> Option<String> {
    raw.map(ToOwned::to_owned)
        .or_else(|| env_nonempty("MSGRAPH_WEBHOOK_CLIENT_STATE"))
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn sync_graph_subscription_record(
    store: &TeamsPipelineStore,
    subscription_payload: Value,
    status: Option<&str>,
    renewed: bool,
) -> TeamsPipelineResult<Value> {
    let mut normalized = GraphSubscription::from_value(subscription_payload)?;
    if normalized.status.is_none() {
        normalized.status = Some(
            if let Some(expiration) = parse_datetime_utc(&normalized.expiration_datetime) {
                if expiration <= Utc::now() {
                    "expired".into()
                } else {
                    status.unwrap_or("active").into()
                }
            } else {
                status.unwrap_or("active").into()
            },
        );
    }
    if let Some(status) = status {
        normalized.status = Some(status.into());
    }
    if renewed {
        normalized.latest_renewal_at = Some(utc_now_iso());
    }
    let subscription_id = normalized.subscription_id.clone();
    store.upsert_subscription(
        &subscription_id,
        strip_empty_object_keys(serde_json::to_value(normalized)?),
    )
}

pub async fn maintain_graph_subscriptions(
    client: &MicrosoftGraphClient,
    store: &TeamsPipelineStore,
    renew_within_hours: u32,
    extend_hours: u32,
    dry_run: bool,
    client_state: Option<&str>,
) -> TeamsPipelineResult<Value> {
    let threshold_hours = renew_within_hours.max(1);
    let extend_hours = extend_hours.max(1);
    let managed_client_state = expected_client_state(client_state);
    let now = Utc::now();
    let remote_subscriptions = client.collect_paginated("/subscriptions").await?;
    let mut remote_ids = HashSet::new();
    let mut synced = 0usize;
    let mut candidates = Vec::new();
    let mut renewed = Vec::new();
    let mut skipped = Vec::new();

    for raw in &remote_subscriptions {
        let subscription_id = raw
            .get("id")
            .and_then(value_to_string)
            .or_else(|| raw.get("subscription_id").and_then(value_to_string))
            .unwrap_or_default();
        if subscription_id.is_empty() {
            continue;
        }
        let managed = store.get_subscription(&subscription_id)?.is_some()
            || managed_client_state
                .as_deref()
                .and_then(|expected| {
                    raw.get("clientState")
                        .and_then(value_to_string)
                        .map(|actual| actual == expected)
                })
                .unwrap_or(false);
        if !managed {
            skipped.push(json!({
                "subscription_id": subscription_id,
                "reason": "not_managed_by_teams_pipeline"
            }));
            continue;
        }
        remote_ids.insert(subscription_id.clone());
        sync_graph_subscription_record(store, raw.clone(), None, false)?;
        synced += 1;
        let Some(expiration_text) = raw.get("expirationDateTime").and_then(value_to_string) else {
            skipped
                .push(json!({"subscription_id": subscription_id, "reason": "missing_expiration"}));
            continue;
        };
        let Some(expiration) = parse_datetime_utc(&expiration_text) else {
            skipped
                .push(json!({"subscription_id": subscription_id, "reason": "invalid_expiration"}));
            continue;
        };
        let seconds_until_expiry = (expiration - now).num_seconds();
        if seconds_until_expiry < 0 {
            store.upsert_subscription(
                &subscription_id,
                json!({
                    "status": "expired",
                    "expiration_datetime": expiration.to_rfc3339_opts(SecondsFormat::Secs, true)
                }),
            )?;
            skipped.push(json!({
                "subscription_id": subscription_id,
                "reason": "already_expired",
                "expiration_datetime": expiration.to_rfc3339_opts(SecondsFormat::Secs, true)
            }));
            continue;
        }
        if seconds_until_expiry > i64::from(threshold_hours) * 3600 {
            skipped.push(json!({
                "subscription_id": subscription_id,
                "reason": "not_due",
                "expires_in_seconds": seconds_until_expiry
            }));
            continue;
        }
        let new_expiration = (std::cmp::max(now, expiration)
            + ChronoDuration::hours(i64::from(extend_hours)))
        .to_rfc3339_opts(SecondsFormat::Secs, true);
        let candidate = json!({
            "subscription_id": subscription_id,
            "resource": raw.get("resource").cloned().unwrap_or(Value::Null),
            "current_expiration": expiration_text,
            "new_expiration": new_expiration
        });
        candidates.push(candidate.clone());
        if dry_run {
            continue;
        }
        let patched = client
            .patch_json(
                &format!("/subscriptions/{}", path_percent_encode(&subscription_id)),
                json!({"expirationDateTime": new_expiration}),
            )
            .await?;
        let mut merged = raw.clone();
        if let (Some(base), Some(patch)) = (merged.as_object_mut(), patched.as_object()) {
            for (key, value) in patch {
                base.insert(key.clone(), value.clone());
            }
        }
        sync_graph_subscription_record(store, merged, Some("active"), true)?;
        renewed.push(json!({"candidate": candidate, "result": patched}));
    }

    for subscription_id in store.list_subscriptions()?.keys() {
        if !remote_ids.contains(subscription_id) {
            store.upsert_subscription(
                subscription_id,
                json!({
                    "status": "missing_remote",
                    "last_seen_missing_remote_at": utc_now_iso()
                }),
            )?;
        }
    }

    Ok(json!({
        "success": true,
        "dry_run": dry_run,
        "store_path": store.path(),
        "remote_subscription_count": remote_subscriptions.len(),
        "synced_subscription_count": synced,
        "candidate_count": candidates.len(),
        "renewed_count": renewed.len(),
        "threshold_hours": threshold_hours,
        "extend_hours": extend_hours,
        "candidates": candidates,
        "renewed": renewed,
        "skipped": skipped
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[derive(Default)]
    struct MockGraph {
        transcript: Mutex<Option<(MeetingArtifact, String)>>,
        recordings: Mutex<Vec<MeetingArtifact>>,
        recording_bytes: Mutex<Vec<u8>>,
        call_record: Mutex<Option<MeetingArtifact>>,
    }

    #[async_trait]
    impl TeamsGraphBackend for MockGraph {
        async fn resolve_meeting_reference(
            &self,
            meeting_id: Option<&str>,
            _join_web_url: Option<&str>,
            tenant_id: Option<&str>,
        ) -> TeamsPipelineResult<TeamsMeetingRef> {
            let mut metadata = Map::new();
            metadata.insert("subject".into(), Value::String("Weekly Sync".into()));
            metadata.insert(
                "participants".into(),
                json!([{"displayName": "Ada"}, {"identity": {"user": {"displayName": "Grace"}}}]),
            );
            Ok(TeamsMeetingRef {
                meeting_id: meeting_id.unwrap_or("meeting-1").into(),
                organizer_user_id: None,
                join_web_url: None,
                calendar_event_id: None,
                thread_id: None,
                tenant_id: tenant_id.map(ToOwned::to_owned),
                metadata,
            })
        }

        async fn fetch_preferred_transcript_text(
            &self,
            _meeting_ref: &TeamsMeetingRef,
        ) -> TeamsPipelineResult<Option<(MeetingArtifact, String)>> {
            Ok(self.transcript.lock().unwrap().clone())
        }

        async fn list_recording_artifacts(
            &self,
            _meeting_ref: &TeamsMeetingRef,
        ) -> TeamsPipelineResult<Vec<MeetingArtifact>> {
            Ok(self.recordings.lock().unwrap().clone())
        }

        async fn download_recording_artifact(
            &self,
            _meeting_ref: &TeamsMeetingRef,
            _recording: &MeetingArtifact,
        ) -> TeamsPipelineResult<Vec<u8>> {
            Ok(self.recording_bytes.lock().unwrap().clone())
        }

        async fn enrich_meeting_with_call_record(
            &self,
            _meeting_ref: &TeamsMeetingRef,
            _call_record_id: Option<&str>,
        ) -> TeamsPipelineResult<Option<MeetingArtifact>> {
            Ok(self.call_record.lock().unwrap().clone())
        }
    }

    struct MockTranscriber;

    #[async_trait]
    impl TeamsTranscriber for MockTranscriber {
        async fn transcribe_recording(
            &self,
            _recording: &MeetingArtifact,
            _bytes: &[u8],
            _config: &TeamsPipelineConfig,
        ) -> TeamsPipelineResult<String> {
            Ok("Action: Follow up with Legal.\nRisk: Budget approval pending.".into())
        }
    }

    struct CountingSummarizer {
        count: AtomicUsize,
    }

    #[async_trait]
    impl TeamsSummarizer for CountingSummarizer {
        async fn summarize(
            &self,
            resolved_meeting: &TeamsMeetingRef,
            transcript_text: &str,
            artifacts: &[MeetingArtifact],
            _config: &TeamsPipelineConfig,
        ) -> TeamsPipelineResult<TeamsMeetingSummaryPayload> {
            self.count.fetch_add(1, Ordering::SeqCst);
            Ok(TeamsMeetingSummaryPayload {
                meeting_ref: resolved_meeting.clone(),
                title: Some("Weekly Sync".into()),
                start_time: None,
                end_time: None,
                participants: collect_participants(resolved_meeting),
                transcript_text: Some(transcript_text.into()),
                summary: Some("Short summary".into()),
                key_decisions: vec!["Ship it.".into()],
                action_items: vec!["Send draft.".into()],
                risks: vec!["Timeline risk.".into()],
                call_metrics: collect_call_metrics(artifacts),
                source_artifacts: artifacts.to_vec(),
                confidence: Some("high".into()),
                confidence_notes: Some("Transcript available.".into()),
                notion_target: None,
                linear_target: None,
                teams_target: None,
            })
        }
    }

    struct MockSink;

    #[async_trait]
    impl TeamsSinkWriter for MockSink {
        async fn write_summary(
            &self,
            sink: TeamsSinkKind,
            _payload: &TeamsMeetingSummaryPayload,
            _config: &Value,
            existing_record: Option<Value>,
        ) -> TeamsPipelineResult<Value> {
            let id_key = match sink {
                TeamsSinkKind::Notion => "page_id",
                TeamsSinkKind::Linear => "issue_id",
                TeamsSinkKind::Teams => "message_id",
            };
            Ok(json!({ id_key: existing_record
                .as_ref()
                .and_then(|v| v.get(id_key))
                .cloned()
                .unwrap_or_else(|| Value::String(format!("{}-1", sink.as_str()))) }))
        }
    }

    fn artifact(kind: &str, id: &str) -> MeetingArtifact {
        MeetingArtifact {
            artifact_type: kind.into(),
            artifact_id: id.into(),
            display_name: Some(format!("{id}.vtt")),
            content_type: None,
            source_url: None,
            download_url: Some(format!("https://example.com/{id}")),
            created_at: None,
            available_at: None,
            size_bytes: None,
            metadata: Map::new(),
        }
    }

    fn pipeline(
        graph: Arc<MockGraph>,
        store: Arc<TeamsPipelineStore>,
        summarizer: Arc<CountingSummarizer>,
        config: TeamsPipelineConfig,
    ) -> TeamsMeetingPipeline {
        TeamsMeetingPipeline::new(graph, store, config)
            .with_transcriber(Arc::new(MockTranscriber))
            .with_summarizer(summarizer)
    }

    #[test]
    fn notification_receipt_hash_matches_canonical_shape() {
        let notification = json!({"b": 2, "a": 1});
        let key = TeamsPipelineStore::build_notification_receipt_key(&notification);
        let expected = Sha256::digest(br#"{"a":1,"b":2}"#);
        assert_eq!(key, format!("sha256:{}", hex::encode(expected)));
        assert_eq!(
            TeamsPipelineStore::build_notification_receipt_key(&json!({"id": "n1"})),
            "id:n1"
        );
    }

    #[test]
    fn store_persists_state() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("teams_pipeline_store.json");
        let store = TeamsPipelineStore::new(&path).unwrap();
        store
            .upsert_subscription(
                "sub-1",
                json!({"client_state": "abc", "resource": "communications/onlineMeetings"}),
            )
            .unwrap();
        store
            .record_event_timestamp("evt-1", Some("2026-05-03T19:30:00Z".into()))
            .unwrap();
        store
            .upsert_job("job-1", json!({"status": "received", "event_id": "evt-1"}))
            .unwrap();
        store
            .upsert_sink_record("notion:meeting-1", json!({"page_id": "page-1"}))
            .unwrap();

        let reloaded = TeamsPipelineStore::new(&path).unwrap();
        assert_eq!(
            reloaded
                .get_subscription("sub-1")
                .unwrap()
                .unwrap()
                .get("subscription_id")
                .unwrap(),
            "sub-1"
        );
        assert_eq!(
            reloaded.get_event_timestamp("evt-1").unwrap().as_deref(),
            Some("2026-05-03T19:30:00Z")
        );
        assert_eq!(
            reloaded
                .get_job("job-1")
                .unwrap()
                .unwrap()
                .get("status")
                .unwrap(),
            "received"
        );
        assert_eq!(
            reloaded
                .get_sink_record("notion:meeting-1")
                .unwrap()
                .unwrap()
                .get("page_id")
                .unwrap(),
            "page-1"
        );
    }

    #[tokio::test]
    async fn transcript_first_path_persists_state() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(TeamsPipelineStore::new(tmp.path().join("store.json")).unwrap());
        let graph = Arc::new(MockGraph::default());
        *graph.transcript.lock().unwrap() = Some((
            artifact("transcript", "tx-1"),
            "Action: Send draft by Friday.\nDecision: Ship the transcript-first path.\nDetailed transcript content.".into(),
        ));
        *graph.call_record.lock().unwrap() = Some(MeetingArtifact {
            artifact_type: "call_record".into(),
            artifact_id: "call-1".into(),
            display_name: Some("call_record".into()),
            content_type: None,
            source_url: None,
            download_url: None,
            created_at: None,
            available_at: None,
            size_bytes: None,
            metadata: Map::from_iter([("metrics".into(), json!({"participant_count": 4}))]),
        });
        let summarizer = Arc::new(CountingSummarizer {
            count: AtomicUsize::new(0),
        });
        let pipeline = pipeline(
            graph,
            store.clone(),
            summarizer.clone(),
            TeamsPipelineConfig {
                transcript_min_chars: 20,
                ..TeamsPipelineConfig::default()
            },
        );

        let job = pipeline
            .run_notification(json!({
                "id": "notif-1",
                "changeType": "updated",
                "resource": "communications/onlineMeetings/meeting-123",
                "resourceData": {"id": "meeting-123"}
            }))
            .await
            .unwrap();

        assert_eq!(job.status, "completed");
        assert_eq!(
            job.selected_artifact_strategy.as_deref(),
            Some("transcript_first")
        );
        assert_eq!(
            job.summary_payload.unwrap().summary.as_deref(),
            Some("Short summary")
        );
        assert_eq!(
            store
                .get_job(&job.job_id)
                .unwrap()
                .unwrap()
                .get("status")
                .unwrap(),
            "completed"
        );
        assert_eq!(summarizer.count.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn recording_fallback_uses_transcriber_and_sinks() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(TeamsPipelineStore::new(tmp.path().join("store.json")).unwrap());
        let graph = Arc::new(MockGraph::default());
        *graph.recordings.lock().unwrap() = vec![artifact("recording", "rec-1")];
        *graph.recording_bytes.lock().unwrap() = b"recording bytes".to_vec();
        let summarizer = Arc::new(CountingSummarizer {
            count: AtomicUsize::new(0),
        });
        let pipeline = pipeline(
            graph,
            store.clone(),
            summarizer,
            TeamsPipelineConfig {
                notion: Some(json!({"enabled": true, "database_id": "db-1"})),
                teams_delivery: Some(json!({"enabled": true, "channel_id": "channel-1"})),
                ..TeamsPipelineConfig::default()
            },
        )
        .with_notion_writer(Arc::new(MockSink))
        .with_teams_sender(Arc::new(MockSink));

        let job = pipeline
            .run_notification(json!({
                "id": "notif-2",
                "changeType": "updated",
                "resource": "communications/onlineMeetings/meeting-456",
                "resourceData": {"id": "meeting-456"}
            }))
            .await
            .unwrap();

        assert_eq!(job.status, "completed");
        assert_eq!(
            job.selected_artifact_strategy.as_deref(),
            Some("recording_stt_fallback")
        );
        assert_eq!(
            store
                .get_sink_record("notion:meeting-456")
                .unwrap()
                .unwrap()
                .get("page_id")
                .unwrap(),
            "notion-1"
        );
        assert_eq!(
            store
                .get_sink_record("teams:meeting-456")
                .unwrap()
                .unwrap()
                .get("message_id")
                .unwrap(),
            "teams-1"
        );
    }

    #[tokio::test]
    async fn missing_transcript_and_recording_schedules_retry() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(TeamsPipelineStore::new(tmp.path().join("store.json")).unwrap());
        let graph = Arc::new(MockGraph::default());
        let summarizer = Arc::new(CountingSummarizer {
            count: AtomicUsize::new(0),
        });
        let pipeline = pipeline(graph, store, summarizer, TeamsPipelineConfig::default());

        let job = pipeline
            .run_notification(json!({
                "id": "notif-3",
                "changeType": "updated",
                "resource": "communications/onlineMeetings/meeting-789",
                "resourceData": {"id": "meeting-789"}
            }))
            .await
            .unwrap();

        assert_eq!(job.status, "retry_scheduled");
        assert_eq!(job.error_info.get("retryable"), Some(&Value::Bool(true)));
        assert!(job
            .error_info
            .get("message")
            .and_then(Value::as_str)
            .unwrap()
            .contains("Recording unavailable"));
    }

    #[tokio::test]
    async fn duplicate_notification_reuses_completed_job() {
        let tmp = tempfile::tempdir().unwrap();
        let store = Arc::new(TeamsPipelineStore::new(tmp.path().join("store.json")).unwrap());
        let graph = Arc::new(MockGraph::default());
        *graph.transcript.lock().unwrap() = Some((
            artifact("transcript", "tx-dup"),
            "Decision: Keep duplicate notifications idempotent.\nAction: Verify cached job reuse."
                .into(),
        ));
        let summarizer = Arc::new(CountingSummarizer {
            count: AtomicUsize::new(0),
        });
        let pipeline = pipeline(
            graph,
            store.clone(),
            summarizer.clone(),
            TeamsPipelineConfig {
                transcript_min_chars: 20,
                ..TeamsPipelineConfig::default()
            },
        );
        let notification = json!({
            "id": "notif-dup",
            "changeType": "updated",
            "resource": "communications/onlineMeetings/meeting-dup",
            "resourceData": {"id": "meeting-dup"}
        });

        let first = pipeline
            .run_notification(notification.clone())
            .await
            .unwrap();
        let second = pipeline.run_notification(notification).await.unwrap();

        assert_eq!(first.status, "completed");
        assert_eq!(second.status, "completed");
        assert_eq!(first.job_id, second.job_id);
        assert_eq!(summarizer.count.load(Ordering::SeqCst), 1);
        assert_eq!(store.list_jobs().unwrap().len(), 1);
    }

    #[test]
    fn summary_helpers_match_python_shape() {
        let parsed = parse_summary_json(
            r#"prefix {"summary":"ok","key_decisions":["Decision: yes"],"action_items":["Action: go"],"risks":["Risk: x"],"confidence":"high","confidence_notes":"clean"} suffix"#,
        )
        .unwrap();
        assert_eq!(parsed.summary, "ok");
        assert_eq!(parsed.key_decisions, vec!["Decision: yes"]);
        assert_eq!(parsed.confidence, "high");

        let meeting = TeamsMeetingRef {
            meeting_id: "m1".into(),
            organizer_user_id: None,
            join_web_url: None,
            calendar_event_id: None,
            thread_id: None,
            tenant_id: None,
            metadata: Map::from_iter([("subject".into(), Value::String("Demo".into()))]),
        };
        let prompt = build_summary_prompt(&meeting, "hello", &[artifact("transcript", "tx")]);
        assert!(prompt.contains("Meeting ID: m1"));
        assert!(prompt.contains("Title: Demo"));
    }

    #[test]
    fn resource_meeting_id_extraction_and_change_type_defaults() {
        assert_eq!(
            extract_meeting_id_from_resource("communications/onlineMeetings/meeting-1/transcripts")
                .as_deref(),
            Some("meeting-1")
        );
        assert_eq!(
            default_change_type_for_resource("communications/onlineMeetings/getAllTranscripts"),
            "created"
        );
        assert_eq!(
            default_change_type_for_resource("communications/onlineMeetings/meeting-1"),
            "updated"
        );
    }
}
