//! Immutable, complete exports for terminal session-run ledger records.
//!
//! The SQLite ledger remains authoritative. This module only serializes a
//! transactionally verified terminal snapshot and verifies the resulting
//! RFC 8785/JCS-v1 envelope offline.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(target_os = "macos")]
use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;

use hermes_core::execution_mode::ResolvedExecutionMode;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::deterministic_replay::{
    ReplayOutboxEntry, ReplayProjectionGap, ReplayProjectionGapKind, VerifiedRunReplayPlan,
};

use crate::run_ledger::{
    delivery_result_evidence_identity_for, delivery_result_exact_evidence_identity_for,
    genesis_hash, recompute_event_hash, verify_event_delivery_projection,
    verify_event_nonresumable_projection, AppendKey, ArtifactDegradationReason,
    ArtifactReferenceState, ArtifactStateReason, BranchId, CanonicalJson, DeliveryFailureReason,
    Destination, DownstreamIdempotencyKey, DurableDeliveryReceipt, EffectIntent, EventId,
    ExactReplayState, ExactReplayStatus, LedgerError, NonResumableEffectRecord,
    NonResumableEffectSnapshot, NonResumableEffectState, NonResumableIdempotencyKey, OutboxRecord,
    OutboxRowId, OutboxState, PrivateFingerprint, ReplayObligationFidelity,
    ReplayObligationSnapshot, ReplayOperationKind, ReplayOperationOutcome, ReplayOperationPhase,
    RunHeader, RunId, RunLedger, RunOutcome, RunOwnerId, RunStatus, SealedArtifactRef, SessionId,
    SessionRunEvent, SessionRunEventKind, Sha256Digest, TenantId, TerminalReason,
    CANONICALIZATION_ALGORITHM, CANONICALIZATION_VERSION, DELIVERY_RESULT_BINDING_SCHEMA,
    LEGACY_DELIVERY_RESULT_BINDING_SCHEMA, RUN_EVENT_BINDING_SCHEMA_VERSION,
    RUN_EXIT_BINDING_SCHEMA, RUN_START_BINDING_SCHEMA,
};

pub const RUN_EXPORT_SCHEMA: &str = "hermes.session-run.export";
/// v3 adds signed, ordered typed replay obligations to the terminal proof.
pub const RUN_EXPORT_SCHEMA_VERSION: u32 = 3;
const PREVIOUS_RUN_EXPORT_SCHEMA_VERSION: u32 = 2;
const LEGACY_RUN_EXPORT_SCHEMA_VERSION: u32 = 1;
pub const RUN_EXPORT_DIGEST_ALGORITHM: &str = "sha-256";
const EXPORT_FILE_SUFFIX: &str = ".run.json";
const LEGACY_SHORT_HASH_LENGTH: usize = 16;

/// A verified, immutable read snapshot returned only by the ledger.
///
/// This is crate-visible because the ledger constructs it under one SQLite
/// read transaction. It intentionally is not a public replacement for the
/// ledger's domain API.
#[derive(Debug, Clone)]
pub(crate) struct LedgerRunSnapshot {
    pub header: RunHeader,
    pub status: RunStatus,
    pub head_hash: String,
    pub events: Vec<SessionRunEvent>,
    pub manifests: Vec<LedgerManifestSnapshot>,
    pub outbox: Vec<LedgerOutboxSnapshot>,
    pub nonresumable_effects: Vec<NonResumableEffectSnapshot>,
    pub replay_obligations: Vec<ReplayObligationSnapshot>,
}

#[derive(Debug, Clone)]
pub(crate) struct LedgerManifestSnapshot {
    pub event_id: EventId,
    pub canonical_json: CanonicalJson,
    pub hash: String,
}

#[derive(Debug, Clone)]
pub(crate) struct LedgerOutboxSnapshot {
    pub record: OutboxRecord,
    pub manifest_order: u32,
}

#[derive(Debug, Error)]
pub enum RunExportError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),

    #[error("run {run_id} is not terminal and fully reconciled")]
    NonTerminalRun { run_id: String },

    #[error("run export publication conflicts with different existing content at {path}")]
    PublicationConflict { path: PathBuf },

    #[error("run export path is not owner-only and regular: {path}")]
    UnsafePath { path: PathBuf },

    #[error("run export I/O failed at {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("run export is not valid UTF-8: {0}")]
    InvalidUtf8(String),

    #[error("run export JSON is invalid: {0}")]
    InvalidJson(String),

    #[error("run export is not exact RFC 8785/JCS-v1 canonical JSON")]
    NonCanonicalJson,

    #[error("run export is structurally invalid: {0}")]
    InvalidExport(String),

    #[error("run export schema {schema:?} version {version} is unsupported")]
    UnsupportedSchema { schema: String, version: u32 },

    #[error("run export canonicalization {algorithm:?} version {version} is unsupported")]
    UnsupportedCanonicalization { algorithm: String, version: u8 },

    #[error("run export digest algorithm {0:?} is unsupported")]
    UnsupportedDigestAlgorithm(String),

    #[error("run export envelope digest does not match the complete envelope")]
    DigestMismatch,

    #[error("legacy v0 JSONL uses 16-hex lineage hashes and cannot be verified as v1")]
    LegacyV0NotVerifiable,

    #[error("run export lacks strict replay evidence: {0}")]
    IncompleteReplayEvidence(String),

    #[cfg(test)]
    #[error("test interruption before run export rename")]
    TestInterruptedBeforeRename,
}

pub type RunExportResult<T> = Result<T, RunExportError>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunExportReceipt {
    pub run_id: RunId,
    pub path: PathBuf,
    pub digest: String,
    pub published: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedRunExport {
    pub run_id: RunId,
    pub event_count: u64,
    pub outbox_count: u64,
    pub digest: String,
    /// Count of read-only v1 delivery-result events accepted under the legacy
    /// no-evidence compatibility rule.
    pub legacy_delivery_result_count: u64,
}

impl VerifiedRunExport {
    /// Current delivery evidence is complete only when verification did not
    /// rely on the read-only legacy v1 no-evidence compatibility rule.
    pub const fn evidence_complete(&self) -> bool {
        self.legacy_delivery_result_count == 0
    }

    /// Verification establishes JCS, envelope digest, event-chain, projection,
    /// and terminal invariants.
    pub const fn integrity_verified(&self) -> bool {
        true
    }

    /// Exports have no signer proof. Integrity must never be relabeled as
    /// exporter authentication.
    pub const fn authentication_verified(&self) -> bool {
        false
    }

    /// Successful export verification includes terminal reconciliation.
    pub const fn terminal_valid(&self) -> bool {
        true
    }
}
/// Explicit classification for callers that encounter a pre-ledger replay log.
/// A legacy v0 result is deliberately not proof of integrity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunExportClassification {
    VerifiedV1(VerifiedRunExport),
    LegacyV0Jsonl {
        record_count: u64,
        raw_sha256: String,
    },
    Unknown,
}

/// Export one complete terminal run to an owner-only immutable path.
///
/// `export_dir` is a dedicated directory. The destination is derived only from
/// the UUID run ID, so callers cannot select an arbitrary publication filename.
pub fn export_run(
    ledger: &RunLedger,
    export_dir: impl AsRef<Path>,
    run_id: RunId,
) -> RunExportResult<RunExportReceipt> {
    export_run_inner(ledger, export_dir.as_ref(), run_id, false)
}

/// Verify a v1 export at a filesystem path. Legacy JSONL must be explicitly
/// classified through [`classify_run_export`] and is never accepted here.
pub fn verify_run_export(path: impl AsRef<Path>) -> RunExportResult<VerifiedRunExport> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|source| RunExportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    verify_run_export_bytes(&bytes)
}

/// Verify an exact JCS-v1 export envelope, including every event-chain link,
/// manifest, outbox projection, run finalization invariant, and envelope
/// digest.
pub fn verify_run_export_bytes(bytes: &[u8]) -> RunExportResult<VerifiedRunExport> {
    Ok(verify_run_export_parse_bytes(bytes)?.summary)
}

/// Verify one canonical export and retain the exact parsed snapshot for
/// provider/tool-free reconstruction. The path is read exactly once; later
/// replay consumes this immutable plan rather than reopening it.
pub fn verify_run_export_replay_plan(
    path: impl AsRef<Path>,
) -> RunExportResult<VerifiedRunReplayPlan> {
    let path = path.as_ref();
    let bytes = fs::read(path).map_err(|source| RunExportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    verify_run_export_replay_plan_bytes(&bytes)
}

/// Build a strict replay plan from the same canonical parse accepted by export
/// verification. Legacy v1 compatibility evidence remains summary-readable but
/// is deliberately not replay-plan evidence.
pub fn verify_run_export_replay_plan_bytes(
    bytes: &[u8],
) -> RunExportResult<VerifiedRunReplayPlan> {
    let parsed = verify_run_export_parse_bytes(bytes)?;
    if parsed.schema_version != RUN_EXPORT_SCHEMA_VERSION {
        return Err(RunExportError::IncompleteReplayEvidence(
            "legacy export schema lacks durable exact-replay evidence".to_string(),
        ));
    }
    let exact_replay_state = parsed.exact_replay_state.ok_or_else(|| {
        RunExportError::IncompleteReplayEvidence(
            "terminal export lacks an exact-replay status".to_string(),
        )
    })?;
    VerifiedRunReplayPlan::from_verified_export(
        parsed.summary,
        bytes.to_vec(),
        parsed.events,
        parsed.replay_obligations,
        parsed.outbox,
        parsed.projection_gaps,
        parsed.session_id,
        parsed.tenant_id,
        parsed.owner_id,
        parsed.branch_id,
        parsed.terminal_reason,
        parsed.final_outcome,
        exact_replay_state,
    )
    .map_err(RunExportError::IncompleteReplayEvidence)
}

fn verify_run_export_parse_bytes(bytes: &[u8]) -> RunExportResult<VerifiedExportParse> {
    if let Ok(text) = std::str::from_utf8(bytes) {
        if legacy_v0_record_count(text).is_some() {
            return Err(RunExportError::LegacyV0NotVerifiable);
        }
    }
    let value = parse_canonical_document(bytes)?;
    verify_v1_parse(value)
}

/// Classify a document without treating old short-hash JSONL as v1 evidence.
pub fn classify_run_export(bytes: &[u8]) -> RunExportResult<RunExportClassification> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| RunExportError::InvalidUtf8(error.to_string()))?;
    if let Some(record_count) = legacy_v0_record_count(text) {
        return Ok(RunExportClassification::LegacyV0Jsonl {
            record_count,
            raw_sha256: sha256_hex(bytes),
        });
    }

    let value = parse_canonical_document(bytes)?;
    if value
        .as_object()
        .and_then(|object| object.get("schema"))
        .is_none()
    {
        return Ok(RunExportClassification::Unknown);
    }
    Ok(RunExportClassification::VerifiedV1(verify_v1_value(value)?))
}

#[cfg(test)]
fn export_run_interrupting_before_rename(
    ledger: &RunLedger,
    export_dir: &Path,
    run_id: RunId,
) -> RunExportResult<RunExportReceipt> {
    export_run_inner(ledger, export_dir, run_id, true)
}

fn export_run_inner(
    ledger: &RunLedger,
    export_dir: &Path,
    run_id: RunId,
    #[allow(unused_variables)] interrupt_before_rename: bool,
) -> RunExportResult<RunExportReceipt> {
    // `export_snapshot` is one validated SQLite read transaction. In
    // particular, it verifies chain/projection data before this module begins
    // serialization, and refuses a status/head watermark race.
    let snapshot = ledger.export_snapshot(run_id)?;
    ensure_terminal(&snapshot.status)?;
    if snapshot.header.run_id != run_id || snapshot.status.run_id != run_id {
        return Err(RunExportError::InvalidExport(
            "ledger snapshot run identity does not match export request".to_string(),
        ));
    }

    let bytes = canonical_export_bytes_from_snapshot(&snapshot)?;
    // Validate the exact document we will publish before touching the target.
    let verified = verify_run_export_bytes(&bytes)?;
    if verified.run_id != run_id {
        return Err(RunExportError::InvalidExport(
            "self-verified export receipt does not match its run snapshot".to_string(),
        ));
    }
    let digest = verified.digest;

    let path = export_path(export_dir, run_id);
    let published = publish_immutable(
        export_dir,
        &path,
        &bytes,
        #[cfg(test)]
        interrupt_before_rename,
    )?;
    return Ok(RunExportReceipt {
        run_id,
        path,
        digest,
        published,
    });
}

/// Rebuild the exact canonical export from one already-validated ledger
/// snapshot. This is crate-visible for replay binding only: it prevents a
/// self-rehashed, same-run export from being treated as runtime authority.
pub(crate) fn canonical_export_bytes_from_snapshot(
    snapshot: &LedgerRunSnapshot,
) -> RunExportResult<Vec<u8>> {
    ensure_terminal(&snapshot.status)?;
    if snapshot.header.run_id != snapshot.status.run_id {
        return Err(RunExportError::InvalidExport(
            "ledger snapshot header and status run identities differ".to_string(),
        ));
    }
    let unsigned = unsigned_from_snapshot(snapshot)?;
    let digest = digest_unsigned(&unsigned)?;
    let envelope = WireEnvelope {
        schema: unsigned.schema.to_string(),
        schema_version: unsigned.schema_version,
        canonicalization_algorithm: unsigned.canonicalization_algorithm.to_string(),
        canonicalization_version: unsigned.canonicalization_version,
        complete: unsigned.complete,
        source_watermark: unsigned.source_watermark,
        range: unsigned.range,
        total_event_count: unsigned.total_event_count,
        header: unsigned.header,
        status: unsigned.status,
        events: unsigned.events,
        manifests: unsigned.manifests,
        outbox: unsigned.outbox,
        nonresumable_effects: unsigned.nonresumable_effects,
        replay_obligations: Some(unsigned.replay_obligations),
        compatibility_aliases: unsigned.compatibility_aliases,
        digest: WireDigest {
            algorithm: RUN_EXPORT_DIGEST_ALGORITHM.to_string(),
            value: digest,
        },
    };
    canonical_bytes(&envelope)
}


fn unsigned_from_snapshot(snapshot: &LedgerRunSnapshot) -> RunExportResult<UnsignedEnvelope> {
    let header = WireHeader::from_header(&snapshot.header)?;
    let status = WireStatus::from_status(&snapshot.status);
    let events = snapshot
        .events
        .iter()
        .map(WireEvent::from_event)
        .collect::<RunExportResult<Vec<_>>>()?;
    let manifests = snapshot
        .manifests
        .iter()
        .map(WireManifest::from_snapshot)
        .collect::<RunExportResult<Vec<_>>>()?;
    let outbox = snapshot
        .outbox
        .iter()
        .map(WireOutbox::from_snapshot)
        .collect::<RunExportResult<Vec<_>>>()?;
    let nonresumable_effects = snapshot
        .nonresumable_effects
        .iter()
        .map(WireNonResumableEffect::from_snapshot)
        .collect::<Vec<_>>();
    let replay_obligations = snapshot
        .replay_obligations
        .iter()
        .map(WireReplayObligation::from_snapshot)
        .collect::<Vec<_>>();

    let event_count = u64::try_from(events.len())
        .map_err(|_| RunExportError::InvalidExport("event count exceeds u64".to_string()))?;
    let first_sequence = events.first().map(|event| event.sequence).unwrap_or(0);
    let last_sequence = events.last().map(|event| event.sequence).unwrap_or(0);
    let range = WireRange {
        first_sequence,
        last_sequence,
    };
    let compatibility_aliases = WireCompatibilityAliases {
        run_id: header.run_id.clone(),
        session_id: header.provenance.session_id.clone(),
        legacy_v0: "unverified".to_string(),
    };
    Ok(UnsignedEnvelope::from_complete_fields(
        true,
        WireSourceWatermark {
            head_hash: snapshot.head_hash.clone(),
            event_count,
            last_sequence,
        },
        range,
        event_count,
        header,
        status,
        events,
        manifests,
        outbox,
        nonresumable_effects,
        replay_obligations,
        compatibility_aliases,
    ))
}

fn verify_v1_value(value: Value) -> RunExportResult<VerifiedRunExport> {
    Ok(verify_v1_parse(value)?.summary)
}

/// One complete immutable result of the canonical verification parse. Keeping
/// this internal prevents a replay caller from combining a summary from one
/// parse with events or bytes from another.
struct VerifiedExportParse {
    summary: VerifiedRunExport,
    events: Vec<SessionRunEvent>,
    projection_gaps: Vec<ReplayProjectionGap>,
    outbox: Vec<ReplayOutboxEntry>,
    replay_obligations: Vec<ReplayObligationSnapshot>,
    session_id: SessionId,
    tenant_id: TenantId,
    owner_id: RunOwnerId,
    branch_id: BranchId,
    terminal_reason: TerminalReason,
    final_outcome: RunOutcome,
    schema_version: u32,
    exact_replay_state: Option<ExactReplayState>,
}

fn verify_v1_parse(value: Value) -> RunExportResult<VerifiedExportParse> {
    let envelope: WireEnvelope = serde_json::from_value(value.clone())
        .map_err(|error| RunExportError::InvalidExport(error.to_string()))?;
    // Classify immutable envelope discriminants before applying a
    // version-specific layout, so unsupported versions fail with the typed
    // compatibility error rather than an incidental shape error.
    validate_envelope_shape(&envelope)?;
    ensure_object_keys(&value, envelope.expected_keys().iter().copied(), "envelope")?;
    validate_wire_document_layout(&value)?;

    let mut unsigned_value = value;
    let object = unsigned_value
        .as_object_mut()
        .ok_or_else(|| RunExportError::InvalidExport("envelope must be an object".to_string()))?;
    let supplied_digest = object
        .remove("digest")
        .ok_or_else(|| RunExportError::InvalidExport("envelope digest is missing".to_string()))?;
    if !supplied_digest.is_object() {
        return Err(RunExportError::InvalidExport(
            "envelope digest must be an object".to_string(),
        ));
    }
    let actual_digest = sha256_hex(&canonical_value_bytes(&unsigned_value)?);
    if actual_digest != envelope.digest.value {
        return Err(RunExportError::DigestMismatch);
    }

    let header_run_id = parse_run_id(&envelope.header.run_id, "header run_id")?;
    validate_status(
        &envelope.status,
        header_run_id,
        envelope.schema_version,
    )?;
    validate_compatibility_aliases(&envelope.compatibility_aliases, &envelope.header)?;
    validate_range_and_watermark(&envelope, header_run_id)?;

    let events = parse_events(&envelope.events, header_run_id)?;
    let replay_obligations = envelope
        .replay_obligations
        .as_deref()
        .unwrap_or_default()
        .iter()
        .map(WireReplayObligation::to_snapshot)
        .collect::<RunExportResult<Vec<_>>>()?;
    let manifests = parse_manifests(&envelope.manifests, header_run_id)?;
    validate_manifest_order(&envelope.manifests, &events)?;
    let outbox = parse_outbox(&envelope.outbox, header_run_id)?;
    validate_outbox_order(&outbox, &events)?;
    let nonresumable_effects =
        parse_nonresumable_effects(&envelope.nonresumable_effects, header_run_id)?;
    validate_nonresumable_effect_order(&nonresumable_effects, &events)?;
    validate_event_chain_and_projections(
        &envelope,
        &events,
        &manifests,
        &outbox,
        &nonresumable_effects,
    )?;
    let projection_gaps = validate_replay_obligations(
        envelope.schema_version,
        &replay_obligations,
        &events,
        &outbox,
        &nonresumable_effects,
    )?;
    validate_exact_replay_proof(&envelope, &events, &replay_obligations)?;
    let legacy_delivery_result_count =
        validate_terminal_projection(&envelope.status, &events, &outbox, &nonresumable_effects)?;
    let terminal_reason = parse_terminal_reason(&envelope.status.exit_terminal_reason)?;
    let final_outcome = parse_run_outcome(&envelope.status.final_outcome, "final_outcome")?;
    let exact_replay_state = envelope
        .status
        .exact_replay
        .as_ref()
        .map(|status| ExactReplayState::from_db(&status.state))
        .transpose()?;
    let tenant_id = TenantId::new(envelope.header.provenance.tenant_id.clone())?;
    let session_id = SessionId::new(envelope.header.provenance.session_id.clone())?;
    let owner_id = RunOwnerId::new(envelope.header.owner_id.clone())?;
    let branch_id = BranchId::new(envelope.header.provenance.branch_id.clone())?;
    let summary = VerifiedRunExport {
        run_id: header_run_id,
        event_count: envelope.total_event_count,
        outbox_count: u64::try_from(outbox.len())
            .map_err(|_| RunExportError::InvalidExport("outbox count exceeds u64".to_string()))?,
        digest: envelope.digest.value,
        legacy_delivery_result_count,
    };

    Ok(VerifiedExportParse {
        summary,
        session_id,
        projection_gaps,
        events,
        outbox: outbox
            .iter()
            .map(|entry| ReplayOutboxEntry::from_outbox_record(&entry.record, entry.manifest_order))
            .collect(),
        replay_obligations,
        terminal_reason,
        final_outcome,
        schema_version: envelope.schema_version,
        tenant_id,
        owner_id,
        branch_id,
        exact_replay_state,
    })
}

fn validate_envelope_shape(envelope: &WireEnvelope) -> RunExportResult<()> {
    if envelope.schema != RUN_EXPORT_SCHEMA
        || !matches!(
            envelope.schema_version,
            LEGACY_RUN_EXPORT_SCHEMA_VERSION
                | PREVIOUS_RUN_EXPORT_SCHEMA_VERSION
                | RUN_EXPORT_SCHEMA_VERSION
        )
    {
        return Err(RunExportError::UnsupportedSchema {
            schema: envelope.schema.clone(),
            version: envelope.schema_version,
        });
    }
    match (
        envelope.schema_version == RUN_EXPORT_SCHEMA_VERSION,
        envelope.replay_obligations.is_some(),
    ) {
        (true, true) | (false, false) => {}
        (true, false) => {
            return Err(RunExportError::IncompleteReplayEvidence(
                "v3 export lacks typed replay obligations".to_string(),
            ));
        }
        (false, true) => {
            return Err(RunExportError::InvalidExport(
                "legacy export carries unsupported replay obligations".to_string(),
            ));
        }
    }
    if envelope.canonicalization_algorithm != CANONICALIZATION_ALGORITHM
        || envelope.canonicalization_version != CANONICALIZATION_VERSION
    {
        return Err(RunExportError::UnsupportedCanonicalization {
            algorithm: envelope.canonicalization_algorithm.clone(),
            version: envelope.canonicalization_version,
        });
    }
    if envelope.digest.algorithm != RUN_EXPORT_DIGEST_ALGORITHM {
        return Err(RunExportError::UnsupportedDigestAlgorithm(
            envelope.digest.algorithm.clone(),
        ));
    }
    validate_sha256(&envelope.digest.value, "envelope digest")?;
    if !envelope.complete {
        return Err(RunExportError::InvalidExport(
            "complete must be true for a terminal export".to_string(),
        ));
    }
    Ok(())
}

fn validate_header(header: &WireHeader, run_id: RunId) -> RunExportResult<()> {
    parse_event_id(&header.start_event_id, "header start_event_id")?;
    crate::run_ledger::RunOwnerId::new(header.owner_id.clone())?;
    if header.started_at_ms < 0 {
        return Err(RunExportError::InvalidExport(
            "header started_at_ms must be non-negative".to_string(),
        ));
    }
    validate_resolved_mode(&header.execution_mode)?;
    header.provenance.validate()?;
    if let Some(parent) = &header.provenance.parent_run_id {
        let parent = parse_run_id(parent, "header parent_run_id")?;
        if parent == run_id {
            return Err(RunExportError::InvalidExport(
                "run cannot parent itself".to_string(),
            ));
        }
    }
    Ok(())
}
fn validate_run_start_binding(header: &WireHeader, event: &SessionRunEvent) -> RunExportResult<()> {
    let binding = event.public_payload.as_value()?;
    ensure_object_keys(
        &binding,
        [
            "schema",
            "schema_version",
            "execution_mode",
            "owner_id",
            "started_at_ms",
            "payload",
        ],
        "run-start binding",
    )?;
    let object = binding.as_object().ok_or_else(|| {
        RunExportError::InvalidExport("run-start binding must be an object".to_string())
    })?;
    let expected_mode = serde_json::to_value(header.execution_mode)
        .map_err(|error| RunExportError::InvalidExport(error.to_string()))?;
    if object.get("schema").and_then(Value::as_str) != Some(RUN_START_BINDING_SCHEMA)
        || object.get("schema_version").and_then(Value::as_u64)
            != Some(u64::from(RUN_EVENT_BINDING_SCHEMA_VERSION))
        || object.get("execution_mode") != Some(&expected_mode)
        || object.get("owner_id").and_then(Value::as_str) != Some(header.owner_id.as_str())
        || object.get("started_at_ms").and_then(Value::as_i64) != Some(header.started_at_ms)
        || event.created_at_ms != header.started_at_ms
    {
        return Err(RunExportError::InvalidExport(
            "run-start event does not bind the immutable run header".to_string(),
        ));
    }
    Ok(())
}

fn validate_run_exit_binding(status: &WireStatus, event: &SessionRunEvent) -> RunExportResult<()> {
    let binding = event.public_payload.as_value()?;
    ensure_object_keys(
        &binding,
        ["schema", "schema_version", "terminal_reason", "payload"],
        "run-exit binding",
    )?;
    let object = binding.as_object().ok_or_else(|| {
        RunExportError::InvalidExport("run-exit binding must be an object".to_string())
    })?;
    if event.event_id.to_string() != status.exit_event_id {
        return Err(RunExportError::InvalidExport(
            "run status exit_event_id does not identify its run-exit event".to_string(),
        ));
    }
    if object.get("schema").and_then(Value::as_str) != Some(RUN_EXIT_BINDING_SCHEMA)
        || object.get("schema_version").and_then(Value::as_u64)
            != Some(u64::from(RUN_EVENT_BINDING_SCHEMA_VERSION))
        || object.get("terminal_reason").and_then(Value::as_str)
            != Some(status.exit_terminal_reason.as_str())
    {
        return Err(RunExportError::InvalidExport(
            "run-exit event does not bind the terminal reason".to_string(),
        ));
    }
    Ok(())
}

fn validate_status(
    status: &WireStatus,
    run_id: RunId,
    schema_version: u32,
) -> RunExportResult<()> {
    if parse_run_id(&status.run_id, "status run_id")? != run_id {
        return Err(RunExportError::InvalidExport(
            "status run_id does not match header".to_string(),
        ));
    }
    if schema_version != LEGACY_RUN_EXPORT_SCHEMA_VERSION && status.exact_replay.is_none() {
        return Err(RunExportError::InvalidExport(
            "v2+ run status lacks durable exact replay state".to_string(),
        ));
    }
    parse_event_id(&status.exit_event_id, "status exit_event_id")?;
    let terminal_reason = parse_terminal_reason(&status.exit_terminal_reason)?;
    let candidate_outcome = parse_run_outcome(&status.candidate_outcome, "candidate_outcome")?;
    let final_outcome = parse_run_outcome(&status.final_outcome, "final_outcome")?;
    if status.pending_required_delivery {
        return Err(RunExportError::InvalidExport(
            "terminal export cannot have pending required delivery".to_string(),
        ));
    }
    let expected_final = if status.required_dead_lettered {
        RunOutcome::DeadLettered
    } else {
        terminal_reason.outcome()
    };
    if candidate_outcome != terminal_reason.outcome() || final_outcome != expected_final {
        return Err(RunExportError::InvalidExport(
            "run status outcomes are not reconciled with its terminal reason".to_string(),
        ));
    }
    Ok(())
}

fn validate_compatibility_aliases(
    aliases: &WireCompatibilityAliases,
    header: &WireHeader,
) -> RunExportResult<()> {
    if aliases.run_id != header.run_id || aliases.session_id != header.provenance.session_id {
        return Err(RunExportError::InvalidExport(
            "compatibility aliases do not match immutable header identity".to_string(),
        ));
    }
    if aliases.legacy_v0 != "unverified" {
        return Err(RunExportError::InvalidExport(
            "legacy compatibility alias must remain unverified".to_string(),
        ));
    }
    Ok(())
}

fn validate_range_and_watermark(envelope: &WireEnvelope, run_id: RunId) -> RunExportResult<()> {
    let event_count = u64::try_from(envelope.events.len())
        .map_err(|_| RunExportError::InvalidExport("event count exceeds u64".to_string()))?;
    if event_count == 0
        || envelope.total_event_count != event_count
        || envelope.source_watermark.event_count != event_count
        || envelope.range.first_sequence != 1
        || envelope.range.last_sequence != event_count
        || envelope.source_watermark.last_sequence != event_count
    {
        return Err(RunExportError::InvalidExport(
            "complete range or event-count watermark is truncated".to_string(),
        ));
    }
    validate_sha256(
        &envelope.source_watermark.head_hash,
        "source watermark head hash",
    )?;
    if parse_run_id(&envelope.header.run_id, "header run_id")? != run_id {
        return Err(RunExportError::InvalidExport(
            "header run_id changed while validating watermark".to_string(),
        ));
    }
    Ok(())
}

fn validate_event_chain_and_projections(
    envelope: &WireEnvelope,
    events: &[SessionRunEvent],
    manifests: &BTreeMap<EventId, CanonicalJson>,
    outbox: &[ParsedOutbox],
    nonresumable_effects: &[NonResumableEffectSnapshot],
) -> RunExportResult<()> {
    let header_run_id = parse_run_id(&envelope.header.run_id, "header run_id")?;
    let event_ids = events
        .iter()
        .map(|event| event.event_id)
        .collect::<BTreeSet<_>>();
    if event_ids.len() != events.len()
        || manifests.len() != events.len()
        || manifests
            .keys()
            .any(|event_id| !event_ids.contains(event_id))
        || outbox
            .iter()
            .any(|entry| !event_ids.contains(&entry.record.event_id))
        || nonresumable_effects
            .iter()
            .any(|entry| !event_ids.contains(&entry.record.event_id))
    {
        return Err(RunExportError::InvalidExport(
            "event, manifest, and outbox rows are not one complete run projection".to_string(),
        ));
    }
    let expected_header_provenance = envelope.header.provenance.canonical_json()?;
    let mut previous_hash = genesis_hash();
    let mut run_start_count = 0_usize;
    let mut run_exit_count = 0_usize;
    let mut local_one_shot_count = 0_usize;
    for (index, event) in events.iter().enumerate() {
        let expected_sequence = u64::try_from(index)
            .map_err(|_| RunExportError::InvalidExport("event index exceeds u64".to_string()))?
            .checked_add(1)
            .ok_or_else(|| RunExportError::InvalidExport("event sequence overflow".to_string()))?;
        if event.run_id != header_run_id || event.sequence != expected_sequence {
            return Err(RunExportError::InvalidExport(
                "event run identity or sequence is not contiguous".to_string(),
            ));
        }
        if event.previous_hash != previous_hash {
            return Err(RunExportError::InvalidExport(
                "event previous hash does not link to its predecessor".to_string(),
            ));
        }
        let recomputed = recompute_event_hash(event)?;
        if recomputed != event.event_hash {
            return Err(RunExportError::InvalidExport(
                "event body hash does not match the authoritative canonical hash".to_string(),
            ));
        }
        let manifest = manifests.get(&event.event_id).ok_or_else(|| {
            RunExportError::InvalidExport("event is missing its canonical manifest".to_string())
        })?;
        let event_outbox = outbox_for_event(outbox, event.event_id)?;
        verify_event_delivery_projection(event, manifest, &event.manifest_hash, &event_outbox)?;
        verify_event_nonresumable_projection(event, nonresumable_effects, events)?;
        if event.effect_intent == EffectIntent::LocalOneShotV1 {
            local_one_shot_count = local_one_shot_count.checked_add(1).ok_or_else(|| {
                RunExportError::InvalidExport("local one-shot count overflow".to_string())
            })?;
        }

        match event.kind {
            SessionRunEventKind::RunStart => {
                run_start_count = run_start_count.checked_add(1).ok_or_else(|| {
                    RunExportError::InvalidExport("run-start count overflow".to_string())
                })?;
                if event.sequence != 1
                    || event.event_id.to_string() != envelope.header.start_event_id
                    || event.run_provenance.as_ref() != Some(&expected_header_provenance)
                {
                    return Err(RunExportError::InvalidExport(
                        "run-start event is not bound to immutable header provenance".to_string(),
                    ));
                }
                validate_run_start_binding(&envelope.header, event)?;
            }
            SessionRunEventKind::RunExit => {
                run_exit_count = run_exit_count.checked_add(1).ok_or_else(|| {
                    RunExportError::InvalidExport("run-exit count overflow".to_string())
                })?;
                validate_run_exit_binding(&envelope.status, event)?;
            }
            SessionRunEventKind::Append
            | SessionRunEventKind::DeliveryResult
            | SessionRunEventKind::ManualAppendDegradedAudit
            | SessionRunEventKind::ArtifactStateAudit => {}
        }
        previous_hash = event.event_hash.clone();
    }

    if run_start_count != 1 || run_exit_count != 1 {
        return Err(RunExportError::InvalidExport(
            "complete terminal run must have exactly one start and one exit event".to_string(),
        ));
    }
    if local_one_shot_count != nonresumable_effects.len() {
        return Err(RunExportError::InvalidExport(
            "non-resumable rows do not cover local one-shot events exactly".to_string(),
        ));
    }
    if previous_hash != envelope.source_watermark.head_hash {
        return Err(RunExportError::InvalidExport(
            "source watermark head hash does not equal the complete event chain".to_string(),
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct ArtifactAuditProof {
    affected_event_id: EventId,
    artifact: SealedArtifactRef,
    from_state: ArtifactReferenceState,
    to_state: ArtifactReferenceState,
    reason: Option<ArtifactStateReason>,
    transition_sequence: u64,
    exact_replay_state: ExactReplayState,
}

fn validate_exact_replay_proof(
    envelope: &WireEnvelope,
    events: &[SessionRunEvent],
    replay_obligations: &[ReplayObligationSnapshot],
) -> RunExportResult<()> {
    let has_artifact_audit = events
        .iter()
        .any(|event| event.kind == SessionRunEventKind::ArtifactStateAudit);
    if envelope.schema_version == LEGACY_RUN_EXPORT_SCHEMA_VERSION {
        if has_artifact_audit {
            return Err(RunExportError::InvalidExport(
                "v1 export cannot prove an artifact-state audit without an exact replay latch"
                    .to_string(),
            ));
        }
        return Ok(());
    }

    let exact = envelope.status.exact_replay.as_ref().ok_or_else(|| {
        RunExportError::InvalidExport(
            "exact replay-aware run status lacks durable exact replay state".to_string(),
        )
    })?;
    let expected_state = ExactReplayState::from_db(&exact.state)?;
    let expected_reason = exact
        .reason
        .as_deref()
        .map(ArtifactStateReason::from_db)
        .transpose()?;
    if expected_reason != expected_exact_replay_reason(expected_state)
        || exact.exact_replay != expected_state.exact_replay()
    {
        return Err(RunExportError::InvalidExport(
            "exact replay status has an invalid closed state/reason pair".to_string(),
        ));
    }
    let has_pending_obligation = replay_obligations
        .iter()
        .any(|obligation| obligation.fidelity == ReplayObligationFidelity::Pending);
    let has_degraded_obligation = replay_obligations.iter().any(|obligation| {
        matches!(
            obligation.fidelity,
            ReplayObligationFidelity::MetadataOnly | ReplayObligationFidelity::Unavailable
        )
    });
    let obligations_are_exact = replay_obligations
        .iter()
        .all(|obligation| obligation.fidelity == ReplayObligationFidelity::ExactPrivate);
    let obligations_prove_state = if has_degraded_obligation {
        expected_state == ExactReplayState::UnavailableSanitized
    } else if has_pending_obligation {
        expected_state == ExactReplayState::Unverified
    } else {
        obligations_are_exact
            && matches!(
                expected_state,
                ExactReplayState::Available
                    | ExactReplayState::UnavailableMissing
                    | ExactReplayState::UnavailableCorrupt
                    | ExactReplayState::UnavailableKeyRevoked
                    | ExactReplayState::Retiring
                    | ExactReplayState::Retired
            )
    };
    if !obligations_prove_state {
        return Err(RunExportError::InvalidExport(
            "exact replay status does not match replay obligation fidelity".to_string(),
        ));
    }

    let event_by_id = events
        .iter()
        .map(|event| (event.event_id.to_string(), event))
        .collect::<BTreeMap<_, _>>();
    let mut reference_transitions =
        BTreeMap::<(String, String), (ArtifactReferenceState, u64)>::new();
    let mut audit_by_id = BTreeMap::<String, (ExactReplayState, i64)>::new();
    let mut observed_exact_state = None;

    for event in events
        .iter()
        .filter(|event| event.kind == SessionRunEventKind::ArtifactStateAudit)
    {
        let audit = parse_artifact_state_audit(event)?;
        let target = event_by_id
            .get(&audit.affected_event_id.to_string())
            .copied()
            .ok_or_else(|| {
                RunExportError::InvalidExport(
                    "artifact-state audit refers to an absent event".to_string(),
                )
            })?;
        if target.sequence >= event.sequence {
            return Err(RunExportError::InvalidExport(
                "artifact-state audit must follow its affected event".to_string(),
            ));
        }
        if target.sealed_artifact_ref.as_ref() != Some(&audit.artifact) {
            return Err(RunExportError::InvalidExport(
                "artifact-state audit artifact does not match its affected event".to_string(),
            ));
        }

        let reference_key = (
            audit.affected_event_id.to_string(),
            audit.artifact.envelope_digest().as_str().to_string(),
        );
        match reference_transitions.get(&reference_key).copied() {
            Some((state, sequence)) => {
                let expected_sequence = sequence.checked_add(1).ok_or_else(|| {
                    RunExportError::InvalidExport(
                        "artifact reference transition sequence overflow".to_string(),
                    )
                })?;
                if audit.from_state != state || audit.transition_sequence != expected_sequence {
                    return Err(RunExportError::InvalidExport(
                        "artifact-state audit does not continue the logical reference transition"
                            .to_string(),
                    ));
                }
            }
            None => {
                if audit.transition_sequence != 1
                    || !matches!(
                        audit.from_state,
                        ArtifactReferenceState::Unverified | ArtifactReferenceState::Available
                    )
                {
                    return Err(RunExportError::InvalidExport(
                        "artifact-state audit has no valid initial logical reference state"
                            .to_string(),
                    ));
                }
            }
        }
        if !artifact_reference_transition_allowed_for_export(audit.from_state, audit.to_state)
            || audit.reason != expected_artifact_reference_reason(audit.to_state)
        {
            return Err(RunExportError::InvalidExport(
                "artifact-state audit has an illegal logical reference transition".to_string(),
            ));
        }
        reference_transitions.insert(reference_key, (audit.to_state, audit.transition_sequence));

        if let Some(previous) = observed_exact_state {
            if previous != audit.exact_replay_state
                && !exact_replay_transition_allowed_for_export(previous, audit.exact_replay_state)
            {
                return Err(RunExportError::InvalidExport(
                    "artifact-state audit reverses the exact replay latch".to_string(),
                ));
            }
        }
        observed_exact_state = Some(audit.exact_replay_state);
        if audit_by_id
            .insert(
                event.event_id.to_string(),
                (audit.exact_replay_state, event.created_at_ms),
            )
            .is_some()
        {
            return Err(RunExportError::InvalidExport(
                "artifact-state audit event identity is not unique".to_string(),
            ));
        }
    }

    if let Some(observed) = observed_exact_state {
        if observed != expected_state {
            return Err(RunExportError::InvalidExport(
                "exact replay status does not match the final artifact-state audit".to_string(),
            ));
        }
    }
    match (
        exact.transition_sequence,
        exact.latched_at_ms,
        exact.latched_by_event_id.as_deref(),
    ) {
        (0, None, None)
            if matches!(
                expected_state,
                ExactReplayState::Unverified | ExactReplayState::Available
            ) => {}
        (0, _, _) => {
            return Err(RunExportError::InvalidExport(
                "initial exact replay status cannot have a latch event".to_string(),
            ));
        }
        (_, Some(latched_at_ms), Some(latched_by_event_id)) => {
            let latch_event = event_by_id.get(latched_by_event_id).copied().ok_or_else(|| {
                RunExportError::InvalidExport(
                    "exact replay latch refers to an absent event".to_string(),
                )
            })?;
            if latch_event.created_at_ms != latched_at_ms {
                return Err(RunExportError::InvalidExport(
                    "exact replay latch timestamp does not match its event".to_string(),
                ));
            }
            let same_event_typed_proof = replay_obligations.iter().any(|obligation| {
                let fidelity_proves_state = match expected_state {
                    ExactReplayState::Available => {
                        obligation.fidelity == ReplayObligationFidelity::ExactPrivate
                    }
                    ExactReplayState::Unverified => {
                        obligation.fidelity == ReplayObligationFidelity::Pending
                    }
                    ExactReplayState::UnavailableSanitized => matches!(
                        obligation.fidelity,
                        ReplayObligationFidelity::MetadataOnly
                            | ReplayObligationFidelity::Unavailable
                    ),
                    ExactReplayState::UnavailableMissing
                    | ExactReplayState::UnavailableCorrupt
                    | ExactReplayState::UnavailableKeyRevoked
                    | ExactReplayState::Retiring
                    | ExactReplayState::Retired => false,
                };
                fidelity_proves_state
                    && match (obligation.phase, obligation.fidelity) {
                        (ReplayOperationPhase::Request, _)
                        | (ReplayOperationPhase::Result, ReplayObligationFidelity::Pending) => {
                            obligation.source_event_id == latch_event.event_id
                        }
                        (ReplayOperationPhase::Result, _) => {
                            obligation.resolved_event_id == Some(latch_event.event_id)
                        }
                    }
            });
            let latch_is_proven = match latch_event.kind {
                SessionRunEventKind::ArtifactStateAudit => audit_by_id
                    .get(latched_by_event_id)
                    .is_some_and(|(state, at_ms)| {
                        *state == expected_state && *at_ms == latched_at_ms
                    }),
                SessionRunEventKind::ManualAppendDegradedAudit => {
                    expected_state == ExactReplayState::UnavailableSanitized
                        && latch_event.artifact_degradation_reason.is_some()
                        && same_event_typed_proof
                }
                SessionRunEventKind::RunStart
                | SessionRunEventKind::Append
                | SessionRunEventKind::DeliveryResult
                | SessionRunEventKind::RunExit => same_event_typed_proof,
            };
            if !latch_is_proven {
                return Err(RunExportError::InvalidExport(
                    "exact replay latch lacks same-event typed evidence".to_string(),
                ));
            }
        }
        _ => {
            return Err(RunExportError::InvalidExport(
                "transitioned exact replay status lacks a complete latch identity".to_string(),
            ));
        }
    }
    Ok(())
}

fn parse_artifact_state_audit(event: &SessionRunEvent) -> RunExportResult<ArtifactAuditProof> {
    if event.effect_intent != EffectIntent::None
        || event.append_key.is_some()
        || event.public_identity.is_some()
        || event.private_fingerprint.is_some()
        || event.sealed_artifact_ref.is_some()
        || event.run_provenance.is_some()
        || event.artifact_degradation_reason.is_some()
    {
        return Err(RunExportError::InvalidExport(
            "artifact-state audit carries effect, identity, or artifact data outside its proof"
                .to_string(),
        ));
    }
    let value = event.public_payload.as_value()?;
    ensure_object_keys(
        &value,
        [
            "schema",
            "schema_version",
            "audit_event_id",
            "affected_event_id",
            "artifact",
            "from_state",
            "to_state",
            "reason",
            "transition_sequence",
            "exact_replay_state",
            "exact_replay",
        ],
        "artifact-state audit",
    )?;
    let object = value.as_object().ok_or_else(|| {
        RunExportError::InvalidExport("artifact-state audit must be an object".to_string())
    })?;
    if required_string_field(object, "schema", "artifact-state audit")?
        != "hermes.artifact-state-audit/v1"
        || required_u64_field(object, "schema_version", "artifact-state audit")? != 1
    {
        return Err(RunExportError::InvalidExport(
            "artifact-state audit schema is unsupported".to_string(),
        ));
    }
    let audit_event_id = parse_event_id(
        required_string_field(object, "audit_event_id", "artifact-state audit")?,
        "artifact-state audit event id",
    )?;
    if audit_event_id != event.event_id {
        return Err(RunExportError::InvalidExport(
            "artifact-state audit id does not bind its event".to_string(),
        ));
    }
    let affected_event_id = parse_event_id(
        required_string_field(object, "affected_event_id", "artifact-state audit")?,
        "artifact-state audit affected event id",
    )?;
    let artifact_value = object
        .get("artifact")
        .ok_or_else(|| RunExportError::InvalidExport("artifact-state audit lacks artifact".to_string()))?;
    ensure_object_keys(
        artifact_value,
        [
            "envelope_digest",
            "key_id",
            "schema_version",
            "encryption_algorithm",
            "canonicalization_algorithm",
            "canonicalization_version",
            "redactor_version",
        ],
        "artifact-state audit reference",
    )?;
    let wire_reference: WireSealedArtifactRef = serde_json::from_value(artifact_value.clone())
        .map_err(|error| RunExportError::InvalidExport(error.to_string()))?;
    let artifact = wire_reference.to_ref()?;
    let from_state = ArtifactReferenceState::from_db(required_string_field(
        object,
        "from_state",
        "artifact-state audit",
    )?)?;
    let to_state = ArtifactReferenceState::from_db(required_string_field(
        object,
        "to_state",
        "artifact-state audit",
    )?)?;
    let reason = optional_string_field(object, "reason", "artifact-state audit")?
        .map(ArtifactStateReason::from_db)
        .transpose()?;
    let transition_sequence =
        required_u64_field(object, "transition_sequence", "artifact-state audit")?;
    let exact_replay_state = ExactReplayState::from_db(required_string_field(
        object,
        "exact_replay_state",
        "artifact-state audit",
    )?)?;
    if required_bool_field(object, "exact_replay", "artifact-state audit")?
        != exact_replay_state.exact_replay()
    {
        return Err(RunExportError::InvalidExport(
            "artifact-state audit exact replay flag conflicts with its state".to_string(),
        ));
    }
    Ok(ArtifactAuditProof {
        affected_event_id,
        artifact,
        from_state,
        to_state,
        reason,
        transition_sequence,
        exact_replay_state,
    })
}

fn required_string_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
    label: &str,
) -> RunExportResult<&'a str> {
    object
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| RunExportError::InvalidExport(format!("{label} {field} must be a string")))
}

fn optional_string_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
    label: &str,
) -> RunExportResult<Option<&'a str>> {
    match object.get(field) {
        Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        _ => Err(RunExportError::InvalidExport(format!(
            "{label} {field} must be null or a string"
        ))),
    }
}

fn required_u64_field(
    object: &serde_json::Map<String, Value>,
    field: &str,
    label: &str,
) -> RunExportResult<u64> {
    object
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| RunExportError::InvalidExport(format!("{label} {field} must be a u64")))
}

fn required_bool_field(
    object: &serde_json::Map<String, Value>,
    field: &str,
    label: &str,
) -> RunExportResult<bool> {
    object
        .get(field)
        .and_then(Value::as_bool)
        .ok_or_else(|| RunExportError::InvalidExport(format!("{label} {field} must be a bool")))
}

fn expected_artifact_reference_reason(
    state: ArtifactReferenceState,
) -> Option<ArtifactStateReason> {
    match state {
        ArtifactReferenceState::UnavailableMissing => Some(ArtifactStateReason::Missing),
        ArtifactReferenceState::UnavailableCorrupt => Some(ArtifactStateReason::Corrupt),
        ArtifactReferenceState::UnavailableKeyRevoked => Some(ArtifactStateReason::KeyRevoked),
        ArtifactReferenceState::Retiring | ArtifactReferenceState::Retired => {
            Some(ArtifactStateReason::RetentionDeleted)
        }
        ArtifactReferenceState::Unverified | ArtifactReferenceState::Available => None,
    }
}

fn expected_exact_replay_reason(state: ExactReplayState) -> Option<ArtifactStateReason> {
    match state {
        ExactReplayState::UnavailableMissing => Some(ArtifactStateReason::Missing),
        ExactReplayState::UnavailableCorrupt => Some(ArtifactStateReason::Corrupt),
        ExactReplayState::UnavailableKeyRevoked => Some(ArtifactStateReason::KeyRevoked),
        ExactReplayState::UnavailableSanitized => Some(ArtifactStateReason::Sanitized),
        ExactReplayState::Retiring | ExactReplayState::Retired => {
            Some(ArtifactStateReason::RetentionDeleted)
        }
        ExactReplayState::Unverified | ExactReplayState::Available => None,
    }
}

fn artifact_reference_transition_allowed_for_export(
    from: ArtifactReferenceState,
    to: ArtifactReferenceState,
) -> bool {
    match from {
        ArtifactReferenceState::Unverified => matches!(
            to,
            ArtifactReferenceState::Available
                | ArtifactReferenceState::UnavailableMissing
                | ArtifactReferenceState::UnavailableCorrupt
                | ArtifactReferenceState::UnavailableKeyRevoked
                | ArtifactReferenceState::Retiring
        ),
        ArtifactReferenceState::Available => matches!(
            to,
            ArtifactReferenceState::UnavailableMissing
                | ArtifactReferenceState::UnavailableCorrupt
                | ArtifactReferenceState::UnavailableKeyRevoked
                | ArtifactReferenceState::Retiring
        ),
        ArtifactReferenceState::UnavailableMissing
        | ArtifactReferenceState::UnavailableCorrupt
        | ArtifactReferenceState::UnavailableKeyRevoked => {
            to == ArtifactReferenceState::Retiring
        }
        ArtifactReferenceState::Retiring => to == ArtifactReferenceState::Retired,
        ArtifactReferenceState::Retired => false,
    }
}

fn exact_replay_transition_allowed_for_export(from: ExactReplayState, to: ExactReplayState) -> bool {
    match from {
        ExactReplayState::Unverified => to != ExactReplayState::Unverified,
        ExactReplayState::Available => {
            !matches!(to, ExactReplayState::Available | ExactReplayState::Unverified)
        }
        ExactReplayState::Retiring => to == ExactReplayState::Retired,
        ExactReplayState::UnavailableMissing
        | ExactReplayState::UnavailableCorrupt
        | ExactReplayState::UnavailableKeyRevoked
        | ExactReplayState::UnavailableSanitized
        | ExactReplayState::Retired => false,
    }
}

fn validate_replay_obligations(
    schema_version: u32,
    obligations: &[ReplayObligationSnapshot],
    events: &[SessionRunEvent],
    outbox: &[ParsedOutbox],
    nonresumable_effects: &[NonResumableEffectSnapshot],
) -> RunExportResult<Vec<ReplayProjectionGap>> {
    if schema_version != RUN_EXPORT_SCHEMA_VERSION {
        if obligations.is_empty() {
            return Ok(Vec::new());
        }
        return Err(RunExportError::InvalidExport(
            "legacy export contains typed replay obligations".to_string(),
        ));
    }
    if obligations.is_empty() {
        return Err(RunExportError::IncompleteReplayEvidence(
            "v3 export has no replay obligations".to_string(),
        ));
    }

    let events_by_id = events
        .iter()
        .map(|event| (event.event_id, event))
        .collect::<BTreeMap<_, _>>();
    let outbox_by_id = outbox
        .iter()
        .map(|entry| (entry.record.outbox_id, entry))
        .collect::<BTreeMap<_, _>>();
    let event_sequence_by_id = events
        .iter()
        .map(|event| (event.event_id, event.sequence))
        .collect::<BTreeMap<_, _>>();
    let mut identities = BTreeSet::new();
    let mut previous_order = None;
    let mut saw_run_start_request = false;

    for obligation in obligations {
        if !obligation.required {
            return Err(RunExportError::InvalidExport(
                "v3 replay obligation must be required".to_string(),
            ));
        }
        let source = events_by_id.get(&obligation.source_event_id).ok_or_else(|| {
            RunExportError::InvalidExport(
                "replay obligation source event is absent from export".to_string(),
            )
        })?;
        if source.sequence != obligation.source_event_sequence {
            return Err(RunExportError::InvalidExport(
                "replay obligation source sequence does not match event".to_string(),
            ));
        }
        validate_replay_operation_instance_binding(
            obligation,
            source,
            &outbox_by_id,
            &event_sequence_by_id,
        )?;
        let identity = (
            obligation.source_event_id,
            obligation.operation.as_str(),
            obligation.phase.as_str(),
            obligation.operation_instance_id.as_str(),
        );
        if !identities.insert(identity) {
            return Err(RunExportError::InvalidExport(
                "replay obligation identity is duplicated".to_string(),
            ));
        }
        let order = (
            obligation.source_event_sequence,
            obligation.operation.as_str(),
            obligation.phase.as_str(),
            obligation.operation_instance_id.as_str(),
        );
        if previous_order.is_some_and(|previous| previous > order) {
            return Err(RunExportError::InvalidExport(
                "replay obligations are not canonically ordered".to_string(),
            ));
        }
        previous_order = Some(order);

        if source.kind == SessionRunEventKind::RunStart
            && obligation.operation == ReplayOperationKind::Run
            && obligation.phase == ReplayOperationPhase::Request
        {
            saw_run_start_request = true;
        }

        match obligation.fidelity {
            ReplayObligationFidelity::Pending => {
                return Err(RunExportError::IncompleteReplayEvidence(
                    "terminal v3 export has a pending replay obligation".to_string(),
                ));
            }
            ReplayObligationFidelity::ExactPrivate => {
                let resolved = obligation.resolved_event_id.ok_or_else(|| {
                    RunExportError::IncompleteReplayEvidence(
                        "exact replay obligation lacks a resolution event".to_string(),
                    )
                })?;
                let resolved_event = events_by_id.get(&resolved).ok_or_else(|| {
                    RunExportError::InvalidExport(
                        "replay obligation resolution event is absent from export".to_string(),
                    )
                })?;
                if obligation.artifact.as_ref() != resolved_event.sealed_artifact_ref.as_ref() {
                    return Err(RunExportError::IncompleteReplayEvidence(
                        "exact replay obligation artifact is not its resolved event artifact"
                            .to_string(),
                    ));
                }
                if obligation.artifact.is_none() || obligation.reason.is_some() {
                    return Err(RunExportError::IncompleteReplayEvidence(
                        "exact replay obligation lacks closed private artifact evidence".to_string(),
                    ));
                }
            }
            ReplayObligationFidelity::MetadataOnly | ReplayObligationFidelity::Unavailable => {
                if obligation.resolved_event_id.is_none()
                    || obligation.reason != Some(ArtifactStateReason::Sanitized)
                {
                    return Err(RunExportError::IncompleteReplayEvidence(
                        "non-exact replay obligation lacks typed unavailable evidence".to_string(),
                    ));
                }
            }
        }

        match obligation.phase {
            ReplayOperationPhase::Request
                if obligation.outcome != ReplayOperationOutcome::NotApplicable =>
            {
                return Err(RunExportError::InvalidExport(
                    "replay request obligation has a non-request outcome".to_string(),
                ));
            }
            ReplayOperationPhase::Result
                if obligation.outcome == ReplayOperationOutcome::NotApplicable =>
            {
                return Err(RunExportError::InvalidExport(
                    "replay result obligation lacks a typed outcome".to_string(),
                ));
            }
            _ => {}
        }
    }

    if !saw_run_start_request {
        return Err(RunExportError::IncompleteReplayEvidence(
            "v3 replay obligations omit the run-start request".to_string(),
        ));
    }
    replay_projection_gaps(obligations, events, outbox, nonresumable_effects)
}

fn validate_replay_operation_instance_binding(
    obligation: &ReplayObligationSnapshot,
    source: &SessionRunEvent,
    outbox_by_id: &BTreeMap<OutboxRowId, &ParsedOutbox>,
    event_sequence_by_id: &BTreeMap<EventId, u64>,
) -> RunExportResult<()> {
    if obligation.operation != ReplayOperationKind::Delivery {
        if obligation.operation_instance_id != source.event_id.to_string() {
            return Err(RunExportError::InvalidExport(
                "non-delivery replay obligation instance does not equal its source event"
                    .to_string(),
            ));
        }
        return Ok(());
    }

    let outbox_id = parse_outbox_id(
        &obligation.operation_instance_id,
        "delivery replay operation instance",
    )?;
    match obligation.phase {
        ReplayOperationPhase::Request => {
            let entry = outbox_by_id.get(&outbox_id).ok_or_else(|| {
                RunExportError::InvalidExport(
                    "delivery request replay obligation references an unknown outbox row"
                        .to_string(),
                )
            })?;
            if entry.record.event_id != source.event_id
                || obligation.resolved_event_id != Some(source.event_id)
            {
                return Err(RunExportError::InvalidExport(
                    "delivery request replay obligation does not bind its source outbox row"
                        .to_string(),
                ));
            }
        }
        ReplayOperationPhase::Result => {
            if source.kind != SessionRunEventKind::DeliveryResult
                || obligation.resolved_event_id != Some(source.event_id)
            {
                return Err(RunExportError::InvalidExport(
                    "delivery result replay obligation does not bind its own result event"
                        .to_string(),
                ));
            }
            let (payload_outbox_id, _, _, _, evidence_fidelity) =
                validate_delivery_result_payload(source, outbox_by_id, event_sequence_by_id)?;
            if payload_outbox_id != outbox_id {
                return Err(RunExportError::InvalidExport(
                    "delivery result replay obligation instance does not match its payload"
                        .to_string(),
                ));
            }
            if obligation.fidelity != evidence_fidelity {
                return Err(RunExportError::IncompleteReplayEvidence(
                    "delivery result replay fidelity does not match its sealed evidence"
                        .to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn replay_projection_gaps(
    obligations: &[ReplayObligationSnapshot],
    events: &[SessionRunEvent],
    outbox: &[ParsedOutbox],
    nonresumable_effects: &[NonResumableEffectSnapshot],
) -> RunExportResult<Vec<ReplayProjectionGap>> {
    let events_by_id = events
        .iter()
        .map(|event| (event.event_id, event))
        .collect::<BTreeMap<_, _>>();
    let outbox_by_id = outbox
        .iter()
        .map(|entry| (entry.record.outbox_id, entry))
        .collect::<BTreeMap<_, _>>();
    let event_sequence_by_id = events
        .iter()
        .map(|event| (event.event_id, event.sequence))
        .collect::<BTreeMap<_, _>>();
    let mut gaps = Vec::new();

    for effect in nonresumable_effects {
        let source = events_by_id.get(&effect.record.event_id).copied().ok_or_else(|| {
            RunExportError::InvalidExport(
                "non-resumable replay projection source event is absent".to_string(),
            )
        })?;
        let instance = source.event_id.to_string();
        append_projection_gap_if_unmapped(
            &mut gaps,
            obligations
                .iter()
                .filter(|obligation| {
                    obligation.source_event_id == source.event_id
                        && obligation.operation == ReplayOperationKind::Tool
                        && obligation.phase == ReplayOperationPhase::Request
                        && obligation.operation_instance_id == instance
                })
                .count(),
            source,
            ReplayOperationKind::Tool,
            ReplayOperationPhase::Request,
            instance.clone(),
            Some(source.event_id),
            ReplayProjectionGapKind::NonresumableIntent,
        )?;
        let resolved = effect
            .record
            .result_event_id
            .map(|event_id| {
                events_by_id.get(&event_id).copied().ok_or_else(|| {
                    RunExportError::InvalidExport(
                        "non-resumable replay projection result event is absent".to_string(),
                    )
                })
            })
            .transpose()?;
        let result_matches = obligations
            .iter()
            .filter(|obligation| {
                obligation.source_event_id == source.event_id
                    && obligation.operation == ReplayOperationKind::Tool
                    && obligation.phase == ReplayOperationPhase::Result
                    && obligation.operation_instance_id == instance
                    && match resolved {
                        Some(event) => obligation.resolved_event_id == Some(event.event_id),
                        None => true,
                    }
            })
            .count();
        append_projection_gap_if_unmapped(
            &mut gaps,
            result_matches,
            resolved.unwrap_or(source),
            ReplayOperationKind::Tool,
            ReplayOperationPhase::Result,
            instance,
            resolved.map(|event| event.event_id),
            ReplayProjectionGapKind::NonresumableResult,
        )?;
    }

    for entry in outbox {
        let source = events_by_id.get(&entry.record.event_id).copied().ok_or_else(|| {
            RunExportError::InvalidExport(
                "outbox replay projection source event is absent".to_string(),
            )
        })?;
        let instance = entry.record.outbox_id.to_string();
        append_projection_gap_if_unmapped(
            &mut gaps,
            obligations
                .iter()
                .filter(|obligation| {
                    obligation.source_event_id == source.event_id
                        && obligation.operation == ReplayOperationKind::Delivery
                        && obligation.phase == ReplayOperationPhase::Request
                        && obligation.operation_instance_id == instance
                })
                .count(),
            source,
            ReplayOperationKind::Delivery,
            ReplayOperationPhase::Request,
            instance,
            Some(source.event_id),
            ReplayProjectionGapKind::DeliveryRequest,
        )?;
    }

    for event in events
        .iter()
        .filter(|event| event.kind == SessionRunEventKind::DeliveryResult)
    {
        // Re-parse through the same strict projection decoder used for terminal
        // verification; never infer an outbox attempt from human-readable text.
        let (outbox_id, _, _, _, _) =
            validate_delivery_result_payload(event, &outbox_by_id, &event_sequence_by_id)?;
        let instance = outbox_id.to_string();
        append_projection_gap_if_unmapped(
            &mut gaps,
            obligations
                .iter()
                .filter(|obligation| {
                    obligation.source_event_id == event.event_id
                        && obligation.source_event_sequence == event.sequence
                        && obligation.operation == ReplayOperationKind::Delivery
                        && obligation.phase == ReplayOperationPhase::Result
                        && obligation.operation_instance_id == instance
                        && obligation.resolved_event_id == Some(event.event_id)
                })
                .count(),
            event,
            ReplayOperationKind::Delivery,
            ReplayOperationPhase::Result,
            instance,
            Some(event.event_id),
            ReplayProjectionGapKind::DeliveryResult,
        )?;
    }
    gaps.sort_by_key(|gap| {
        (
            gap.source_event_sequence(),
            gap.operation().as_str(),
            gap.phase().as_str(),
            gap.operation_instance_id().to_owned(),
        )
    });
    Ok(gaps)
}

fn append_projection_gap_if_unmapped(
    gaps: &mut Vec<ReplayProjectionGap>,
    mapped_count: usize,
    source: &SessionRunEvent,
    operation: ReplayOperationKind,
    phase: ReplayOperationPhase,
    operation_instance_id: String,
    resolved_event_id: Option<EventId>,
    kind: ReplayProjectionGapKind,
) -> RunExportResult<()> {
    match mapped_count {
        0 => gaps.push(ReplayProjectionGap::new(
            source.event_id,
            source.sequence,
            operation,
            phase,
            operation_instance_id,
            resolved_event_id,
            kind,
        )),
        1 => {}
        _ => {
            return Err(RunExportError::InvalidExport(
                "runtime projection maps to multiple replay obligations".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_terminal_projection(
    status: &WireStatus,
    events: &[SessionRunEvent],
    outbox: &[ParsedOutbox],
    nonresumable_effects: &[NonResumableEffectSnapshot],
) -> RunExportResult<u64> {
    let required = outbox
        .iter()
        .filter(|entry| entry.record.required)
        .collect::<Vec<_>>();
    let required_dead_lettered = required
        .iter()
        .any(|entry| entry.record.state == OutboxState::DeadLettered);
    if status.required_dead_lettered != required_dead_lettered {
        return Err(RunExportError::InvalidExport(
            "required-dead-letter status does not match outbox projection".to_string(),
        ));
    }
    if !required_dead_lettered
        && required
            .iter()
            .any(|entry| entry.record.state != OutboxState::Acknowledged)
    {
        return Err(RunExportError::InvalidExport(
            "terminal successful projection has unresolved required outbox delivery".to_string(),
        ));
    }

    let outbox_by_id = outbox
        .iter()
        .map(|entry| (entry.record.outbox_id, entry))
        .collect::<BTreeMap<_, _>>();
    let event_sequence_by_id = events
        .iter()
        .map(|event| (event.event_id, event.sequence))
        .collect::<BTreeMap<_, _>>();
    let mut delivery_progress = BTreeMap::<OutboxRowId, (u32, Option<OutboxState>)>::new();
    let mut legacy_delivery_result_count = 0_u64;
    for event in events
        .iter()
        .filter(|event| event.kind == SessionRunEventKind::DeliveryResult)
    {
        let (outbox_id, state, attempt_count, legacy, _) =
            validate_delivery_result_payload(event, &outbox_by_id, &event_sequence_by_id)?;
        if legacy {
            legacy_delivery_result_count =
                legacy_delivery_result_count.checked_add(1).ok_or_else(|| {
                    RunExportError::InvalidExport(
                        "legacy delivery-result count exceeds u64".to_string(),
                    )
                })?;
        }
        let progress = delivery_progress.entry(outbox_id).or_insert((0, None));
        if progress.1.is_some() {
            return Err(RunExportError::InvalidExport(
                "delivery-result event follows a terminal result for the same outbox row"
                    .to_string(),
            ));
        }
        if attempt_count <= progress.0 {
            return Err(RunExportError::InvalidExport(
                "delivery-result attempts are not strictly increasing".to_string(),
            ));
        }
        progress.0 = attempt_count;
        if matches!(state, OutboxState::Acknowledged | OutboxState::DeadLettered) {
            progress.1 = Some(state);
        }
    }
    for entry in outbox.iter().filter(|entry| {
        matches!(
            entry.record.state,
            OutboxState::Acknowledged | OutboxState::DeadLettered
        )
    }) {
        let expected = Some((entry.record.attempt_count, Some(entry.record.state)));
        if delivery_progress.get(&entry.record.outbox_id).copied() != expected {
            return Err(RunExportError::InvalidExport(
                "terminal outbox row is missing coherent final delivery-result evidence"
                    .to_string(),
            ));
        }
    }
    let unknown_count = nonresumable_effects
        .iter()
        .filter(|effect| effect.record.state == NonResumableEffectState::UnknownNonResumable)
        .count();
    if nonresumable_effects.iter().any(|effect| {
        matches!(
            effect.record.state,
            NonResumableEffectState::Armed | NonResumableEffectState::Executing
        )
    }) {
        return Err(RunExportError::InvalidExport(
            "terminal export retains an armed or executing non-resumable effect".to_string(),
        ));
    }
    let terminal_reason = parse_terminal_reason(&status.exit_terminal_reason)?;
    if terminal_reason == TerminalReason::Completed
        && nonresumable_effects
            .iter()
            .any(|effect| effect.record.state != NonResumableEffectState::Completed)
    {
        return Err(RunExportError::InvalidExport(
            "completed run contains an incomplete non-resumable effect".to_string(),
        ));
    }
    if (terminal_reason == TerminalReason::EffectNonResumableUnknown) != (unknown_count > 0) {
        return Err(RunExportError::InvalidExport(
            "terminal reason and unknown non-resumable effects disagree".to_string(),
        ));
    }
    Ok(legacy_delivery_result_count)
}

fn validate_delivery_result_payload(
    event: &SessionRunEvent,
    outbox_by_id: &BTreeMap<OutboxRowId, &ParsedOutbox>,
    event_sequence_by_id: &BTreeMap<EventId, u64>,
) -> RunExportResult<(OutboxRowId, OutboxState, u32, bool, ReplayObligationFidelity)> {
    let value = serde_json::from_str::<Value>(event.public_payload.as_str())
        .map_err(|error| RunExportError::InvalidExport(error.to_string()))?;
    ensure_object_keys(
        &value,
        [
            "attempt_count",
            "delivery_error",
            "max_attempts",
            "outbox_id",
            "schema",
            "state",
        ],
        "delivery-result payload",
    )?;
    let payload: WireDeliveryResult = serde_json::from_value(value)
        .map_err(|error| RunExportError::InvalidExport(error.to_string()))?;
    let outbox_id = parse_outbox_id(&payload.outbox_id, "delivery-result outbox_id")?;
    let outbox = outbox_by_id.get(&outbox_id).ok_or_else(|| {
        RunExportError::InvalidExport(
            "delivery-result references an unknown outbox row".to_string(),
        )
    })?;
    let source_sequence = event_sequence_by_id
        .get(&outbox.record.event_id)
        .ok_or_else(|| {
            RunExportError::InvalidExport(
                "delivery-result source outbox event is missing".to_string(),
            )
        })?;
    if event.sequence <= *source_sequence {
        return Err(RunExportError::InvalidExport(
            "delivery-result event does not follow its source outbox event".to_string(),
        ));
    }
    let state = parse_outbox_state(&payload.state)?;
    let error = payload
        .delivery_error
        .as_deref()
        .map(parse_delivery_failure_reason)
        .transpose()?;
    let has_delivery_evidence = event.public_identity.is_some()
        && event.private_fingerprint.is_some()
        && event.sealed_artifact_ref.is_some();
    let (legacy, replay_fidelity) = match payload.schema.as_str() {
        LEGACY_DELIVERY_RESULT_BINDING_SCHEMA => {
            if has_delivery_evidence {
                return Err(RunExportError::InvalidExport(
                    "legacy v1 delivery-result must not carry sealed result evidence".to_string(),
                ));
            }
            (true, ReplayObligationFidelity::Unavailable)
        }
        DELIVERY_RESULT_BINDING_SCHEMA => {
            let replay_fidelity = if has_delivery_evidence {
                let outcome = match state {
                    OutboxState::Acknowledged => "returned",
                    OutboxState::Ready | OutboxState::DeadLettered => "handler_error",
                    OutboxState::Leased => {
                        return Err(RunExportError::InvalidExport(
                            "delivery-result cannot project a leased state".to_string(),
                        ));
                    }
                };
                let actual_identity = event
                    .public_identity
                    .as_ref()
                    .ok_or_else(|| {
                        RunExportError::InvalidExport(
                            "delivery-result evidence identity is missing".to_string(),
                        )
                    })?
                    .as_value()
                    .map_err(RunExportError::Ledger)?;
                let metadata_identity = delivery_result_evidence_identity_for(
                    &outbox_id,
                    outbox.record.destination,
                    &outbox.record.idempotency_key,
                    outcome,
                );
                let exact_identity = delivery_result_exact_evidence_identity_for(
                    &outbox_id,
                    outbox.record.destination,
                    &outbox.record.idempotency_key,
                    outcome,
                );
                let replay_fidelity = if actual_identity == exact_identity {
                    ReplayObligationFidelity::ExactPrivate
                } else if actual_identity == metadata_identity {
                    ReplayObligationFidelity::Unavailable
                } else {
                    return Err(RunExportError::InvalidExport(
                        "delivery-result evidence identity does not bind its outbox completion"
                            .to_string(),
                    ));
                };
                if event.sealed_artifact_ref.as_ref() == Some(&outbox.record.sealed_artifact_ref) {
                    return Err(RunExportError::InvalidExport(
                        "delivery-result evidence reuses the source sealed artifact".to_string(),
                    ));
                }
                replay_fidelity
            } else if matches!(
                error,
                Some(
                    DeliveryFailureReason::MissingRoute
                        | DeliveryFailureReason::InvalidPayload
                        | DeliveryFailureReason::AttemptsExhausted
                )
            ) {
                ReplayObligationFidelity::Unavailable
            } else {
                return Err(RunExportError::InvalidExport(
                    "v2 external delivery-result is missing sealed result evidence".to_string(),
                ));
            };
            (false, replay_fidelity)
        }
        _ => {
            return Err(RunExportError::InvalidExport(
                "delivery-result payload schema is unsupported".to_string(),
            ));
        }
    };
    if payload.max_attempts != outbox.record.max_attempts
        || payload.attempt_count == 0
        || payload.attempt_count > outbox.record.attempt_count
    {
        return Err(RunExportError::InvalidExport(
            "delivery-result attempt projection is inconsistent with outbox".to_string(),
        ));
    }
    if matches!(state, OutboxState::Acknowledged | OutboxState::DeadLettered)
        && (payload.attempt_count != outbox.record.attempt_count
            || error != outbox.record.delivery_error)
    {
        return Err(RunExportError::InvalidExport(
            "terminal delivery-result does not match final outbox attempt and error".to_string(),
        ));
    }
    match state {
        OutboxState::Ready => {
            if error.is_none() {
                return Err(RunExportError::InvalidExport(
                    "ready delivery-result must carry a failure reason".to_string(),
                ));
            }
        }
        OutboxState::Acknowledged => {
            if error.is_some() || outbox.record.state != OutboxState::Acknowledged {
                return Err(RunExportError::InvalidExport(
                    "acknowledged delivery-result does not match final outbox state".to_string(),
                ));
            }
        }
        OutboxState::DeadLettered => {
            if error.is_none() || outbox.record.state != OutboxState::DeadLettered {
                return Err(RunExportError::InvalidExport(
                    "dead-letter delivery-result does not match final outbox state".to_string(),
                ));
            }
        }
        OutboxState::Leased => {
            return Err(RunExportError::InvalidExport(
                "delivery-result cannot project a leased state".to_string(),
            ));
        }
    }
    Ok((
        outbox_id,
        state,
        payload.attempt_count,
        legacy,
        replay_fidelity,
    ))
}

fn parse_manifests(
    manifests: &[WireManifest],
    _run_id: RunId,
) -> RunExportResult<BTreeMap<EventId, CanonicalJson>> {
    let mut parsed = BTreeMap::new();
    for manifest in manifests {
        let event_id = parse_event_id(&manifest.event_id, "manifest event_id")?;
        validate_sha256(&manifest.manifest_hash, "manifest hash")?;
        let canonical = CanonicalJson::from_value(&manifest.manifest)?;
        if sha256_hex(canonical.as_str().as_bytes()) != manifest.manifest_hash {
            return Err(RunExportError::InvalidExport(
                "manifest hash does not equal its canonical JSON".to_string(),
            ));
        }
        if parsed.insert(event_id, canonical).is_some() {
            return Err(RunExportError::InvalidExport(
                "duplicate event manifest".to_string(),
            ));
        }
    }
    if parsed.is_empty() {
        return Err(RunExportError::InvalidExport(
            "complete export cannot omit all manifests".to_string(),
        ));
    }
    Ok(parsed)
}

fn parse_outbox(outbox: &[WireOutbox], run_id: RunId) -> RunExportResult<Vec<ParsedOutbox>> {
    let mut outbox_ids = BTreeSet::new();
    let mut parsed = Vec::with_capacity(outbox.len());
    for entry in outbox {
        ensure_wire_outbox_shape(entry)?;
        let record = entry.to_record()?;
        if record.run_id != run_id {
            return Err(RunExportError::InvalidExport(
                "outbox row run_id does not match export run".to_string(),
            ));
        }
        if !outbox_ids.insert(record.outbox_id) {
            return Err(RunExportError::InvalidExport(
                "duplicate outbox identifier".to_string(),
            ));
        }
        parsed.push(ParsedOutbox {
            record,
            manifest_order: entry.manifest_order,
        });
    }
    Ok(parsed)
}

fn parse_nonresumable_effects(
    effects: &[WireNonResumableEffect],
    run_id: RunId,
) -> RunExportResult<Vec<NonResumableEffectSnapshot>> {
    let mut event_ids = BTreeSet::new();
    let mut parsed = Vec::with_capacity(effects.len());
    for effect in effects {
        let snapshot = effect.to_snapshot()?;
        if snapshot.record.run_id != run_id {
            return Err(RunExportError::InvalidExport(
                "non-resumable effect run_id does not match export run".to_string(),
            ));
        }
        if !event_ids.insert(snapshot.record.event_id) {
            return Err(RunExportError::InvalidExport(
                "duplicate non-resumable effect source event".to_string(),
            ));
        }
        parsed.push(snapshot);
    }
    Ok(parsed)
}

fn parse_events(events: &[WireEvent], run_id: RunId) -> RunExportResult<Vec<SessionRunEvent>> {
    let mut event_ids = BTreeSet::new();
    let mut parsed = Vec::with_capacity(events.len());
    for event in events {
        let parsed_event = event.to_event()?;
        if parsed_event.run_id != run_id {
            return Err(RunExportError::InvalidExport(
                "event run_id does not match export run".to_string(),
            ));
        }
        if !event_ids.insert(parsed_event.event_id) {
            return Err(RunExportError::InvalidExport(
                "duplicate event identifier".to_string(),
            ));
        }
        parsed.push(parsed_event);
    }
    Ok(parsed)
}
fn validate_manifest_order(
    manifests: &[WireManifest],
    events: &[SessionRunEvent],
) -> RunExportResult<()> {
    if manifests.len() != events.len() {
        return Err(RunExportError::InvalidExport(
            "manifest rows do not cover every event exactly once".to_string(),
        ));
    }
    for (manifest, event) in manifests.iter().zip(events) {
        if parse_event_id(&manifest.event_id, "manifest event_id")? != event.event_id {
            return Err(RunExportError::InvalidExport(
                "manifest rows are not in immutable event sequence order".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_outbox_order(
    outbox: &[ParsedOutbox],
    events: &[SessionRunEvent],
) -> RunExportResult<()> {
    let event_sequences = events
        .iter()
        .map(|event| (event.event_id, event.sequence))
        .collect::<BTreeMap<_, _>>();
    let mut previous = None;
    for entry in outbox {
        let sequence = event_sequences
            .get(&entry.record.event_id)
            .copied()
            .ok_or_else(|| {
                RunExportError::InvalidExport(
                    "outbox row references an event outside this run".to_string(),
                )
            })?;
        let current = (sequence, entry.manifest_order);
        if previous.is_some_and(|previous| previous >= current) {
            return Err(RunExportError::InvalidExport(
                "outbox rows are not in immutable event/manifest order".to_string(),
            ));
        }
        previous = Some(current);
    }
    Ok(())
}

fn validate_nonresumable_effect_order(
    effects: &[NonResumableEffectSnapshot],
    events: &[SessionRunEvent],
) -> RunExportResult<()> {
    let event_sequences = events
        .iter()
        .map(|event| (event.event_id, event.sequence))
        .collect::<BTreeMap<_, _>>();
    let mut previous = None;
    for effect in effects {
        let sequence = event_sequences
            .get(&effect.record.event_id)
            .copied()
            .ok_or_else(|| {
                RunExportError::InvalidExport(
                    "non-resumable effect references an event outside this run".to_string(),
                )
            })?;
        if previous.is_some_and(|previous| previous >= sequence) {
            return Err(RunExportError::InvalidExport(
                "non-resumable effects are not in immutable event order".to_string(),
            ));
        }
        previous = Some(sequence);
    }
    Ok(())
}

fn outbox_for_event(
    outbox: &[ParsedOutbox],
    event_id: EventId,
) -> RunExportResult<Vec<OutboxRecord>> {
    let mut selected = outbox
        .iter()
        .filter(|entry| entry.record.event_id == event_id)
        .collect::<Vec<_>>();
    selected.sort_by_key(|entry| entry.manifest_order);
    for (index, entry) in selected.iter().enumerate() {
        let expected = u32::try_from(index)
            .map_err(|_| RunExportError::InvalidExport("manifest order exceeds u32".to_string()))?;
        if entry.manifest_order != expected {
            return Err(RunExportError::InvalidExport(
                "outbox manifest order is not contiguous".to_string(),
            ));
        }
    }
    Ok(selected
        .into_iter()
        .map(|entry| entry.record.clone())
        .collect())
}

fn ensure_terminal(status: &RunStatus) -> RunExportResult<()> {
    let Some(terminal_reason) = status.exit_terminal_reason else {
        return Err(RunExportError::NonTerminalRun {
            run_id: status.run_id.to_string(),
        });
    };
    let Some(candidate_outcome) = status.candidate_outcome else {
        return Err(RunExportError::NonTerminalRun {
            run_id: status.run_id.to_string(),
        });
    };
    let Some(final_outcome) = status.final_outcome else {
        return Err(RunExportError::NonTerminalRun {
            run_id: status.run_id.to_string(),
        });
    };
    if status.exit_event_id.is_none() {
        return Err(RunExportError::NonTerminalRun {
            run_id: status.run_id.to_string(),
        });
    }
    if status.pending_required_delivery
        || candidate_outcome != terminal_reason.outcome()
        || final_outcome
            != if status.required_dead_lettered {
                RunOutcome::DeadLettered
            } else {
                terminal_reason.outcome()
            }
    {
        return Err(RunExportError::NonTerminalRun {
            run_id: status.run_id.to_string(),
        });
    }
    Ok(())
}

fn digest_unsigned(unsigned: &UnsignedEnvelope) -> RunExportResult<String> {
    Ok(sha256_hex(&canonical_bytes(unsigned)?))
}

fn export_path(export_dir: &Path, run_id: RunId) -> PathBuf {
    export_dir.join(format!("{run_id}{EXPORT_FILE_SUFFIX}"))
}

fn publish_immutable(
    export_dir: &Path,
    destination: &Path,
    bytes: &[u8],
    #[cfg(test)] interrupt_before_rename: bool,
) -> RunExportResult<bool> {
    ensure_private_directory(export_dir)?;
    if let Some(existing) = read_existing_export(destination)? {
        return if existing == bytes {
            Ok(false)
        } else {
            Err(RunExportError::PublicationConflict {
                path: destination.to_path_buf(),
            })
        };
    }

    let temporary = temporary_path(destination);
    let file = create_private_new_file(&temporary)?;
    // A graceful error closes and removes the hidden temporary. A process kill
    // can still leave it behind, but never exposes it at the immutable path.
    let mut temporary_cleanup = TemporaryFileCleanup::new(&temporary, file);
    temporary_cleanup
        .file_mut()
        .write_all(bytes)
        .map_err(|source| RunExportError::Io {
            path: temporary.clone(),
            source,
        })?;
    temporary_cleanup
        .file_mut()
        .sync_all()
        .map_err(|source| RunExportError::Io {
            path: temporary.clone(),
            source,
        })?;
    temporary_cleanup.close();

    #[cfg(test)]
    if interrupt_before_rename {
        return Err(RunExportError::TestInterruptedBeforeRename);
    }

    match rename_no_replace(&temporary, destination) {
        Ok(()) => {
            temporary_cleanup.disarm();
            sync_directory(export_dir)?;
            Ok(true)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            match read_existing_export(destination)? {
                Some(existing) if existing == bytes => Ok(false),
                Some(_) => Err(RunExportError::PublicationConflict {
                    path: destination.to_path_buf(),
                }),
                None => Err(RunExportError::Io {
                    path: destination.to_path_buf(),
                    source: error,
                }),
            }
        }
        Err(source) => Err(RunExportError::Io {
            path: destination.to_path_buf(),
            source,
        }),
    }
}

struct TemporaryFileCleanup<'a> {
    path: &'a Path,
    file: Option<File>,
    armed: bool,
}

impl<'a> TemporaryFileCleanup<'a> {
    fn new(path: &'a Path, file: File) -> Self {
        Self {
            path,
            file: Some(file),
            armed: true,
        }
    }

    fn file_mut(&mut self) -> &mut File {
        self.file
            .as_mut()
            .expect("temporary export file is open before publication")
    }

    fn close(&mut self) {
        drop(self.file.take());
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for TemporaryFileCleanup<'_> {
    fn drop(&mut self) {
        self.close();
        if self.armed {
            let _ = fs::remove_file(self.path);
        }
    }
}

fn temporary_path(destination: &Path) -> PathBuf {
    let file_name = destination
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("run-export");
    destination.with_file_name(format!(".{file_name}.{}.tmp", uuid::Uuid::new_v4()))
}

fn create_private_new_file(path: &Path) -> RunExportResult<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path).map_err(|source| RunExportError::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn read_existing_export(path: &Path) -> RunExportResult<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() || !is_owner_only(&metadata)
            {
                return Err(RunExportError::UnsafePath {
                    path: path.to_path_buf(),
                });
            }
            fs::read(path)
                .map(Some)
                .map_err(|source| RunExportError::Io {
                    path: path.to_path_buf(),
                    source,
                })
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(source) => Err(RunExportError::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn ensure_private_directory(path: &Path) -> RunExportResult<()> {
    let existed = path.exists();
    fs::create_dir_all(path).map_err(|source| RunExportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    #[cfg(unix)]
    if !existed {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
            RunExportError::Io {
                path: path.to_path_buf(),
                source,
            }
        })?;
    }
    let metadata = fs::symlink_metadata(path).map_err(|source| RunExportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() || !is_owner_only(&metadata) {
        return Err(RunExportError::UnsafePath {
            path: path.to_path_buf(),
        });
    }
    Ok(())
}

fn sync_directory(path: &Path) -> RunExportResult<()> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| RunExportError::Io {
                path: path.to_path_buf(),
                source,
            })
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(unix)]
fn is_owner_only(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.mode() & 0o077 == 0 && metadata.uid() == unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn is_owner_only(_metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(target_os = "macos")]
fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::unix::ffi::OsStrExt;
    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source path contains NUL"))?;
    let destination = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "destination path contains NUL")
    })?;
    // `RENAME_EXCL` gives Darwin's atomic no-replace rename primitive. It is
    // deliberately not preceded by a replace-capable std::fs::rename.
    let result = unsafe {
        libc::renameatx_np(
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(all(unix, not(target_os = "macos")))]
fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    // `link` atomically creates the destination only when it does not already
    // exist. It is the conservative no-replace fallback where renameatx_np is
    // unavailable; the source temp is removed only after publication.
    fs::hard_link(source, destination)?;
    let _ = fs::remove_file(source);
    Ok(())
}

#[cfg(not(unix))]
fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    fs::hard_link(source, destination)?;
    let _ = fs::remove_file(source);
    Ok(())
}

fn parse_canonical_document(bytes: &[u8]) -> RunExportResult<Value> {
    let text = std::str::from_utf8(bytes)
        .map_err(|error| RunExportError::InvalidUtf8(error.to_string()))?;
    let value: Value = serde_json::from_str(text)
        .map_err(|error| RunExportError::InvalidJson(error.to_string()))?;
    if canonical_value_bytes(&value)? != bytes {
        return Err(RunExportError::NonCanonicalJson);
    }
    Ok(value)
}

fn canonical_bytes<T: Serialize>(value: &T) -> RunExportResult<Vec<u8>> {
    serde_json_canonicalizer::to_vec(value)
        .map_err(|error| RunExportError::InvalidExport(error.to_string()))
}

fn canonical_value_bytes(value: &Value) -> RunExportResult<Vec<u8>> {
    canonical_bytes(value)
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn validate_sha256(value: &str, label: &str) -> RunExportResult<()> {
    Sha256Digest::new(value.to_string())
        .map(|_| ())
        .map_err(|_| {
            RunExportError::InvalidExport(format!(
                "{label} must be exactly 64 lowercase hexadecimal characters"
            ))
        })
}

fn ensure_object_keys<'a>(
    value: &Value,
    keys: impl IntoIterator<Item = &'a str>,
    label: &str,
) -> RunExportResult<()> {
    let object = value
        .as_object()
        .ok_or_else(|| RunExportError::InvalidExport(format!("{label} must be an object")))?;
    let expected = keys.into_iter().collect::<BTreeSet<_>>();
    let actual = object.keys().map(String::as_str).collect::<BTreeSet<_>>();
    if expected != actual {
        return Err(RunExportError::InvalidExport(format!(
            "{label} has missing or unknown fields"
        )));
    }
    Ok(())
}

fn validate_wire_document_layout(value: &Value) -> RunExportResult<()> {
    let root = value
        .as_object()
        .ok_or_else(|| RunExportError::InvalidExport("envelope must be an object".to_string()))?;
    ensure_object_keys(
        object_field(root, "source_watermark", "envelope")?,
        ["head_hash", "event_count", "last_sequence"],
        "source watermark",
    )?;
    ensure_object_keys(
        object_field(root, "range", "envelope")?,
        ["first_sequence", "last_sequence"],
        "event range",
    )?;
    ensure_object_keys(
        object_field(root, "compatibility_aliases", "envelope")?,
        ["run_id", "session_id", "legacy_v0"],
        "compatibility aliases",
    )?;
    ensure_object_keys(
        object_field(root, "digest", "envelope")?,
        ["algorithm", "value"],
        "envelope digest",
    )?;

    let header = object_field(root, "header", "envelope")?;
    ensure_object_keys(
        header,
        [
            "run_id",
            "start_event_id",
            "execution_mode",
            "started_at_ms",
            "owner_id",
            "provenance",
        ],
        "run header",
    )?;
    let header = header
        .as_object()
        .ok_or_else(|| RunExportError::InvalidExport("run header must be an object".to_string()))?;
    ensure_object_keys(
        object_field(header, "execution_mode", "run header")?,
        ["mode", "source"],
        "execution mode",
    )?;
    validate_wire_provenance(object_field(header, "provenance", "run header")?)?;

    let status = object_field(root, "status", "envelope")?;
    let schema_version = root.get("schema_version").and_then(Value::as_u64);
    match schema_version {
        Some(version)
            if version == u64::from(PREVIOUS_RUN_EXPORT_SCHEMA_VERSION)
                || version == u64::from(RUN_EXPORT_SCHEMA_VERSION) =>
        {
            ensure_object_keys(
                status,
                [
                    "run_id",
                    "exit_event_id",
                    "exit_terminal_reason",
                    "candidate_outcome",
                    "final_outcome",
                    "required_dead_lettered",
                    "pending_required_delivery",
                    "exact_replay",
                ],
                "run status",
            )?;
            validate_optional_object(
                object_field(status.as_object().ok_or_else(|| {
                    RunExportError::InvalidExport("run status must be an object".to_string())
                })?, "exact_replay", "run status")?,
                [
                    "state",
                    "reason",
                    "exact_replay",
                    "transition_sequence",
                    "latched_at_ms",
                    "latched_by_event_id",
                ],
                "exact replay status",
            )?;
        }
        _ => ensure_object_keys(
            status,
            [
                "run_id",
                "exit_event_id",
                "exit_terminal_reason",
                "candidate_outcome",
                "final_outcome",
                "required_dead_lettered",
                "pending_required_delivery",
            ],
            "run status",
        )?,
    }

    for event in array_field(root, "events", "envelope")? {
        ensure_object_keys(
            event,
            [
                "event_id",
                "run_id",
                "sequence",
                "kind",
                "append_key",
                "effect_intent",
                "artifact_degradation_reason",
                "public_identity",
                "private_fingerprint",
                "sealed_artifact_ref",
                "run_provenance",
                "public_payload",
                "manifest_hash",
                "previous_hash",
                "event_hash",
                "created_at_ms",
            ],
            "event row",
        )?;
        let event = event.as_object().ok_or_else(|| {
            RunExportError::InvalidExport("event row must be an object".to_string())
        })?;
        validate_optional_object(
            object_field(event, "private_fingerprint", "event row")?,
            [
                "algorithm",
                "algorithm_version",
                "key_id",
                "key_version",
                "value",
            ],
            "private fingerprint",
        )?;
        validate_optional_artifact_ref(object_field(event, "sealed_artifact_ref", "event row")?)?;
        let provenance = object_field(event, "run_provenance", "event row")?;
        if !provenance.is_null() {
            validate_wire_provenance(provenance)?;
        }
    }

    for manifest in array_field(root, "manifests", "envelope")? {
        ensure_object_keys(
            manifest,
            ["event_id", "manifest_hash", "manifest"],
            "manifest row",
        )?;
    }

    for outbox in array_field(root, "outbox", "envelope")? {
        ensure_object_keys(
            outbox,
            [
                "outbox_id",
                "run_id",
                "event_id",
                "manifest_order",
                "destination",
                "idempotency_key",
                "required",
                "public_payload",
                "sealed_artifact_ref",
                "state",
                "attempt_count",
                "max_attempts",
                "lease_generation",
                "delivery_receipt",
                "delivery_error",
            ],
            "outbox row",
        )?;
        let outbox = outbox.as_object().ok_or_else(|| {
            RunExportError::InvalidExport("outbox row must be an object".to_string())
        })?;
        validate_optional_artifact_ref(object_field(outbox, "sealed_artifact_ref", "outbox row")?)?;
        if object_field(outbox, "sealed_artifact_ref", "outbox row")?.is_null() {
            return Err(RunExportError::InvalidExport(
                "outbox sealed artifact reference must be present".to_string(),
            ));
        }
    }
    for effect in array_field(root, "nonresumable_effects", "envelope")? {
        ensure_object_keys(
            effect,
            [
                "event_id",
                "run_id",
                "state",
                "idempotency_key",
                "claim_binding",
                "claimed_at_ms",
                "result_payload_digest",
                "result_event_id",
                "terminal_at_ms",
            ],
            "non-resumable effect row",
        )?;
    }
    Ok(())
}

fn object_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
    label: &str,
) -> RunExportResult<&'a Value> {
    object.get(field).ok_or_else(|| {
        RunExportError::InvalidExport(format!("{label} is missing required field {field:?}"))
    })
}

fn array_field<'a>(
    object: &'a serde_json::Map<String, Value>,
    field: &str,
    label: &str,
) -> RunExportResult<&'a [Value]> {
    object_field(object, field, label)?
        .as_array()
        .map(Vec::as_slice)
        .ok_or_else(|| {
            RunExportError::InvalidExport(format!("{label} field {field:?} must be an array"))
        })
}

fn validate_optional_object<'a>(
    value: &Value,
    keys: impl IntoIterator<Item = &'a str>,
    label: &str,
) -> RunExportResult<()> {
    if value.is_null() {
        Ok(())
    } else {
        ensure_object_keys(value, keys, label)
    }
}

fn validate_optional_artifact_ref(value: &Value) -> RunExportResult<()> {
    validate_optional_object(
        value,
        [
            "envelope_digest",
            "key_id",
            "schema_version",
            "encryption_algorithm",
            "canonicalization_algorithm",
            "canonicalization_version",
            "redactor_version",
        ],
        "sealed artifact reference",
    )
}

fn validate_wire_provenance(value: &Value) -> RunExportResult<()> {
    ensure_object_keys(
        value,
        [
            "canonicalization_algorithm",
            "canonicalization_version",
            "session_id",
            "parent_run_id",
            "branch_id",
            "tenant_id",
            "action_id",
            "objective_id",
            "task_id",
            "model_id",
            "provider_id",
            "config_digest",
            "prompt_digest",
            "input_digest",
            "output_digest",
            "tool_digest",
            "skill_digest",
            "policy_digest",
            "sandbox_digest",
        ],
        "run provenance",
    )?;
    let provenance = value.as_object().ok_or_else(|| {
        RunExportError::InvalidExport("run provenance must be an object".to_string())
    })?;
    for digest in [
        "prompt_digest",
        "input_digest",
        "output_digest",
        "tool_digest",
        "skill_digest",
        "policy_digest",
        "sandbox_digest",
    ] {
        ensure_object_keys(
            object_field(provenance, digest, "run provenance")?,
            ["availability", "value"],
            "provenance digest",
        )?;
    }
    Ok(())
}

fn parse_run_id(value: &str, label: &str) -> RunExportResult<RunId> {
    RunId::from_str(value).map_err(|_| RunExportError::InvalidExport(format!("invalid {label}")))
}

fn parse_event_id(value: &str, label: &str) -> RunExportResult<EventId> {
    EventId::from_str(value).map_err(|_| RunExportError::InvalidExport(format!("invalid {label}")))
}

fn parse_outbox_id(value: &str, label: &str) -> RunExportResult<OutboxRowId> {
    OutboxRowId::from_str(value)
        .map_err(|_| RunExportError::InvalidExport(format!("invalid {label}")))
}

fn validate_operation_instance_id(value: &str) -> RunExportResult<String> {
    const MAX_OPERATION_INSTANCE_ID_BYTES: usize = 256;
    if value.is_empty()
        || value.len() > MAX_OPERATION_INSTANCE_ID_BYTES
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err(RunExportError::InvalidExport(
            "replay obligation operation_instance_id is invalid".to_string(),
        ));
    }
    Ok(value.to_string())
}

fn parse_effect_intent(value: &str) -> RunExportResult<EffectIntent> {
    EffectIntent::from_db(value).map_err(RunExportError::from)
}

fn parse_artifact_degradation_reason(value: &str) -> RunExportResult<ArtifactDegradationReason> {
    ArtifactDegradationReason::from_db(value).map_err(RunExportError::from)
}

fn parse_event_kind(value: &str) -> RunExportResult<SessionRunEventKind> {
    SessionRunEventKind::from_db(value).map_err(RunExportError::from)
}

fn parse_destination(value: &str) -> RunExportResult<Destination> {
    Destination::from_db(value).map_err(RunExportError::from)
}

fn parse_outbox_state(value: &str) -> RunExportResult<OutboxState> {
    OutboxState::from_db(value).map_err(RunExportError::from)
}

fn parse_nonresumable_effect_state(value: &str) -> RunExportResult<NonResumableEffectState> {
    NonResumableEffectState::from_db(value).map_err(RunExportError::from)
}

fn parse_delivery_failure_reason(value: &str) -> RunExportResult<DeliveryFailureReason> {
    DeliveryFailureReason::from_db(value).map_err(RunExportError::from)
}

fn parse_terminal_reason(value: &str) -> RunExportResult<TerminalReason> {
    TerminalReason::from_db(value).map_err(RunExportError::from)
}

fn parse_run_outcome(value: &str, label: &str) -> RunExportResult<RunOutcome> {
    RunOutcome::from_db(value)
        .map_err(|_| RunExportError::InvalidExport(format!("invalid {label}")))
}

fn legacy_v0_record_count(text: &str) -> Option<u64> {
    if text.is_empty() || !text.ends_with('\n') {
        return None;
    }
    let mut count = 0_u64;
    for line in text.lines() {
        if line.is_empty() {
            return None;
        }
        let value = serde_json::from_str::<Value>(line).ok()?;
        let object = value.as_object()?;
        let previous_hash = object.get("prev_hash")?.as_str()?;
        let event_hash = object.get("event_hash")?.as_str()?;
        if !is_legacy_short_hash(previous_hash) || !is_legacy_short_hash(event_hash) {
            return None;
        }
        count = count.checked_add(1)?;
    }
    (count > 0).then_some(count)
}

fn is_legacy_short_hash(value: &str) -> bool {
    value.len() == LEGACY_SHORT_HASH_LENGTH
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEnvelope {
    schema: String,
    schema_version: u32,
    canonicalization_algorithm: String,
    canonicalization_version: u8,
    complete: bool,
    source_watermark: WireSourceWatermark,
    range: WireRange,
    total_event_count: u64,
    header: WireHeader,
    status: WireStatus,
    events: Vec<WireEvent>,
    manifests: Vec<WireManifest>,
    outbox: Vec<WireOutbox>,
    nonresumable_effects: Vec<WireNonResumableEffect>,
    #[serde(default)]
    replay_obligations: Option<Vec<WireReplayObligation>>,
    compatibility_aliases: WireCompatibilityAliases,
    digest: WireDigest,
}

impl WireEnvelope {
    const LEGACY_KEYS: [&'static str; 16] = [
        "schema",
        "schema_version",
        "canonicalization_algorithm",
        "canonicalization_version",
        "complete",
        "source_watermark",
        "range",
        "total_event_count",
        "header",
        "status",
        "events",
        "manifests",
        "outbox",
        "nonresumable_effects",
        "compatibility_aliases",
        "digest",
    ];
    const V3_KEYS: [&'static str; 17] = [
        "schema",
        "schema_version",
        "canonicalization_algorithm",
        "canonicalization_version",
        "complete",
        "source_watermark",
        "range",
        "total_event_count",
        "header",
        "status",
        "events",
        "manifests",
        "outbox",
        "nonresumable_effects",
        "replay_obligations",
        "compatibility_aliases",
        "digest",
    ];

    fn expected_keys(&self) -> &'static [&'static str] {
        if self.schema_version == RUN_EXPORT_SCHEMA_VERSION {
            &Self::V3_KEYS
        } else {
            &Self::LEGACY_KEYS
        }
    }
}

#[derive(Debug, Clone, Serialize)]
struct UnsignedEnvelope {
    schema: &'static str,
    schema_version: u32,
    canonicalization_algorithm: &'static str,
    canonicalization_version: u8,
    complete: bool,
    source_watermark: WireSourceWatermark,
    range: WireRange,
    total_event_count: u64,
    header: WireHeader,
    status: WireStatus,
    events: Vec<WireEvent>,
    manifests: Vec<WireManifest>,
    outbox: Vec<WireOutbox>,
    nonresumable_effects: Vec<WireNonResumableEffect>,
    replay_obligations: Vec<WireReplayObligation>,
    compatibility_aliases: WireCompatibilityAliases,
}

impl UnsignedEnvelope {
    fn from_complete_fields(
        complete: bool,
        source_watermark: WireSourceWatermark,
        range: WireRange,
        total_event_count: u64,
        header: WireHeader,
        status: WireStatus,
        events: Vec<WireEvent>,
        manifests: Vec<WireManifest>,
        outbox: Vec<WireOutbox>,
        nonresumable_effects: Vec<WireNonResumableEffect>,
        replay_obligations: Vec<WireReplayObligation>,
        compatibility_aliases: WireCompatibilityAliases,
    ) -> Self {
        Self {
            schema: RUN_EXPORT_SCHEMA,
            schema_version: RUN_EXPORT_SCHEMA_VERSION,
            canonicalization_algorithm: CANONICALIZATION_ALGORITHM,
            canonicalization_version: CANONICALIZATION_VERSION,
            complete,
            source_watermark,
            range,
            total_event_count,
            header,
            status,
            events,
            manifests,
            outbox,
            nonresumable_effects,
            replay_obligations,
            compatibility_aliases,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDigest {
    algorithm: String,
    value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireSourceWatermark {
    head_hash: String,
    event_count: u64,
    last_sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRange {
    first_sequence: u64,
    last_sequence: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireCompatibilityAliases {
    run_id: String,
    session_id: String,
    legacy_v0: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireHeader {
    run_id: String,
    start_event_id: String,
    execution_mode: ResolvedExecutionMode,
    started_at_ms: i64,
    owner_id: String,
    provenance: WireRunProvenance,
}

impl WireHeader {
    fn from_header(header: &RunHeader) -> RunExportResult<Self> {
        let provenance = serde_json::to_value(&header.provenance)
            .map_err(|error| RunExportError::InvalidExport(error.to_string()))?;
        let provenance = serde_json::from_value(provenance)
            .map_err(|error| RunExportError::InvalidExport(error.to_string()))?;
        Ok(Self {
            run_id: header.run_id.to_string(),
            start_event_id: header.start_event_id.to_string(),
            execution_mode: header.execution_mode,
            started_at_ms: header.started_at_ms,
            owner_id: header.owner_id.as_str().to_string(),
            provenance,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireStatus {
    run_id: String,
    exit_event_id: String,
    exit_terminal_reason: String,
    candidate_outcome: String,
    final_outcome: String,
    required_dead_lettered: bool,
    pending_required_delivery: bool,
    #[serde(default)]
    exact_replay: Option<WireExactReplayStatus>,
}

impl WireStatus {
    fn from_status(status: &RunStatus) -> Self {
        Self {
            run_id: status.run_id.to_string(),
            exit_event_id: status
                .exit_event_id
                .expect("terminal status checked before serialization")
                .to_string(),
            exit_terminal_reason: status
                .exit_terminal_reason
                .expect("terminal status checked before serialization")
                .as_str()
                .to_string(),
            candidate_outcome: status
                .candidate_outcome
                .expect("terminal status checked before serialization")
                .as_str()
                .to_string(),
            final_outcome: status
                .final_outcome
                .expect("terminal status checked before serialization")
                .as_str()
                .to_string(),
            required_dead_lettered: status.required_dead_lettered,
            pending_required_delivery: status.pending_required_delivery,
            exact_replay: Some(WireExactReplayStatus::from_status(&status.exact_replay)),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireExactReplayStatus {
    state: String,
    reason: Option<String>,
    exact_replay: bool,
    transition_sequence: u64,
    latched_at_ms: Option<i64>,
    latched_by_event_id: Option<String>,
}

impl WireExactReplayStatus {
    fn from_status(status: &ExactReplayStatus) -> Self {
        Self {
            state: status.state.as_str().to_string(),
            reason: status.reason.map(ArtifactStateReason::as_str).map(str::to_string),
            exact_replay: status.exact_replay,
            transition_sequence: status.transition_sequence,
            latched_at_ms: status.latched_at_ms,
            latched_by_event_id: status
                .latched_by_event_id
                .map(|event_id| event_id.to_string()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireEvent {
    event_id: String,
    run_id: String,
    sequence: u64,
    kind: String,
    append_key: Option<String>,
    effect_intent: String,
    artifact_degradation_reason: Option<String>,
    public_identity: Option<Value>,
    private_fingerprint: Option<WirePrivateFingerprint>,
    sealed_artifact_ref: Option<WireSealedArtifactRef>,
    run_provenance: Option<WireRunProvenance>,
    public_payload: Value,
    manifest_hash: String,
    previous_hash: String,
    event_hash: String,
    created_at_ms: i64,
}

impl WireEvent {
    fn from_event(event: &SessionRunEvent) -> RunExportResult<Self> {
        Ok(Self {
            event_id: event.event_id.to_string(),
            run_id: event.run_id.to_string(),
            sequence: event.sequence,
            kind: event.kind.as_str().to_string(),
            append_key: event.append_key.as_ref().map(ToString::to_string),
            effect_intent: event.effect_intent.as_str().to_string(),
            artifact_degradation_reason: event
                .artifact_degradation_reason
                .map(|reason| reason.as_str().to_string()),
            public_identity: event
                .public_identity
                .as_ref()
                .map(canonical_json_to_value)
                .transpose()?,
            private_fingerprint: event
                .private_fingerprint
                .as_ref()
                .map(WirePrivateFingerprint::from_fingerprint),
            sealed_artifact_ref: event
                .sealed_artifact_ref
                .as_ref()
                .map(WireSealedArtifactRef::from_ref),
            run_provenance: event
                .run_provenance
                .as_ref()
                .map(WireRunProvenance::from_canonical_json)
                .transpose()?,
            public_payload: canonical_json_to_value(&event.public_payload)?,
            manifest_hash: event.manifest_hash.clone(),
            previous_hash: event.previous_hash.clone(),
            event_hash: event.event_hash.clone(),
            created_at_ms: event.created_at_ms,
        })
    }

    fn to_event(&self) -> RunExportResult<SessionRunEvent> {
        ensure_wire_event_values(self)?;
        let event_id = parse_event_id(&self.event_id, "event_id")?;
        let run_id = parse_run_id(&self.run_id, "event run_id")?;
        let kind = parse_event_kind(&self.kind)?;
        let append_key = self
            .append_key
            .as_deref()
            .map(|value| AppendKey::new(value.to_string()))
            .transpose()?;
        let effect_intent = parse_effect_intent(&self.effect_intent)?;
        let artifact_degradation_reason = self
            .artifact_degradation_reason
            .as_deref()
            .map(parse_artifact_degradation_reason)
            .transpose()?;
        let public_identity = self
            .public_identity
            .as_ref()
            .map(CanonicalJson::from_value)
            .transpose()?;
        let private_fingerprint = self
            .private_fingerprint
            .as_ref()
            .map(WirePrivateFingerprint::to_fingerprint)
            .transpose()?;
        let sealed_artifact_ref = self
            .sealed_artifact_ref
            .as_ref()
            .map(WireSealedArtifactRef::to_ref)
            .transpose()?;
        let run_provenance = self
            .run_provenance
            .as_ref()
            .map(WireRunProvenance::canonical_json)
            .transpose()?;
        let public_payload = CanonicalJson::from_value(&self.public_payload)?;
        validate_sha256(&self.manifest_hash, "event manifest hash")?;
        validate_sha256(&self.previous_hash, "event previous hash")?;
        validate_sha256(&self.event_hash, "event hash")?;
        Ok(SessionRunEvent {
            event_id,
            run_id,
            sequence: self.sequence,
            kind,
            append_key,
            effect_intent,
            artifact_degradation_reason,
            public_identity,
            private_fingerprint,
            sealed_artifact_ref,
            run_provenance,
            public_payload,
            manifest_hash: self.manifest_hash.clone(),
            previous_hash: self.previous_hash.clone(),
            event_hash: self.event_hash.clone(),
            created_at_ms: self.created_at_ms,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WirePrivateFingerprint {
    algorithm: String,
    algorithm_version: u8,
    key_id: String,
    key_version: u32,
    value: String,
}

impl WirePrivateFingerprint {
    fn from_fingerprint(value: &PrivateFingerprint) -> Self {
        Self {
            algorithm: value.algorithm().as_str().to_string(),
            algorithm_version: value.algorithm_version(),
            key_id: value.key_id().as_str().to_string(),
            key_version: value.key_version(),
            value: value.as_hex(),
        }
    }

    fn to_fingerprint(&self) -> RunExportResult<PrivateFingerprint> {
        PrivateFingerprint::from_db(
            &self.algorithm,
            i64::from(self.algorithm_version),
            self.key_id.clone(),
            i64::from(self.key_version),
            self.value.clone(),
        )
        .map_err(RunExportError::from)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct WireSealedArtifactRef {
    envelope_digest: String,
    key_id: String,
    schema_version: u32,
    encryption_algorithm: String,
    canonicalization_algorithm: String,
    canonicalization_version: u8,
    redactor_version: String,
}

impl WireSealedArtifactRef {
    fn from_ref(value: &SealedArtifactRef) -> Self {
        Self {
            envelope_digest: value.envelope_digest().as_str().to_string(),
            key_id: value.key_id().as_str().to_string(),
            schema_version: value.schema_version(),
            encryption_algorithm: value.encryption_algorithm().to_string(),
            canonicalization_algorithm: value.canonicalization_algorithm().to_string(),
            canonicalization_version: value.canonicalization_version(),
            redactor_version: value.redactor_version().as_str().to_string(),
        }
    }

    fn to_ref(&self) -> RunExportResult<SealedArtifactRef> {
        SealedArtifactRef::from_db(
            self.envelope_digest.clone(),
            self.key_id.clone(),
            i64::from(self.schema_version),
            self.encryption_algorithm.clone(),
            self.canonicalization_algorithm.clone(),
            i64::from(self.canonicalization_version),
            self.redactor_version.clone(),
        )
        .map_err(RunExportError::from)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireManifest {
    event_id: String,
    manifest_hash: String,
    manifest: Value,
}

impl WireManifest {
    fn from_snapshot(snapshot: &LedgerManifestSnapshot) -> RunExportResult<Self> {
        Ok(Self {
            event_id: snapshot.event_id.to_string(),
            manifest_hash: snapshot.hash.clone(),
            manifest: canonical_json_to_value(&snapshot.canonical_json)?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireOutbox {
    outbox_id: String,
    run_id: String,
    event_id: String,
    manifest_order: u32,
    destination: String,
    idempotency_key: String,
    required: bool,
    public_payload: Value,
    sealed_artifact_ref: WireSealedArtifactRef,
    state: String,
    attempt_count: u32,
    max_attempts: u32,
    lease_generation: u64,
    delivery_receipt: Option<String>,
    delivery_error: Option<String>,
}

impl WireOutbox {
    fn from_snapshot(snapshot: &LedgerOutboxSnapshot) -> RunExportResult<Self> {
        let record = &snapshot.record;
        Ok(Self {
            outbox_id: record.outbox_id.to_string(),
            run_id: record.run_id.to_string(),
            event_id: record.event_id.to_string(),
            manifest_order: snapshot.manifest_order,
            destination: record.destination.as_str().to_string(),
            idempotency_key: record.idempotency_key.as_str().to_string(),
            required: record.required,
            public_payload: canonical_json_to_value(&record.public_payload)?,
            sealed_artifact_ref: WireSealedArtifactRef::from_ref(&record.sealed_artifact_ref),
            state: record.state.as_str().to_string(),
            attempt_count: record.attempt_count,
            max_attempts: record.max_attempts,
            lease_generation: record.lease_generation,
            delivery_receipt: record
                .delivery_receipt
                .as_ref()
                .map(|value| value.as_str().to_string()),
            delivery_error: record
                .delivery_error
                .map(|value| value.as_str().to_string()),
        })
    }

    fn to_record(&self) -> RunExportResult<OutboxRecord> {
        let outbox_id = parse_outbox_id(&self.outbox_id, "outbox_id")?;
        let run_id = parse_run_id(&self.run_id, "outbox run_id")?;
        let event_id = parse_event_id(&self.event_id, "outbox event_id")?;
        let destination = parse_destination(&self.destination)?;
        let idempotency_key =
            DownstreamIdempotencyKey::from_sha256_hex(self.idempotency_key.clone())?;
        let public_payload = CanonicalJson::from_value(&self.public_payload)?;
        let sealed_artifact_ref = self.sealed_artifact_ref.to_ref()?;
        let state = parse_outbox_state(&self.state)?;
        let delivery_receipt = self
            .delivery_receipt
            .as_deref()
            .map(|value| DurableDeliveryReceipt::new(value.to_string()))
            .transpose()?;
        let delivery_error = self
            .delivery_error
            .as_deref()
            .map(parse_delivery_failure_reason)
            .transpose()?;
        Ok(OutboxRecord {
            outbox_id,
            run_id,
            event_id,
            destination,
            idempotency_key,
            required: self.required,
            public_payload,
            sealed_artifact_ref,
            state,
            attempt_count: self.attempt_count,
            max_attempts: self.max_attempts,
            lease_generation: self.lease_generation,
            delivery_receipt,
            delivery_error,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireNonResumableEffect {
    event_id: String,
    run_id: String,
    state: String,
    idempotency_key: String,
    claim_binding: Option<String>,
    claimed_at_ms: Option<i64>,
    result_payload_digest: Option<String>,
    result_event_id: Option<String>,
    terminal_at_ms: Option<i64>,
}

impl WireNonResumableEffect {
    fn from_snapshot(snapshot: &NonResumableEffectSnapshot) -> Self {
        Self {
            event_id: snapshot.record.event_id.to_string(),
            run_id: snapshot.record.run_id.to_string(),
            state: snapshot.record.state.as_str().to_string(),
            idempotency_key: snapshot.record.idempotency_key.as_str().to_string(),
            claim_binding: snapshot
                .claim_binding
                .as_ref()
                .map(|value| value.as_str().to_string()),
            claimed_at_ms: snapshot.record.claimed_at_ms,
            result_payload_digest: snapshot
                .record
                .result_payload_digest
                .as_ref()
                .map(|value| value.as_str().to_string()),
            result_event_id: snapshot
                .record
                .result_event_id
                .map(|value| value.to_string()),
            terminal_at_ms: snapshot.record.terminal_at_ms,
        }
    }

    fn to_snapshot(&self) -> RunExportResult<NonResumableEffectSnapshot> {
        if self.claimed_at_ms.is_some_and(|value| value < 0)
            || self.terminal_at_ms.is_some_and(|value| value < 0)
        {
            return Err(RunExportError::InvalidExport(
                "non-resumable effect timestamps must be non-negative".to_string(),
            ));
        }
        Ok(NonResumableEffectSnapshot {
            record: NonResumableEffectRecord {
                run_id: parse_run_id(&self.run_id, "non-resumable effect run_id")?,
                event_id: parse_event_id(&self.event_id, "non-resumable effect event_id")?,
                state: parse_nonresumable_effect_state(&self.state)?,
                idempotency_key: NonResumableIdempotencyKey::from_sha256_hex(
                    self.idempotency_key.clone(),
                )?,
                claimed_at_ms: self.claimed_at_ms,
                result_payload_digest: self
                    .result_payload_digest
                    .as_ref()
                    .map(|value| Sha256Digest::new(value.clone()))
                    .transpose()?,
                result_event_id: self
                    .result_event_id
                    .as_deref()
                    .map(|value| parse_event_id(value, "non-resumable result_event_id"))
                    .transpose()?,
                terminal_at_ms: self.terminal_at_ms,
            },
            claim_binding: self
                .claim_binding
                .as_ref()
                .map(|value| Sha256Digest::new(value.clone()))
                .transpose()?,
        })
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireReplayObligation {
    source_event_id: String,
    source_event_sequence: u64,
    operation: String,
    phase: String,
    operation_instance_id: String,
    required: bool,
    fidelity: String,
    outcome: String,
    resolved_event_id: Option<String>,
    artifact: Option<WireSealedArtifactRef>,
    reason: Option<String>,
}

impl WireReplayObligation {
    fn from_snapshot(snapshot: &ReplayObligationSnapshot) -> Self {
        Self {
            source_event_id: snapshot.source_event_id.to_string(),
            source_event_sequence: snapshot.source_event_sequence,
            operation: snapshot.operation.as_str().to_string(),
            phase: snapshot.phase.as_str().to_string(),
            operation_instance_id: snapshot.operation_instance_id.clone(),
            required: snapshot.required,
            fidelity: snapshot.fidelity.as_str().to_string(),
            outcome: snapshot.outcome.as_str().to_string(),
            resolved_event_id: snapshot.resolved_event_id.map(|value| value.to_string()),
            artifact: snapshot.artifact.as_ref().map(WireSealedArtifactRef::from_ref),
            reason: snapshot.reason.map(|value| value.as_str().to_string()),
        }
    }

    fn to_snapshot(&self) -> RunExportResult<ReplayObligationSnapshot> {
        Ok(ReplayObligationSnapshot {
            source_event_id: parse_event_id(
                &self.source_event_id,
                "replay obligation source event_id",
            )?,
            source_event_sequence: self.source_event_sequence,
            operation: ReplayOperationKind::from_db(&self.operation)?,
            operation_instance_id: validate_operation_instance_id(
                &self.operation_instance_id,
            )?,
            phase: ReplayOperationPhase::from_db(&self.phase)?,
            required: self.required,
            fidelity: ReplayObligationFidelity::from_db(&self.fidelity)?,
            outcome: ReplayOperationOutcome::from_db(&self.outcome)?,
            resolved_event_id: self
                .resolved_event_id
                .as_deref()
                .map(|value| parse_event_id(value, "replay obligation resolved event_id"))
                .transpose()?,
            artifact: self
                .artifact
                .as_ref()
                .map(WireSealedArtifactRef::to_ref)
                .transpose()?,
            reason: self
                .reason
                .as_deref()
                .map(ArtifactStateReason::from_db)
                .transpose()?,
        })
    }
}

#[derive(Debug, Clone)]
struct ParsedOutbox {
    record: OutboxRecord,
    manifest_order: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireDeliveryResult {
    attempt_count: u32,
    delivery_error: Option<String>,
    max_attempts: u32,
    outbox_id: String,
    schema: String,
    state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRunProvenance {
    canonicalization_algorithm: String,
    canonicalization_version: u8,
    session_id: String,
    parent_run_id: Option<String>,
    branch_id: String,
    tenant_id: String,
    action_id: String,
    objective_id: String,
    task_id: String,
    model_id: String,
    provider_id: String,
    config_digest: String,
    prompt_digest: WireProvenanceDigest,
    input_digest: WireProvenanceDigest,
    output_digest: WireProvenanceDigest,
    tool_digest: WireProvenanceDigest,
    skill_digest: WireProvenanceDigest,
    policy_digest: WireProvenanceDigest,
    sandbox_digest: WireProvenanceDigest,
}

impl WireRunProvenance {
    fn from_canonical_json(value: &CanonicalJson) -> RunExportResult<Self> {
        serde_json::from_str(value.as_str())
            .map_err(|error| RunExportError::InvalidExport(error.to_string()))
    }

    fn canonical_json(&self) -> RunExportResult<CanonicalJson> {
        self.validate()?;
        CanonicalJson::from_value(self).map_err(RunExportError::from)
    }

    fn validate(&self) -> RunExportResult<()> {
        if self.canonicalization_algorithm != CANONICALIZATION_ALGORITHM
            || self.canonicalization_version != CANONICALIZATION_VERSION
        {
            return Err(RunExportError::UnsupportedCanonicalization {
                algorithm: self.canonicalization_algorithm.clone(),
                version: self.canonicalization_version,
            });
        }
        crate::run_ledger::SessionId::new(self.session_id.clone())?;
        crate::run_ledger::BranchId::new(self.branch_id.clone())?;
        crate::run_ledger::TenantId::new(self.tenant_id.clone())?;
        crate::run_ledger::ActionId::new(self.action_id.clone())?;
        crate::run_ledger::ObjectiveId::new(self.objective_id.clone())?;
        crate::run_ledger::TaskId::new(self.task_id.clone())?;
        crate::run_ledger::ModelId::new(self.model_id.clone())?;
        crate::run_ledger::ProviderId::new(self.provider_id.clone())?;
        validate_sha256(&self.config_digest, "provenance config digest")?;
        self.prompt_digest.validate("prompt_digest")?;
        self.input_digest.validate("input_digest")?;
        self.output_digest.validate("output_digest")?;
        self.tool_digest.validate("tool_digest")?;
        self.skill_digest.validate("skill_digest")?;
        self.policy_digest.validate("policy_digest")?;
        self.sandbox_digest.validate("sandbox_digest")?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "availability", content = "value", rename_all = "snake_case")]
enum WireProvenanceDigest {
    Present(String),
    Absent(String),
}

impl WireProvenanceDigest {
    fn validate(&self, label: &str) -> RunExportResult<()> {
        match self {
            Self::Present(value) => validate_sha256(value, label),
            Self::Absent(value) if value == "unavailable" || value == "not_applicable" => Ok(()),
            Self::Absent(_) => Err(RunExportError::InvalidExport(format!(
                "{label} has an unknown absence reason"
            ))),
        }
    }
}

fn canonical_json_to_value(value: &CanonicalJson) -> RunExportResult<Value> {
    serde_json::from_str(value.as_str())
        .map_err(|error| RunExportError::InvalidExport(error.to_string()))
}

fn ensure_wire_event_values(event: &WireEvent) -> RunExportResult<()> {
    if event.sequence == 0 {
        return Err(RunExportError::InvalidExport(
            "event sequence must be positive".to_string(),
        ));
    }
    let kind = parse_event_kind(&event.kind)?;
    let manual_degradation = kind == SessionRunEventKind::ManualAppendDegradedAudit;
    let delivery_result = kind == SessionRunEventKind::DeliveryResult;
    let run_start = kind == SessionRunEventKind::RunStart;
    if manual_degradation {
        if event.append_key.is_none()
            || event.effect_intent != EffectIntent::None.as_str()
            || event.artifact_degradation_reason.is_none()
            || event.public_identity.is_some()
            || event.private_fingerprint.is_some()
            || event.sealed_artifact_ref.is_some()
        {
            return Err(RunExportError::InvalidExport(
                "manual degradation audit fields are invalid".to_string(),
            ));
        }
    } else if delivery_result {
        if event.append_key.is_some()
            || event.artifact_degradation_reason.is_some()
            || event.effect_intent != EffectIntent::None.as_str()
        {
            return Err(RunExportError::InvalidExport(
                "delivery-result event has invalid append fields".to_string(),
            ));
        }
        match (
            event.public_identity.is_some(),
            event.private_fingerprint.is_some(),
            event.sealed_artifact_ref.is_some(),
        ) {
            (true, true, true) | (false, false, false) => {}
            _ => {
                return Err(RunExportError::InvalidExport(
                    "delivery-result evidence fields must be present together".to_string(),
                ));
            }
        }
    } else if run_start {
        // A v4 RunStart may bind its exact sealed artifact directly. It is
        // deliberately not an append, so its append key remains absent while
        // its identity, fingerprint, and reference form one immutable trio.
        if event.append_key.is_some()
            || event.effect_intent != EffectIntent::None.as_str()
            || event.artifact_degradation_reason.is_some()
        {
            return Err(RunExportError::InvalidExport(
                "run-start event has invalid append fields".to_string(),
            ));
        }
        match (
            event.public_identity.is_some(),
            event.private_fingerprint.is_some(),
            event.sealed_artifact_ref.is_some(),
        ) {
            (true, true, true) | (false, false, false) => {}
            _ => {
                return Err(RunExportError::InvalidExport(
                    "run-start exact identity fields must be present together".to_string(),
                ));
            }
        }
    } else {
        if event.artifact_degradation_reason.is_some() {
            return Err(RunExportError::InvalidExport(
                "only manual degradation audit may carry an artifact degradation reason"
                    .to_string(),
            ));
        }
        match (
            event.append_key.is_some(),
            event.public_identity.is_some(),
            event.private_fingerprint.is_some(),
        ) {
            (true, true, true) | (false, false, false) => {}
            _ => {
                return Err(RunExportError::InvalidExport(
                    "append identity fields must be present together".to_string(),
                ));
            }
        }
        if kind == SessionRunEventKind::Append {
            if event.append_key.is_none() {
                return Err(RunExportError::InvalidExport(
                    "append event lacks append identity".to_string(),
                ));
            }
        } else if event.append_key.is_some() {
            return Err(RunExportError::InvalidExport(
                "non-append event has append identity".to_string(),
            ));
        }
    }
    if kind == SessionRunEventKind::RunStart {
        if event.run_provenance.is_none() || event.append_key.is_some() {
            return Err(RunExportError::InvalidExport(
                "run-start fields are invalid".to_string(),
            ));
        }
    } else if event.run_provenance.is_some() {
        return Err(RunExportError::InvalidExport(
            "only run-start may contain run provenance".to_string(),
        ));
    }
    if event.effect_intent != EffectIntent::None.as_str() && event.sealed_artifact_ref.is_none() {
        return Err(RunExportError::InvalidExport(
            "effect-capable event lacks sealed artifact reference".to_string(),
        ));
    }
    Ok(())
}

fn ensure_wire_outbox_shape(entry: &WireOutbox) -> RunExportResult<()> {
    if entry.max_attempts == 0
        || entry.attempt_count > entry.max_attempts
        || entry.lease_generation != u64::from(entry.attempt_count)
    {
        return Err(RunExportError::InvalidExport(
            "outbox attempt bounds or lease generation are invalid".to_string(),
        ));
    }
    let state = parse_outbox_state(&entry.state)?;
    match state {
        OutboxState::Acknowledged => {
            if entry.attempt_count == 0
                || entry.delivery_receipt.is_none()
                || entry.delivery_error.is_some()
            {
                return Err(RunExportError::InvalidExport(
                    "acknowledged outbox row lacks receipt invariants".to_string(),
                ));
            }
        }
        OutboxState::DeadLettered => {
            if entry.attempt_count != entry.max_attempts
                || entry.delivery_receipt.is_some()
                || entry.delivery_error.is_none()
            {
                return Err(RunExportError::InvalidExport(
                    "dead-lettered outbox row lacks terminal error invariants".to_string(),
                ));
            }
        }
        OutboxState::Ready => {
            if entry.attempt_count >= entry.max_attempts
                || entry.delivery_receipt.is_some()
                || entry.delivery_error.is_some()
            {
                return Err(RunExportError::InvalidExport(
                    "ready outbox row contains terminal or exhausted state".to_string(),
                ));
            }
        }
        OutboxState::Leased => {
            if entry.attempt_count == 0
                || entry.delivery_receipt.is_some()
                || entry.delivery_error.is_some()
            {
                return Err(RunExportError::InvalidExport(
                    "leased outbox row lacks active-attempt invariants".to_string(),
                ));
            }
        }
    }
    Ok(())
}

fn validate_resolved_mode(mode: &ResolvedExecutionMode) -> RunExportResult<()> {
    let valid_mode = matches!(
        mode.mode.as_str(),
        "manual" | "reviewed" | "autonomous_local" | "ci" | "remote_worker"
    );
    let valid_source = matches!(
        mode.source.as_str(),
        "built_in_default" | "profile" | "config_file" | "environment" | "cli"
    );
    if valid_mode && valid_source {
        Ok(())
    } else {
        Err(RunExportError::InvalidExport(
            "execution mode is not a closed variant".to_string(),
        ))
    }
}

#[cfg(test)]
mod tests;
