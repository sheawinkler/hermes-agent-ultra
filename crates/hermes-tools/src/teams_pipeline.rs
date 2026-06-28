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

include!("teams_pipeline/store_records.rs");
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

include!("teams_pipeline/graph_backend.rs");

include!("teams_pipeline/subscriptions.rs");

#[cfg(test)]
mod tests;
