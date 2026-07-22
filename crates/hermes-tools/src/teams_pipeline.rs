//! Rust-native Microsoft Teams meeting summary pipeline.
//!
//! This module ports the Teams pipeline runtime into Rust. The Python plugin
//! files are reference material only; job state, Graph access, transcript-first
//! orchestration, recording/STT fallback, summaries, and sink bookkeeping live
//! here.

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, SecondsFormat, Utc};
use parking_lot::Mutex;
use reqwest::{Client, Method, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::process::Command;
use uuid::Uuid;

use crate::bounded_subprocess::{
    run_bounded_command_with_output_limit, BoundedCommandError, BoundedOutputDirectoryLimit,
};
use hermes_config::hermes_home;
use hermes_core::{subprocess::CommandNoWindowExt, ToolError};

pub const DEFAULT_TEAMS_PIPELINE_STORE_FILENAME: &str = "rust_teams_pipeline_store.json";
const TEAMS_FFMPEG_TIMEOUT: Duration = Duration::from_secs(120);
const TEAMS_FFMPEG_SEGMENT_SECONDS: u64 = 300;
const TEAMS_FFMPEG_AUDIO_BITRATE: &str = "64k";
const TEAMS_FFMPEG_AUDIO_BITRATE_BITS_PER_SECOND: u64 = 64_000;
const MAX_TEAMS_RECORDING_BYTES: usize = 512 * 1024 * 1024;
const MAX_TEAMS_TRANSCRIPT_BYTES: usize = 8 * 1024 * 1024;
const MAX_TEAMS_GRAPH_JSON_BYTES: usize = 4 * 1024 * 1024;
const MAX_TEAMS_GRAPH_PAGINATION_PAGES: usize = 32;
const MAX_TEAMS_GRAPH_PAGINATION_ITEMS: usize = 4_096;
const MAX_TEAMS_GRAPH_PAGINATION_BYTES: usize = 16 * 1024 * 1024;
const MAX_TEAMS_GRAPH_TOKEN_BYTES: usize = 64 * 1024;
const MAX_TEAMS_GRAPH_ERROR_BYTES: usize = 64 * 1024;
// OpenAI and Groq accept files below 25 MB. Keep a decimal margin for the
// multipart envelope and use it for every selected STT route.
const MAX_TEAMS_STT_UPLOAD_BYTES: u64 = 24_000_000;
const MAX_TEAMS_AUDIO_SEGMENTS: usize = 128;
// 64 kbps mono MP3 for every allowed five-minute segment, plus 1 MiB of
// container overhead per segment. The running monitor kills ffmpeg before a
// hostile timestamp stream can consume an unbounded amount of disk.
const MAX_TEAMS_AUDIO_OUTPUT_BYTES: u64 = (MAX_TEAMS_AUDIO_SEGMENTS as u64
    * TEAMS_FFMPEG_SEGMENT_SECONDS
    * TEAMS_FFMPEG_AUDIO_BITRATE_BITS_PER_SECOND
    / 8)
    + (MAX_TEAMS_AUDIO_SEGMENTS as u64 * 1024 * 1024);
// Sanitization below emits ASCII only, so this component cap is identical in
// UTF-8 bytes and Windows UTF-16 code units.
const MAX_SAFE_TEAMS_RECORDING_STEM_BYTES: usize = 128;

pub const TERMINAL_PIPELINE_STATES: &[&str] = &["completed", "failed", "retry_scheduled"];
pub const ACTIVE_PIPELINE_STATES: &[&str] = &[
    "received",
    "resolving_meeting",
    "fetching_transcript",
    "downloading_recording",
    "transcribing_audio",
    "summarizing",
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
    #[error("Microsoft Graph HTTP {status}: {message}")]
    Graph {
        status: u16,
        message: String,
        retry_after_secs: Option<u64>,
    },
}

impl TeamsPipelineError {
    fn is_retryable(&self) -> bool {
        matches!(self, Self::Retryable(_) | Self::ArtifactNotFound(_))
            || matches!(
                self,
                Self::Graph {
                    status: 0 | 408 | 429 | 500..=u16::MAX,
                    ..
                }
            )
    }

    fn retry_after_secs(&self) -> Option<u64> {
        match self {
            Self::Graph {
                retry_after_secs, ..
            } => *retry_after_secs,
            _ => None,
        }
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

fn parse_u64(value: Option<&Value>, default: u64) -> u64 {
    match value {
        Some(Value::Number(value)) => value.as_u64().unwrap_or(default),
        Some(Value::String(value)) => value.trim().parse::<u64>().unwrap_or(default),
        _ => default,
    }
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct ScopedTeamsArtifactResource {
    organizer_user_id: String,
    meeting_id: String,
    artifact_type: String,
    artifact_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TeamsNotificationIdentity {
    tenant_id: String,
    subscription_id: String,
    change_type: String,
    scoped_resource: ScopedTeamsArtifactResource,
}

fn stable_notification_identity(
    notification: &Value,
) -> TeamsPipelineResult<TeamsNotificationIdentity> {
    let resource_data = object_field(notification, "resourceData")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let top_resource = value_string(object_field(notification, "resource"));
    let data_resource = resource_data.get("@odata.id").and_then(value_to_string);
    let top_scope = top_resource
        .as_deref()
        .map(parse_scoped_teams_artifact_resource)
        .transpose()?
        .flatten();
    let data_scope = data_resource
        .as_deref()
        .map(parse_scoped_teams_artifact_resource)
        .transpose()?
        .flatten();
    let scoped_resource = match (top_scope, data_scope) {
        (Some(top), Some(data)) if top != data => {
            return Err(TeamsPipelineError::Invalid(
                "Graph notification resource and resourceData.@odata.id identify different Teams artifacts."
                    .into(),
            ));
        }
        (Some(scope), _) | (_, Some(scope)) => scope,
        (None, None) => {
            return Err(TeamsPipelineError::Invalid(
                "Graph notification has no parseable scoped users/{organizer}/onlineMeetings/{meeting}/{artifact} resource."
                    .into(),
            ));
        }
    };
    if let Some(resource_data_artifact_id) = resource_data.get("id").and_then(value_to_string) {
        let resource_data_artifact_id = decode_resource_identifier(&resource_data_artifact_id)?;
        if resource_data_artifact_id != scoped_resource.artifact_id {
            return Err(TeamsPipelineError::Invalid(
                "Graph notification resourceData.id does not match its scoped artifact id.".into(),
            ));
        }
    }
    if let Some(resource_data_meeting_id) = resource_data.get("meetingId").and_then(value_to_string)
    {
        let resource_data_meeting_id = decode_resource_identifier(&resource_data_meeting_id)?;
        if resource_data_meeting_id != scoped_resource.meeting_id {
            return Err(TeamsPipelineError::Invalid(
                "Graph notification resourceData.meetingId does not match its scoped meeting id."
                    .into(),
            ));
        }
    }
    if let Some(resource_data_organizer) = resource_data
        .get("organizerUserId")
        .and_then(value_to_string)
    {
        if normalize_organizer_user_id(&resource_data_organizer)?
            != scoped_resource.organizer_user_id
        {
            return Err(TeamsPipelineError::Invalid(
                "Graph notification resourceData.organizerUserId does not match its scoped organizer."
                    .into(),
            ));
        }
    }
    let tenant_id = resource_data
        .get("tenantId")
        .and_then(value_to_string)
        .or_else(|| value_string(object_field(notification, "tenantId")))
        .ok_or_else(|| {
            TeamsPipelineError::Invalid(
                "Graph notification tenantId is required for idempotency.".into(),
            )
        })?;
    let subscription_id = value_string(object_field(notification, "subscriptionId"))
        .or_else(|| value_string(object_field(notification, "subscription_id")))
        .ok_or_else(|| {
            TeamsPipelineError::Invalid(
                "Graph notification subscriptionId is required for idempotency.".into(),
            )
        })?;
    let change_type = value_string(object_field(notification, "changeType"))
        .ok_or_else(|| {
            TeamsPipelineError::Invalid(
                "Graph notification changeType is required for idempotency.".into(),
            )
        })?
        .to_ascii_lowercase();
    Ok(TeamsNotificationIdentity {
        tenant_id,
        subscription_id,
        change_type,
        scoped_resource,
    })
}

fn stable_notification_receipt_key(
    notification: &Value,
) -> TeamsPipelineResult<(String, TeamsNotificationIdentity)> {
    let identity = stable_notification_identity(notification)?;
    let canonical = format!(
        "tenant={}\nsubscription={}\nchange={}\norganizer={}\nmeeting={}\nartifact_type={}\nartifact={}",
        identity.tenant_id,
        identity.subscription_id,
        identity.change_type,
        identity.scoped_resource.organizer_user_id,
        identity.scoped_resource.meeting_id,
        identity.scoped_resource.artifact_type,
        identity.scoped_resource.artifact_id,
    );
    let digest = Sha256::digest(canonical.as_bytes());
    Ok((format!("graph:sha256:{}", hex::encode(digest)), identity))
}

fn normalize_organizer_user_id(value: &str) -> TeamsPipelineResult<String> {
    Uuid::parse_str(value.trim())
        .map(|identifier| identifier.to_string())
        .map_err(|_| {
            TeamsPipelineError::Invalid(
                "Teams organizer_user_id must be a Microsoft Entra object-id UUID.".into(),
            )
        })
}

fn parse_scoped_teams_artifact_resource(
    resource: &str,
) -> TeamsPipelineResult<Option<ScopedTeamsArtifactResource>> {
    let mut parts = resource
        .trim()
        .trim_start_matches('/')
        .split('?')
        .next()
        .unwrap_or_default()
        .split('/')
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>();
    if parts
        .first()
        .is_some_and(|part| matches!(*part, "v1.0" | "beta"))
    {
        parts.remove(0);
    }
    let Some(users_index) = parts.iter().position(|part| *part == "users") else {
        return Ok(None);
    };
    if users_index != 0 {
        return Err(TeamsPipelineError::Invalid(
            "Graph notification scoped resource must begin with users/{organizer}.".into(),
        ));
    }
    let organizer = parts.get(1).ok_or_else(|| {
        TeamsPipelineError::Invalid(
            "Graph notification scoped resource has no organizer id.".into(),
        )
    })?;
    let organizer_user_id = normalize_organizer_user_id(&decode_resource_identifier(organizer)?)?;
    let (meeting_id, artifact_cursor) =
        parse_scoped_resource_collection(&parts, 2, "onlineMeetings")?;
    let artifact_component = parts.get(artifact_cursor).ok_or_else(|| {
        TeamsPipelineError::Invalid(
            "Graph notification scoped resource has no transcript or recording collection.".into(),
        )
    })?;
    let artifact_type = if *artifact_component == "transcripts"
        || artifact_component.starts_with("transcripts(")
    {
        "transcript"
    } else if *artifact_component == "recordings" || artifact_component.starts_with("recordings(") {
        "recording"
    } else {
        return Err(TeamsPipelineError::Invalid(
            "Graph notification scoped resource must identify a transcript or recording artifact."
                .into(),
        ));
    };
    let collection = if artifact_type == "transcript" {
        "transcripts"
    } else {
        "recordings"
    };
    let (artifact_id, after_artifact) =
        parse_scoped_resource_collection(&parts, artifact_cursor, collection)?;
    if parts
        .get(after_artifact..)
        .is_some_and(|remaining| !remaining.is_empty() && remaining != ["content"])
    {
        return Err(TeamsPipelineError::Invalid(
            "Graph notification scoped resource has unsupported trailing path components.".into(),
        ));
    }
    Ok(Some(ScopedTeamsArtifactResource {
        organizer_user_id,
        meeting_id,
        artifact_type: artifact_type.into(),
        artifact_id,
    }))
}

fn parse_scoped_resource_collection(
    parts: &[&str],
    index: usize,
    collection: &str,
) -> TeamsPipelineResult<(String, usize)> {
    let component = parts.get(index).ok_or_else(|| {
        TeamsPipelineError::Invalid(format!(
            "Graph notification resource has no {collection} collection."
        ))
    })?;
    if *component == collection {
        let identifier = parts.get(index + 1).ok_or_else(|| {
            TeamsPipelineError::Invalid(format!(
                "Graph notification resource has no {collection} identifier."
            ))
        })?;
        return Ok((decode_resource_identifier(identifier)?, index + 2));
    }
    let prefix = format!("{collection}(");
    if let Some(inside) = component
        .strip_prefix(&prefix)
        .and_then(|component| component.strip_suffix(')'))
    {
        let quoted = inside
            .strip_prefix('\'')
            .and_then(|inside| inside.strip_suffix('\''))
            .ok_or_else(|| {
                TeamsPipelineError::Invalid(format!(
                    "Graph notification OData {collection} identifier must be single-quoted."
                ))
            })?;
        return Ok((
            decode_resource_identifier(&quoted.replace("''", "'"))?,
            index + 1,
        ));
    }
    Err(TeamsPipelineError::Invalid(format!(
        "Graph notification resource expected {collection} collection."
    )))
}

fn decode_resource_identifier(value: &str) -> TeamsPipelineResult<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(TeamsPipelineError::Invalid(
            "Graph notification resource contains an empty identifier.".into(),
        ));
    }
    let mut decoded = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = bytes
                .get(index + 1)
                .copied()
                .and_then(hex_nibble)
                .ok_or_else(|| {
                    TeamsPipelineError::Invalid(
                        "Graph notification resource has an invalid percent escape.".into(),
                    )
                })?;
            let low = bytes
                .get(index + 2)
                .copied()
                .and_then(hex_nibble)
                .ok_or_else(|| {
                    TeamsPipelineError::Invalid(
                        "Graph notification resource has an invalid percent escape.".into(),
                    )
                })?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded)
        .map_err(|_| {
            TeamsPipelineError::Invalid(
                "Graph notification resource identifier is not valid UTF-8.".into(),
            )
        })
        .map(|value| value.trim().to_string())
        .and_then(|value| {
            if value.is_empty() {
                Err(TeamsPipelineError::Invalid(
                    "Graph notification resource contains an empty identifier.".into(),
                ))
            } else {
                Ok(value)
            }
        })
}

fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
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
pub fn resolve_teams_pipeline_store_path(path: Option<&Path>) -> TeamsPipelineResult<PathBuf> {
    let selected = path
        .filter(|path| !path.as_os_str().is_empty())
        .map(Path::to_path_buf)
        .unwrap_or_else(|| hermes_home().join(DEFAULT_TEAMS_PIPELINE_STORE_FILENAME));
    let python_store = env_nonempty("MSGRAPH_WEBHOOK_STORE_PATH")
        .map(PathBuf::from)
        .unwrap_or_else(|| hermes_home().join("teams_pipeline_store.json"));
    if comparable_store_path(&selected) == comparable_store_path(&python_store) {
        return Err(TeamsPipelineError::Config(format!(
            "Rust Teams CLI store {} overlaps the Python Teams webhook store {}; choose a distinct --store-path.",
            selected.display(),
            python_store.display()
        )));
    }
    Ok(selected)
}

fn comparable_store_path(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    if let Ok(canonical) = std::fs::canonicalize(&absolute) {
        return canonical;
    }
    let Some(file_name) = absolute.file_name() else {
        return absolute;
    };
    absolute
        .parent()
        .and_then(|parent| std::fs::canonicalize(parent).ok())
        .map(|parent| parent.join(file_name))
        .unwrap_or(absolute)
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
    /// Maximum server Retry-After delay recorded for an operator; this
    /// pipeline never dispatches retries automatically.
    pub retry_after_cap_secs: u64,
    pub retry_attempt_cap: u32,
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
            retry_after_cap_secs: 3_600,
            retry_attempt_cap: 5,
        }
    }
}

impl TeamsPipelineConfig {
    pub fn from_value(value: Option<Value>) -> TeamsPipelineResult<Self> {
        let Some(value) = value else {
            return Ok(Self::default());
        };
        let object = value.as_object().ok_or_else(|| {
            TeamsPipelineError::Config("Teams pipeline config must be an object.".into())
        })?;
        let unsupported = ["notion", "linear", "teams_delivery"]
            .into_iter()
            .filter(|key| object.contains_key(*key))
            .collect::<Vec<_>>();
        if !unsupported.is_empty() {
            return Err(TeamsPipelineError::Config(format!(
                "External Teams sinks ({}) are unsupported in this Rust release; the optional source plugin is not packaged or verified.",
                unsupported.join(", ")
            )));
        }
        let default = Self::default();
        Ok(Self {
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
            retry_after_cap_secs: parse_u64(
                object.get("retry_after_cap_secs"),
                default.retry_after_cap_secs,
            )
            .clamp(1, 86_400),
            retry_attempt_cap: parse_u64(
                object.get("retry_attempt_cap"),
                u64::from(default.retry_attempt_cap),
            )
            .clamp(1, 100) as u32,
        })
    }
}

#[async_trait]
pub trait TeamsGraphBackend: Send + Sync {
    async fn resolve_meeting_reference(
        &self,
        meeting_id: Option<&str>,
        organizer_user_id: Option<&str>,
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

pub struct TeamsMeetingPipeline {
    graph_backend: Arc<dyn TeamsGraphBackend>,
    store: Arc<TeamsPipelineStore>,
    config: TeamsPipelineConfig,
    transcriber: Arc<dyn TeamsTranscriber>,
    summarizer: Arc<dyn TeamsSummarizer>,
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
        let (event_id, identity) = stable_notification_receipt_key(&notification)?;
        let resource_data = object_field(&notification, "resourceData")
            .and_then(Value::as_object)
            .cloned()
            .unwrap_or_default();
        let mut metadata = Map::from_iter([
            (
                "notification_artifact_id".into(),
                Value::String(identity.scoped_resource.artifact_id.clone()),
            ),
            (
                "notification_artifact_type".into(),
                Value::String(identity.scoped_resource.artifact_type.clone()),
            ),
        ]);
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
            source_event_type: identity.change_type.clone(),
            dedupe_key: event_id.clone(),
            status: "received".into(),
            retry_count: 0,
            created_at: None,
            updated_at: None,
            meeting_ref: Some(TeamsMeetingRef {
                meeting_id: identity.scoped_resource.meeting_id.clone(),
                organizer_user_id: Some(identity.scoped_resource.organizer_user_id.clone()),
                join_web_url: resource_data.get("joinWebUrl").and_then(value_to_string),
                calendar_event_id: None,
                thread_id: None,
                tenant_id: Some(identity.tenant_id.clone()),
                metadata,
            }),
            selected_artifact_strategy: None,
            summary_payload: None,
            error_info: Map::new(),
        };
        self.store
            .create_notification_job(&event_id, &notification, job)
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

        let resolved_meeting = self
            .graph_backend
            .resolve_meeting_reference(
                Some(&meeting_ref.meeting_id),
                meeting_ref.organizer_user_id.as_deref(),
                meeting_ref.join_web_url.as_deref(),
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

        let call_record_id = meeting_ref
            .metadata
            .get("call_record_id")
            .and_then(value_to_string);
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

        job.status = "completed".into();
        self.persist_job(job)
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
        TeamsPipelineError::Graph { .. } => "Graph",
    }
}

fn append_teams_transcript_chunk(transcript: &mut String, chunk: &str) -> TeamsPipelineResult<()> {
    let separator_bytes = usize::from(!transcript.is_empty());
    let total_bytes = transcript
        .len()
        .checked_add(separator_bytes)
        .and_then(|bytes| bytes.checked_add(chunk.len()))
        .ok_or_else(|| {
            TeamsPipelineError::Retryable("Teams transcript size overflowed its limit.".into())
        })?;
    if total_bytes > MAX_TEAMS_TRANSCRIPT_BYTES {
        return Err(TeamsPipelineError::Retryable(format!(
            "Combined STT transcript exceeds the {} byte limit.",
            MAX_TEAMS_TRANSCRIPT_BYTES
        )));
    }
    if separator_bytes != 0 {
        transcript.push('\n');
    }
    transcript.push_str(chunk);
    Ok(())
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
        if bytes.is_empty() {
            return Err(TeamsPipelineError::Retryable(
                "Recording fallback received an empty recording.".into(),
            ));
        }
        if bytes.len() > MAX_TEAMS_RECORDING_BYTES {
            return Err(TeamsPipelineError::Retryable(format!(
                "Recording fallback input exceeds the {} byte limit.",
                MAX_TEAMS_RECORDING_BYTES
            )));
        }
        let temp_root = config
            .tmp_dir
            .clone()
            .unwrap_or_else(|| hermes_home().join("tmp").join("teams_pipeline"));
        let run_dir_guard = create_private_teams_run_dir(&temp_root).await?;
        let run_dir = run_dir_guard.path().to_path_buf();
        let result: TeamsPipelineResult<String> = async {
            let recording_name = recording
                .display_name
                .clone()
                .unwrap_or_else(|| format!("{}.mp4", recording.artifact_id));
            let recording_path = run_dir.join(safe_file_name(&recording_name));
            write_private_teams_recording(&recording_path, bytes).await?;
            let segment_dir = run_dir.join("audio-segments");
            let audio_paths =
                prepare_audio_paths(&recording_path, &segment_dir, config.ffmpeg_extract_audio)
                    .await?;
            let handler = crate::tools::transcription::TranscriptionHandler;
            let mut transcript_text = String::new();
            for audio_path in audio_paths {
                let response = handler
                    .execute_with_trusted_roots(
                        json!({
                            "audio_path": audio_path.to_string_lossy(),
                            "model": config.stt_model.as_deref(),
                        }),
                        std::slice::from_ref(&run_dir),
                    )
                    .await?;
                let parsed: Value = serde_json::from_str(&response)?;
                let transcript = parsed
                    .get("text")
                    .and_then(Value::as_str)
                    .or_else(|| parsed.get("transcript").and_then(Value::as_str))
                    .unwrap_or("")
                    .trim();
                if transcript.is_empty() {
                    return Err(TeamsPipelineError::Retryable(
                        "STT returned an empty transcript.".into(),
                    ));
                }
                append_teams_transcript_chunk(&mut transcript_text, transcript)?;
            }
            Ok(transcript_text)
        }
        .await;
        let cleanup = tokio::task::spawn_blocking(move || run_dir_guard.close())
            .await
            .map_err(|error| {
                TeamsPipelineError::Io(format!("Teams temp cleanup task failed: {error}"))
            })?;
        cleanup?;
        result
    }
}

struct PrivateTeamsRunDir {
    path: Option<PathBuf>,
}

impl PrivateTeamsRunDir {
    fn path(&self) -> &Path {
        self.path
            .as_deref()
            .expect("private Teams run directory is present")
    }

    fn close(mut self) -> std::io::Result<()> {
        let result = std::fs::remove_dir_all(self.path());
        if result.is_ok() {
            self.path.take();
        }
        result
    }
}

impl Drop for PrivateTeamsRunDir {
    fn drop(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = std::fs::remove_dir_all(path);
        }
    }
}

async fn create_private_teams_run_dir(temp_root: &Path) -> TeamsPipelineResult<PrivateTeamsRunDir> {
    let temp_root = temp_root.to_path_buf();
    tokio::task::spawn_blocking(move || create_private_teams_run_dir_sync(&temp_root))
        .await
        .map_err(|error| {
            TeamsPipelineError::Io(format!("Teams temp directory task failed: {error}"))
        })?
        .map_err(TeamsPipelineError::from)
}

fn create_private_teams_run_dir_sync(temp_root: &Path) -> std::io::Result<PrivateTeamsRunDir> {
    // No secret is created until the shared policy has verified the root and
    // every reachable ancestor. `create_dir_all` may only create empty
    // directories; the following check rejects an unsafe custom root before
    // the run directory is allocated.
    std::fs::create_dir_all(temp_root)?;
    crate::tts_audio::ensure_safe_private_media_parent(
        &temp_root.join(".teams-run-directory-probe"),
    )?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mut builder = tempfile::Builder::new();
        builder
            .prefix("teams-recording-")
            .permissions(std::fs::Permissions::from_mode(0o700));
        let temp_dir = builder.tempdir_in(temp_root)?;
        let path = temp_dir.keep();
        if let Err(error) =
            crate::tts_audio::ensure_safe_private_media_parent(&path.join(".teams-run-probe"))
        {
            let _ = std::fs::remove_dir_all(&path);
            return Err(error);
        }
        return Ok(PrivateTeamsRunDir { path: Some(path) });
    }
    #[cfg(windows)]
    {
        for _ in 0..64 {
            let path = temp_root.join(format!("teams-recording-{}", Uuid::new_v4()));
            match windows_private_path::create_directory(&path) {
                Ok(()) => {
                    if let Err(error) = crate::tts_audio::ensure_safe_private_media_parent(
                        &path.join(".teams-run-probe"),
                    ) {
                        let _ = std::fs::remove_dir_all(&path);
                        return Err(error);
                    }
                    return Ok(PrivateTeamsRunDir { path: Some(path) });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "could not allocate a unique private Teams run directory",
        ));
    }
    #[cfg(not(any(unix, windows)))]
    {
        let path = temp_root.join(format!("teams-recording-{}", Uuid::new_v4()));
        std::fs::create_dir(&path)?;
        crate::tts_audio::ensure_safe_private_media_parent(&path.join(".teams-run-probe"))?;
        Ok(PrivateTeamsRunDir { path: Some(path) })
    }
}

async fn write_private_teams_recording(path: &Path, bytes: &[u8]) -> TeamsPipelineResult<()> {
    let path = path.to_path_buf();
    let file = tokio::task::spawn_blocking(move || {
        crate::tts_audio::create_private_temp_file(&path, &path, false)
    })
    .await
    .map_err(|error| {
        TeamsPipelineError::Io(format!("Teams recording file task failed: {error}"))
    })??;
    let mut file = tokio::fs::File::from_std(file);
    file.write_all(bytes).await?;
    file.flush().await?;
    Ok(())
}

async fn create_private_teams_directory(path: &Path) -> TeamsPipelineResult<()> {
    let path = path.to_path_buf();
    tokio::task::spawn_blocking(move || create_private_teams_directory_sync(&path))
        .await
        .map_err(|error| {
            TeamsPipelineError::Io(format!("Teams directory task failed: {error}"))
        })??;
    Ok(())
}

fn create_private_teams_directory_sync(path: &Path) -> std::io::Result<()> {
    crate::tts_audio::ensure_safe_private_media_parent(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::create_dir(path)?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    #[cfg(windows)]
    {
        windows_private_path::create_directory(path)?;
    }
    #[cfg(not(any(unix, windows)))]
    {
        std::fs::create_dir(path)?;
    }
    if let Err(error) =
        crate::tts_audio::ensure_safe_private_media_parent(&path.join(".teams-segment-probe"))
    {
        let _ = std::fs::remove_dir_all(path);
        return Err(error);
    }
    Ok(())
}

async fn ensure_private_teams_media_file(path: &Path) -> TeamsPipelineResult<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).await?;
    }
    #[cfg(windows)]
    {
        let path = path.to_path_buf();
        tokio::task::spawn_blocking(move || windows_private_path::set_owner_only_dacl(&path))
            .await
            .map_err(|error| {
                TeamsPipelineError::Io(format!("Teams file DACL task failed: {error}"))
            })??;
    }
    Ok(())
}

#[cfg(windows)]
mod windows_private_path {
    use std::ffi::c_void;
    use std::mem::size_of;
    use std::os::windows::ffi::OsStrExt;
    use std::path::Path;

    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    const ERROR_INSUFFICIENT_BUFFER: i32 = 122;
    const ERROR_ALREADY_EXISTS: i32 = 183;

    #[repr(C)]
    struct SecurityAttributes {
        length: u32,
        security_descriptor: *mut c_void,
        inherit_handle: i32,
    }

    #[link(name = "advapi32")]
    unsafe extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            string_security_descriptor: *const u16,
            string_sd_revision: u32,
            security_descriptor: *mut *mut c_void,
            security_descriptor_size: *mut u32,
        ) -> i32;
        fn ConvertSecurityDescriptorToStringSecurityDescriptorW(
            security_descriptor: *const c_void,
            requested_string_sd_revision: u32,
            security_information: u32,
            string_security_descriptor: *mut *mut u16,
            string_security_descriptor_len: *mut u32,
        ) -> i32;
        fn GetFileSecurityW(
            file_name: *const u16,
            requested_information: u32,
            security_descriptor: *mut c_void,
            length: u32,
            length_needed: *mut u32,
        ) -> i32;
        fn SetFileSecurityW(
            file_name: *const u16,
            security_information: u32,
            security_descriptor: *const c_void,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn CreateDirectoryW(
            path_name: *const u16,
            security_attributes: *const SecurityAttributes,
        ) -> i32;
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }

    fn wide(path: &Path) -> std::io::Result<Vec<u16>> {
        let mut value = path.as_os_str().encode_wide().collect::<Vec<_>>();
        if value.contains(&0) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path contains NUL",
            ));
        }
        value.push(0);
        Ok(value)
    }

    fn private_descriptor() -> std::io::Result<*mut c_void> {
        let sddl = "D:P(A;;FA;;;OW)"
            .encode_utf16()
            .chain(std::iter::once(0))
            .collect::<Vec<_>>();
        let mut descriptor = std::ptr::null_mut();
        let mut size = 0_u32;
        if unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                sddl.as_ptr(),
                1,
                &mut descriptor,
                &mut size,
            )
        } == 0
            || descriptor.is_null()
            || size == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        Ok(descriptor)
    }

    pub(super) fn create_directory(path: &Path) -> std::io::Result<()> {
        let path = wide(path)?;
        let descriptor = private_descriptor()?;
        let attributes = SecurityAttributes {
            length: size_of::<SecurityAttributes>() as u32,
            security_descriptor: descriptor,
            inherit_handle: 0,
        };
        let created = unsafe { CreateDirectoryW(path.as_ptr(), &attributes) };
        unsafe {
            LocalFree(descriptor);
        }
        if created == 0 {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(ERROR_ALREADY_EXISTS) {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    error,
                ));
            }
            return Err(error);
        }
        Ok(())
    }

    pub(super) fn set_owner_only_dacl(path: &Path) -> std::io::Result<()> {
        let path = wide(path)?;
        let descriptor = private_descriptor()?;
        let updated =
            unsafe { SetFileSecurityW(path.as_ptr(), DACL_SECURITY_INFORMATION, descriptor) };
        unsafe {
            LocalFree(descriptor);
        }
        if updated == 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn has_protected_owner_only_dacl(path: &Path) -> std::io::Result<bool> {
        let path = wide(path)?;
        let mut needed = 0_u32;
        let initial = unsafe {
            GetFileSecurityW(
                path.as_ptr(),
                DACL_SECURITY_INFORMATION,
                std::ptr::null_mut(),
                0,
                &mut needed,
            )
        };
        if initial != 0 || needed == 0 {
            return Err(std::io::Error::last_os_error());
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(ERROR_INSUFFICIENT_BUFFER) {
            return Err(error);
        }
        let mut descriptor = vec![0_u8; needed as usize];
        if unsafe {
            GetFileSecurityW(
                path.as_ptr(),
                DACL_SECURITY_INFORMATION,
                descriptor.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        } == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let mut rendered = std::ptr::null_mut();
        let mut rendered_len = 0_u32;
        if unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                descriptor.as_ptr().cast(),
                1,
                DACL_SECURITY_INFORMATION,
                &mut rendered,
                &mut rendered_len,
            )
        } == 0
            || rendered.is_null()
        {
            return Err(std::io::Error::last_os_error());
        }
        let sddl = unsafe {
            String::from_utf16_lossy(std::slice::from_raw_parts(rendered, rendered_len as usize))
        };
        unsafe {
            LocalFree(rendered.cast());
        }
        Ok(sddl.trim_end_matches('\0') == "D:P(A;;FA;;;OW)")
    }
}

fn safe_file_name(value: &str) -> String {
    let Some(leaf) = value
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .map(str::trim)
    else {
        return "recording.mp4".into();
    };
    let cleaned = leaf
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>();
    let (stem, requested_extension) = cleaned.rsplit_once('.').unwrap_or((cleaned.as_str(), ""));
    let stem = stem.trim_matches('.');
    if stem.is_empty() {
        return "recording.mp4".into();
    }
    let extension = requested_extension.to_ascii_lowercase();
    let extension = if is_safe_recording_extension(&extension) {
        extension.as_str()
    } else {
        "mp4"
    };
    let stem = if is_windows_reserved_file_stem(stem) {
        "recording"
    } else {
        stem
    };
    let stem = &stem[..stem.len().min(MAX_SAFE_TEAMS_RECORDING_STEM_BYTES)];
    format!("{stem}.{extension}")
}

fn is_safe_recording_extension(extension: &str) -> bool {
    is_supported_transcription_audio_extension(extension)
        || matches!(extension, "mp4" | "mov" | "mkv" | "avi" | "mpeg" | "mpg")
}

// Windows reserves these device basenames even when a normal-looking
// extension is appended. Normalize them on every platform so an artifact
// chosen elsewhere remains creatable on Windows.
fn is_windows_reserved_file_stem(stem: &str) -> bool {
    let device = stem.split('.').next().unwrap_or(stem);
    if ["con", "prn", "aux", "nul", "conin_", "conout_"]
        .iter()
        .any(|reserved| device.eq_ignore_ascii_case(reserved))
    {
        return true;
    }
    device.len() == 4
        && (device[..3].eq_ignore_ascii_case("com") || device[..3].eq_ignore_ascii_case("lpt"))
        && matches!(device.as_bytes()[3], b'1'..=b'9')
}

fn is_supported_transcription_audio_extension(extension: &str) -> bool {
    matches!(
        extension,
        "wav" | "mp3" | "m4a" | "webm" | "ogg" | "oga" | "flac"
    )
}

async fn prepare_audio_paths(
    recording_path: &Path,
    segment_dir: &Path,
    ffmpeg_extract_audio: bool,
) -> TeamsPipelineResult<Vec<PathBuf>> {
    prepare_audio_paths_with_timeout(
        recording_path,
        segment_dir,
        ffmpeg_extract_audio,
        TEAMS_FFMPEG_TIMEOUT,
    )
    .await
}

async fn prepare_audio_paths_with_timeout(
    recording_path: &Path,
    segment_dir: &Path,
    ffmpeg_extract_audio: bool,
    timeout: Duration,
) -> TeamsPipelineResult<Vec<PathBuf>> {
    let metadata = tokio::fs::metadata(recording_path).await?;
    let extension = recording_path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !ffmpeg_extract_audio
        && is_supported_transcription_audio_extension(&extension)
        && metadata.len() < MAX_TEAMS_STT_UPLOAD_BYTES
    {
        return Ok(vec![recording_path.to_path_buf()]);
    }

    create_private_teams_directory(segment_dir).await?;
    let segment_template = segment_dir.join("segment-%04d.mp3");
    let mut command = Command::new("ffmpeg");
    command
        .arg("-nostdin")
        .arg("-hide_banner")
        .arg("-loglevel")
        .arg("error")
        .arg("-y")
        .arg("-i")
        .arg(recording_path)
        .arg("-vn")
        .arg("-map")
        .arg("0:a:0")
        .arg("-map_metadata")
        .arg("-1")
        .arg("-ac")
        .arg("1")
        .arg("-ar")
        .arg("16000")
        .arg("-c:a")
        .arg("libmp3lame")
        .arg("-b:a")
        .arg(TEAMS_FFMPEG_AUDIO_BITRATE)
        .arg("-minrate")
        .arg(TEAMS_FFMPEG_AUDIO_BITRATE)
        .arg("-maxrate")
        .arg(TEAMS_FFMPEG_AUDIO_BITRATE)
        .arg("-bufsize")
        .arg(TEAMS_FFMPEG_AUDIO_BITRATE)
        .arg("-f")
        .arg("segment")
        .arg("-segment_time")
        .arg(TEAMS_FFMPEG_SEGMENT_SECONDS.to_string())
        .arg("-segment_start_number")
        .arg("0")
        .arg("-reset_timestamps")
        .arg("1")
        .arg("-segment_format")
        .arg("mp3")
        .arg(&segment_template)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .suppress_windows_console();
    let output_limit = BoundedOutputDirectoryLimit::new(
        segment_dir.to_path_buf(),
        MAX_TEAMS_AUDIO_SEGMENTS,
        MAX_TEAMS_AUDIO_OUTPUT_BYTES,
    );
    let output = match run_bounded_command_with_output_limit(command, timeout, true, output_limit)
        .await
    {
        Ok(output) => output,
        Err(BoundedCommandError::Start(error)) => {
            return Err(TeamsPipelineError::Retryable(format!(
                "Recording fallback requires ffmpeg for mono MP3 extraction, but ffmpeg failed to start: {error}"
            )));
        }
        Err(BoundedCommandError::Stdin(error)) => {
            return Err(TeamsPipelineError::Retryable(format!(
                "ffmpeg audio extraction rejected its closed stdin: {error}"
            )));
        }
        Err(BoundedCommandError::Wait(error)) => {
            return Err(TeamsPipelineError::Retryable(format!(
                "ffmpeg audio extraction wait failed: {error}"
            )));
        }
        Err(BoundedCommandError::Stderr(error)) => {
            return Err(TeamsPipelineError::Retryable(format!(
                "ffmpeg audio extraction diagnostics failed: {error}"
            )));
        }
        Err(BoundedCommandError::OutputLimit(error)) => {
            return Err(TeamsPipelineError::Retryable(format!(
                "ffmpeg audio extraction exceeded its bounded output limit: {error}"
            )));
        }
        Err(BoundedCommandError::TimedOut) => {
            return Err(TeamsPipelineError::Retryable(format!(
                "ffmpeg audio extraction timed out after {:.3} seconds",
                timeout.as_secs_f64()
            )));
        }
    };
    let detail = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if !output.status.success() {
        let detail = if detail.is_empty() {
            String::new()
        } else {
            format!(": {detail}")
        };
        return Err(TeamsPipelineError::Retryable(format!(
            "ffmpeg mono MP3 extraction failed with {}{detail}",
            output.status
        )));
    }
    collect_teams_audio_segments(segment_dir).await
}

async fn collect_teams_audio_segments(segment_dir: &Path) -> TeamsPipelineResult<Vec<PathBuf>> {
    let mut entries = tokio::fs::read_dir(segment_dir).await?;
    let mut segments = Vec::new();
    while let Some(entry) = entries.next_entry().await? {
        if segments.len() >= MAX_TEAMS_AUDIO_SEGMENTS {
            return Err(TeamsPipelineError::Retryable(format!(
                "ffmpeg produced more than {MAX_TEAMS_AUDIO_SEGMENTS} Teams audio segments."
            )));
        }
        let file_name = entry.file_name();
        let name = file_name.to_str().ok_or_else(|| {
            TeamsPipelineError::Retryable("ffmpeg produced a non-UTF-8 segment name.".into())
        })?;
        let index = teams_audio_segment_index(name).ok_or_else(|| {
            TeamsPipelineError::Retryable(format!(
                "ffmpeg produced an unexpected Teams segment name: {name}"
            ))
        })?;
        if !entry.file_type().await?.is_file() {
            return Err(TeamsPipelineError::Retryable(format!(
                "ffmpeg produced a non-file Teams segment: {name}"
            )));
        }
        let path = entry.path();
        let metadata = entry.metadata().await?;
        if metadata.len() == 0 {
            return Err(TeamsPipelineError::Retryable(format!(
                "ffmpeg produced an empty Teams segment: {name}"
            )));
        }
        if metadata.len() >= MAX_TEAMS_STT_UPLOAD_BYTES {
            return Err(TeamsPipelineError::Retryable(format!(
                "ffmpeg produced Teams segment {name} at {} bytes, above the {} byte STT upload limit.",
                metadata.len(),
                MAX_TEAMS_STT_UPLOAD_BYTES
            )));
        }
        ensure_private_teams_media_file(&path).await?;
        segments.push((index, path));
    }
    if segments.is_empty() {
        return Err(TeamsPipelineError::Retryable(
            "ffmpeg produced no audio segments for the recording.".into(),
        ));
    }
    segments.sort_unstable_by_key(|(index, _)| *index);
    for (expected, (actual, _)) in segments.iter().enumerate() {
        if *actual != expected {
            return Err(TeamsPipelineError::Retryable(format!(
                "ffmpeg produced non-contiguous Teams segment order; expected {expected:04}, got {actual:04}."
            )));
        }
    }
    Ok(segments.into_iter().map(|(_, path)| path).collect())
}

fn teams_audio_segment_index(name: &str) -> Option<usize> {
    let index = name.strip_prefix("segment-")?.strip_suffix(".mp3")?;
    if index.len() != 4 || !index.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    index.parse().ok()
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
        _config: &TeamsPipelineConfig,
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
