//! Fail-closed redaction, identity derivation, envelope encryption, and private CAS storage.
//!
//! The public append identity is deliberately separate from the credential-free
//! private metadata wrapper. The latter leaves this module as plaintext only
//! after an explicit artifact-read authorization decision.

use crate::run_ledger::{ReplayOperationKind, ReplayOperationOutcome, ReplayOperationPhase};
use aes_gcm::aead::{rand_core::RngCore, Aead, AeadCore, KeyInit, OsRng, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use base64::prelude::{Engine as _, BASE64_STANDARD};
use hermes_intelligence::{is_sensitive_field_name, redact_sensitive_text};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::SystemTime;
use zeroize::{Zeroize, Zeroizing};

pub const ARTIFACT_CANONICALIZATION: &str = "rfc8785-jcs";
pub const ARTIFACT_CANONICALIZATION_VERSION: u8 = 1;
pub const ARTIFACT_ENCRYPTION_ALGORITHM: &str = "aes-256-gcm";
pub const ARTIFACT_FINGERPRINT_ALGORITHM: &str = "hmac-sha256";
pub const ARTIFACT_FINGERPRINT_VERSION: u8 = 1;
pub const SEALED_ARTIFACT_SCHEMA_VERSION: u32 = 1;
const APPEND_IDENTITY_DOMAIN: &[u8] = b"hermes.session-run.append-private-fingerprint/v1";
const SOURCE_COMMITMENT_DOMAIN: &[u8] = b"hermes.session-run.metadata-source-commitment/v1";
const SOURCE_COMMITMENT_VERSION: u8 = 1;
const PLAINTEXT_DIGEST_BYTES: usize = 32;
const AES_GCM_TAG_BYTES: usize = 16;
const AES_GCM_NONCE_BYTES: usize = 12;
const MAX_ARTIFACT_BYTES: usize = 128 * 1024 * 1024;
const MAX_ARTIFACT_IDENTIFIER_BYTES: usize = 128;
const ARTIFACT_FILESYSTEM_SAFETY_MARGIN_BYTES: u64 = 4 * 1024 * 1024;
const STORE_LOCK_FILE: &str = ".artifact-store.lock";
const CAPACITY_RESERVATION_LOCK_FILE: &str = ".artifact-capacity.lock";
const STORE_ID_FILE: &str = ".artifact-store-id";
const KEY_PROVISION_MARKER_PREFIX: &str = ".artifact-key-provision-";
const KEY_PROVISION_MARKER_SUFFIX: &str = ".json";
const KEY_PROVISION_MARKER_SCHEMA: &str = "hermes.artifact-key-provision/v1";

fn is_key_provision_marker_filename(name: &str) -> Result<bool, SealedArtifactError> {
    let Some(digest) = name
        .strip_prefix(KEY_PROVISION_MARKER_PREFIX)
        .and_then(|value| value.strip_suffix(KEY_PROVISION_MARKER_SUFFIX))
    else {
        return Ok(false);
    };
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(SealedArtifactError::InvalidMetadata(
            "artifact key provisioning marker filename is malformed".to_string(),
        ));
    }
    Ok(true)
}

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, thiserror::Error)]
pub enum SealedArtifactError {
    #[error("artifact redaction failed: {0}")]
    Redaction(String),
    #[error("invalid artifact metadata: {0}")]
    InvalidMetadata(String),
    #[error("artifact canonicalization failed: {0}")]
    Canonicalization(#[from] serde_json::Error),
    #[error("artifact cryptographic operation failed: {0}")]
    Cryptography(String),
    #[error("artifact envelope is invalid: {0}")]
    InvalidEnvelope(String),
    #[error("artifact access denied: {0}")]
    Unauthorized(String),
    #[error("artifact key is unavailable: {0}")]
    KeyUnavailable(String),
    #[error("authoritative artifact references are unavailable: {0}")]
    ReferenceSnapshot(String),
    #[error("artifact CAS conflict for digest {0}")]
    CasConflict(String),
    #[doc(hidden)]
    #[error("injected artifact CAS failure at {0}")]
    InjectedFailure(&'static str),
    #[error("artifact is too large: {actual} bytes exceeds {limit}")]
    TooLarge { actual: usize, limit: usize },
    #[error(
        "artifact storage free space is below admission threshold: {available_bytes} bytes available, {minimum_bytes} required"
    )]
    InsufficientFreeSpace {
        available_bytes: u64,
        minimum_bytes: u64,
    },
    #[error("artifact I/O failed: {0}")]
    Io(#[from] io::Error),
}

/// Output of a versioned, task-aware redactor.
///
/// `credential_free_private` may retain private task context, but it MUST NOT
/// retain provider credentials, authorization headers, cookies, or raw secret
/// material. Implementations fail rather than returning a partially redacted
/// object when classification is uncertain.
#[derive(Debug, Clone, PartialEq)]
pub struct RedactedArtifact {
    pub public_identity: Value,
    pub credential_free_private: Value,
}

pub trait ArtifactRedactor: Send + Sync {
    fn version(&self) -> &str;
    fn redact(&self, raw: &Value) -> Result<RedactedArtifact, SealedArtifactError>;
}

/// Closed credential-redacted replay projection for arbitrary runtime evidence.
/// The public ledger receives only a keyed commitment; replay data is encrypted
/// and can leave the private store only after an explicit authorization decision.
pub(crate) struct LedgerEvidenceRedactor;

impl ArtifactRedactor for LedgerEvidenceRedactor {
    fn version(&self) -> &str {
        "ledger-evidence-v5"
    }

    fn redact(&self, raw: &Value) -> Result<RedactedArtifact, SealedArtifactError> {
        let root = exact_artifact_object(raw, &["identity", "private"], "artifact root")?;
        let identity = root
            .get("identity")
            .ok_or_else(|| invalid_artifact("artifact identity is missing"))?;
        let identity_object = exact_artifact_object(
            identity,
            &[
                "artifact_kind",
                "evidence_id",
                "evidence_subject",
                "outcome",
                "replay_fidelity",
                "schema",
            ],
            "artifact identity",
        )?;
        if identity_object.get("schema").and_then(Value::as_str)
            != Some("hermes.runtime-evidence/v1")
        {
            return Err(invalid_artifact("unsupported runtime evidence schema"));
        }
        let artifact_kind = required_bounded_string(identity_object, "artifact_kind")?;
        if !matches!(artifact_kind, "request" | "result") {
            return Err(invalid_artifact("invalid runtime evidence artifact kind"));
        }
        let replay_fidelity = required_bounded_string(identity_object, "replay_fidelity")?;
        if !matches!(replay_fidelity, "credential_redacted" | "exact_private") {
            return Err(invalid_artifact("unsupported replay fidelity"));
        }
        let evidence_id = required_bounded_string(identity_object, "evidence_id")?;
        let evidence_subject = required_bounded_string(identity_object, "evidence_subject")?;
        let outcome = required_bounded_string(identity_object, "outcome")?;
        if (artifact_kind == "request" && outcome != "not_applicable")
            || (artifact_kind == "result"
                && !matches!(
                    outcome,
                    "returned" | "handler_error" | "effect_non_resumable_unknown"
                ))
            || (outcome == "effect_non_resumable_unknown" && evidence_subject != "llm-response")
        {
            return Err(invalid_artifact("invalid runtime evidence outcome"));
        }
        validate_runtime_evidence_subject(artifact_kind, evidence_subject)?;
        validate_runtime_evidence_id(evidence_subject, evidence_id)?;

        let private = root
            .get("private")
            .ok_or_else(|| invalid_artifact("artifact private evidence is missing"))?;
        let exact_validation = validate_exact_runtime_evidence(identity_object, private);
        let credential_free_private = match (replay_fidelity, exact_validation) {
            ("exact_private", Ok(_)) | ("credential_redacted", Ok(_)) => {
                // Exact preparation substitutes canonical private bytes after
                // this redactor returns. Do not create a second projection that
                // could be mistaken for the replay transcript.
                Value::Null
            }
            ("exact_private", Err(error)) => return Err(error),
            ("credential_redacted", Err(_)) => {
                if let Some(projection) =
                    closed_runtime_wrapper_summary(evidence_id, evidence_subject, private)?
                {
                    projection
                } else {
                    closed_runtime_replay_payload(
                        artifact_kind,
                        evidence_id,
                        evidence_subject,
                        private,
                    )?
                }
            }
            _ => unreachable!("replay fidelity was validated above"),
        };
        Ok(RedactedArtifact {
            public_identity: identity.clone(),
            credential_free_private,
        })
    }
}

const REDACTED_REPLAY_SECRET: &str = "[REDACTED_SECRET]";

fn credential_free_replay_payload(raw: &Value) -> Value {
    match raw {
        Value::Object(object) => Value::Object(
            object
                .iter()
                .map(|(key, value)| {
                    let value = if is_sensitive_field_name(key) {
                        Value::String(REDACTED_REPLAY_SECRET.to_string())
                    } else {
                        credential_free_replay_payload(value)
                    };
                    (key.clone(), value)
                })
                .collect(),
        ),
        Value::Array(values) => {
            Value::Array(values.iter().map(credential_free_replay_payload).collect())
        }
        Value::String(value) => Value::String(redact_sensitive_text(value)),
        value => value.clone(),
    }
}

const CLOSED_RUNTIME_REPLAY_SCHEMA: &str = "hermes.runtime-evidence/private-summary/v1";
const ALPHA_RUNTIME_V1_PRIVATE_TASK_SCHEMA: &str = "hermes.alpha-runtime-v1/private-task/v1";
const ALPHA_RUNTIME_V1_ENVELOPE_SCHEMA: &str = "hermes.alpha-runtime-v1/envelope/v1";
const ALPHA_RUNTIME_V1_DELIVERY_EVIDENCE_SCHEMA: &str =
    "hermes.alpha-runtime-v1/delivery-evidence/v1";
const DELIVERY_EVIDENCE_SUBJECT_PREFIX: &str = "hermes.session-run.outbox/";
const EXACT_RUN_START_PRIVATE_SCHEMA: &str = "hermes.agent-run-start-private/v1";

/// The only runtime transcript payloads that may be sealed as exact private
/// replay evidence. Every other runtime shape is metadata-only by design.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExactRuntimeEvidenceSchema {
    RunStart,
    LlmRequestV1,
    LlmResultV1,
    ToolIntentV1,
    ToolResultV1,
    DelegationRequestV1,
    DelegationResultV1,
    OauthRequestV1,
    OauthResultV1,
    AgentOutputV1,
    /// Closed Alpha outbox delivery completion. Unlike generic runtime
    /// transcripts, its public identity is explicitly `exact_private` and is
    /// additionally bound to the immutable leased outbox row by the ledger.
    DeliveryResultV1,
}

/// The event-local phase carried by a closed runtime transcript.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExactRuntimeEvidencePhase {
    Request,
    Result,
}

impl ExactRuntimeEvidencePhase {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Result => "result",
        }
    }
}

/// Closed outcome carried by a runtime transcript binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExactRuntimeEvidenceOutcome {
    NotApplicable,
    Returned,
    HandlerError,
    EffectNonResumableUnknown,
}

impl ExactRuntimeEvidenceOutcome {
    fn parse(value: &str) -> Result<Self, SealedArtifactError> {
        match value {
            "not_applicable" => Ok(Self::NotApplicable),
            "returned" => Ok(Self::Returned),
            "handler_error" => Ok(Self::HandlerError),
            "effect_non_resumable_unknown" => Ok(Self::EffectNonResumableUnknown),
            _ => Err(invalid_artifact("unsupported exact runtime outcome")),
        }
    }

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::NotApplicable => "not_applicable",
            Self::Returned => "returned",
            Self::HandlerError => "handler_error",
            Self::EffectNonResumableUnknown => "effect_non_resumable_unknown",
        }
    }
}

/// Typed, credential-free binding proven by the pure exact-runtime validator.
///
/// Consumers receive this only after both the committed public identity and
/// canonical private transcript agree. It deliberately carries no payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExactRuntimeEvidenceBinding {
    pub schema: ExactRuntimeEvidenceSchema,
    pub phase: ExactRuntimeEvidencePhase,
    pub outcome: ExactRuntimeEvidenceOutcome,
    pub evidence_id: String,
    pub evidence_subject: String,
}

/// Fidelity proven by the sealed-artifact private-JCS classifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReplayArtifactPrivateFidelity {
    ExactPrivate,
    MetadataOnly,
}

/// Classification produced by the unified private-JCS replay parser.
///
/// Exact classifications retain the parsed private evidence only in the opaque,
/// scrub-on-drop fragment. Metadata-only classifications deliberately expose no
/// private payload at all.
pub(crate) enum ReplayArtifactPrivateClassification {
    Exact(ReconstructedPrivateFragment),
    MetadataOnly {
        binding: ExactRuntimeEvidenceBinding,
    },
}

impl ReplayArtifactPrivateClassification {
    pub(crate) const fn fidelity(&self) -> ReplayArtifactPrivateFidelity {
        match self {
            Self::Exact(_) => ReplayArtifactPrivateFidelity::ExactPrivate,
            Self::MetadataOnly { .. } => ReplayArtifactPrivateFidelity::MetadataOnly,
        }
    }

    pub(crate) fn binding(&self) -> &ExactRuntimeEvidenceBinding {
        match self {
            Self::Exact(fragment) => fragment.binding(),
            Self::MetadataOnly { binding } => binding,
        }
    }
}

impl std::fmt::Debug for ReplayArtifactPrivateClassification {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Exact(fragment) => formatter.debug_tuple("Exact").field(fragment).finish(),
            Self::MetadataOnly { binding } => formatter
                .debug_struct("MetadataOnly")
                .field("binding", binding)
                .finish(),
        }
    }
}

const EXACT_RUNTIME_EVIDENCE_SCHEMA: &str = "hermes.runtime-evidence/v1";
const EXACT_RUNTIME_PRIVATE_KEYS: &[&str] = &[
    "evidence_id",
    "evidence_subject",
    "payload",
    "phase",
    "schema",
];
const EXACT_RUN_START_PRIVATE_KEYS: &[&str] = &[
    "schema",
    "system_prompt",
    "messages",
    "system_prompt_source",
    "injected_skills_block",
    "advertised_tools",
    "tool_contract",
    "config_projection",
    "execution_containment",
];
const EXACT_RUN_START_EXECUTION_CONTAINMENT_KEYS: &[&str] = &[
    "schema",
    "admission",
    "backend_kind",
    "scope",
    "policy_version",
    "configuration_digest",
];
const EXACT_RUN_START_CONFIG_PROJECTION_KEYS: &[&str] = &[
    "api_mode",
    "background_review_enabled",
    "background_review_metrics_enabled",
    "budget",
    "checkpoint_interval_turns",
    "code_index_enabled",
    "code_index_max_files",
    "code_index_max_symbols",
    "execution_mode",
    "lsp_context_enabled",
    "lsp_context_max_chars",
    "max_concurrent_delegates",
    "max_cost_usd",
    "max_delegate_depth",
    "max_tokens",
    "max_turns",
    "memory_flush_interval",
    "memory_nudge_interval",
    "preflight_context_compress",
    "quiet_mode",
    "rollback_on_tool_error_threshold",
    "skill_creation_nudge_interval",
    "skip_context_files",
    "skip_memory",
    "stream",
];

fn validate_runtime_evidence_subject(
    artifact_kind: &str,
    evidence_subject: &str,
) -> Result<(), SealedArtifactError> {
    let accepted = match artifact_kind {
        "request" => matches!(
            evidence_subject,
            "run-session"
                | "run-provenance"
                | "llm-request"
                | "tool-invocation"
                | "delegation"
                | "oauth-refresh"
                | "alpha-runtime-v1"
        ),
        "result" => {
            matches!(
                evidence_subject,
                "llm-response"
                    | "tool-invocation"
                    | "delegation"
                    | "agent-output"
                    | "oauth-refresh"
            ) || is_closed_delivery_evidence_subject(evidence_subject)
        }
        _ => false,
    };
    if accepted {
        Ok(())
    } else {
        Err(invalid_artifact("unsupported runtime evidence subject"))
    }
}

/// Delivery subjects carry a SHA-256 idempotency key that can resemble a
/// secret to the generic text redactor. Admit that shape only when every
/// segment is a closed, derived outbox identity; no generic exception applies.
fn is_closed_delivery_evidence_subject(value: &str) -> bool {
    let Some(value) = value.strip_prefix(DELIVERY_EVIDENCE_SUBJECT_PREFIX) else {
        return false;
    };
    let Some((destination, idempotency_key)) = value.split_once('/') else {
        return false;
    };
    destination == "alpha_runtime_v1" && is_lowercase_sha256_hex(idempotency_key)
}

/// Evidence IDs normally pass the generic text redactor. Runtime-generated
/// public IDs may contain opaque hashes or UUIDs whose digits can look like an
/// unrelated sensitive-text pattern (for example a phone number). Accept only
/// their closed derived forms rather than weakening generic ID validation.
fn validate_runtime_evidence_id(
    evidence_subject: &str,
    evidence_id: &str,
) -> Result<(), SealedArtifactError> {
    match evidence_subject {
        "oauth-refresh" => {
            let digest = evidence_id
                .strip_prefix("oauth-refresh-")
                .ok_or_else(|| invalid_artifact("OAuth refresh evidence id is invalid"))?;
            if is_lowercase_sha256_hex(digest) {
                return Ok(());
            }
            return Err(invalid_artifact("OAuth refresh evidence id is invalid"));
        }
        "agent-output"
            if evidence_id
                .strip_prefix("run-output-")
                .is_some_and(is_canonical_lowercase_uuid) =>
        {
            return Ok(());
        }
        "llm-request" | "llm-response"
            if evidence_id
                .strip_prefix("llm-attempt-")
                .is_some_and(|attempt| !attempt.is_empty() && attempt.parse::<u64>().is_ok()) =>
        {
            return Ok(());
        }
        subject
            if is_closed_delivery_evidence_subject(subject)
                && is_canonical_lowercase_uuid(evidence_id) =>
        {
            return Ok(());
        }
        _ => {}
    }
    if redact_sensitive_text(evidence_id) != evidence_id {
        return Err(invalid_artifact(
            "evidence_id contains a credential or secret indicator",
        ));
    }
    Ok(())
}

fn is_lowercase_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn is_canonical_lowercase_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')
            }
        })
}

fn closed_runtime_replay_payload(
    artifact_kind: &str,
    evidence_id: &str,
    evidence_subject: &str,
    private: &Value,
) -> Result<Value, SealedArtifactError> {
    match (artifact_kind, evidence_subject) {
        ("request", "run-session") => {
            if evidence_id != "session-identity" {
                return Err(invalid_artifact("run-session evidence id is invalid"));
            }
            let private =
                exact_artifact_object(private, &["session_id"], "session private evidence")?;
            required_safe_bounded_string(private, "session_id")?;
            Ok(closed_runtime_summary("session-identity"))
        }
        ("request", "run-provenance") => {
            if evidence_id != "run-start" {
                return Err(invalid_artifact("run-provenance evidence id is invalid"));
            }
            validate_closed_exact_run_start_payload(private)?;
            Ok(closed_runtime_summary("run-start"))
        }
        ("request", "alpha-runtime-v1") => {
            if evidence_id != "alpha-runtime-v1-task" {
                return Err(invalid_artifact("AlphaRuntimeV1 evidence id is invalid"));
            }
            closed_alpha_task_payload(private)
        }
        ("result", subject) if is_closed_delivery_evidence_subject(subject) => {
            closed_delivery_result_payload(private)
        }
        ("result", "agent-output") => {
            let private =
                exact_artifact_object(private, &["response"], "agent-output private evidence")?;
            let response = private
                .get("response")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_artifact("agent output response must be a string"))?;
            if response.len() > MAX_ARTIFACT_BYTES {
                return Err(SealedArtifactError::TooLarge {
                    actual: response.len(),
                    limit: MAX_ARTIFACT_BYTES,
                });
            }
            Ok(serde_json::json!({
                "response_byte_len": response.len(),
                "schema": CLOSED_RUNTIME_REPLAY_SCHEMA,
                "shape": "agent-output",
            }))
        }
        _ => closed_bound_runtime_payload(evidence_subject, private),
    }
}

/// It is deliberately narrower than `AgentConfig` and is accepted only after
/// field-aware credential validation. Tool-schema property names remain exact
/// metadata, while every string value and all non-schema fields must be
/// credential-free so an authorized unseal returns the prepared runtime bytes.
/// Validate the immutable private RunStart payload without constructing or
/// sealing an artifact. Deterministic replay uses this as a pure boundary
/// check after authorized unsealing.
pub(crate) fn validate_closed_exact_run_start_payload(
    private: &Value,
) -> Result<(), SealedArtifactError> {
    validate_closed_exact_run_start_private(private)
}

/// Parse canonical private JCS under a scrub-on-drop guard before validating
/// RunStart evidence and its committed public identity. Replay reports receive
/// no private value from this API.
pub(crate) fn validate_closed_exact_run_start_private_jcs(
    public_identity: &Value,
    private_jcs: &[u8],
) -> Result<ExactRuntimeEvidenceBinding, SealedArtifactError> {
    let binding = validate_exact_run_start_identity(public_identity)?;
    let private = ScrubbedJsonValue::parse_canonical(private_jcs)?;
    validate_closed_exact_run_start_private(private.as_value())?;
    Ok(binding)
}

fn validate_exact_run_start_identity(
    public_identity: &Value,
) -> Result<ExactRuntimeEvidenceBinding, SealedArtifactError> {
    let identity = exact_artifact_object(
        public_identity,
        &[
            "artifact_kind",
            "evidence_id",
            "evidence_subject",
            "outcome",
            "replay_fidelity",
            "schema",
        ],
        "exact run-start public identity",
    )?;
    if required_bounded_string(identity, "artifact_kind")? != "request"
        || required_bounded_string(identity, "evidence_id")? != "run-start"
        || required_safe_bounded_string(identity, "evidence_subject")? != "run-provenance"
        || required_bounded_string(identity, "outcome")? != "not_applicable"
        || required_bounded_string(identity, "replay_fidelity")? != "credential_redacted"
        || required_bounded_string(identity, "schema")? != EXACT_RUNTIME_EVIDENCE_SCHEMA
    {
        return Err(invalid_artifact(
            "exact run-start public identity is not event-bound",
        ));
    }
    Ok(ExactRuntimeEvidenceBinding {
        schema: ExactRuntimeEvidenceSchema::RunStart,
        phase: ExactRuntimeEvidencePhase::Request,
        outcome: ExactRuntimeEvidenceOutcome::NotApplicable,
        evidence_id: "run-start".to_string(),
        evidence_subject: "run-provenance".to_string(),
    })
}

fn validate_closed_exact_run_start_private(private: &Value) -> Result<(), SealedArtifactError> {
    let private = exact_artifact_object(
        private,
        EXACT_RUN_START_PRIVATE_KEYS,
        "exact run-start private evidence",
    )?;
    if required_bounded_string(private, "schema")? != EXACT_RUN_START_PRIVATE_SCHEMA {
        return Err(invalid_artifact(
            "unsupported exact run-start private evidence schema",
        ));
    }
    let system_prompt_source = required_bounded_string(private, "system_prompt_source")?;
    validate_nullable_bounded_string(private, "system_prompt")?;
    validate_nullable_bounded_string(private, "injected_skills_block")?;
    let messages = private
        .get("messages")
        .filter(|value| value.is_array())
        .ok_or_else(|| invalid_artifact("messages must be an array"))?;
    ensure_credential_free_identity(messages, "exact run-start messages")?;
    let advertised_tools = private
        .get("advertised_tools")
        .filter(|value| value.is_array())
        .ok_or_else(|| invalid_artifact("advertised_tools must be an array"))?;
    validate_exact_tool_collection(advertised_tools, false)?;
    let tool_contract = private
        .get("tool_contract")
        .filter(|value| value.is_array())
        .ok_or_else(|| invalid_artifact("tool_contract must be an array"))?;
    validate_exact_tool_collection(tool_contract, true)?;
    match system_prompt_source {
        "stored" => {
            if !private.get("system_prompt").is_some_and(Value::is_string)
                || !private
                    .get("injected_skills_block")
                    .is_some_and(Value::is_null)
            {
                return Err(invalid_artifact(
                    "stored system prompt must be present without injected skills provenance",
                ));
            }
        }
        "fresh" => {
            if !private.get("system_prompt").is_some_and(Value::is_string) {
                return Err(invalid_artifact("fresh system prompt must be present"));
            }
        }
        "unavailable" => {
            let no_system_prompt = private
                .get("system_prompt")
                .is_some_and(|value| value.is_null() || value.as_str().is_some_and(str::is_empty));
            if !no_system_prompt
                || !private
                    .get("injected_skills_block")
                    .is_some_and(Value::is_null)
                || !advertised_tools.as_array().is_some_and(Vec::is_empty)
                || !tool_contract.as_array().is_some_and(Vec::is_empty)
            {
                return Err(invalid_artifact(
                    "unavailable system prompt is permitted only for blocked input evidence",
                ));
            }
        }
        _ => {
            return Err(invalid_artifact(
                "system_prompt_source must be stored, fresh, or unavailable",
            ));
        }
    }
    validate_exact_run_start_execution_containment(private)?;
    let config = private
        .get("config_projection")
        .ok_or_else(|| invalid_artifact("config_projection is missing"))?;
    exact_artifact_object(
        config,
        EXACT_RUN_START_CONFIG_PROJECTION_KEYS,
        "exact run-start config projection",
    )?;
    ensure_credential_free_identity(config, "exact run-start config projection")?;

    let canonical = Zeroizing::new(serde_json_canonicalizer::to_vec(private)?);
    if canonical.len() > MAX_ARTIFACT_BYTES {
        return Err(SealedArtifactError::TooLarge {
            actual: canonical.len(),
            limit: MAX_ARTIFACT_BYTES,
        });
    }
    Ok(())
}

/// Validate a closed exact runtime transcript payload and return the exact
/// private value that may be sealed. The outer runtime identity remains
/// credential-free metadata; the private payload itself is admitted only for
/// one of the explicit schemas above.
fn closed_exact_runtime_payload(
    schema: ExactRuntimeEvidenceSchema,
    safe_raw: &Value,
    private_evidence: &Value,
) -> Result<(), SealedArtifactError> {
    let root = exact_artifact_object(safe_raw, &["identity", "private"], "exact runtime root")?;
    let raw_private = root
        .get("private")
        .ok_or_else(|| invalid_artifact("exact runtime private evidence is missing"))?;
    ensure_canonical_bytes_match(
        raw_private,
        private_evidence,
        "exact private evidence does not match the validated artifact",
    )?;
    if schema == ExactRuntimeEvidenceSchema::RunStart {
        return validate_closed_exact_run_start_private(private_evidence);
    }

    let identity = exact_artifact_object(
        root.get("identity")
            .ok_or_else(|| invalid_artifact("exact runtime identity is missing"))?,
        &[
            "artifact_kind",
            "evidence_id",
            "evidence_subject",
            "outcome",
            "replay_fidelity",
            "schema",
        ],
        "exact runtime identity",
    )?;
    let binding = validate_exact_runtime_evidence(identity, private_evidence)?;
    if binding.schema != schema {
        return Err(invalid_artifact(
            "exact runtime identity does not match its selected closed transcript schema",
        ));
    }
    Ok(())
}

/// Revalidate a committed exact runtime artifact after unsealing it.
///
/// This is the single parser used by both artifact admission and deterministic
/// reconstruction. It accepts only canonical private JCS and returns the
/// credential-free event-local binding; it never executes a provider or tool.
pub(crate) fn validate_closed_exact_runtime_private_jcs(
    public_identity: &Value,
    private_jcs: &[u8],
) -> Result<ExactRuntimeEvidenceBinding, SealedArtifactError> {
    let private_evidence = ScrubbedJsonValue::parse_canonical(private_jcs)?;
    let identity = exact_artifact_object(
        public_identity,
        &[
            "artifact_kind",
            "evidence_id",
            "evidence_subject",
            "outcome",
            "replay_fidelity",
            "schema",
        ],
        "exact runtime identity",
    )?;
    validate_exact_runtime_evidence(identity, private_evidence.as_value())
}

/// Classify a committed replay artifact without allowing private JSON to escape
/// the sealed-artifact boundary. Both exact and metadata-only payloads are
/// canonicalized and shape-validated under a scrub-on-drop guard.
pub(crate) fn validate_replay_artifact_private_jcs(
    public_identity: &Value,
    private_jcs: &[u8],
) -> Result<ReplayArtifactPrivateClassification, SealedArtifactError> {
    let private = ScrubbedJsonValue::parse_canonical(private_jcs)?;
    let is_run_start = public_identity
        .get("evidence_subject")
        .and_then(Value::as_str)
        == Some("run-provenance");
    if private
        .as_value()
        .get("replay_fidelity")
        .and_then(Value::as_str)
        == Some("metadata_only")
    {
        validate_metadata_only_replay_private(private.as_value())?;
        let binding = if is_run_start {
            validate_exact_run_start_identity(public_identity)?
        } else {
            let identity = exact_artifact_object(
                public_identity,
                &[
                    "artifact_kind",
                    "evidence_id",
                    "evidence_subject",
                    "outcome",
                    "replay_fidelity",
                    "schema",
                ],
                "metadata runtime identity",
            )?;
            exact_runtime_binding_from_identity(identity)?.0
        };
        return Ok(ReplayArtifactPrivateClassification::MetadataOnly { binding });
    }

    let binding = if is_run_start {
        let binding = validate_exact_run_start_identity(public_identity)?;
        validate_closed_exact_run_start_private(private.as_value())?;
        binding
    } else {
        let identity = exact_artifact_object(
            public_identity,
            &[
                "artifact_kind",
                "evidence_id",
                "evidence_subject",
                "outcome",
                "replay_fidelity",
                "schema",
            ],
            "exact runtime identity",
        )?;
        validate_exact_runtime_evidence(identity, private.as_value())?
    };
    Ok(ReplayArtifactPrivateClassification::Exact(
        ReconstructedPrivateFragment::new(binding, private),
    ))
}

fn validate_metadata_only_replay_private(value: &Value) -> Result<(), SealedArtifactError> {
    let object = exact_artifact_object(
        value,
        &[
            "payload",
            "replay_fidelity",
            "source_commitment",
            "source_commitment_algorithm",
            "source_commitment_version",
        ],
        "metadata-only replay private evidence",
    )?;
    if required_bounded_string(object, "replay_fidelity")? != "metadata_only"
        || required_bounded_string(object, "source_commitment_algorithm")?
            != ARTIFACT_FINGERPRINT_ALGORITHM
        || object
            .get("source_commitment_version")
            .and_then(Value::as_u64)
            != Some(u64::from(SOURCE_COMMITMENT_VERSION))
    {
        return Err(invalid_artifact(
            "metadata-only replay private evidence has an unsupported commitment",
        ));
    }
    let commitment = required_bounded_string(object, "source_commitment")?;
    if !is_lowercase_sha256_hex(commitment) {
        return Err(invalid_artifact(
            "metadata-only replay source commitment is malformed",
        ));
    }
    let payload = object
        .get("payload")
        .ok_or_else(|| invalid_artifact("metadata-only replay payload is missing"))?;
    ensure_credential_free_identity(payload, "metadata-only replay payload")?;
    Ok(())
}

fn validate_exact_runtime_evidence(
    identity: &serde_json::Map<String, Value>,
    private_evidence: &Value,
) -> Result<ExactRuntimeEvidenceBinding, SealedArtifactError> {
    let (binding, artifact_kind) = exact_runtime_binding_from_identity(identity)?;
    let private = validate_exact_runtime_private_binding(
        private_evidence,
        &binding,
        artifact_kind,
        required_bounded_string(identity, "schema")?,
    )?;
    match binding.schema {
        ExactRuntimeEvidenceSchema::LlmRequestV1 => validate_exact_llm_request_private(private)?,
        ExactRuntimeEvidenceSchema::LlmResultV1 => validate_exact_llm_result_private(private)?,
        ExactRuntimeEvidenceSchema::ToolIntentV1 => {
            validate_exact_tool_intent_private(private, &binding)?
        }
        ExactRuntimeEvidenceSchema::ToolResultV1 => {
            validate_exact_tool_result_private(private, &binding)?
        }
        ExactRuntimeEvidenceSchema::DelegationRequestV1 => {
            validate_exact_delegation_request_private(private, &binding)?
        }
        ExactRuntimeEvidenceSchema::DelegationResultV1 => {
            validate_exact_delegation_result_private(private, &binding)?
        }
        ExactRuntimeEvidenceSchema::OauthRequestV1 => {
            validate_exact_oauth_request_private(private)?
        }
        ExactRuntimeEvidenceSchema::OauthResultV1 => {
            validate_exact_oauth_result_private(private, &binding)?
        }
        ExactRuntimeEvidenceSchema::AgentOutputV1 => validate_exact_agent_output_private(private)?,
        ExactRuntimeEvidenceSchema::DeliveryResultV1 => {
            validate_exact_delivery_result_private(private, &binding)?
        }
        ExactRuntimeEvidenceSchema::RunStart => unreachable!("not an exact runtime transcript"),
    }
    Ok(binding)
}

fn exact_runtime_binding_from_identity<'a>(
    identity: &'a serde_json::Map<String, Value>,
) -> Result<(ExactRuntimeEvidenceBinding, &'a str), SealedArtifactError> {
    let replay_fidelity = required_bounded_string(identity, "replay_fidelity")?;
    if required_bounded_string(identity, "schema")? != EXACT_RUNTIME_EVIDENCE_SCHEMA
        || !matches!(replay_fidelity, "credential_redacted" | "exact_private")
    {
        return Err(invalid_artifact(
            "unsupported exact runtime identity schema",
        ));
    }
    let artifact_kind = required_bounded_string(identity, "artifact_kind")?;
    let evidence_id = required_bounded_string(identity, "evidence_id")?;
    let evidence_subject = required_bounded_string(identity, "evidence_subject")?;
    let outcome =
        ExactRuntimeEvidenceOutcome::parse(required_bounded_string(identity, "outcome")?)?;
    validate_runtime_evidence_subject(artifact_kind, evidence_subject)?;
    validate_runtime_evidence_id(evidence_subject, evidence_id)?;
    let (schema, phase) = match (artifact_kind, evidence_subject, outcome) {
        ("request", "llm-request", ExactRuntimeEvidenceOutcome::NotApplicable) => (
            ExactRuntimeEvidenceSchema::LlmRequestV1,
            ExactRuntimeEvidencePhase::Request,
        ),
        (
            "result",
            "llm-response",
            ExactRuntimeEvidenceOutcome::Returned
            | ExactRuntimeEvidenceOutcome::HandlerError
            | ExactRuntimeEvidenceOutcome::EffectNonResumableUnknown,
        ) => (
            ExactRuntimeEvidenceSchema::LlmResultV1,
            ExactRuntimeEvidencePhase::Result,
        ),
        ("request", "tool-invocation", ExactRuntimeEvidenceOutcome::NotApplicable) => (
            ExactRuntimeEvidenceSchema::ToolIntentV1,
            ExactRuntimeEvidencePhase::Request,
        ),
        (
            "result",
            "tool-invocation",
            ExactRuntimeEvidenceOutcome::Returned
            | ExactRuntimeEvidenceOutcome::HandlerError
            | ExactRuntimeEvidenceOutcome::EffectNonResumableUnknown,
        ) => (
            ExactRuntimeEvidenceSchema::ToolResultV1,
            ExactRuntimeEvidencePhase::Result,
        ),
        ("request", "delegation", ExactRuntimeEvidenceOutcome::NotApplicable) => (
            ExactRuntimeEvidenceSchema::DelegationRequestV1,
            ExactRuntimeEvidencePhase::Request,
        ),
        (
            "result",
            "delegation",
            ExactRuntimeEvidenceOutcome::Returned
            | ExactRuntimeEvidenceOutcome::HandlerError
            | ExactRuntimeEvidenceOutcome::EffectNonResumableUnknown,
        ) => (
            ExactRuntimeEvidenceSchema::DelegationResultV1,
            ExactRuntimeEvidencePhase::Result,
        ),
        ("request", "oauth-refresh", ExactRuntimeEvidenceOutcome::NotApplicable) => (
            ExactRuntimeEvidenceSchema::OauthRequestV1,
            ExactRuntimeEvidencePhase::Request,
        ),
        (
            "result",
            "oauth-refresh",
            ExactRuntimeEvidenceOutcome::Returned
            | ExactRuntimeEvidenceOutcome::HandlerError
            | ExactRuntimeEvidenceOutcome::EffectNonResumableUnknown,
        ) => (
            ExactRuntimeEvidenceSchema::OauthResultV1,
            ExactRuntimeEvidencePhase::Result,
        ),
        (
            "result",
            "agent-output",
            ExactRuntimeEvidenceOutcome::Returned | ExactRuntimeEvidenceOutcome::HandlerError,
        ) => (
            ExactRuntimeEvidenceSchema::AgentOutputV1,
            ExactRuntimeEvidencePhase::Result,
        ),
        (
            "result",
            subject,
            ExactRuntimeEvidenceOutcome::Returned | ExactRuntimeEvidenceOutcome::HandlerError,
        ) if is_closed_delivery_evidence_subject(subject) => (
            ExactRuntimeEvidenceSchema::DeliveryResultV1,
            ExactRuntimeEvidencePhase::Result,
        ),
        _ => {
            return Err(invalid_artifact(
                "exact runtime identity does not match a closed transcript schema",
            ));
        }
    };
    if replay_fidelity == "exact_private" && schema != ExactRuntimeEvidenceSchema::DeliveryResultV1
    {
        return Err(invalid_artifact(
            "only the closed delivery result schema may claim exact-private identity",
        ));
    }
    Ok((
        ExactRuntimeEvidenceBinding {
            schema,
            phase,
            outcome,
            evidence_id: evidence_id.to_string(),
            evidence_subject: evidence_subject.to_string(),
        },
        artifact_kind,
    ))
}

fn validate_exact_runtime_private_binding<'a>(
    private_evidence: &'a Value,
    binding: &ExactRuntimeEvidenceBinding,
    artifact_kind: &str,
    outer_schema: &str,
) -> Result<&'a serde_json::Map<String, Value>, SealedArtifactError> {
    let private = exact_artifact_object(
        private_evidence,
        EXACT_RUNTIME_PRIVATE_KEYS,
        "exact runtime private evidence",
    )?;
    if required_bounded_string(private, "evidence_id")? != binding.evidence_id
        || required_bounded_string(private, "evidence_subject")? != binding.evidence_subject
        || required_bounded_string(private, "phase")? != binding.phase.as_str()
        || required_bounded_string(private, "phase")? != artifact_kind
        || required_bounded_string(private, "schema")? != outer_schema
        || required_bounded_string(private, "schema")? != EXACT_RUNTIME_EVIDENCE_SCHEMA
    {
        return Err(invalid_artifact(
            "exact runtime private evidence is not bound to its public event identity",
        ));
    }
    Ok(private)
}

fn validate_exact_llm_request_private(
    private: &serde_json::Map<String, Value>,
) -> Result<(), SealedArtifactError> {
    let payload = exact_artifact_object(
        private
            .get("payload")
            .ok_or_else(|| invalid_artifact("exact LLM request payload is missing"))?,
        &[
            "extra_body",
            "max_tokens",
            "messages",
            "model",
            "provider",
            "stream",
            "temperature",
            "tools",
        ],
        "exact LLM request payload",
    )?;
    required_safe_bounded_string(payload, "model")?;
    required_safe_bounded_string(payload, "provider")?;
    if !payload.get("stream").is_some_and(Value::is_boolean) {
        return Err(invalid_artifact("exact LLM request stream must be boolean"));
    }
    for field in ["extra_body", "max_tokens", "temperature"] {
        validate_exact_runtime_value(
            payload
                .get(field)
                .ok_or_else(|| invalid_artifact("exact LLM request optional field is missing"))?,
            0,
        )?;
    }
    validate_exact_runtime_value(
        payload
            .get("messages")
            .filter(|value| value.is_array())
            .ok_or_else(|| invalid_artifact("exact LLM request messages must be an array"))?,
        0,
    )?;
    validate_exact_tool_collection(
        payload
            .get("tools")
            .ok_or_else(|| invalid_artifact("exact LLM request tools are missing"))?,
        false,
    )
}

fn validate_exact_llm_result_private(
    private: &serde_json::Map<String, Value>,
) -> Result<(), SealedArtifactError> {
    let payload = private
        .get("payload")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_artifact("exact LLM result payload must be an object"))?;
    let has_response = payload.contains_key("response");
    let has_error = payload.contains_key("error");
    if has_response == has_error
        || payload
            .keys()
            .any(|key| key != "response" && key != "error" && key != "stream")
    {
        return Err(invalid_artifact(
            "exact LLM result payload shape is invalid",
        ));
    }
    if let Some(response) = payload.get("response") {
        let response = exact_artifact_object(
            response,
            &["finish_reason", "message", "model", "usage"],
            "exact LLM response",
        )?;
        required_safe_bounded_string(response, "model")?;
        validate_exact_runtime_value(
            response
                .get("message")
                .ok_or_else(|| invalid_artifact("exact LLM response message is missing"))?,
            0,
        )?;
        for field in ["finish_reason", "usage"] {
            validate_exact_runtime_value(
                response
                    .get(field)
                    .ok_or_else(|| invalid_artifact("exact LLM response field is missing"))?,
                0,
            )?;
        }
    }
    if let Some(error) = payload.get("error").and_then(Value::as_str) {
        validate_exact_runtime_string(error, "exact LLM error")?;
    } else if has_error {
        return Err(invalid_artifact("exact LLM error must be a string"));
    }
    if let Some(stream) = payload.get("stream") {
        let stream = exact_artifact_object(
            stream,
            &["chunk_count", "sha256"],
            "exact LLM stream summary",
        )?;
        if stream.get("chunk_count").and_then(Value::as_u64).is_none() {
            return Err(invalid_artifact("exact LLM stream chunk count is invalid"));
        }
        required_sha256_digest(stream, "sha256")?;
    }
    Ok(())
}

fn validate_exact_tool_intent_private(
    private: &serde_json::Map<String, Value>,
    binding: &ExactRuntimeEvidenceBinding,
) -> Result<(), SealedArtifactError> {
    let payload = exact_artifact_object(
        private
            .get("payload")
            .ok_or_else(|| invalid_artifact("exact tool intent payload is missing"))?,
        &[
            "arguments",
            "call_id",
            "effect_intent",
            "execution_trust",
            "tool_name",
        ],
        "exact tool intent payload",
    )?;
    if required_bounded_string(payload, "call_id")? != binding.evidence_id {
        return Err(invalid_artifact(
            "exact tool intent call ID is not event-bound",
        ));
    }
    required_safe_bounded_string(payload, "tool_name")?;
    let arguments = payload
        .get("arguments")
        .filter(|arguments| arguments.is_object())
        .ok_or_else(|| invalid_artifact("exact tool intent arguments must be an object"))?;
    validate_exact_runtime_value(arguments, 0)?;
    match (
        required_bounded_string(payload, "execution_trust")?,
        required_bounded_string(payload, "effect_intent")?,
    ) {
        ("observation", "none") | ("vetted_local_one_shot", "local_one_shot_v1") => Ok(()),
        _ => Err(invalid_artifact(
            "exact tool intent trust and effect class are inconsistent",
        )),
    }
}

fn validate_exact_tool_result_private(
    private: &serde_json::Map<String, Value>,
    binding: &ExactRuntimeEvidenceBinding,
) -> Result<(), SealedArtifactError> {
    let payload = private
        .get("payload")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_artifact("exact tool result payload must be an object"))?;
    let expected_keys: &[&str] = match binding.outcome {
        ExactRuntimeEvidenceOutcome::Returned => &["call_id", "outcome", "tool_name", "value"],
        ExactRuntimeEvidenceOutcome::HandlerError
        | ExactRuntimeEvidenceOutcome::EffectNonResumableUnknown => {
            &["call_id", "error", "outcome", "tool_name"]
        }
        ExactRuntimeEvidenceOutcome::NotApplicable => {
            return Err(invalid_artifact(
                "exact tool result cannot be not_applicable",
            ));
        }
    };
    if payload.len() != expected_keys.len()
        || expected_keys.iter().any(|key| !payload.contains_key(*key))
    {
        return Err(invalid_artifact(
            "exact tool result payload shape is invalid",
        ));
    }
    if required_bounded_string(payload, "call_id")? != binding.evidence_id
        || required_bounded_string(payload, "outcome")? != binding.outcome.as_str()
    {
        return Err(invalid_artifact("exact tool result is not event-bound"));
    }
    required_safe_bounded_string(payload, "tool_name")?;
    match binding.outcome {
        ExactRuntimeEvidenceOutcome::Returned => validate_exact_runtime_value(
            payload
                .get("value")
                .ok_or_else(|| invalid_artifact("exact tool result value is missing"))?,
            0,
        ),
        ExactRuntimeEvidenceOutcome::HandlerError
        | ExactRuntimeEvidenceOutcome::EffectNonResumableUnknown => {
            let error = payload
                .get("error")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid_artifact("exact tool result error must be a string"))?;
            if error.is_empty() {
                return Err(invalid_artifact(
                    "exact tool result error must not be empty",
                ));
            }
            validate_exact_runtime_string(error, "exact tool result error")
        }
        ExactRuntimeEvidenceOutcome::NotApplicable => unreachable!("rejected above"),
    }
}

fn validate_exact_delegation_request_private(
    private: &serde_json::Map<String, Value>,
    binding: &ExactRuntimeEvidenceBinding,
) -> Result<(), SealedArtifactError> {
    let payload = exact_artifact_object(
        private
            .get("payload")
            .ok_or_else(|| invalid_artifact("exact delegation request payload is missing"))?,
        &[
            "background",
            "call_id",
            "context",
            "invocation_key",
            "model",
            "task",
            "tool_name",
            "toolset",
        ],
        "exact delegation request payload",
    )?;
    if required_bounded_string(payload, "call_id")? != binding.evidence_id
        || required_bounded_string(payload, "tool_name")? != "delegate_task"
    {
        return Err(invalid_artifact(
            "exact delegation request is not bound to its tool call",
        ));
    }
    required_safe_bounded_string(payload, "invocation_key")?;
    validate_exact_runtime_string(
        required_bounded_string(payload, "task")?,
        "exact delegation task",
    )?;
    for field in ["context", "model", "toolset"] {
        validate_nullable_bounded_string(payload, field)?;
    }
    if !payload.get("background").is_some_and(Value::is_boolean) {
        return Err(invalid_artifact(
            "exact delegation background flag must be boolean",
        ));
    }
    Ok(())
}

fn validate_exact_delegation_result_private(
    private: &serde_json::Map<String, Value>,
    binding: &ExactRuntimeEvidenceBinding,
) -> Result<(), SealedArtifactError> {
    let payload = private
        .get("payload")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_artifact("exact delegation result payload must be an object"))?;
    let expected_keys: &[&str] = match binding.outcome {
        ExactRuntimeEvidenceOutcome::Returned => &[
            "call_id",
            "invocation_key",
            "outcome",
            "response",
            "tool_name",
        ],
        ExactRuntimeEvidenceOutcome::HandlerError
        | ExactRuntimeEvidenceOutcome::EffectNonResumableUnknown => {
            &["call_id", "error", "invocation_key", "outcome", "tool_name"]
        }
        ExactRuntimeEvidenceOutcome::NotApplicable => {
            return Err(invalid_artifact(
                "exact delegation result cannot be not_applicable",
            ));
        }
    };
    if payload.len() != expected_keys.len()
        || expected_keys.iter().any(|key| !payload.contains_key(*key))
        || required_bounded_string(payload, "call_id")? != binding.evidence_id
        || required_bounded_string(payload, "tool_name")? != "delegate_task"
        || required_bounded_string(payload, "outcome")? != binding.outcome.as_str()
    {
        return Err(invalid_artifact(
            "exact delegation result payload shape or binding is invalid",
        ));
    }
    required_safe_bounded_string(payload, "invocation_key")?;
    let field = if binding.outcome == ExactRuntimeEvidenceOutcome::Returned {
        "response"
    } else {
        "error"
    };
    validate_exact_runtime_string(
        required_bounded_string(payload, field)?,
        "exact delegation result value",
    )
}

fn validate_exact_oauth_request_private(
    private: &serde_json::Map<String, Value>,
) -> Result<(), SealedArtifactError> {
    let payload = exact_artifact_object(
        private
            .get("payload")
            .ok_or_else(|| invalid_artifact("exact OAuth request payload is missing"))?,
        &["provider"],
        "exact OAuth request payload",
    )?;
    required_safe_bounded_string(payload, "provider")?;
    Ok(())
}

fn validate_exact_oauth_result_private(
    private: &serde_json::Map<String, Value>,
    binding: &ExactRuntimeEvidenceBinding,
) -> Result<(), SealedArtifactError> {
    let payload = private
        .get("payload")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid_artifact("exact OAuth result payload must be an object"))?;
    let expected_keys: &[&str] = match binding.outcome {
        ExactRuntimeEvidenceOutcome::Returned => &["provider", "resolution"],
        ExactRuntimeEvidenceOutcome::HandlerError
        | ExactRuntimeEvidenceOutcome::EffectNonResumableUnknown => &["error_class", "provider"],
        ExactRuntimeEvidenceOutcome::NotApplicable => {
            return Err(invalid_artifact(
                "exact OAuth result cannot be not_applicable",
            ));
        }
    };
    if payload.len() != expected_keys.len()
        || expected_keys.iter().any(|key| !payload.contains_key(*key))
    {
        return Err(invalid_artifact(
            "exact OAuth result payload shape is invalid",
        ));
    }
    required_safe_bounded_string(payload, "provider")?;
    if binding.outcome == ExactRuntimeEvidenceOutcome::Returned {
        let resolution = payload
            .get("resolution")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid_artifact("exact OAuth resolution must be an object"))?;
        if resolution
            .keys()
            .any(|key| key != "status" && key != "expires_at")
        {
            return Err(invalid_artifact(
                "exact OAuth resolution contains an unsupported field",
            ));
        }
        match resolution.get("status").and_then(Value::as_str) {
            Some("not_configured" | "current" | "refreshed") => {}
            _ => return Err(invalid_artifact("exact OAuth resolution status is invalid")),
        }
        if let Some(expires_at) = resolution.get("expires_at") {
            validate_exact_runtime_string(
                expires_at
                    .as_str()
                    .ok_or_else(|| invalid_artifact("exact OAuth expiry must be a string"))?,
                "exact OAuth expiry",
            )?;
        }
    } else {
        match required_bounded_string(payload, "error_class")? {
            "auth_failed" | "credential_unavailable" | "operation_failed" => {}
            _ => return Err(invalid_artifact("exact OAuth error class is invalid")),
        }
    }
    Ok(())
}

fn validate_exact_agent_output_private(
    private: &serde_json::Map<String, Value>,
) -> Result<(), SealedArtifactError> {
    let response = private
        .get("payload")
        .and_then(Value::as_object)
        .and_then(|payload| payload.get("response"))
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_artifact("exact agent output response must be a string"))?;
    validate_exact_runtime_string(response, "exact agent output response")
}

/// Validate the one delivery completion payload permitted to be exact. The
/// receipt is an Alpha-local deterministic derivation of the already-public
/// idempotency digest, never an opaque downstream response.
fn validate_exact_delivery_result_private(
    private: &serde_json::Map<String, Value>,
    binding: &ExactRuntimeEvidenceBinding,
) -> Result<(), SealedArtifactError> {
    let payload = private
        .get("payload")
        .ok_or_else(|| invalid_artifact("exact delivery result payload is missing"))?;
    closed_delivery_result_payload(payload)?;
    let payload = payload
        .as_object()
        .ok_or_else(|| invalid_artifact("exact delivery result payload must be an object"))?;
    for field in ["event_id", "outbox_id", "run_id"] {
        let value = required_bounded_string(payload, field)?;
        if !is_canonical_lowercase_uuid(value) {
            return Err(invalid_artifact(
                "exact delivery result identifier is not a canonical UUID",
            ));
        }
    }
    if required_bounded_string(payload, "outbox_id")? != binding.evidence_id {
        return Err(invalid_artifact(
            "exact delivery result payload does not bind its outbox identity",
        ));
    }
    let idempotency_key = required_bounded_string(payload, "idempotency_key")?;
    if !is_lowercase_sha256_hex(idempotency_key)
        || binding.evidence_subject.rsplit('/').next() != Some(idempotency_key)
    {
        return Err(invalid_artifact(
            "exact delivery result payload does not bind its idempotency identity",
        ));
    }
    let completion = required_bounded_string(payload, "completion")?;
    match completion {
        "acknowledged" => {
            if !payload.get("delivery_failure").is_some_and(Value::is_null)
                || payload.get("delivery_receipt").and_then(Value::as_str)
                    != Some(&format!("alpha-runtime-v1:{idempotency_key}"))
            {
                return Err(invalid_artifact(
                    "exact acknowledged delivery result has an invalid receipt binding",
                ));
            }
        }
        "nacked" => {
            if !payload.get("delivery_receipt").is_some_and(Value::is_null)
                || !matches!(
                    payload.get("delivery_failure").and_then(Value::as_str),
                    Some(
                        "missing_route"
                            | "invalid_payload"
                            | "route_decode_failed"
                            | "invalid_variant"
                            | "sealed_task_mismatch"
                            | "durable_store_failure"
                            | "handler_rejected"
                            | "attempts_exhausted"
                    )
                )
            {
                return Err(invalid_artifact(
                    "exact nacked delivery result has an invalid failure binding",
                ));
            }
        }
        _ => unreachable!("closed delivery payload validates completion"),
    }
    Ok(())
}

fn validate_exact_runtime_value(value: &Value, depth: usize) -> Result<(), SealedArtifactError> {
    if depth > 64 {
        return Err(invalid_artifact(
            "exact runtime payload nesting exceeds 64 levels",
        ));
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
        Value::String(value) => validate_exact_runtime_string(value, "exact runtime payload"),
        Value::Array(values) => values
            .iter()
            .try_for_each(|value| validate_exact_runtime_value(value, depth + 1)),
        Value::Object(values) => values.iter().try_for_each(|(key, value)| {
            if key.is_empty()
                || key.len() > MAX_ARTIFACT_IDENTIFIER_BYTES
                || key.contains('\0')
                || is_sensitive_field_name(key)
                || redact_sensitive_text(key) != *key
            {
                return Err(invalid_artifact(
                    "exact runtime payload contains an unsafe field name",
                ));
            }
            validate_exact_runtime_value(value, depth + 1)
        }),
    }
}

fn validate_exact_runtime_string(value: &str, label: &str) -> Result<(), SealedArtifactError> {
    if value.len() > MAX_ARTIFACT_BYTES
        || value.contains('\0')
        || redact_sensitive_text(value) != value
        || contains_unredacted_credential_token(value)
    {
        return Err(invalid_artifact(&format!(
            "{label} contains a credential or invalid string"
        )));
    }
    Ok(())
}

fn ensure_canonical_bytes_match(
    left: &Value,
    right: &Value,
    message: &str,
) -> Result<(), SealedArtifactError> {
    let left = Zeroizing::new(serde_json_canonicalizer::to_vec(left)?);
    let right = Zeroizing::new(serde_json_canonicalizer::to_vec(right)?);
    if left.as_slice() != right.as_slice() {
        return Err(invalid_artifact(message));
    }
    Ok(())
}

/// Keep the exact private start evidence closed over the same public,
/// credential-free containment projection that admitted the run. This is
/// provenance of admission, not a backend command receipt.
fn validate_exact_run_start_execution_containment(
    private: &serde_json::Map<String, Value>,
) -> Result<(), SealedArtifactError> {
    let containment = private
        .get("execution_containment")
        .ok_or_else(|| invalid_artifact("execution_containment is missing"))?;
    let containment = exact_artifact_object(
        containment,
        EXACT_RUN_START_EXECUTION_CONTAINMENT_KEYS,
        "exact run-start execution containment",
    )?;
    if required_safe_bounded_string(containment, "schema")?
        != "hermes.execution-containment-provenance/v1"
    {
        return Err(invalid_artifact(
            "unsupported exact run-start execution containment schema",
        ));
    }
    match required_safe_bounded_string(containment, "admission")? {
        "manual_host" => {
            for field in [
                "backend_kind",
                "scope",
                "policy_version",
                "configuration_digest",
            ] {
                if !containment.get(field).is_some_and(Value::is_null) {
                    return Err(invalid_artifact(
                        "manual execution containment must not claim a backend certificate",
                    ));
                }
            }
        }
        "certified_backend_configured" => {
            if required_safe_bounded_string(containment, "backend_kind")? != "docker"
                || required_safe_bounded_string(containment, "scope")? != "closed_execution"
                || containment.get("policy_version").and_then(Value::as_u64) != Some(1)
            {
                return Err(invalid_artifact(
                    "certified execution containment is not the supported closed backend",
                ));
            }
            required_sha256_digest(containment, "configuration_digest")?;
        }
        _ => {
            return Err(invalid_artifact(
                "execution containment admission is invalid",
            ));
        }
    }
    let containment = Value::Object(containment.clone());
    ensure_credential_free_identity(&containment, "exact run-start execution containment")
}

fn validate_nullable_bounded_string(
    object: &serde_json::Map<String, Value>,
    field: &str,
) -> Result<(), SealedArtifactError> {
    match object.get(field) {
        Some(Value::Null) => Ok(()),
        Some(Value::String(value))
            if value.len() <= MAX_ARTIFACT_BYTES && !value.contains('\0') =>
        {
            ensure_credential_free_identity(
                object.get(field).expect("matched nullable string"),
                field,
            )
        }
        _ => Err(invalid_artifact(&format!(
            "{field} must be null or a bounded string"
        ))),
    }
}

fn ensure_credential_free_identity(value: &Value, label: &str) -> Result<(), SealedArtifactError> {
    let redacted = credential_free_replay_payload(value);
    let original = Zeroizing::new(serde_json_canonicalizer::to_vec(value)?);
    let redacted = Zeroizing::new(serde_json_canonicalizer::to_vec(&redacted)?);
    if original.as_slice() != redacted.as_slice()
        || contains_unredacted_credential_token_value(value)
    {
        return Err(invalid_artifact(&format!(
            "{label} contains a credential or secret indicator"
        )));
    }
    Ok(())
}

fn contains_unredacted_credential_token_value(value: &Value) -> bool {
    match value {
        Value::String(value) => contains_unredacted_credential_token(value),
        Value::Array(values) => values
            .iter()
            .any(contains_unredacted_credential_token_value),
        Value::Object(values) => values
            .values()
            .any(contains_unredacted_credential_token_value),
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

/// Tool schemas are data *about* inputs, not tool input values. In particular,
/// property names such as `api_key` remain literal schema metadata. We only
/// inspect values recursively and reject credential-bearing strings/defaults/
/// examples/consts rather than applying field-name based replacement.
fn validate_exact_tool_collection(
    tools: &Value,
    contracts: bool,
) -> Result<(), SealedArtifactError> {
    let tools = tools
        .as_array()
        .ok_or_else(|| invalid_artifact("tool schema collection must be an array"))?;
    for tool in tools {
        let schema = if contracts {
            let contract =
                exact_artifact_object(tool, &["schema", "execution_trust"], "exact tool contract")?;
            validate_tool_schema_value(
                contract
                    .get("execution_trust")
                    .ok_or_else(|| invalid_artifact("tool execution trust is missing"))?,
                0,
            )?;
            contract
                .get("schema")
                .ok_or_else(|| invalid_artifact("tool contract schema is missing"))?
        } else {
            tool
        };
        let schema = exact_artifact_object(
            schema,
            &["name", "description", "parameters"],
            "advertised tool schema",
        )?;
        for field in ["name", "description"] {
            validate_tool_schema_value(
                schema
                    .get(field)
                    .ok_or_else(|| invalid_artifact(&format!("tool {field} is missing")))?,
                0,
            )?;
        }
        let parameters = schema
            .get("parameters")
            .ok_or_else(|| invalid_artifact("tool parameters are missing"))?;
        if !parameters.is_object() {
            return Err(invalid_artifact("tool parameters must be an object"));
        }
        validate_tool_schema_value(parameters, 0)?;
    }
    Ok(())
}

fn validate_tool_schema_value(value: &Value, depth: usize) -> Result<(), SealedArtifactError> {
    if depth > 64 {
        return Err(invalid_artifact("tool schema nesting exceeds 64 levels"));
    }
    match value {
        Value::Null | Value::Bool(_) | Value::Number(_) => Ok(()),
        Value::String(value) => {
            if value.len() > MAX_ARTIFACT_BYTES
                || value.contains('\0')
                || redact_sensitive_text(value) != *value
                || contains_unredacted_credential_token(value)
            {
                return Err(invalid_artifact(
                    "tool schema contains a credential or invalid string value",
                ));
            }
            Ok(())
        }
        Value::Array(values) => values
            .iter()
            .try_for_each(|value| validate_tool_schema_value(value, depth + 1)),
        Value::Object(values) => {
            for (key, value) in values {
                match key.as_str() {
                    "properties" | "$defs" => {
                        validate_tool_schema_named_schema_map(value, depth + 1)?
                    }
                    "required" => validate_tool_schema_property_names(value)?,
                    _ => validate_tool_schema_value(value, depth + 1)?,
                }
            }
            Ok(())
        }
    }
}

fn validate_tool_schema_named_schema_map(
    value: &Value,
    depth: usize,
) -> Result<(), SealedArtifactError> {
    let schemas = value
        .as_object()
        .ok_or_else(|| invalid_artifact("tool schema named schema map must be an object"))?;
    for (name, schema) in schemas {
        validate_tool_schema_metadata_name(name)?;
        validate_tool_schema_value(schema, depth + 1)?;
    }
    Ok(())
}

fn validate_tool_schema_property_names(value: &Value) -> Result<(), SealedArtifactError> {
    let names = value
        .as_array()
        .ok_or_else(|| invalid_artifact("tool schema required names must be an array"))?;
    for name in names {
        let name = name
            .as_str()
            .ok_or_else(|| invalid_artifact("tool schema required name must be a string"))?;
        validate_tool_schema_metadata_name(name)?;
    }
    Ok(())
}

fn validate_tool_schema_metadata_name(name: &str) -> Result<(), SealedArtifactError> {
    if name.is_empty()
        || name.len() > MAX_ARTIFACT_IDENTIFIER_BYTES
        || name.contains('\0')
        || contains_unredacted_credential_token(name)
        || (!is_sensitive_field_name(name) && redact_sensitive_text(name) != name)
    {
        return Err(invalid_artifact(
            "tool schema metadata name contains a credential or is invalid",
        ));
    }
    Ok(())
}

fn closed_runtime_summary(shape: &str) -> Value {
    serde_json::json!({
        "schema": CLOSED_RUNTIME_REPLAY_SCHEMA,
        "shape": shape,
    })
}

fn closed_bound_runtime_payload(
    evidence_subject: &str,
    private: &Value,
) -> Result<Value, SealedArtifactError> {
    let private = exact_artifact_object(
        private,
        &["evidence_id", "evidence_subject", "payload"],
        "bound runtime private evidence",
    )?;
    let evidence_id = required_bounded_string(private, "evidence_id")?;
    validate_runtime_evidence_id(evidence_subject, evidence_id)?;
    required_safe_bounded_string(private, "evidence_subject")?;
    let payload = private
        .get("payload")
        .ok_or_else(|| invalid_artifact("bound runtime payload is missing"))?;
    let payload_kind = match payload {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    };
    Ok(serde_json::json!({
        "payload_kind": payload_kind,
        "schema": CLOSED_RUNTIME_REPLAY_SCHEMA,
        "shape": "bound-runtime-evidence",
    }))
}

/// Convert an attempted closed transcript envelope into a metadata-only
/// projection without retaining any unrecognized inner fields. This is the
/// safe fallback for a runtime transcript that cannot claim exact fidelity.
fn closed_runtime_wrapper_summary(
    _evidence_id: &str,
    _evidence_subject: &str,
    private: &Value,
) -> Result<Option<Value>, SealedArtifactError> {
    let Some(private) = private.as_object() else {
        return Ok(None);
    };
    let wrapper_like = private.contains_key("evidence_id")
        && private.contains_key("evidence_subject")
        && (private.contains_key("phase") || private.contains_key("schema"));
    if !wrapper_like {
        return Ok(None);
    }
    let payload_kind = match private.get("payload") {
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
        None => "missing",
    };
    Ok(Some(serde_json::json!({
        "payload_kind": payload_kind,
        "schema": CLOSED_RUNTIME_REPLAY_SCHEMA,
        "shape": "bound-runtime-evidence",
    })))
}

fn closed_alpha_task_payload(private: &Value) -> Result<Value, SealedArtifactError> {
    let private = exact_artifact_object(
        private,
        &["envelope", "schema", "task"],
        "AlphaRuntimeV1 private task",
    )?;
    if required_bounded_string(private, "schema")? != ALPHA_RUNTIME_V1_PRIVATE_TASK_SCHEMA {
        return Err(invalid_artifact(
            "unsupported AlphaRuntimeV1 private task schema",
        ));
    }
    let envelope = exact_artifact_object(
        private
            .get("envelope")
            .ok_or_else(|| invalid_artifact("AlphaRuntimeV1 envelope is missing"))?,
        &["event_type", "loop_id", "schema", "variant"],
        "AlphaRuntimeV1 private task envelope",
    )?;
    if required_bounded_string(envelope, "schema")? != ALPHA_RUNTIME_V1_ENVELOPE_SCHEMA
        || required_bounded_string(envelope, "variant")? != "background_job"
    {
        return Err(invalid_artifact(
            "unsupported AlphaRuntimeV1 private task envelope",
        ));
    }
    required_safe_bounded_string(envelope, "event_type")?;
    required_safe_bounded_string(envelope, "loop_id")?;
    let task = private
        .get("task")
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_artifact("AlphaRuntimeV1 task must be a string"))?;
    if task.is_empty() || task.len() > MAX_ARTIFACT_BYTES || task.contains('\0') {
        return Err(invalid_artifact("AlphaRuntimeV1 task is invalid"));
    }
    // The shared text redactor may mask rather than remove a token. An executable
    // task must not preserve even a masked credential fragment in sealed storage.
    let redacted_task = Zeroizing::new(redact_sensitive_text(task));
    if redacted_task.as_str() != task
        || contains_unredacted_credential_token(redacted_task.as_str())
    {
        return Err(invalid_artifact(
            "AlphaRuntimeV1 task contains a credential or secret indicator",
        ));
    }
    Ok(serde_json::json!({
        "envelope": {
            "event_type": envelope.get("event_type").unwrap(),
            "loop_id": envelope.get("loop_id").unwrap(),
            "schema": ALPHA_RUNTIME_V1_ENVELOPE_SCHEMA,
            "variant": "background_job",
        },
        "schema": ALPHA_RUNTIME_V1_PRIVATE_TASK_SCHEMA,
        "task": redacted_task.as_str(),
    }))
}

fn contains_unredacted_credential_token(text: &str) -> bool {
    let lowercase = Zeroizing::new(text.to_ascii_lowercase());
    contains_known_provider_token(lowercase.as_str())
        || contains_bearer_credential(lowercase.as_str())
        || contains_credential_assignment(lowercase.as_str())
}

fn contains_known_provider_token(text: &str) -> bool {
    const PROVIDER_PREFIXES: &[(&str, usize)] = &[
        ("github_pat_", 10),
        ("bb_live_", 10),
        ("sk_live_", 10),
        ("sk_test_", 10),
        ("rk_live_", 10),
        ("dop_v1_", 10),
        ("doo_v1_", 10),
        ("retaindb_", 10),
        ("gsk_", 10),
        ("mem0_", 10),
        ("pypi-", 10),
        ("tvly-", 10),
        ("pplx-", 10),
        ("hsk-", 10),
        ("xoxb-", 10),
        ("xoxa-", 10),
        ("xoxp-", 10),
        ("xoxr-", 10),
        ("xoxs-", 10),
        ("gaaaa", 20),
        ("aiza", 30),
        ("akia", 16),
        ("xai-", 30),
        ("sk-", 10),
        ("ghp_", 10),
        ("gho_", 10),
        ("ghu_", 10),
        ("ghs_", 10),
        ("ghr_", 10),
        ("fal_", 10),
        ("fc-", 10),
        ("sg.", 10),
        ("hf_", 10),
        ("r8_", 10),
        ("npm_", 10),
        ("am_", 10),
        ("sk_", 10),
        ("exa_", 10),
        ("syt_", 10),
        ("brv_", 10),
    ];

    for (prefix, minimum_suffix) in PROVIDER_PREFIXES {
        let mut offset = 0;
        while let Some(found) = text[offset..].find(prefix) {
            let start = offset + found;
            let after_prefix = start + prefix.len();
            if is_credential_boundary(text, start) {
                let suffix_length = text[after_prefix..]
                    .chars()
                    .take_while(|character| {
                        character.is_ascii_alphanumeric() || matches!(character, '_' | '-' | '=')
                    })
                    .count();
                if suffix_length >= *minimum_suffix {
                    return true;
                }
            }
            offset = after_prefix;
        }
    }
    false
}

fn contains_bearer_credential(text: &str) -> bool {
    let mut offset = 0;
    while let Some(found) = text[offset..].find("bearer") {
        let start = offset + found;
        let after_bearer = start + "bearer".len();
        let remainder = &text[after_bearer..];
        if is_credential_boundary(text, start)
            && remainder
                .chars()
                .next()
                .is_some_and(|character| character.is_ascii_whitespace())
        {
            let value = credential_atom(remainder);
            if value.len() >= 16 && !is_redaction_placeholder(value) {
                return true;
            }
        }
        offset = after_bearer;
    }
    false
}

fn contains_credential_assignment(text: &str) -> bool {
    const CREDENTIAL_FIELDS: &[&str] = &[
        "proxy-authorization",
        "authorization",
        "x-api-key",
        "api-key",
        "api_key",
        "apikey",
        "access-token",
        "access_token",
        "refresh-token",
        "refresh_token",
        "auth-token",
        "auth_token",
        "provider-token",
        "provider_token",
        "client-secret",
        "client_secret",
        "raw-secret",
        "raw_secret",
        "secret-value",
        "secret_value",
        "secret-input",
        "secret_input",
        "key-material",
        "key_material",
        "set-cookie",
        "password",
        "passwd",
        "cookie",
        "token",
        "secret",
    ];

    for field in CREDENTIAL_FIELDS {
        let mut offset = 0;
        while let Some(found) = text[offset..].find(field) {
            let start = offset + found;
            let after_field = start + field.len();
            if is_credential_boundary(text, start) {
                if let Some(value) = credential_assignment_value(&text[after_field..]) {
                    if looks_like_sensitive_credential_value(value) {
                        return true;
                    }
                }
            }
            offset = after_field;
        }
    }
    false
}

fn is_credential_boundary(text: &str, index: usize) -> bool {
    text[..index].chars().next_back().map_or(true, |character| {
        !character.is_ascii_alphanumeric() && !matches!(character, '_' | '-')
    })
}

fn credential_assignment_value(remainder: &str) -> Option<&str> {
    let remainder = remainder.trim_start();
    let remainder = remainder
        .strip_prefix('"')
        .or_else(|| remainder.strip_prefix('\''))
        .unwrap_or(remainder)
        .trim_start();
    let delimiter = remainder.chars().next()?;
    if !matches!(delimiter, ':' | '=') {
        return None;
    }
    Some(credential_atom(&remainder[delimiter.len_utf8()..]))
}

fn credential_atom(value: &str) -> &str {
    value
        .trim_start_matches(|character: char| {
            character.is_ascii_whitespace() || matches!(character, '"' | '\'')
        })
        .split(|character: char| {
            character.is_ascii_whitespace() || matches!(character, ',' | ';' | '"' | '\'')
        })
        .next()
        .unwrap_or_default()
}

fn looks_like_sensitive_credential_value(value: &str) -> bool {
    if value.is_empty()
        || is_redaction_placeholder(value)
        || is_obvious_credential_placeholder(value)
    {
        return false;
    }
    let length = value.chars().count();
    let mut has_lowercase = false;
    let mut has_uppercase = false;
    let mut has_digit = false;
    let mut has_non_word_symbol = false;
    let mut has_separator = false;
    for character in value.chars() {
        if character.is_ascii_lowercase() {
            has_lowercase = true;
        } else if character.is_ascii_uppercase() {
            has_uppercase = true;
        } else if character.is_ascii_digit() {
            has_digit = true;
        } else if matches!(character, '_' | '-') {
            has_separator = true;
        } else {
            has_non_word_symbol = true;
        }
    }
    if length >= 16 && (!has_separator || has_uppercase || has_digit || has_non_word_symbol) {
        return true;
    }
    if length < 12 {
        return false;
    }
    let classes = has_lowercase as usize
        + has_uppercase as usize
        + has_digit as usize
        + has_non_word_symbol as usize;
    classes >= 3
}

fn is_obvious_credential_placeholder(value: &str) -> bool {
    matches!(
        value,
        "env"
            | "environment"
            | "environment_variable"
            | "from_environment"
            | "configuration"
            | "config"
            | "placeholder"
            | "redacted"
    ) || value.starts_with('$')
        || value.starts_with("${")
}

fn is_redaction_placeholder(value: &str) -> bool {
    value.starts_with("[redacted") || value.starts_with("<redacted")
}

fn closed_delivery_result_payload(private: &Value) -> Result<Value, SealedArtifactError> {
    let (_, completion) = validate_delivery_result_private_shape(private)?;
    Ok(serde_json::json!({
        "completion": completion,
        "schema": CLOSED_RUNTIME_REPLAY_SCHEMA,
        "shape": "delivery-result",
    }))
}

/// Validate the sealed delivery completion payload once for both the
/// credential-free replay summary and the exact-runtime evidence boundary.
/// The caller performs any event-local identity binding separately.
fn validate_delivery_result_private_shape(
    private: &Value,
) -> Result<(&serde_json::Map<String, Value>, &str), SealedArtifactError> {
    let private = exact_artifact_object(
        private,
        &[
            "attempt_count",
            "completion",
            "delivery_failure",
            "delivery_receipt",
            "event_id",
            "idempotency_key",
            "outbox_id",
            "run_id",
            "schema",
            "source_sealed_artifact_ref",
        ],
        "delivery-result private evidence",
    )?;
    if required_bounded_string(private, "schema")? != ALPHA_RUNTIME_V1_DELIVERY_EVIDENCE_SCHEMA {
        return Err(invalid_artifact(
            "unsupported delivery-result private schema",
        ));
    }
    private
        .get("attempt_count")
        .and_then(Value::as_u64)
        .ok_or_else(|| invalid_artifact("delivery attempt_count must be an unsigned integer"))?;
    let completion = required_bounded_string(private, "completion")?;
    if !matches!(completion, "acknowledged" | "nacked") {
        return Err(invalid_artifact("delivery completion is invalid"));
    }
    for field in ["delivery_failure", "delivery_receipt"] {
        let value = private
            .get(field)
            .ok_or_else(|| invalid_artifact(&format!("{field} is missing")))?;
        if !value.is_null() && value.as_str().is_none() {
            return Err(invalid_artifact(&format!(
                "{field} must be a string or null"
            )));
        }
    }
    match completion {
        "acknowledged"
            if private.get("delivery_failure").is_some_and(Value::is_null)
                && private
                    .get("delivery_receipt")
                    .and_then(Value::as_str)
                    .is_some() => {}
        "nacked"
            if private.get("delivery_receipt").is_some_and(Value::is_null)
                && private
                    .get("delivery_failure")
                    .and_then(Value::as_str)
                    .is_some() => {}
        _ => {
            return Err(invalid_artifact(
                "delivery completion and receipt/failure fields are inconsistent",
            ));
        }
    }
    for field in ["event_id", "outbox_id", "run_id"] {
        let value = required_bounded_string(private, field)?;
        if !is_canonical_lowercase_uuid(value) {
            ensure_credential_free_identity(
                private.get(field).expect("required identifier"),
                field,
            )?;
        }
    }
    required_sha256_digest(private, "idempotency_key")?;
    let reference = exact_artifact_object(
        private
            .get("source_sealed_artifact_ref")
            .ok_or_else(|| invalid_artifact("delivery source artifact reference is missing"))?,
        &[
            "canonicalization_algorithm",
            "canonicalization_version",
            "encryption_algorithm",
            "envelope_digest",
            "key_id",
            "redactor_version",
            "schema_version",
        ],
        "delivery source artifact reference",
    )?;
    for field in [
        "canonicalization_algorithm",
        "encryption_algorithm",
        "key_id",
        "redactor_version",
    ] {
        required_safe_bounded_string(reference, field)?;
    }
    required_sha256_digest(reference, "envelope_digest")?;
    for field in ["canonicalization_version", "schema_version"] {
        reference
            .get(field)
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid_artifact(&format!("{field} must be an unsigned integer")))?;
    }
    Ok((private, completion))
}

fn bound_observer_preview(rendered: String, max_chars: usize) -> String {
    let limit = max_chars.max(1);
    let mut chars = rendered.chars();
    let mut preview: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        preview.push('…');
    }
    preview
}

pub(crate) fn credential_free_observer_preview(raw: &Value, max_chars: usize) -> String {
    let redacted = credential_free_replay_payload(raw);
    let rendered = match redacted {
        Value::String(value) => value,
        other => other.to_string(),
    };
    bound_observer_preview(rendered, max_chars)
}

pub(crate) fn credential_free_observer_text_preview(raw: &str, max_chars: usize) -> String {
    bound_observer_preview(redact_sensitive_text(raw), max_chars)
}

fn exact_artifact_object<'a>(
    value: &'a Value,
    expected_keys: &[&str],
    label: &str,
) -> Result<&'a serde_json::Map<String, Value>, SealedArtifactError> {
    let object = value
        .as_object()
        .ok_or_else(|| invalid_artifact(&format!("{label} must be an object")))?;
    if object.len() != expected_keys.len()
        || expected_keys.iter().any(|key| !object.contains_key(*key))
    {
        return Err(invalid_artifact(&format!(
            "{label} contains missing or unknown fields"
        )));
    }
    Ok(object)
}

fn required_bounded_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
) -> Result<&'a str, SealedArtifactError> {
    let value = object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| invalid_artifact(&format!("{field} must be a string")))?;
    if value.is_empty() || value.len() > 256 {
        return Err(invalid_artifact(&format!(
            "{field} must contain 1 to 256 bytes"
        )));
    }
    Ok(value)
}

fn required_safe_bounded_string<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
) -> Result<&'a str, SealedArtifactError> {
    let value = required_bounded_string(object, field)?;
    if redact_sensitive_text(value) != value {
        return Err(invalid_artifact(&format!(
            "{field} contains a credential or secret indicator"
        )));
    }
    Ok(value)
}

/// Accept a digest only in a closed schema field that semantically requires it.
/// Do not use this for arbitrary text or credential-bearing fields.
fn required_sha256_digest<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
) -> Result<&'a str, SealedArtifactError> {
    let value = required_bounded_string(object, field)?;
    validate_digest(value)?;
    Ok(value)
}

fn invalid_artifact(message: &str) -> SealedArtifactError {
    SealedArtifactError::Redaction(message.to_string())
}

fn marker_key_unavailable(error: SealedArtifactError) -> SealedArtifactError {
    match error {
        SealedArtifactError::KeyUnavailable(_) => error,
        other => SealedArtifactError::KeyUnavailable(format!(
            "artifact key provisioning marker is invalid: {other}"
        )),
    }
}

/// Keyed identity metadata safe to persist in the execution ledger.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppendIdentity {
    pub public_identity_jcs: Vec<u8>,
    pub private_fingerprint: String,
    pub fingerprint_algorithm: String,
    pub fingerprint_version: u8,
    pub fingerprint_key_id: String,
    pub fingerprint_key_version: u32,
    pub redactor_version: String,
    pub canonicalization_algorithm: String,
    pub canonicalization_version: u8,
}

/// A prepared append payload. Its private bytes zeroize on drop and cannot be
/// serialized or cloned.
pub struct PreparedArtifact {
    identity: AppendIdentity,
    private_jcs: Zeroizing<Vec<u8>>,
}

impl fmt::Debug for PreparedArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PreparedArtifact")
            .field("identity", &self.identity)
            .field("private_jcs", &"[REDACTED]")
            .finish()
    }
}

impl PreparedArtifact {
    pub fn identity(&self) -> &AppendIdentity {
        &self.identity
    }

    pub fn into_identity(self) -> AppendIdentity {
        self.identity.clone()
    }

    pub(crate) fn private_jcs_len(&self) -> usize {
        self.private_jcs.len()
    }

    pub(crate) fn rebind_fingerprint(
        &mut self,
        fingerprint_key: &FingerprintKey,
    ) -> Result<(), SealedArtifactError> {
        self.identity.private_fingerprint = derive_private_fingerprint(
            &self.identity.public_identity_jcs,
            &self.private_jcs,
            fingerprint_key,
        )?;
        self.identity.fingerprint_algorithm = ARTIFACT_FINGERPRINT_ALGORITHM.to_string();
        self.identity.fingerprint_version = ARTIFACT_FINGERPRINT_VERSION;
        self.identity.fingerprint_key_id = fingerprint_key.key_id.clone();
        self.identity.fingerprint_key_version = fingerprint_key.key_version;
        Ok(())
    }
}

pub struct FingerprintKey {
    key_id: String,
    key_version: u32,
    material: Zeroizing<[u8; 32]>,
}

impl fmt::Debug for FingerprintKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FingerprintKey")
            .field("key_id", &self.key_id)
            .field("key_version", &self.key_version)
            .field("material", &"[REDACTED]")
            .finish()
    }
}

impl FingerprintKey {
    pub fn new(
        key_id: impl Into<String>,
        key_version: u32,
        material: [u8; 32],
    ) -> Result<Self, SealedArtifactError> {
        let key_id = key_id.into();
        validate_identifier("fingerprint key id", &key_id)?;
        if key_version == 0 {
            return Err(SealedArtifactError::InvalidMetadata(
                "fingerprint key version must be non-zero".to_string(),
            ));
        }
        Ok(Self {
            key_id,
            key_version,
            material: Zeroizing::new(material),
        })
    }
}

/// Redact and canonicalize exactly once, then derive the append identity with
/// explicit domain separation and length framing.
pub fn prepare_artifact(
    raw: &Value,
    redactor: &dyn ArtifactRedactor,
    fingerprint_key: &FingerprintKey,
) -> Result<PreparedArtifact, SealedArtifactError> {
    validate_identifier("redactor version", redactor.version())?;
    let redacted = redactor.redact(raw)?;
    prepare_redacted_artifact(redacted, None, redactor.version(), fingerprint_key)
}

/// Prepare a metadata-only artifact. A separately domain-separated keyed
/// source commitment detects changes in exact canonical evidence without
/// retaining arbitrary keys or values; the append fingerprint covers the
/// complete credential-free wrapper that is actually sealed.
pub(crate) fn prepare_artifact_bound(
    safe_raw: &Value,
    private_evidence: &Value,
    redactor: &dyn ArtifactRedactor,
    fingerprint_key: &FingerprintKey,
) -> Result<PreparedArtifact, SealedArtifactError> {
    validate_identifier("redactor version", redactor.version())?;
    let mut redacted = redactor.redact(safe_raw)?;
    let source_jcs = match serde_json_canonicalizer::to_vec(private_evidence) {
        Ok(value) => Zeroizing::new(value),
        Err(error) => {
            zeroize_json_value(&mut redacted.credential_free_private);
            return Err(error.into());
        }
    };
    if source_jcs.len() > MAX_ARTIFACT_BYTES {
        zeroize_json_value(&mut redacted.credential_free_private);
        return Err(SealedArtifactError::TooLarge {
            actual: source_jcs.len(),
            limit: MAX_ARTIFACT_BYTES,
        });
    }
    let mut source_mac =
        match <HmacSha256 as Mac>::new_from_slice(fingerprint_key.material.as_ref()) {
            Ok(mac) => mac,
            Err(_) => {
                zeroize_json_value(&mut redacted.credential_free_private);
                return Err(SealedArtifactError::Cryptography(
                    "invalid HMAC key".to_string(),
                ));
            }
        };
    source_mac.update(SOURCE_COMMITMENT_DOMAIN);
    if let Err(error) = update_length_framed(&mut source_mac, &source_jcs) {
        zeroize_json_value(&mut redacted.credential_free_private);
        return Err(error);
    }
    let public_identity = redacted.public_identity;
    let mut credential_free = serde_json::json!({
        "payload": redacted.credential_free_private,
        "replay_fidelity": "metadata_only",
        "source_commitment": hex::encode(source_mac.finalize().into_bytes()),
        "source_commitment_algorithm": ARTIFACT_FINGERPRINT_ALGORITHM,
        "source_commitment_version": SOURCE_COMMITMENT_VERSION,
    });
    let bound_private = serde_json_canonicalizer::to_vec(&credential_free);
    zeroize_json_value(&mut credential_free);
    let bound_private = Zeroizing::new(bound_private?);
    prepare_redacted_artifact(
        RedactedArtifact {
            public_identity,
            credential_free_private: Value::Null,
        },
        Some(bound_private),
        redactor.version(),
        fingerprint_key,
    )
}

/// Prepare the immutable private RunStart payload for exact replay.
pub(crate) fn prepare_exact_artifact_bound(
    safe_raw: &Value,
    private_evidence: &Value,
    redactor: &dyn ArtifactRedactor,
    fingerprint_key: &FingerprintKey,
) -> Result<PreparedArtifact, SealedArtifactError> {
    prepare_exact_artifact_bound_for(
        ExactRuntimeEvidenceSchema::RunStart,
        safe_raw,
        private_evidence,
        redactor,
        fingerprint_key,
    )
}

/// Prepare one explicitly closed runtime transcript artifact for exact replay.
/// The caller cannot make an arbitrary shape exact: `schema` selects the
/// complete validator before any plaintext is handed to the sealer.
pub(crate) fn prepare_exact_runtime_artifact_bound(
    schema: ExactRuntimeEvidenceSchema,
    safe_raw: &Value,
    private_evidence: &Value,
    redactor: &dyn ArtifactRedactor,
    fingerprint_key: &FingerprintKey,
) -> Result<PreparedArtifact, SealedArtifactError> {
    if schema == ExactRuntimeEvidenceSchema::RunStart {
        return Err(invalid_artifact(
            "RunStart must use the dedicated exact preparation entry point",
        ));
    }
    prepare_exact_artifact_bound_for(
        schema,
        safe_raw,
        private_evidence,
        redactor,
        fingerprint_key,
    )
}

fn prepare_exact_artifact_bound_for(
    schema: ExactRuntimeEvidenceSchema,
    safe_raw: &Value,
    private_evidence: &Value,
    redactor: &dyn ArtifactRedactor,
    fingerprint_key: &FingerprintKey,
) -> Result<PreparedArtifact, SealedArtifactError> {
    validate_identifier("redactor version", redactor.version())?;
    closed_exact_runtime_payload(schema, safe_raw, private_evidence)?;
    // RunStart validates each closed field separately because JSON-schema
    // property names are metadata, not runtime credential fields. Every other
    // exact schema has no such key namespace and remains subject to recursive
    // identity redaction.
    if schema != ExactRuntimeEvidenceSchema::RunStart {
        let mut recursively_redacted = credential_free_replay_payload(private_evidence);
        let private_identity = ensure_canonical_bytes_match(
            private_evidence,
            &recursively_redacted,
            "exact private evidence requires a redaction identity transform",
        );
        zeroize_json_value(&mut recursively_redacted);
        private_identity?;
    }
    let expected_identity = safe_raw
        .get("identity")
        .ok_or_else(|| invalid_artifact("exact runtime identity is missing"))?;
    let exact_private = Zeroizing::new(serde_json_canonicalizer::to_vec(private_evidence)?);
    let mut redacted = redactor.redact(safe_raw)?;
    let identity_matches = ensure_canonical_bytes_match(
        expected_identity,
        &redacted.public_identity,
        "exact public identity requires a redaction identity transform",
    );
    zeroize_json_value(&mut redacted.credential_free_private);
    identity_matches?;
    prepare_redacted_artifact(
        RedactedArtifact {
            public_identity: redacted.public_identity,
            credential_free_private: Value::Null,
        },
        Some(exact_private),
        redactor.version(),
        fingerprint_key,
    )
}

fn prepare_redacted_artifact(
    redacted: RedactedArtifact,
    bound_private_evidence: Option<Zeroizing<Vec<u8>>>,
    redactor_version: &str,
    fingerprint_key: &FingerprintKey,
) -> Result<PreparedArtifact, SealedArtifactError> {
    let RedactedArtifact {
        public_identity,
        mut credential_free_private,
    } = redacted;
    let public_identity_jcs = match serde_json_canonicalizer::to_vec(&public_identity) {
        Ok(value) => value,
        Err(error) => {
            zeroize_json_value(&mut credential_free_private);
            return Err(error.into());
        }
    };
    let private_result = match bound_private_evidence {
        Some(private_evidence) => Ok(private_evidence),
        None => serde_json_canonicalizer::to_vec(&credential_free_private).map(Zeroizing::new),
    };
    zeroize_json_value(&mut credential_free_private);
    let private_jcs = private_result?;
    if private_jcs.len() > MAX_ARTIFACT_BYTES {
        return Err(SealedArtifactError::TooLarge {
            actual: private_jcs.len(),
            limit: MAX_ARTIFACT_BYTES,
        });
    }

    let private_fingerprint =
        derive_private_fingerprint(&public_identity_jcs, &private_jcs, fingerprint_key)?;

    Ok(PreparedArtifact {
        identity: AppendIdentity {
            public_identity_jcs,
            private_fingerprint,
            fingerprint_algorithm: ARTIFACT_FINGERPRINT_ALGORITHM.to_string(),
            fingerprint_version: ARTIFACT_FINGERPRINT_VERSION,
            fingerprint_key_id: fingerprint_key.key_id.clone(),
            fingerprint_key_version: fingerprint_key.key_version,
            redactor_version: redactor_version.to_string(),
            canonicalization_algorithm: ARTIFACT_CANONICALIZATION.to_string(),
            canonicalization_version: ARTIFACT_CANONICALIZATION_VERSION,
        },
        private_jcs,
    })
}

/// Verify a committed private-JCS fingerprint without exposing parsed private
/// data. The private JSON is parsed only under [`ScrubbedJsonValue`] to prove
/// canonicality, then recursively scrubbed on return.
pub(crate) fn validate_private_jcs_commitment(
    public_identity_jcs: &[u8],
    private_jcs: &[u8],
    fingerprint_key: &FingerprintKey,
    expected_fingerprint: &str,
) -> Result<(), SealedArtifactError> {
    let _private = ScrubbedJsonValue::parse_canonical(private_jcs)?;
    let actual = derive_private_fingerprint(public_identity_jcs, private_jcs, fingerprint_key)?;
    if actual != expected_fingerprint {
        return Err(SealedArtifactError::InvalidMetadata(
            "private JCS does not match its committed fingerprint".to_string(),
        ));
    }
    Ok(())
}

fn derive_private_fingerprint(
    public_identity_jcs: &[u8],
    private_jcs: &[u8],
    fingerprint_key: &FingerprintKey,
) -> Result<String, SealedArtifactError> {
    let mut mac = <HmacSha256 as Mac>::new_from_slice(fingerprint_key.material.as_ref())
        .map_err(|_| SealedArtifactError::Cryptography("invalid HMAC key".to_string()))?;
    mac.update(APPEND_IDENTITY_DOMAIN);
    update_length_framed(&mut mac, public_identity_jcs)?;
    update_length_framed(&mut mac, private_jcs)?;
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn update_length_framed(mac: &mut HmacSha256, bytes: &[u8]) -> Result<(), SealedArtifactError> {
    let length = u64::try_from(bytes.len()).map_err(|_| SealedArtifactError::TooLarge {
        actual: bytes.len(),
        limit: usize::MAX,
    })?;
    mac.update(&length.to_be_bytes());
    mac.update(bytes);
    Ok(())
}

pub struct SealingKey {
    key_id: String,
    material: Zeroizing<[u8; 32]>,
}

impl fmt::Debug for SealingKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SealingKey")
            .field("key_id", &self.key_id)
            .field("material", &"[REDACTED]")
            .finish()
    }
}

impl Clone for SealingKey {
    fn clone(&self) -> Self {
        Self {
            key_id: self.key_id.clone(),
            material: Zeroizing::new(*self.material),
        }
    }
}

impl SealingKey {
    pub fn new(key_id: impl Into<String>, material: [u8; 32]) -> Result<Self, SealedArtifactError> {
        let key_id = key_id.into();
        validate_identifier("sealing key id", &key_id)?;
        Ok(Self {
            key_id,
            material: Zeroizing::new(material),
        })
    }
}

pub trait ArtifactKeyResolver: Send + Sync {
    fn active_key(&self) -> Result<SealingKey, SealedArtifactError>;
    fn resolve_key(&self, key_id: &str) -> Result<SealingKey, SealedArtifactError>;
}

/// A small in-process keyring suitable for wiring to an OS-backed key resolver.
/// It never serializes key bytes and supports decrypting artifacts written by
/// rotated keys while selecting exactly one active write key.
pub struct StaticArtifactKeyring {
    active_key_id: String,
    keys: HashMap<String, SealingKey>,
}

impl fmt::Debug for StaticArtifactKeyring {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StaticArtifactKeyring")
            .field("active_key_id", &self.active_key_id)
            .field("key_count", &self.keys.len())
            .finish()
    }
}

impl StaticArtifactKeyring {
    pub fn new(
        active: SealingKey,
        decrypt_only: impl IntoIterator<Item = SealingKey>,
    ) -> Result<Self, SealedArtifactError> {
        let active_key_id = active.key_id.clone();
        let mut keys = HashMap::new();
        keys.insert(active.key_id.clone(), active);
        for key in decrypt_only {
            if keys.insert(key.key_id.clone(), key).is_some() {
                return Err(SealedArtifactError::InvalidMetadata(
                    "duplicate sealing key id".to_string(),
                ));
            }
        }
        Ok(Self {
            active_key_id,
            keys,
        })
    }
}

impl ArtifactKeyResolver for StaticArtifactKeyring {
    fn active_key(&self) -> Result<SealingKey, SealedArtifactError> {
        self.resolve_key(&self.active_key_id)
    }

    fn resolve_key(&self, key_id: &str) -> Result<SealingKey, SealedArtifactError> {
        self.keys
            .get(key_id)
            .cloned()
            .ok_or_else(|| SealedArtifactError::KeyUnavailable(key_id.to_string()))
    }
}
pub trait ArtifactCredentialProvisionGuard: Send {}

pub trait ArtifactCredentialStore: Send + Sync {
    fn load_secret(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, SealedArtifactError>;

    fn store_secret(
        &self,
        service: &str,
        account: &str,
        secret: &[u8],
    ) -> Result<(), SealedArtifactError>;

    fn acquire_provision_lock(
        &self,
        service: &str,
        store_namespace: &str,
    ) -> Result<Box<dyn ArtifactCredentialProvisionGuard>, SealedArtifactError>;
}

#[derive(Debug, Default)]
pub struct SystemArtifactCredentialStore;

impl ArtifactCredentialStore for SystemArtifactCredentialStore {
    fn load_secret(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, SealedArtifactError> {
        #[cfg(any(target_vendor = "apple", target_os = "windows", target_os = "linux"))]
        {
            let entry = keyring::Entry::new(service, account).map_err(|error| {
                SealedArtifactError::KeyUnavailable(format!(
                    "credential entry initialization failed: {error}"
                ))
            })?;
            match entry.get_secret() {
                Ok(secret) => Ok(Some(Zeroizing::new(secret))),
                Err(keyring::Error::NoEntry) => Ok(None),
                Err(error) => Err(SealedArtifactError::KeyUnavailable(format!(
                    "credential read failed: {error}"
                ))),
            }
        }
        #[cfg(not(any(target_vendor = "apple", target_os = "windows", target_os = "linux")))]
        {
            let _ = (service, account);
            Err(SealedArtifactError::KeyUnavailable(
                "no supported OS credential backend is compiled for this platform".to_string(),
            ))
        }
    }

    fn store_secret(
        &self,
        service: &str,
        account: &str,
        secret: &[u8],
    ) -> Result<(), SealedArtifactError> {
        #[cfg(any(target_vendor = "apple", target_os = "windows", target_os = "linux"))]
        {
            let entry = keyring::Entry::new(service, account).map_err(|error| {
                SealedArtifactError::KeyUnavailable(format!(
                    "credential entry initialization failed: {error}"
                ))
            })?;
            entry.set_secret(secret).map_err(|error| {
                SealedArtifactError::KeyUnavailable(format!("credential write failed: {error}"))
            })
        }
        #[cfg(not(any(target_vendor = "apple", target_os = "windows", target_os = "linux")))]
        {
            let _ = (service, account, secret);
            Err(SealedArtifactError::KeyUnavailable(
                "no supported OS credential backend is compiled for this platform".to_string(),
            ))
        }
    }
    fn acquire_provision_lock(
        &self,
        service: &str,
        store_namespace: &str,
    ) -> Result<Box<dyn ArtifactCredentialProvisionGuard>, SealedArtifactError> {
        let _ = (service, store_namespace);
        Ok(Box::new(SystemCredentialProvisionGuard {
            _file: acquire_system_credential_provision_lock()?,
        }))
    }
}

/// Process-local credential backend for isolated tests and ephemeral harnesses.
///
/// Secrets are never written to the OS keyring or filesystem and disappear
/// when the owning runtime is dropped. Production runtimes must use
/// [`SystemArtifactCredentialStore`].
#[derive(Debug, Default)]
pub(crate) struct EphemeralArtifactCredentialStore {
    secrets: Mutex<HashMap<(String, String), Vec<u8>>>,
    provision_lock: Arc<EphemeralCredentialProvisionLock>,
}

#[derive(Debug, Default)]
struct EphemeralCredentialProvisionLock {
    held: Mutex<bool>,
    available: Condvar,
}

struct EphemeralCredentialProvisionGuard {
    lock: Arc<EphemeralCredentialProvisionLock>,
}

impl Drop for EphemeralCredentialProvisionGuard {
    fn drop(&mut self) {
        if let Ok(mut held) = self.lock.held.lock() {
            *held = false;
            self.lock.available.notify_one();
        }
    }
}

impl ArtifactCredentialProvisionGuard for EphemeralCredentialProvisionGuard {}

impl ArtifactCredentialStore for EphemeralArtifactCredentialStore {
    fn load_secret(
        &self,
        service: &str,
        account: &str,
    ) -> Result<Option<Zeroizing<Vec<u8>>>, SealedArtifactError> {
        let secrets = self.secrets.lock().map_err(|_| {
            SealedArtifactError::KeyUnavailable(
                "ephemeral credential store lock poisoned".to_string(),
            )
        })?;
        Ok(secrets
            .get(&(service.to_string(), account.to_string()))
            .cloned()
            .map(Zeroizing::new))
    }

    fn store_secret(
        &self,
        service: &str,
        account: &str,
        secret: &[u8],
    ) -> Result<(), SealedArtifactError> {
        let mut secrets = self.secrets.lock().map_err(|_| {
            SealedArtifactError::KeyUnavailable(
                "ephemeral credential store lock poisoned".to_string(),
            )
        })?;
        secrets.insert((service.to_string(), account.to_string()), secret.to_vec());
        Ok(())
    }

    fn acquire_provision_lock(
        &self,
        _service: &str,
        _store_namespace: &str,
    ) -> Result<Box<dyn ArtifactCredentialProvisionGuard>, SealedArtifactError> {
        let mut held = self.provision_lock.held.lock().map_err(|_| {
            SealedArtifactError::KeyUnavailable("ephemeral provision lock poisoned".to_string())
        })?;
        while *held {
            held = self.provision_lock.available.wait(held).map_err(|_| {
                SealedArtifactError::KeyUnavailable("ephemeral provision lock poisoned".to_string())
            })?;
        }
        *held = true;
        drop(held);
        Ok(Box::new(EphemeralCredentialProvisionGuard {
            lock: self.provision_lock.clone(),
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OsArtifactKeyManagerConfig {
    service: String,
    store_namespace: String,
    active_sealing_key_id: String,
    active_fingerprint_key_id: String,
    active_fingerprint_key_version: u32,
}

/// Public references that bind a configured key pair to one protected CAS.
/// This record intentionally never contains key material.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ArtifactKeyProvisionMarker {
    schema: String,
    service: String,
    store_namespace: String,
    sealing_key_id: String,
    fingerprint_key_id: String,
    fingerprint_key_version: u32,
}

impl ArtifactKeyProvisionMarker {
    fn from_config(config: &OsArtifactKeyManagerConfig) -> Self {
        Self {
            schema: KEY_PROVISION_MARKER_SCHEMA.to_string(),
            service: config.service.clone(),
            store_namespace: config.store_namespace.clone(),
            sealing_key_id: config.active_sealing_key_id.clone(),
            fingerprint_key_id: config.active_fingerprint_key_id.clone(),
            fingerprint_key_version: config.active_fingerprint_key_version,
        }
    }

    fn validate(&self) -> Result<(), SealedArtifactError> {
        if self.schema != KEY_PROVISION_MARKER_SCHEMA {
            return Err(SealedArtifactError::KeyUnavailable(
                "artifact key provisioning marker schema is unsupported".to_string(),
            ));
        }
        validate_identifier("artifact credential service", &self.service)
            .map_err(marker_key_unavailable)?;
        validate_identifier("artifact store namespace", &self.store_namespace)
            .map_err(marker_key_unavailable)?;
        validate_identifier("active sealing key id", &self.sealing_key_id)
            .map_err(marker_key_unavailable)?;
        validate_identifier("active fingerprint key id", &self.fingerprint_key_id)
            .map_err(marker_key_unavailable)?;
        if self.sealing_key_id == self.fingerprint_key_id {
            return Err(SealedArtifactError::KeyUnavailable(
                "artifact key provisioning marker reuses the sealing key id".to_string(),
            ));
        }
        if self.fingerprint_key_version == 0 {
            return Err(SealedArtifactError::KeyUnavailable(
                "artifact key provisioning marker has zero fingerprint key version".to_string(),
            ));
        }
        Ok(())
    }
}

impl OsArtifactKeyManagerConfig {
    pub fn new(
        service: impl Into<String>,
        active_sealing_key_id: impl Into<String>,
        active_fingerprint_key_id: impl Into<String>,
        active_fingerprint_key_version: u32,
        artifact_store: &FilesystemSealedArtifactStore,
    ) -> Result<Self, SealedArtifactError> {
        Self::for_namespace(
            service,
            artifact_store.credential_namespace(),
            active_sealing_key_id,
            active_fingerprint_key_id,
            active_fingerprint_key_version,
        )
    }

    /// Construct for a stable profile namespace when the artifact store is not
    /// filesystem-backed. The namespace is public metadata, must survive moves
    /// and restores, and must not be reused by an unrelated profile.
    pub fn for_namespace(
        service: impl Into<String>,
        store_namespace: impl Into<String>,
        active_sealing_key_id: impl Into<String>,
        active_fingerprint_key_id: impl Into<String>,
        active_fingerprint_key_version: u32,
    ) -> Result<Self, SealedArtifactError> {
        let service = service.into();
        let store_namespace = store_namespace.into();
        let active_sealing_key_id = active_sealing_key_id.into();
        let active_fingerprint_key_id = active_fingerprint_key_id.into();
        validate_identifier("artifact credential service", &service)?;
        validate_identifier("artifact store namespace", &store_namespace)?;
        validate_identifier("active sealing key id", &active_sealing_key_id)?;
        validate_identifier("active fingerprint key id", &active_fingerprint_key_id)?;
        if active_sealing_key_id == active_fingerprint_key_id {
            return Err(SealedArtifactError::InvalidMetadata(
                "sealing and append-identity key ids must be distinct".to_string(),
            ));
        }
        if active_fingerprint_key_version == 0 {
            return Err(SealedArtifactError::InvalidMetadata(
                "active fingerprint key version must be non-zero".to_string(),
            ));
        }
        Ok(Self {
            service,
            store_namespace,
            active_sealing_key_id,
            active_fingerprint_key_id,
            active_fingerprint_key_version,
        })
    }
}

pub struct OsArtifactKeyManager {
    config: OsArtifactKeyManagerConfig,
    credentials: Arc<dyn ArtifactCredentialStore>,
}

impl fmt::Debug for OsArtifactKeyManager {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("OsArtifactKeyManager")
            .field("service", &self.config.service)
            .field("store_namespace", &self.config.store_namespace)
            .field("active_sealing_key_id", &self.config.active_sealing_key_id)
            .field(
                "active_fingerprint_key_id",
                &self.config.active_fingerprint_key_id,
            )
            .field(
                "active_fingerprint_key_version",
                &self.config.active_fingerprint_key_version,
            )
            .finish_non_exhaustive()
    }
}

impl OsArtifactKeyManager {
    pub fn system(config: OsArtifactKeyManagerConfig) -> Self {
        Self::with_store(config, Arc::new(SystemArtifactCredentialStore))
    }

    pub fn with_store(
        config: OsArtifactKeyManagerConfig,
        credentials: Arc<dyn ArtifactCredentialStore>,
    ) -> Self {
        Self {
            config,
            credentials,
        }
    }

    /// Test-only unscoped provisioning for stores that intentionally have no
    /// filesystem/CAS lifecycle. Production initialization must bind keys to a
    /// protected store through [`Self::provision_active_keys_for_store`].
    #[cfg(test)]
    pub(crate) fn provision_active_keys(&self) -> Result<(), SealedArtifactError> {
        let _credential_lock = self
            .credentials
            .acquire_provision_lock(&self.config.service, &self.config.store_namespace)?;
        self.provision_secret(
            &self.sealing_account(&self.config.active_sealing_key_id),
            "active sealing key",
        )?;
        self.provision_secret(
            &self.fingerprint_account(
                &self.config.active_fingerprint_key_id,
                self.config.active_fingerprint_key_version,
            ),
            "active fingerprint key",
        )?;
        self.validate_active_credentials()
    }

    /// Explicitly initialize the key pair for one protected CAS. Once the
    /// owner-only marker exists, this path becomes resolve-only: a deleted,
    /// malformed, or unavailable credential is never regenerated.
    pub fn provision_active_keys_for_store(
        &self,
        artifact_store: &FilesystemSealedArtifactStore,
    ) -> Result<(), SealedArtifactError> {
        if self.config.store_namespace != artifact_store.credential_namespace() {
            return Err(SealedArtifactError::InvalidMetadata(
                "artifact key manager is bound to a different artifact store".to_string(),
            ));
        }
        artifact_store.verify_root()?;
        let _store_lock = artifact_store.exclusive_operation_lock()?;
        let _credential_lock = self
            .credentials
            .acquire_provision_lock(&self.config.service, &self.config.store_namespace)?;
        let marker = ArtifactKeyProvisionMarker::from_config(&self.config);

        if self
            .load_provision_marker(artifact_store, &marker)?
            .is_some()
        {
            return self.validate_active_credentials();
        }

        let sealing_account = self.sealing_account(&self.config.active_sealing_key_id);
        let fingerprint_account = self.fingerprint_account(
            &self.config.active_fingerprint_key_id,
            self.config.active_fingerprint_key_version,
        );
        let sealing = self
            .credentials
            .load_secret(&self.config.service, &sealing_account)?;
        let fingerprint = self
            .credentials
            .load_secret(&self.config.service, &fingerprint_account)?;

        let store_is_empty = artifact_store.empty_for_key_provisioning()?;
        if !store_is_empty && !self.has_valid_marker_lineage(artifact_store)? {
            return Err(SealedArtifactError::KeyUnavailable(
                "artifact key provisioning marker is absent and protected store has no valid key lineage"
                    .to_string(),
            ));
        }
        match (sealing, fingerprint) {
            (Some(sealing), Some(fingerprint)) => {
                validate_key_material_length(&sealing, "active sealing key")?;
                validate_key_material_length(&fingerprint, "active fingerprint key")?;
                self.validate_active_credentials()?;
                self.write_provision_marker(artifact_store, &marker)
            }
            (None, None) => {
                self.provision_secret(&sealing_account, "active sealing key")?;
                self.provision_secret(&fingerprint_account, "active fingerprint key")?;
                self.validate_active_credentials()?;
                self.write_provision_marker(artifact_store, &marker)
            }
            // A new manager configuration may share either historical key
            // component while adding its other component. The lineage gate
            // above distinguishes that explicit rotation from an unmarked
            // first-install interruption once the CAS contains data.
            (Some(sealing), None) => {
                validate_key_material_length(&sealing, "active sealing key")?;
                self.provision_secret(&fingerprint_account, "active fingerprint key")?;
                self.validate_active_credentials()?;
                self.write_provision_marker(artifact_store, &marker)
            }
            (None, Some(fingerprint)) => {
                validate_key_material_length(&fingerprint, "active fingerprint key")?;
                self.provision_secret(&sealing_account, "active sealing key")?;
                self.validate_active_credentials()?;
                self.write_provision_marker(artifact_store, &marker)
            }
        }
    }

    /// Resolve a previously initialized key pair without creating credentials
    /// or modifying protected-store state. Runtime entrypoints must use this
    /// path; first install and rotation are explicit provisioning operations.
    pub fn resolve_active_keys_for_store(
        &self,
        artifact_store: &FilesystemSealedArtifactStore,
    ) -> Result<(), SealedArtifactError> {
        if self.config.store_namespace != artifact_store.credential_namespace() {
            return Err(SealedArtifactError::InvalidMetadata(
                "artifact key manager is bound to a different artifact store".to_string(),
            ));
        }
        artifact_store.verify_root()?;
        let _store_lock = artifact_store.shared_operation_lock()?;
        let marker = ArtifactKeyProvisionMarker::from_config(&self.config);
        if self
            .load_provision_marker(artifact_store, &marker)?
            .is_none()
        {
            return Err(SealedArtifactError::KeyUnavailable(
                "artifact key provisioning marker is absent; explicit initialization is required"
                    .to_string(),
            ));
        }
        self.validate_active_credentials()
    }

    pub fn active_fingerprint_key(&self) -> Result<FingerprintKey, SealedArtifactError> {
        self.resolve_fingerprint_key(
            &self.config.active_fingerprint_key_id,
            self.config.active_fingerprint_key_version,
        )
    }

    pub fn resolve_fingerprint_key(
        &self,
        key_id: &str,
        key_version: u32,
    ) -> Result<FingerprintKey, SealedArtifactError> {
        validate_identifier("fingerprint key id", key_id)?;
        if key_version == 0 {
            return Err(SealedArtifactError::InvalidMetadata(
                "fingerprint key version must be non-zero".to_string(),
            ));
        }
        let label = format!("fingerprint key {key_id} version {key_version}");
        let secret =
            self.required_secret(&self.fingerprint_account(key_id, key_version), &label)?;
        let mut material = [0_u8; 32];
        material.copy_from_slice(secret.as_slice());
        let key = FingerprintKey::new(key_id, key_version, material);
        material.zeroize();
        key
    }

    fn validate_active_credentials(&self) -> Result<(), SealedArtifactError> {
        let sealing = self.required_secret(
            &self.sealing_account(&self.config.active_sealing_key_id),
            "active sealing key",
        )?;
        let fingerprint = self.required_secret(
            &self.fingerprint_account(
                &self.config.active_fingerprint_key_id,
                self.config.active_fingerprint_key_version,
            ),
            "active fingerprint key",
        )?;
        if sealing.as_slice() == fingerprint.as_slice() {
            return Err(SealedArtifactError::KeyUnavailable(
                "active sealing and append-identity credentials must differ".to_string(),
            ));
        }
        Ok(())
    }

    fn has_valid_marker_lineage(
        &self,
        artifact_store: &FilesystemSealedArtifactStore,
    ) -> Result<bool, SealedArtifactError> {
        for entry in fs::read_dir(&artifact_store.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let is_marker = match is_key_provision_marker_filename(name) {
                Ok(is_marker) => is_marker,
                Err(_) => continue,
            };
            if !is_marker {
                continue;
            }
            let marker = match self.load_provision_marker_path(&entry.path()) {
                Ok(Some(marker)) => marker,
                Ok(None) | Err(_) => continue,
            };
            if marker.service != self.config.service
                || marker.store_namespace != self.config.store_namespace
            {
                continue;
            }
            let sealing = self.credentials.load_secret(
                &marker.service,
                &self.sealing_account(&marker.sealing_key_id),
            )?;
            let fingerprint = self.credentials.load_secret(
                &marker.service,
                &self.fingerprint_account(
                    &marker.fingerprint_key_id,
                    marker.fingerprint_key_version,
                ),
            )?;
            let (Some(sealing), Some(fingerprint)) = (sealing, fingerprint) else {
                continue;
            };
            if validate_key_material_length(&sealing, "lineage sealing key").is_ok()
                && validate_key_material_length(&fingerprint, "lineage fingerprint key").is_ok()
                && sealing.as_slice() != fingerprint.as_slice()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn provision_marker_path(
        &self,
        artifact_store: &FilesystemSealedArtifactStore,
        marker: &ArtifactKeyProvisionMarker,
    ) -> Result<PathBuf, SealedArtifactError> {
        let canonical = serde_json_canonicalizer::to_vec(marker)?;
        Ok(artifact_store.root.join(format!(
            "{KEY_PROVISION_MARKER_PREFIX}{}{}",
            hex::encode(Sha256::digest(canonical)),
            KEY_PROVISION_MARKER_SUFFIX
        )))
    }

    fn load_provision_marker(
        &self,
        artifact_store: &FilesystemSealedArtifactStore,
        expected: &ArtifactKeyProvisionMarker,
    ) -> Result<Option<ArtifactKeyProvisionMarker>, SealedArtifactError> {
        let path = self.provision_marker_path(artifact_store, expected)?;
        let marker = self.load_provision_marker_path(&path)?;
        if marker.as_ref().is_some_and(|marker| marker != expected) {
            return Err(SealedArtifactError::KeyUnavailable(
                "artifact key provisioning marker does not bind this key manager".to_string(),
            ));
        }
        Ok(marker)
    }

    fn load_provision_marker_path(
        &self,
        path: &Path,
    ) -> Result<Option<ArtifactKeyProvisionMarker>, SealedArtifactError> {
        let mut file = match open_private_file(path) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(SealedArtifactError::KeyUnavailable(format!(
                    "artifact key provisioning marker is inaccessible: {error}"
                )));
            }
        };
        let length = usize::try_from(
            file.metadata()
                .map_err(|error| {
                    SealedArtifactError::KeyUnavailable(format!(
                        "artifact key provisioning marker metadata is inaccessible: {error}"
                    ))
                })?
                .len(),
        )
        .unwrap_or(usize::MAX);
        if length == 0 || length > 4096 {
            return Err(SealedArtifactError::KeyUnavailable(
                "artifact key provisioning marker has an invalid length".to_string(),
            ));
        }
        let mut bytes = Vec::with_capacity(length);
        file.read_to_end(&mut bytes).map_err(|error| {
            SealedArtifactError::KeyUnavailable(format!(
                "artifact key provisioning marker cannot be read: {error}"
            ))
        })?;
        let marker: ArtifactKeyProvisionMarker = serde_json::from_slice(&bytes).map_err(|_| {
            SealedArtifactError::KeyUnavailable(
                "artifact key provisioning marker is malformed".to_string(),
            )
        })?;
        marker.validate()?;
        let canonical = serde_json_canonicalizer::to_vec(&marker).map_err(|_| {
            SealedArtifactError::KeyUnavailable(
                "artifact key provisioning marker cannot be canonicalized".to_string(),
            )
        })?;
        let expected_name = format!(
            "{KEY_PROVISION_MARKER_PREFIX}{}{}",
            hex::encode(Sha256::digest(&canonical)),
            KEY_PROVISION_MARKER_SUFFIX
        );
        if canonical != bytes
            || path.file_name().and_then(|name| name.to_str()) != Some(expected_name.as_str())
        {
            return Err(SealedArtifactError::KeyUnavailable(
                "artifact key provisioning marker filename or content is invalid".to_string(),
            ));
        }
        Ok(Some(marker))
    }

    fn write_provision_marker(
        &self,
        artifact_store: &FilesystemSealedArtifactStore,
        marker: &ArtifactKeyProvisionMarker,
    ) -> Result<(), SealedArtifactError> {
        let destination = self.provision_marker_path(artifact_store, marker)?;
        let bytes = serde_json_canonicalizer::to_vec(marker)?;
        let temporary = artifact_store.temporary_path();
        let mut cleanup = TemporaryArtifact::new(temporary.clone());
        let mut file = create_private_file(&temporary)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        match atomic_rename_noreplace(&temporary, &destination) {
            Ok(()) => {
                cleanup.disarm();
                sync_directory(&artifact_store.root)?;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(SealedArtifactError::Io(error)),
        }
        if self
            .load_provision_marker(artifact_store, marker)?
            .is_none()
        {
            return Err(SealedArtifactError::KeyUnavailable(
                "artifact key provisioning marker disappeared after publication".to_string(),
            ));
        }
        Ok(())
    }

    fn provision_secret(&self, account: &str, label: &str) -> Result<(), SealedArtifactError> {
        if let Some(existing) = self
            .credentials
            .load_secret(&self.config.service, account)?
        {
            return validate_key_material_length(&existing, label);
        }

        let mut generated = Zeroizing::new(vec![0_u8; 32]);
        OsRng.fill_bytes(generated.as_mut_slice());
        self.credentials
            .store_secret(&self.config.service, account, &generated)?;
        let observed = self.required_secret(account, label)?;
        if observed.as_slice() != generated.as_slice() {
            return Err(SealedArtifactError::KeyUnavailable(format!(
                "{label} changed during provisioning"
            )));
        }
        Ok(())
    }

    fn required_secret(
        &self,
        account: &str,
        label: &str,
    ) -> Result<Zeroizing<Vec<u8>>, SealedArtifactError> {
        let secret = self
            .credentials
            .load_secret(&self.config.service, account)?
            .ok_or_else(|| {
                SealedArtifactError::KeyUnavailable(format!("{label} is not provisioned"))
            })?;
        validate_key_material_length(&secret, label)?;
        Ok(secret)
    }

    fn sealing_account(&self, key_id: &str) -> String {
        format!(
            "hermes-artifact:{}:sealing:{key_id}",
            self.config.store_namespace
        )
    }

    fn fingerprint_account(&self, key_id: &str, key_version: u32) -> String {
        format!(
            "hermes-artifact:{}:fingerprint:{key_id}:v{key_version}",
            self.config.store_namespace
        )
    }
}

impl ArtifactKeyResolver for OsArtifactKeyManager {
    fn active_key(&self) -> Result<SealingKey, SealedArtifactError> {
        self.resolve_key(&self.config.active_sealing_key_id)
    }

    fn resolve_key(&self, key_id: &str) -> Result<SealingKey, SealedArtifactError> {
        validate_identifier("sealing key id", key_id)?;
        let secret = self.required_secret(
            &self.sealing_account(key_id),
            &format!("sealing key {key_id}"),
        )?;
        let mut material = [0_u8; 32];
        material.copy_from_slice(secret.as_slice());
        let key = SealingKey::new(key_id, material);
        material.zeroize();
        key
    }
}

fn validate_key_material_length(material: &[u8], label: &str) -> Result<(), SealedArtifactError> {
    if material.len() != 32 {
        return Err(SealedArtifactError::KeyUnavailable(format!(
            "{label} must contain exactly 32 bytes"
        )));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SealedArtifactRef {
    /// SHA-256 of the encrypted, canonical envelope. This is not a digest of
    /// the private plaintext.
    pub envelope_digest: String,
    pub key_id: String,
    pub schema_version: u32,
    pub encryption_algorithm: String,
    pub canonicalization_algorithm: String,
    pub canonicalization_version: u8,
    pub redactor_version: String,
}
impl TryFrom<&crate::run_ledger::SealedArtifactRef> for SealedArtifactRef {
    type Error = SealedArtifactError;

    fn try_from(value: &crate::run_ledger::SealedArtifactRef) -> Result<Self, Self::Error> {
        let reference = Self {
            envelope_digest: value.envelope_digest().as_str().to_string(),
            key_id: value.key_id().as_str().to_string(),
            schema_version: value.schema_version(),
            encryption_algorithm: value.encryption_algorithm().to_string(),
            canonicalization_algorithm: value.canonicalization_algorithm().to_string(),
            canonicalization_version: value.canonicalization_version(),
            redactor_version: value.redactor_version().as_str().to_string(),
        };
        validate_reference(&reference)?;
        Ok(reference)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct EnvelopeHeader {
    schema_version: u32,
    encryption_algorithm: String,
    key_id: String,
    nonce_base64: String,
    canonicalization_algorithm: String,
    canonicalization_version: u8,
    redactor_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SealedEnvelope {
    header: EnvelopeHeader,
    ciphertext_base64: String,
}

pub(crate) trait ExactArtifactAuthorization: Send + Sync {
    fn authorize_exact_read(&self, artifact: &SealedArtifactRef)
        -> Result<(), SealedArtifactError>;
}

pub trait ArtifactPublicationGuard: Send {}

pub trait SealedArtifactStore: Send + Sync {
    /// Publish while retaining whatever store-wide exclusion prevents retention
    /// from deleting the artifact before its authoritative ledger commit.
    fn publish_for_commit(
        &self,
        envelope_digest: &str,
        bytes: &[u8],
    ) -> Result<Box<dyn ArtifactPublicationGuard>, SealedArtifactError>;

    fn put_if_absent(
        &self,
        envelope_digest: &str,
        bytes: &[u8],
    ) -> Result<(), SealedArtifactError> {
        drop(self.publish_for_commit(envelope_digest, bytes)?);
        Ok(())
    }

    fn read(&self, envelope_digest: &str) -> Result<Vec<u8>, SealedArtifactError>;
    fn remove(&self, envelope_digest: &str) -> Result<(), SealedArtifactError>;
}

/// Production filesystem capacity query used for sealed-artifact admission.
/// The query itself never mutates the protected store.
pub(crate) trait ArtifactFreeSpaceProvider: Send + Sync {
    fn available_bytes(&self, root: &Path) -> Result<u64, SealedArtifactError>;
}

#[derive(Debug)]
struct SystemArtifactFreeSpaceProvider;

impl ArtifactFreeSpaceProvider for SystemArtifactFreeSpaceProvider {
    fn available_bytes(&self, root: &Path) -> Result<u64, SealedArtifactError> {
        fs2::available_space(root).map_err(SealedArtifactError::Io)
    }
}

static SYSTEM_ARTIFACT_FREE_SPACE_PROVIDER: SystemArtifactFreeSpaceProvider =
    SystemArtifactFreeSpaceProvider;

/// Admission guard for a protected filesystem CAS. It is checked before an
/// append reservation is created, so capacity pressure cannot leave a
/// reservation, publication, event, or outbox side effect behind.
pub(crate) struct ArtifactFreeSpaceGuard<'a> {
    root: &'a Path,
    provider: &'a dyn ArtifactFreeSpaceProvider,
}

/// Holds the cross-process capacity reservation from admission through the
/// corresponding CAS publication and ledger commit.
pub(crate) struct ArtifactCapacityReservationGuard {
    _capacity_lock: AdvisoryFileLock,
}

impl fmt::Debug for ArtifactFreeSpaceGuard<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ArtifactFreeSpaceGuard")
            .field("root", &self.root)
            .finish_non_exhaustive()
    }
}

impl<'a> ArtifactFreeSpaceGuard<'a> {
    pub(crate) fn for_filesystem_store(store: &'a FilesystemSealedArtifactStore) -> Self {
        Self {
            root: store.root(),
            provider: &SYSTEM_ARTIFACT_FREE_SPACE_PROVIDER,
        }
    }

    #[cfg(test)]
    pub(crate) fn with_provider(
        root: &'a Path,
        provider: &'a dyn ArtifactFreeSpaceProvider,
    ) -> Self {
        Self { root, provider }
    }

    /// Acquire the cross-process capacity lock, then admit this exact
    /// envelope. The returned guard must outlive reservation, publication, and
    /// ledger commit so two writers cannot both spend the same free space.
    pub(crate) fn reserve_for_prepared(
        &self,
        prepared: &PreparedArtifact,
    ) -> Result<ArtifactCapacityReservationGuard, SealedArtifactError> {
        let capacity_lock = acquire_capacity_reservation_lock(self.root)?;
        self.ensure_available_for_private_bytes(prepared.private_jcs_len())?;
        Ok(ArtifactCapacityReservationGuard {
            _capacity_lock: capacity_lock,
        })
    }

    fn ensure_available_for_private_bytes(
        &self,
        private_jcs_len: usize,
    ) -> Result<(), SealedArtifactError> {
        let minimum_bytes = sealed_envelope_required_free_bytes(private_jcs_len)?;
        let available_bytes = self.provider.available_bytes(self.root)?;
        if available_bytes < minimum_bytes {
            return Err(SealedArtifactError::InsufficientFreeSpace {
                available_bytes,
                minimum_bytes,
            });
        }
        Ok(())
    }
}

/// Compute the physical bytes required to create one sealed CAS object.
///
/// The ciphertext has a fixed GCM tag, then receives standard Base64
/// encoding. Header strings are bounded identifiers, so the preflight uses
/// their maximum legal serialized size without resolving a sealing key before
/// manual-mode degradation can be recorded. The capacity margin covers
/// filesystem allocation and directory metadata during temp-file publication.
pub(crate) fn sealed_envelope_required_free_bytes(
    private_jcs_len: usize,
) -> Result<u64, SealedArtifactError> {
    if private_jcs_len > MAX_ARTIFACT_BYTES {
        return Err(SealedArtifactError::TooLarge {
            actual: private_jcs_len,
            limit: MAX_ARTIFACT_BYTES,
        });
    }

    let framed_plaintext_len = checked_envelope_size_add(private_jcs_len, PLAINTEXT_DIGEST_BYTES)?;
    let ciphertext_len = checked_envelope_size_add(framed_plaintext_len, AES_GCM_TAG_BYTES)?;
    let ciphertext_base64_len = checked_base64_encoded_len(ciphertext_len)?;
    let max_identifier = "x".repeat(MAX_ARTIFACT_IDENTIFIER_BYTES);
    let envelope = SealedEnvelope {
        header: EnvelopeHeader {
            schema_version: SEALED_ARTIFACT_SCHEMA_VERSION,
            encryption_algorithm: ARTIFACT_ENCRYPTION_ALGORITHM.to_string(),
            key_id: max_identifier.clone(),
            nonce_base64: "A".repeat(checked_base64_encoded_len(AES_GCM_NONCE_BYTES)?),
            canonicalization_algorithm: ARTIFACT_CANONICALIZATION.to_string(),
            canonicalization_version: ARTIFACT_CANONICALIZATION_VERSION,
            redactor_version: max_identifier,
        },
        ciphertext_base64: String::new(),
    };
    let fixed_envelope_len = serde_json_canonicalizer::to_vec(&envelope)?.len();
    let envelope_len = checked_envelope_size_add(fixed_envelope_len, ciphertext_base64_len)?;
    let envelope_len = u64::try_from(envelope_len).map_err(|_| {
        SealedArtifactError::InvalidMetadata(
            "sealed envelope size exceeds supported filesystem capacity".to_string(),
        )
    })?;
    envelope_len
        .checked_add(ARTIFACT_FILESYSTEM_SAFETY_MARGIN_BYTES)
        .ok_or_else(|| {
            SealedArtifactError::InvalidMetadata(
                "sealed envelope capacity requirement overflows".to_string(),
            )
        })
}

fn checked_envelope_size_add(left: usize, right: usize) -> Result<usize, SealedArtifactError> {
    left.checked_add(right).ok_or_else(|| {
        SealedArtifactError::InvalidMetadata("sealed envelope size overflows".to_string())
    })
}

fn checked_base64_encoded_len(bytes: usize) -> Result<usize, SealedArtifactError> {
    let groups = checked_envelope_size_add(bytes, 2)? / 3;
    groups.checked_mul(4).ok_or_else(|| {
        SealedArtifactError::InvalidMetadata("sealed envelope base64 size overflows".to_string())
    })
}

/// A sealed artifact whose publication lock remains held until the caller's
/// authoritative commit closure returns. This value is intentionally neither
/// cloneable nor serializable.
pub struct PendingSealedArtifact {
    identity: AppendIdentity,
    reference: SealedArtifactRef,
    publication_guard: Box<dyn ArtifactPublicationGuard>,
}

impl fmt::Debug for PendingSealedArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingSealedArtifact")
            .field("identity", &self.identity)
            .field("reference", &self.reference)
            .finish_non_exhaustive()
    }
}

impl PendingSealedArtifact {
    pub fn identity(&self) -> &AppendIdentity {
        &self.identity
    }

    pub fn reference(&self) -> &SealedArtifactRef {
        &self.reference
    }

    pub fn commit_with<T, E>(
        self,
        commit: impl FnOnce(AppendIdentity, SealedArtifactRef) -> Result<T, E>,
    ) -> Result<T, E> {
        let Self {
            identity,
            reference,
            publication_guard,
        } = self;
        let result = commit(identity, reference);
        drop(publication_guard);
        result
    }
}

pub struct ArtifactSealer<'a> {
    keys: &'a dyn ArtifactKeyResolver,
    store: &'a dyn SealedArtifactStore,
}

impl<'a> ArtifactSealer<'a> {
    pub fn new(keys: &'a dyn ArtifactKeyResolver, store: &'a dyn SealedArtifactStore) -> Self {
        Self { keys, store }
    }

    /// Seal only after the caller has won its append reservation. The prepared
    /// value is consumed so stale reservation losers cannot accidentally reuse
    /// its private buffer.
    pub fn seal(
        &self,
        prepared: PreparedArtifact,
    ) -> Result<PendingSealedArtifact, SealedArtifactError> {
        let key = self.keys.active_key()?;
        let nonce = Aes256Gcm::generate_nonce(&mut OsRng);
        let header = EnvelopeHeader {
            schema_version: SEALED_ARTIFACT_SCHEMA_VERSION,
            encryption_algorithm: ARTIFACT_ENCRYPTION_ALGORITHM.to_string(),
            key_id: key.key_id.clone(),
            nonce_base64: BASE64_STANDARD.encode(nonce.as_slice()),
            canonicalization_algorithm: ARTIFACT_CANONICALIZATION.to_string(),
            canonicalization_version: ARTIFACT_CANONICALIZATION_VERSION,
            redactor_version: prepared.identity.redactor_version.clone(),
        };
        let associated_data = serde_json_canonicalizer::to_vec(&header)?;

        let plaintext_digest = Sha256::digest(prepared.private_jcs.as_slice());
        let mut framed_plaintext = Zeroizing::new(Vec::with_capacity(
            PLAINTEXT_DIGEST_BYTES + prepared.private_jcs.len(),
        ));
        framed_plaintext.extend_from_slice(&plaintext_digest);
        framed_plaintext.extend_from_slice(&prepared.private_jcs);

        let cipher = Aes256Gcm::new_from_slice(key.material.as_ref())
            .map_err(|_| SealedArtifactError::Cryptography("invalid AES key".to_string()))?;
        let ciphertext = cipher
            .encrypt(
                &nonce,
                Payload {
                    msg: framed_plaintext.as_slice(),
                    aad: &associated_data,
                },
            )
            .map_err(|_| SealedArtifactError::Cryptography("encryption failed".to_string()))?;
        let envelope = SealedEnvelope {
            header: header.clone(),
            ciphertext_base64: BASE64_STANDARD.encode(ciphertext),
        };
        let envelope_bytes = serde_json_canonicalizer::to_vec(&envelope)?;
        let envelope_digest = hex::encode(Sha256::digest(&envelope_bytes));
        let publication_guard = self
            .store
            .publish_for_commit(&envelope_digest, &envelope_bytes)?;

        let reference = SealedArtifactRef {
            envelope_digest,
            key_id: header.key_id,
            schema_version: header.schema_version,
            encryption_algorithm: header.encryption_algorithm,
            canonicalization_algorithm: header.canonicalization_algorithm,
            canonicalization_version: header.canonicalization_version,
            redactor_version: header.redactor_version,
        };
        Ok(PendingSealedArtifact {
            identity: prepared.identity,
            reference,
            publication_guard,
        })
    }

    pub(crate) fn unseal_exact(
        &self,
        reference: &SealedArtifactRef,
        authorization: &dyn ExactArtifactAuthorization,
    ) -> Result<Zeroizing<Vec<u8>>, SealedArtifactError> {
        authorization.authorize_exact_read(reference)?;
        self.unseal_verified(reference)
    }

    /// Verify storage, envelope metadata, authenticated encryption, and private
    /// canonicalization without exposing the recovered plaintext.
    pub fn verify_exact_integrity(
        &self,
        reference: &SealedArtifactRef,
    ) -> Result<(), SealedArtifactError> {
        drop(self.unseal_verified(reference)?);
        Ok(())
    }

    fn unseal_verified(
        &self,
        reference: &SealedArtifactRef,
    ) -> Result<Zeroizing<Vec<u8>>, SealedArtifactError> {
        let envelope_bytes = self.store.read(&reference.envelope_digest)?;
        self.unseal_verified_envelope_bytes(reference, &envelope_bytes)
    }

    /// Verify and decrypt bytes read under a concrete store operation guard.
    /// Authorization is intentionally separate and must happen before that
    /// guard is acquired.
    pub(crate) fn unseal_verified_envelope_bytes(
        &self,
        reference: &SealedArtifactRef,
        envelope_bytes: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, SealedArtifactError> {
        validate_reference(reference)?;
        if envelope_bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(SealedArtifactError::TooLarge {
                actual: envelope_bytes.len(),
                limit: MAX_ARTIFACT_BYTES,
            });
        }
        let actual_envelope_digest = hex::encode(Sha256::digest(&envelope_bytes));
        if actual_envelope_digest != reference.envelope_digest {
            return Err(SealedArtifactError::InvalidEnvelope(
                "encrypted envelope digest mismatch".to_string(),
            ));
        }
        let envelope: SealedEnvelope = serde_json::from_slice(&envelope_bytes)?;
        let canonical_envelope = serde_json_canonicalizer::to_vec(&envelope)?;
        if canonical_envelope != envelope_bytes {
            return Err(SealedArtifactError::InvalidEnvelope(
                "envelope is not canonical RFC 8785 JSON".to_string(),
            ));
        }
        validate_header_matches_reference(&envelope.header, reference)?;

        let nonce_bytes = BASE64_STANDARD
            .decode(&envelope.header.nonce_base64)
            .map_err(|_| {
                SealedArtifactError::InvalidEnvelope("invalid nonce encoding".to_string())
            })?;
        if nonce_bytes.len() != 12 {
            return Err(SealedArtifactError::InvalidEnvelope(
                "AES-GCM nonce must be 12 bytes".to_string(),
            ));
        }
        let ciphertext = BASE64_STANDARD
            .decode(&envelope.ciphertext_base64)
            .map_err(|_| {
                SealedArtifactError::InvalidEnvelope("invalid ciphertext encoding".to_string())
            })?;
        let associated_data = serde_json_canonicalizer::to_vec(&envelope.header)?;
        let key = self.keys.resolve_key(&envelope.header.key_id)?;
        let cipher = Aes256Gcm::new_from_slice(key.material.as_ref())
            .map_err(|_| SealedArtifactError::Cryptography("invalid AES key".to_string()))?;
        let plaintext = Zeroizing::new(
            cipher
                .decrypt(
                    Nonce::from_slice(&nonce_bytes),
                    Payload {
                        msg: &ciphertext,
                        aad: &associated_data,
                    },
                )
                .map_err(|_| {
                    SealedArtifactError::InvalidEnvelope(
                        "authentication tag verification failed".to_string(),
                    )
                })?,
        );
        if plaintext.len() < PLAINTEXT_DIGEST_BYTES {
            return Err(SealedArtifactError::InvalidEnvelope(
                "decrypted payload is truncated".to_string(),
            ));
        }
        let (expected_digest, private_jcs) = plaintext.split_at(PLAINTEXT_DIGEST_BYTES);
        let actual_digest = Sha256::digest(private_jcs);
        if actual_digest.as_slice() != expected_digest {
            return Err(SealedArtifactError::InvalidEnvelope(
                "private plaintext digest mismatch".to_string(),
            ));
        }
        let mut private_value: Value = serde_json::from_slice(private_jcs)?;
        let canonical_result = serde_json_canonicalizer::to_vec(&private_value);
        zeroize_json_value(&mut private_value);
        let canonical_private = Zeroizing::new(canonical_result?);
        if canonical_private.as_slice() != private_jcs {
            return Err(SealedArtifactError::InvalidEnvelope(
                "private payload is not canonical RFC 8785 JSON".to_string(),
            ));
        }
        Ok(Zeroizing::new(private_jcs.to_vec()))
    }
}

/// Parsed private JSON that recursively scrubs all strings and object keys on
/// every return path. It is intentionally not clonable and exposes only a
/// borrowed value to pure validators.
struct ScrubbedJsonValue {
    value: Value,
}

impl ScrubbedJsonValue {
    fn parse_canonical(private_jcs: &[u8]) -> Result<Self, SealedArtifactError> {
        let value = serde_json::from_slice(private_jcs).map_err(|error| {
            invalid_artifact(&format!("exact runtime private JCS is invalid: {error}"))
        })?;
        let value = Self { value };
        let canonical = Zeroizing::new(serde_json_canonicalizer::to_vec(value.as_value())?);
        if canonical.as_slice() != private_jcs {
            return Err(invalid_artifact(
                "exact runtime private evidence is not canonical RFC 8785 JSON",
            ));
        }
        Ok(value)
    }

    fn as_value(&self) -> &Value {
        &self.value
    }
}

impl Drop for ScrubbedJsonValue {
    fn drop(&mut self) {
        zeroize_json_value(&mut self.value);
    }
}

/// Opaque reconstructed exact evidence retained only by an authorized replay
/// report. It owns the parsed canonical JSON and recursively scrubs it on drop.
///
/// This type intentionally does not implement `Clone` or `Serialize`. The
/// only payload access is a higher-ranked closure, which cannot return a
/// borrowed reference to the private value.
pub(crate) struct ReconstructedPrivateFragment {
    binding: ExactRuntimeEvidenceBinding,
    private: ScrubbedJsonValue,
}

impl ReconstructedPrivateFragment {
    fn new(binding: ExactRuntimeEvidenceBinding, private: ScrubbedJsonValue) -> Self {
        Self { binding, private }
    }

    pub(crate) fn binding(&self) -> &ExactRuntimeEvidenceBinding {
        &self.binding
    }

    pub(crate) fn with_value<R>(&self, read: impl for<'value> FnOnce(&'value Value) -> R) -> R {
        read(self.private.as_value())
    }
}

impl std::fmt::Debug for ReconstructedPrivateFragment {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ReconstructedPrivateFragment")
            .field("binding", &self.binding)
            .field("private", &"<redacted>")
            .finish()
    }
}

fn zeroize_json_value(value: &mut Value) {
    match value {
        Value::String(text) => text.zeroize(),
        Value::Array(items) => {
            for item in items {
                zeroize_json_value(item);
            }
        }
        Value::Object(fields) => {
            for (mut key, mut field) in std::mem::take(fields) {
                key.zeroize();
                zeroize_json_value(&mut field);
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => {}
    }
}

fn validate_header_matches_reference(
    header: &EnvelopeHeader,
    reference: &SealedArtifactRef,
) -> Result<(), SealedArtifactError> {
    if header.schema_version != reference.schema_version
        || header.encryption_algorithm != reference.encryption_algorithm
        || header.key_id != reference.key_id
        || header.canonicalization_algorithm != reference.canonicalization_algorithm
        || header.canonicalization_version != reference.canonicalization_version
        || header.redactor_version != reference.redactor_version
    {
        return Err(SealedArtifactError::InvalidEnvelope(
            "envelope header does not match durable reference".to_string(),
        ));
    }
    Ok(())
}

fn validate_reference(reference: &SealedArtifactRef) -> Result<(), SealedArtifactError> {
    validate_digest(&reference.envelope_digest)?;
    validate_identifier("sealing key id", &reference.key_id)?;
    validate_identifier("redactor version", &reference.redactor_version)?;
    if reference.schema_version != SEALED_ARTIFACT_SCHEMA_VERSION {
        return Err(SealedArtifactError::InvalidEnvelope(format!(
            "unsupported artifact schema version {}",
            reference.schema_version
        )));
    }
    if reference.encryption_algorithm != ARTIFACT_ENCRYPTION_ALGORITHM {
        return Err(SealedArtifactError::InvalidEnvelope(format!(
            "unsupported encryption algorithm {}",
            reference.encryption_algorithm
        )));
    }
    if reference.canonicalization_algorithm != ARTIFACT_CANONICALIZATION
        || reference.canonicalization_version != ARTIFACT_CANONICALIZATION_VERSION
    {
        return Err(SealedArtifactError::InvalidEnvelope(format!(
            "unsupported canonicalization {} v{}",
            reference.canonicalization_algorithm, reference.canonicalization_version
        )));
    }
    Ok(())
}

fn validate_identifier(label: &str, value: &str) -> Result<(), SealedArtifactError> {
    if value.is_empty()
        || value.len() > MAX_ARTIFACT_IDENTIFIER_BYTES
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(SealedArtifactError::InvalidMetadata(format!(
            "{label} must be 1..=128 safe ASCII characters"
        )));
    }
    Ok(())
}

fn validate_digest(digest: &str) -> Result<(), SealedArtifactError> {
    if digest.len() != 64
        || !digest
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(SealedArtifactError::InvalidMetadata(
            "artifact digest must be 64 lowercase hexadecimal characters".to_string(),
        ));
    }
    Ok(())
}

/// Private, flat content-addressed storage. The caller provisions the root;
/// opening it fails unless it is an owner-only, non-link directory. Writers
/// share a cross-process store lock from temp creation through verified atomic
/// rename and directory fsync; recovery takes that lock exclusively.
pub struct FilesystemSealedArtifactStore {
    root: PathBuf,
    store_id: String,
    #[cfg(unix)]
    root_device: u64,
    #[cfg(unix)]
    root_inode: u64,
}

/// A cross-process shared CAS operation guard. Hold this before querying the
/// ledger for an exact read; never acquire the ledger transaction first.
pub struct FilesystemArtifactSharedOperationGuard<'a> {
    store: &'a FilesystemSealedArtifactStore,
    _store_lock: AdvisoryFileLock,
}

/// A cross-process exclusive CAS operation guard. Publication, retirement,
/// unlink, and the following ledger transition use this same guard so no
/// operation recursively reacquires the advisory lock.
pub struct FilesystemArtifactExclusiveOperationGuard<'a> {
    store: &'a FilesystemSealedArtifactStore,
    _store_lock: AdvisoryFileLock,
}

/// The outcome of a retention pass performed while the CAS store is exclusively
/// locked. Only artifacts absent from the caller's current durable reference set
/// are eligible for deletion.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ArtifactStoreMaintenance {
    pub stale_temporary_files_removed: usize,
    pub stale_unreferenced_files_removed: usize,
}

impl fmt::Debug for FilesystemSealedArtifactStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FilesystemSealedArtifactStore")
            .field("root", &self.root)
            .finish()
    }
}

impl FilesystemSealedArtifactStore {
    pub fn open(root: impl AsRef<Path>) -> Result<Self, SealedArtifactError> {
        let root = root.as_ref().to_path_buf();
        let before = validate_private_store_root(&root)?;
        let recovery_lock = acquire_store_lock(&root, StoreLockMode::Exclusive)?;
        cleanup_stale_temporary_artifacts(&root)?;
        let store_id = load_or_create_store_id(&root)?;
        let metadata = validate_private_store_root(&root)?;
        #[cfg(unix)]
        if unix_device(&before) != unix_device(&metadata)
            || unix_inode(&before) != unix_inode(&metadata)
        {
            return Err(SealedArtifactError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "artifact store root identity changed during recovery",
            )));
        }
        drop(recovery_lock);
        Ok(Self {
            root,
            store_id,
            #[cfg(unix)]
            root_device: unix_device(&metadata),
            #[cfg(unix)]
            root_inode: unix_inode(&metadata),
        })
    }

    /// Root used for the protected, private CAS. Callers may use this only for
    /// filesystem capacity checks; mutation remains exclusively through this
    /// store.
    pub fn root(&self) -> &Path {
        &self.root
    }
    fn credential_namespace(&self) -> String {
        self.store_id.clone()
    }

    fn verify_root(&self) -> Result<(), SealedArtifactError> {
        let metadata = validate_private_store_root(&self.root)?;
        #[cfg(unix)]
        if unix_device(&metadata) != self.root_device || unix_inode(&metadata) != self.root_inode {
            return Err(SealedArtifactError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "artifact store root identity changed",
            )));
        }
        Ok(())
    }

    fn path(&self, digest: &str) -> Result<PathBuf, SealedArtifactError> {
        validate_digest(digest)?;
        Ok(self.root.join(format!("{digest}.sealed")))
    }

    /// Caller holds the exclusive store lock. Any existing sealed envelope,
    /// including a malformed one, makes missing credentials fail closed rather
    /// than allowing a new key pair to strand existing exact replay data.
    fn contains_sealed_artifact(&self) -> Result<bool, SealedArtifactError> {
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            let Some(digest) = name.strip_suffix(".sealed") else {
                continue;
            };
            validate_digest(digest).map_err(marker_key_unavailable)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Only the store lock and store identifier exist in a freshly initialized
    /// CAS. A marker is state, not emptiness: a caller must validate its
    /// lineage before extending a marked store with another key configuration.
    fn empty_for_key_provisioning(&self) -> Result<bool, SealedArtifactError> {
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                return Err(SealedArtifactError::KeyUnavailable(
                    "artifact store contains a non-UTF-8 entry during key provisioning".to_string(),
                ));
            };
            if name == STORE_LOCK_FILE
                || name == CAPACITY_RESERVATION_LOCK_FILE
                || name == STORE_ID_FILE
            {
                continue;
            }
            return Ok(false);
        }
        Ok(true)
    }

    fn temporary_path(&self) -> PathBuf {
        self.root
            .join(format!(".artifact-{}.tmp", uuid::Uuid::new_v4()))
    }

    fn shared_operation_lock(&self) -> Result<AdvisoryFileLock, SealedArtifactError> {
        acquire_store_lock(&self.root, StoreLockMode::Shared)
    }

    fn exclusive_operation_lock(&self) -> Result<AdvisoryFileLock, SealedArtifactError> {
        acquire_store_lock(&self.root, StoreLockMode::Exclusive)
    }

    /// Acquire the shared half of the explicit store→ledger operation order.
    pub fn acquire_shared_operation_guard(
        &self,
    ) -> Result<FilesystemArtifactSharedOperationGuard<'_>, SealedArtifactError> {
        self.verify_root()?;
        Ok(FilesystemArtifactSharedOperationGuard {
            store: self,
            _store_lock: self.shared_operation_lock()?,
        })
    }

    /// Acquire the exclusive half of the explicit store→ledger operation
    /// order. Callers must not invoke ordinary store read/remove methods while
    /// this guard is alive, because those methods acquire a second advisory
    /// lock.
    pub fn acquire_exclusive_operation_guard(
        &self,
    ) -> Result<FilesystemArtifactExclusiveOperationGuard<'_>, SealedArtifactError> {
        self.verify_root()?;
        Ok(FilesystemArtifactExclusiveOperationGuard {
            store: self,
            _store_lock: self.exclusive_operation_lock()?,
        })
    }

    fn read_while_locked(&self, envelope_digest: &str) -> Result<Vec<u8>, SealedArtifactError> {
        let path = self.path(envelope_digest)?;
        let mut file = open_private_file(&path)?;
        let length = usize::try_from(file.metadata()?.len()).unwrap_or(usize::MAX);
        if length > MAX_ARTIFACT_BYTES {
            return Err(SealedArtifactError::TooLarge {
                actual: length,
                limit: MAX_ARTIFACT_BYTES,
            });
        }
        let mut bytes = Vec::with_capacity(length);
        file.read_to_end(&mut bytes)?;
        let actual = hex::encode(Sha256::digest(&bytes));
        if actual != envelope_digest {
            return Err(SealedArtifactError::CasConflict(
                envelope_digest.to_string(),
            ));
        }
        Ok(bytes)
    }

    fn remove_while_locked(&self, envelope_digest: &str) -> Result<(), SealedArtifactError> {
        let path = self.path(envelope_digest)?;
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(SealedArtifactError::Io(error)),
        }
        verify_existing_cas_file(&path, envelope_digest)?;
        fs::remove_file(path)?;
        sync_directory(&self.root)?;
        Ok(())
    }

    /// Remove crash leftovers and expired envelope files only while no store
    /// operation can be active. The exclusive store lock is acquired before
    /// reading the authoritative ledger reference set, preserving the same
    /// store-then-ledger lock order as publication and commit.
    pub fn cleanup_stale_from_ledger(
        &self,
        stale_temporary_before: SystemTime,
        stale_artifact_before: SystemTime,
        ledger: &crate::run_ledger::RunLedger,
    ) -> Result<ArtifactStoreMaintenance, SealedArtifactError> {
        self.cleanup_stale_with_reference_loader(
            stale_temporary_before,
            stale_artifact_before,
            || {
                ledger
                    .referenced_sealed_envelope_digests()
                    .map_err(|error| SealedArtifactError::ReferenceSnapshot(error.to_string()))
                    .map(|digests| {
                        digests
                            .into_iter()
                            .map(|digest| digest.as_str().to_string())
                            .collect()
                    })
            },
        )
    }

    fn cleanup_stale_with_reference_loader(
        &self,
        stale_temporary_before: SystemTime,
        stale_artifact_before: SystemTime,
        load_referenced_digests: impl FnOnce() -> Result<HashSet<String>, SealedArtifactError>,
    ) -> Result<ArtifactStoreMaintenance, SealedArtifactError> {
        self.verify_root()?;
        let _store_lock = self.exclusive_operation_lock()?;
        let referenced_digests = load_referenced_digests()?;
        let mut maintenance = ArtifactStoreMaintenance::default();
        let mut changed = false;

        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name == STORE_LOCK_FILE
                || name == CAPACITY_RESERVATION_LOCK_FILE
                || name == STORE_ID_FILE
                || is_key_provision_marker_filename(name)?
            {
                continue;
            }
            let path = entry.path();
            let modified = fs::symlink_metadata(&path)?.modified()?;

            if name.starts_with(".artifact-") {
                let Some(uuid) = name
                    .strip_prefix(".artifact-")
                    .and_then(|value| value.strip_suffix(".tmp"))
                else {
                    return Err(SealedArtifactError::Io(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "malformed artifact temporary entry",
                    )));
                };
                uuid::Uuid::parse_str(uuid).map_err(|_| {
                    SealedArtifactError::Io(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "malformed artifact temporary UUID",
                    ))
                })?;
                if modified > stale_temporary_before {
                    continue;
                }
                drop(open_private_file(&path)?);
                fs::remove_file(path)?;
                maintenance.stale_temporary_files_removed += 1;
                changed = true;
                continue;
            }

            let Some(digest) = name.strip_suffix(".sealed") else {
                continue;
            };
            validate_digest(digest)?;
            if modified > stale_artifact_before || referenced_digests.contains(digest) {
                continue;
            }
            verify_existing_cas_file(&path, digest)?;
            fs::remove_file(path)?;
            maintenance.stale_unreferenced_files_removed += 1;
            changed = true;
        }
        if changed {
            sync_directory(&self.root)?;
        }
        Ok(maintenance)
    }

    #[cfg(test)]
    fn corrupt_temporary_before_verification(
        &self,
        envelope_digest: &str,
        bytes: &[u8],
    ) -> Result<(), SealedArtifactError> {
        self.verify_root()?;
        let _store_lock = self.shared_operation_lock()?;
        let destination = self.path(envelope_digest)?;
        let temporary = self.temporary_path();
        let _cleanup = TemporaryArtifact::new(temporary.clone());
        let mut file = create_private_file(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        file.write_all(b"corrupt-after-fsync")?;
        file.sync_all()?;
        let verified_digest = digest_open_private_file(&mut file)?;
        if verified_digest != envelope_digest {
            return Err(SealedArtifactError::CasConflict(
                envelope_digest.to_string(),
            ));
        }
        atomic_rename_noreplace(&temporary, &destination)?;
        Err(SealedArtifactError::InvalidEnvelope(
            "corruption injection did not change digest".to_string(),
        ))
    }

    fn put_verified_while_locked(
        &self,
        envelope_digest: &str,
        bytes: &[u8],
        crash_at: Option<CasCrashPoint>,
    ) -> Result<(), SealedArtifactError> {
        #[cfg(not(test))]
        let _ = crash_at;
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(SealedArtifactError::TooLarge {
                actual: bytes.len(),
                limit: MAX_ARTIFACT_BYTES,
            });
        }
        self.verify_root()?;
        let caller_digest = hex::encode(Sha256::digest(bytes));
        if caller_digest != envelope_digest {
            return Err(SealedArtifactError::CasConflict(
                envelope_digest.to_string(),
            ));
        }
        let destination = self.path(envelope_digest)?;
        match fs::symlink_metadata(&destination) {
            Ok(_) => {
                verify_existing_cas_file(&destination, envelope_digest)?;
                // This also completes recovery if a prior process died after
                // rename but before syncing the directory entry.
                sync_directory(&self.root)?;
                return Ok(());
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(SealedArtifactError::Io(error)),
        }

        let temporary = self.temporary_path();
        let mut cleanup = TemporaryArtifact::new(temporary.clone());
        let mut file = create_private_file(&temporary)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        #[cfg(test)]
        maybe_inject_cas_crash(crash_at, CasCrashPoint::AfterTempFsync, &mut cleanup)?;

        // The digest used for publication is recomputed from the durable temp
        // descriptor, never trusted from the caller's in-memory buffer.
        let verified_digest = digest_open_private_file(&mut file)?;
        if verified_digest != envelope_digest {
            return Err(SealedArtifactError::CasConflict(
                envelope_digest.to_string(),
            ));
        }
        #[cfg(test)]
        maybe_inject_cas_crash(
            crash_at,
            CasCrashPoint::AfterDigestVerification,
            &mut cleanup,
        )?;

        match atomic_rename_noreplace(&temporary, &destination) {
            Ok(()) => {
                cleanup.disarm();
                #[cfg(test)]
                maybe_inject_cas_crash(crash_at, CasCrashPoint::AfterAtomicRename, &mut cleanup)?;
                verify_existing_cas_file(&destination, envelope_digest)?;
                sync_directory(&self.root).map_err(SealedArtifactError::Io)
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                verify_existing_cas_file(&destination, envelope_digest)?;
                sync_directory(&self.root).map_err(SealedArtifactError::Io)
            }
            Err(error) => Err(SealedArtifactError::Io(error)),
        }
    }

    #[cfg(test)]
    fn put_with_injected_crash(
        &self,
        envelope_digest: &str,
        bytes: &[u8],
        crash_at: CasCrashPoint,
    ) -> Result<(), SealedArtifactError> {
        let _store_lock = self.shared_operation_lock()?;

        self.put_verified_while_locked(envelope_digest, bytes, Some(crash_at))
    }
}

impl FilesystemArtifactSharedOperationGuard<'_> {
    /// Read immutable envelope bytes without recursively taking another store
    /// lock. The caller still owns authorization and decrypt verification.
    pub fn locked_read(&self, envelope_digest: &str) -> Result<Vec<u8>, SealedArtifactError> {
        self.store.read_while_locked(envelope_digest)
    }
}

impl FilesystemArtifactExclusiveOperationGuard<'_> {
    /// Publish verified immutable envelope bytes while retaining this exclusive
    /// operation guard through the subsequent ledger commit.
    pub fn publish_verified(
        &self,
        envelope_digest: &str,
        bytes: &[u8],
    ) -> Result<(), SealedArtifactError> {
        self.store
            .put_verified_while_locked(envelope_digest, bytes, None)
    }

    /// Read immutable envelope bytes without recursively acquiring a shared
    /// advisory lock.
    pub fn locked_read(&self, envelope_digest: &str) -> Result<Vec<u8>, SealedArtifactError> {
        self.store.read_while_locked(envelope_digest)
    }

    /// Unlink an already tombstoned object and directory-fsync while retaining
    /// this exclusive guard. The caller must commit the retired observation
    /// immediately after this succeeds.
    pub fn locked_remove(&self, envelope_digest: &str) -> Result<(), SealedArtifactError> {
        self.store.remove_while_locked(envelope_digest)
    }
}

impl SealedArtifactStore for FilesystemSealedArtifactStore {
    fn publish_for_commit(
        &self,
        envelope_digest: &str,
        bytes: &[u8],
    ) -> Result<Box<dyn ArtifactPublicationGuard>, SealedArtifactError> {
        let store_lock = self.shared_operation_lock()?;
        self.put_verified_while_locked(envelope_digest, bytes, None)?;
        Ok(Box::new(FilesystemArtifactPublicationGuard {
            _store_lock: store_lock,
        }))
    }

    fn read(&self, envelope_digest: &str) -> Result<Vec<u8>, SealedArtifactError> {
        self.verify_root()?;
        let _store_lock = self.shared_operation_lock()?;
        let path = self.path(envelope_digest)?;
        let mut file = open_private_file(&path)?;
        let length = usize::try_from(file.metadata()?.len()).unwrap_or(usize::MAX);
        if length > MAX_ARTIFACT_BYTES {
            return Err(SealedArtifactError::TooLarge {
                actual: length,
                limit: MAX_ARTIFACT_BYTES,
            });
        }
        let mut bytes = Vec::with_capacity(length);
        file.read_to_end(&mut bytes)?;
        let actual = hex::encode(Sha256::digest(&bytes));
        if actual != envelope_digest {
            return Err(SealedArtifactError::CasConflict(
                envelope_digest.to_string(),
            ));
        }
        Ok(bytes)
    }

    fn remove(&self, envelope_digest: &str) -> Result<(), SealedArtifactError> {
        self.verify_root()?;
        let _store_lock = self.exclusive_operation_lock()?;
        let path = self.path(envelope_digest)?;
        match fs::symlink_metadata(&path) {
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(SealedArtifactError::Io(error)),
        }
        verify_existing_cas_file(&path, envelope_digest)?;
        fs::remove_file(path)?;
        sync_directory(&self.root)?;
        Ok(())
    }
}

include!("sealed_artifact/storage_support.rs");
#[cfg(test)]
#[path = "sealed_artifact/tests.rs"]
mod tests;
