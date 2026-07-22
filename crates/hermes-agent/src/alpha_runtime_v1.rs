//! Certified, durable AlphaRuntimeV1 delivery.
//!
//! The public ledger envelope intentionally contains no task text. The task is
//! sealed before the intent becomes visible, exact-unsealed only by the
//! certified adapter, and represented in the downstream job by an immutable
//! receipt/ref binding rather than a plaintext replay copy.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use hermes_core::execution_mode::ResolvedExecutionMode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

use crate::artifact_lifecycle::{
    ArtifactAppendCoordinator, ArtifactAppendOutcome, ArtifactAppendRequest, ArtifactLifecycleError,
};
use crate::run_ledger::{
    delivery_result_evidence_identity, delivery_result_exact_evidence_identity, ActionId,
    AppendIdentity, AppendKey, ArtifactDegradationReason, BranchId, CanonicalJson,
    CertifiedOutboxDestinationHandler, ClaimNext, DeliveryCompletion, DeliveryCompletionReceipt,
    DeliveryEvidence, DeliveryFailureReason, Destination, DigestAbsence, DurableDeliveryReceipt,
    EffectIntent, EventId, ModelId, ObjectiveId, OutboxLease, OutboxRouteRegistry, OutboxRowId,
    OutboxState, ProvenanceDigest, ProviderId, RunExit, RunId, RunLedger, RunOwnerId,
    RunProvenance, RunStart, RunStatus, SealedArtifactRef, SessionId, Sha256Digest, TaskId,
    TenantId, TerminalReason,
};
use crate::sealed_artifact::{
    prepare_artifact, prepare_exact_runtime_artifact_bound, ArtifactSealer,
    ExactArtifactAuthorization, ExactRuntimeEvidenceSchema, FilesystemSealedArtifactStore,
    LedgerEvidenceRedactor, OsArtifactKeyManager, SealedArtifactError,
    SealedArtifactRef as ArtifactSealedArtifactRef,
};

pub const ALPHA_RUNTIME_V1_ENVELOPE_SCHEMA: &str = "hermes.alpha-runtime-v1/envelope/v1";
const ALPHA_RUNTIME_V1_PRIVATE_TASK_SCHEMA: &str = "hermes.alpha-runtime-v1/private-task/v1";
const ALPHA_RUNTIME_V1_JOB_SCHEMA: &str = "hermes.alpha-runtime-v1/background-job/v1";
const ALPHA_RUNTIME_V1_RECEIPT_SCHEMA: &str = "hermes.alpha-runtime-v1/dedupe-receipt/v1";
const ALPHA_RUNTIME_V1_DELIVERY_EVIDENCE_SCHEMA: &str =
    "hermes.alpha-runtime-v1/delivery-evidence/v1";
const RUNTIME_EVIDENCE_SCHEMA: &str = "hermes.runtime-evidence/v1";
const SEALED_TASK_PLACEHOLDER: &str = "[sealed alpha-runtime-v1 task]";
const MAX_TASK_BYTES: usize = 64 * 1024;
const ALPHA_RUNTIME_DIR: &str = "alpha-runtime-v1";
const RECEIPTS_DIR: &str = "receipts";
const BACKGROUND_JOBS_DIR: &str = "background_jobs";

#[derive(Debug, thiserror::Error)]
pub enum AlphaRuntimeV1Error {
    #[error("AlphaRuntimeV1 loop identity is not allowlisted: {0}")]
    InvalidLoopId(String),
    #[error("AlphaRuntimeV1 event type is not allowlisted: {0}")]
    InvalidEventType(String),
    #[error("AlphaRuntimeV1 envelope is invalid: {0}")]
    InvalidEnvelope(String),
    #[error("AlphaRuntimeV1 task must not be empty")]
    EmptyTask,
    #[error("AlphaRuntimeV1 task exceeds {MAX_TASK_BYTES} bytes")]
    TaskTooLarge,
    #[error("AlphaRuntimeV1 task contains a NUL byte")]
    TaskContainsNul,
    #[error("AlphaRuntimeV1 sealed intent was manually degraded: {}", .0.as_str())]
    ManualIntentDegraded(ArtifactDegradationReason),
    #[error("AlphaRuntimeV1 outbox invariant failed: {0}")]
    Invariant(String),
    #[error("AlphaRuntimeV1 durable path is unsafe: {0}")]
    UnsafePath(PathBuf),
    #[error("AlphaRuntimeV1 durable publication conflicts with existing data: {0}")]
    PublicationConflict(PathBuf),
    #[error("AlphaRuntimeV1 I/O failed at {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error(transparent)]
    Ledger(#[from] crate::run_ledger::LedgerError),
    #[error(transparent)]
    Artifact(#[from] SealedArtifactError),
    #[error(transparent)]
    Lifecycle(#[from] ArtifactLifecycleError),
    #[error("AlphaRuntimeV1 JSON failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlphaRuntimeV1LoopId {
    PrimaryObjectiveLoop,
    SecondaryMonitorLoop,
    ResearchImprovementLoop,
}

impl AlphaRuntimeV1LoopId {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PrimaryObjectiveLoop => "primary-objective-loop",
            Self::SecondaryMonitorLoop => "secondary-monitor-loop",
            Self::ResearchImprovementLoop => "research-improvement-loop",
        }
    }

    fn parse(value: &str) -> Result<Self, AlphaRuntimeV1Error> {
        match value.trim() {
            "primary-objective-loop" => Ok(Self::PrimaryObjectiveLoop),
            "secondary-monitor-loop" => Ok(Self::SecondaryMonitorLoop),
            "research-improvement-loop" => Ok(Self::ResearchImprovementLoop),
            other => Err(AlphaRuntimeV1Error::InvalidLoopId(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlphaRuntimeV1EventType {
    Tick,
    Manual,
    Scheduled,
    Trigger,
    Recovery,
}

impl AlphaRuntimeV1EventType {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tick => "tick",
            Self::Manual => "manual",
            Self::Scheduled => "scheduled",
            Self::Trigger => "trigger",
            Self::Recovery => "recovery",
        }
    }

    fn parse(value: &str) -> Result<Self, AlphaRuntimeV1Error> {
        match value.trim() {
            "tick" => Ok(Self::Tick),
            "manual" => Ok(Self::Manual),
            "scheduled" => Ok(Self::Scheduled),
            "trigger" => Ok(Self::Trigger),
            "recovery" => Ok(Self::Recovery),
            other => Err(AlphaRuntimeV1Error::InvalidEventType(other.to_string())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AlphaRuntimeV1Variant {
    BackgroundJob,
}

impl AlphaRuntimeV1Variant {
    const fn as_str(self) -> &'static str {
        match self {
            Self::BackgroundJob => "background_job",
        }
    }

    fn parse(value: &str) -> Result<Self, AlphaRuntimeV1Error> {
        match value {
            "background_job" => Ok(Self::BackgroundJob),
            other => Err(AlphaRuntimeV1Error::InvalidEnvelope(format!(
                "variant {other:?} is not allowlisted"
            ))),
        }
    }
}

/// Closed public projection of a mission request. It is deliberately free of
/// task text and is the sole payload accepted by `EffectIntent::AlphaRuntimeV1`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlphaRuntimeV1Envelope {
    loop_id: AlphaRuntimeV1LoopId,
    event_type: AlphaRuntimeV1EventType,
    variant: AlphaRuntimeV1Variant,
}

impl AlphaRuntimeV1Envelope {
    fn new(loop_id: AlphaRuntimeV1LoopId, event_type: AlphaRuntimeV1EventType) -> Self {
        Self {
            loop_id,
            event_type,
            variant: AlphaRuntimeV1Variant::BackgroundJob,
        }
    }

    pub const fn loop_id(&self) -> AlphaRuntimeV1LoopId {
        self.loop_id
    }

    pub const fn event_type(&self) -> AlphaRuntimeV1EventType {
        self.event_type
    }

    pub const fn variant(&self) -> AlphaRuntimeV1Variant {
        self.variant
    }

    fn to_value(&self) -> Value {
        json!({
            "event_type": self.event_type.as_str(),
            "loop_id": self.loop_id.as_str(),
            "schema": ALPHA_RUNTIME_V1_ENVELOPE_SCHEMA,
            "variant": self.variant.as_str(),
        })
    }

    fn from_value(value: &Value) -> Result<Self, AlphaRuntimeV1Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawEnvelope {
            schema: String,
            variant: String,
            loop_id: String,
            event_type: String,
        }

        let raw: RawEnvelope = serde_json::from_value(value.clone()).map_err(|error| {
            AlphaRuntimeV1Error::InvalidEnvelope(format!("closed envelope decode failed: {error}"))
        })?;
        if raw.schema != ALPHA_RUNTIME_V1_ENVELOPE_SCHEMA {
            return Err(AlphaRuntimeV1Error::InvalidEnvelope(format!(
                "schema {:?} is not allowlisted",
                raw.schema
            )));
        }
        Ok(Self {
            loop_id: AlphaRuntimeV1LoopId::parse(&raw.loop_id)?,
            event_type: AlphaRuntimeV1EventType::parse(&raw.event_type)?,
            variant: AlphaRuntimeV1Variant::parse(&raw.variant)?,
        })
    }
}

impl Serialize for AlphaRuntimeV1Envelope {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.to_value().serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AlphaRuntimeV1Envelope {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;
        Self::from_value(&value).map_err(serde::de::Error::custom)
    }
}

/// Validated closed input to a durable AlphaRuntimeV1 enqueue operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlphaRuntimeV1Request {
    envelope: AlphaRuntimeV1Envelope,
    task: String,
}

impl AlphaRuntimeV1Request {
    pub fn new(
        loop_id: impl AsRef<str>,
        event_type: impl AsRef<str>,
        task: impl Into<String>,
    ) -> Result<Self, AlphaRuntimeV1Error> {
        let task = task.into();
        if task.trim().is_empty() {
            return Err(AlphaRuntimeV1Error::EmptyTask);
        }
        if task.len() > MAX_TASK_BYTES {
            return Err(AlphaRuntimeV1Error::TaskTooLarge);
        }
        if task.contains('\0') {
            return Err(AlphaRuntimeV1Error::TaskContainsNul);
        }
        Ok(Self {
            envelope: AlphaRuntimeV1Envelope::new(
                AlphaRuntimeV1LoopId::parse(loop_id.as_ref())?,
                AlphaRuntimeV1EventType::parse(event_type.as_ref())?,
            ),
            task,
        })
    }

    pub fn envelope(&self) -> &AlphaRuntimeV1Envelope {
        &self.envelope
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AlphaRuntimeV1EnqueueReceipt {
    pub run_id: RunId,
    pub event_id: EventId,
    pub outbox_id: OutboxRowId,
    pub envelope: AlphaRuntimeV1Envelope,
}

/// Public metadata for a newly durable background job. It is safe to use for
/// scheduling and reporting and contains no task text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AlphaRuntimeV1BackgroundJob {
    job_id: String,
    run_id: RunId,
    event_id: EventId,
    idempotency_key: String,
    envelope: AlphaRuntimeV1Envelope,
    sealed_artifact_ref: SealedArtifactRef,
    status_path: PathBuf,
    log_path: PathBuf,
}

impl AlphaRuntimeV1BackgroundJob {
    pub fn id(&self) -> &str {
        &self.job_id
    }

    pub fn status_path(&self) -> &Path {
        &self.status_path
    }

    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    fn from_lease(
        state_root: &Path,
        lease: &OutboxLease,
        envelope: AlphaRuntimeV1Envelope,
    ) -> Self {
        let idempotency_key = lease.idempotency_key.as_str().to_string();
        let job_id = deterministic_job_id(&idempotency_key);
        let jobs_dir = state_root.join(BACKGROUND_JOBS_DIR);
        let status_path = jobs_dir.join(format!("{job_id}.json"));
        let log_path = jobs_dir.join(format!("{job_id}.log"));
        Self {
            job_id,
            run_id: lease.run_id,
            event_id: lease.event_id,
            idempotency_key,
            envelope,
            sealed_artifact_ref: lease.sealed_artifact_ref.clone(),
            status_path,
            log_path,
        }
    }

    fn status_value(&self) -> Value {
        json!({
            "alpha_runtime_v1": {
                "event_id": self.event_id.to_string(),
                "idempotency_key": self.idempotency_key,
                "public_envelope": self.envelope,
                "run_id": self.run_id.to_string(),
                "schema": ALPHA_RUNTIME_V1_JOB_SCHEMA,
                "sealed_artifact_ref": SealedArtifactRefWire::from(&self.sealed_artifact_ref),
            },
            "attempts": 0,
            "created_at": chrono::Utc::now().to_rfc3339(),
            "exit_code": Value::Null,
            "finished_at": Value::Null,
            "id": self.job_id,
            "log_path": self.log_path,
            "pid": Value::Null,
            "started_at": Value::Null,
            "status": "queued",
            "task": SEALED_TASK_PLACEHOLDER,
        })
    }

    pub fn from_status_value(
        state_root: &Path,
        status_path: &Path,
        value: &Value,
    ) -> Result<Option<Self>, AlphaRuntimeV1Error> {
        let object = value.as_object().ok_or_else(|| {
            AlphaRuntimeV1Error::InvalidEnvelope("background status is not an object".to_string())
        })?;
        let Some(metadata) = object.get("alpha_runtime_v1") else {
            return Ok(None);
        };
        let raw: AlphaRuntimeV1BackgroundJobWire = serde_json::from_value(metadata.clone())
            .map_err(|error| {
                AlphaRuntimeV1Error::InvalidEnvelope(format!(
                    "AlphaRuntimeV1 background metadata decode failed: {error}"
                ))
            })?;
        if raw.schema != ALPHA_RUNTIME_V1_JOB_SCHEMA {
            return Err(AlphaRuntimeV1Error::InvalidEnvelope(format!(
                "background job schema {:?} is not allowlisted",
                raw.schema
            )));
        }
        let job_id = required_string(object, "id")?;
        let task_placeholder = required_string(object, "task")?;
        if task_placeholder != SEALED_TASK_PLACEHOLDER {
            return Err(AlphaRuntimeV1Error::InvalidEnvelope(
                "AlphaRuntimeV1 job stores plaintext task material".to_string(),
            ));
        }
        let run_id = RunId::from_str(&raw.run_id).map_err(|error| {
            AlphaRuntimeV1Error::InvalidEnvelope(format!(
                "invalid run id in background job: {error}"
            ))
        })?;
        let event_id = EventId::from_str(&raw.event_id).map_err(|error| {
            AlphaRuntimeV1Error::InvalidEnvelope(format!(
                "invalid event id in background job: {error}"
            ))
        })?;
        validate_idempotency_key(&raw.idempotency_key)?;
        let envelope = AlphaRuntimeV1Envelope::from_value(&raw.public_envelope)?;
        let sealed_artifact_ref = raw.sealed_artifact_ref.into_ledger()?;
        let expected_job_id = deterministic_job_id(&raw.idempotency_key);
        if job_id != expected_job_id {
            return Err(AlphaRuntimeV1Error::InvalidEnvelope(
                "background job id does not bind its idempotency key".to_string(),
            ));
        }
        let jobs_dir = ensure_private_child_directory(state_root, BACKGROUND_JOBS_DIR)?;
        let expected_status_path = jobs_dir.join(format!("{expected_job_id}.json"));
        if status_path != expected_status_path.as_path() {
            return Err(AlphaRuntimeV1Error::UnsafePath(status_path.to_path_buf()));
        }
        let metadata =
            fs::symlink_metadata(status_path).map_err(|source| AlphaRuntimeV1Error::Io {
                path: status_path.to_path_buf(),
                source,
            })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() || !is_owner_only(&metadata) {
            return Err(AlphaRuntimeV1Error::UnsafePath(status_path.to_path_buf()));
        }
        let log_path = jobs_dir.join(format!("{expected_job_id}.log"));
        Ok(Some(Self {
            job_id,
            run_id,
            event_id,
            idempotency_key: raw.idempotency_key,
            envelope,
            sealed_artifact_ref,
            status_path: expected_status_path,
            log_path,
        }))
    }

    fn same_binding(&self, other: &Self) -> bool {
        self.job_id == other.job_id
            && self.run_id == other.run_id
            && self.event_id == other.event_id
            && self.idempotency_key == other.idempotency_key
            && self.envelope == other.envelope
            && self.sealed_artifact_ref == other.sealed_artifact_ref
            && self.status_path == other.status_path
            && self.log_path == other.log_path
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct AlphaRuntimeV1DispatchSummary {
    pub claimed: usize,
    pub acknowledged: usize,
    pub retryable: usize,
    pub dead_lettered: usize,
    pub rejected: usize,
    pub empty: bool,
    #[serde(skip)]
    newly_durable_jobs: Vec<AlphaRuntimeV1BackgroundJob>,
}

impl Default for AlphaRuntimeV1DispatchSummary {
    fn default() -> Self {
        Self {
            claimed: 0,
            acknowledged: 0,
            retryable: 0,
            dead_lettered: 0,
            rejected: 0,
            empty: false,
            newly_durable_jobs: Vec::new(),
        }
    }
}

impl AlphaRuntimeV1DispatchSummary {
    pub fn newly_durable_jobs(&self) -> &[AlphaRuntimeV1BackgroundJob] {
        &self.newly_durable_jobs
    }
}

pub(crate) fn enqueue(
    ledger: &RunLedger,
    keys: &OsArtifactKeyManager,
    store: &FilesystemSealedArtifactStore,
    mode: ResolvedExecutionMode,
    request: AlphaRuntimeV1Request,
) -> Result<AlphaRuntimeV1EnqueueReceipt, AlphaRuntimeV1Error> {
    let run_id = RunId::new();
    let envelope = request.envelope.clone();
    let task = request.task;
    let public_payload = CanonicalJson::from_value(&envelope.to_value())?;
    let config_digest = canonical_digest(&json!({
        "schema": ALPHA_RUNTIME_V1_ENVELOPE_SCHEMA,
        "variant": AlphaRuntimeV1Variant::BackgroundJob.as_str(),
    }))?;
    let provenance = RunProvenance::new(
        SessionId::new(format!("alpha-runtime-v1-{run_id}"))?,
        None,
        BranchId::new("main")?,
        TenantId::new("local")?,
        ActionId::new("alpha-runtime-v1")?,
        ObjectiveId::new(format!("alpha-runtime-v1-{}", envelope.loop_id().as_str()))?,
        TaskId::new(format!(
            "alpha-runtime-v1-{}-{run_id}",
            envelope.event_type().as_str()
        ))?,
        ModelId::new("alpha-runtime-v1")?,
        ProviderId::new("hermes-agent")?,
        config_digest,
        ProvenanceDigest::absent(DigestAbsence::Unavailable),
        ProvenanceDigest::absent(DigestAbsence::Unavailable),
        ProvenanceDigest::absent(DigestAbsence::Unavailable),
        ProvenanceDigest::absent(DigestAbsence::Unavailable),
        ProvenanceDigest::absent(DigestAbsence::Unavailable),
        ProvenanceDigest::absent(DigestAbsence::Unavailable),
        ProvenanceDigest::absent(DigestAbsence::Unavailable),
    );
    let owner_id = RunOwnerId::new(format!("alpha-runtime-v1-{}", uuid::Uuid::new_v4()))?;
    let created = ledger.create_run_and_start(
        mode,
        owner_id,
        RunStart::new(
            run_id,
            provenance,
            CanonicalJson::from_value(&json!({
                "envelope": envelope.to_value(),
                "schema": "hermes.alpha-runtime-v1/run-start/v1",
            }))?,
        ),
    )?;
    let private_task = json!({
        "envelope": envelope.to_value(),
        "schema": ALPHA_RUNTIME_V1_PRIVATE_TASK_SCHEMA,
        "task": task,
    });
    let safe_task = runtime_evidence(
        "alpha-runtime-v1-task",
        "alpha-runtime-v1",
        "request",
        "not_applicable",
        &private_task,
    );
    let append_key = AppendKey::new(format!("alpha-runtime-v1-intent-{run_id}"))?;
    let coordinator = ArtifactAppendCoordinator::new_with_filesystem_admission(ledger, keys, store);
    let append = coordinator.append_bound(
        &ArtifactAppendRequest {
            mode,
            owner_lease: &created.owner_lease,
            append_key,
            effect_intent: EffectIntent::AlphaRuntimeV1,
            public_payload,
            raw_artifact: &safe_task,
            redactor: &LedgerEvidenceRedactor,
        },
        &private_task,
    );
    let append = match append {
        Ok(append) => append,
        Err(error) => {
            let _ = finalize_enqueue_failure(ledger, mode, &created.owner_lease);
            return Err(error.into());
        }
    };
    let committed = match append {
        ArtifactAppendOutcome::Committed(receipt) => receipt,
        ArtifactAppendOutcome::InFlight { .. } => {
            let _ = finalize_enqueue_failure(ledger, mode, &created.owner_lease);
            return Err(AlphaRuntimeV1Error::Invariant(
                "a newly-created AlphaRuntimeV1 run did not commit its sealed intent".to_string(),
            ));
        }
        ArtifactAppendOutcome::ManualDegraded { reason, .. } => {
            let _ = finalize_enqueue_failure(ledger, mode, &created.owner_lease);
            return Err(AlphaRuntimeV1Error::ManualIntentDegraded(reason));
        }
    };
    if committed.outbox_ids.len() != 1 {
        let _ = finalize_enqueue_failure(ledger, mode, &created.owner_lease);
        return Err(AlphaRuntimeV1Error::Invariant(
            "AlphaRuntimeV1 intent did not derive exactly one required outbox row".to_string(),
        ));
    }
    ledger.finalize_run_exit(
        mode,
        &created.owner_lease,
        RunExit::new(
            TerminalReason::Completed,
            CanonicalJson::from_value(&json!({
                "schema": "hermes.alpha-runtime-v1/run-exit/v1",
                "status": "pending_delivery",
            }))?,
        ),
    )?;
    Ok(AlphaRuntimeV1EnqueueReceipt {
        run_id,
        event_id: committed.event_id,
        outbox_id: committed.outbox_ids[0],
        envelope,
    })
}

pub(crate) fn dispatch(
    ledger: &RunLedger,
    keys: Arc<OsArtifactKeyManager>,
    store: Arc<FilesystemSealedArtifactStore>,
    state_root: &Path,
    mode: ResolvedExecutionMode,
    max_claims: usize,
) -> Result<AlphaRuntimeV1DispatchSummary, AlphaRuntimeV1Error> {
    let adapter = Arc::new(AlphaRuntimeV1Adapter::new(state_root, keys, store)?);
    let routes = adapter.certified_routes()?;
    let mut summary = AlphaRuntimeV1DispatchSummary::default();
    for _ in 0..max_claims.max(1) {
        match ledger.claim_next(mode, &routes)? {
            ClaimNext::Empty => {
                summary.empty = true;
                break;
            }
            ClaimNext::Rejected { state, .. } => {
                summary.rejected = summary.rejected.saturating_add(1);
                classify_outbox_state(&mut summary, state);
            }
            ClaimNext::Claimed(lease) => {
                summary.claimed = summary.claimed.saturating_add(1);
                let completion = match routes.deliver_leased(&lease) {
                    Ok(receipt) => DeliveryCompletion::Acknowledged(receipt),
                    Err(reason) => DeliveryCompletion::Nacked(reason),
                };
                let completed = adapter.complete_with_evidence(ledger, mode, &lease, completion)?;
                classify_outbox_state(&mut summary, completed.state);
                if completed.state == OutboxState::Acknowledged {
                    if let Some(job) = adapter.take_new_job(&lease.idempotency_key) {
                        summary.newly_durable_jobs.push(job);
                    }
                }
            }
        }
    }
    Ok(summary)
}

pub(crate) fn resolve_background_job_task(
    keys: &OsArtifactKeyManager,
    store: &FilesystemSealedArtifactStore,
    state_root: &Path,
    job: &AlphaRuntimeV1BackgroundJob,
) -> Result<String, AlphaRuntimeV1Error> {
    let receipt = read_receipt(state_root, &job.idempotency_key)?;
    if !receipt.matches_job(job) {
        return Err(AlphaRuntimeV1Error::Invariant(
            "immutable AlphaRuntimeV1 receipt does not match the queued job".to_string(),
        ));
    }
    let reference = receipt.sealed_artifact_ref.clone().into_ledger()?;
    let envelope = AlphaRuntimeV1Envelope::from_value(&receipt.public_envelope)?;
    unseal_task(keys, store, &reference, &envelope)
}

struct AlphaRuntimeV1Adapter {
    state_root: PathBuf,
    keys: Arc<OsArtifactKeyManager>,
    store: Arc<FilesystemSealedArtifactStore>,
    newly_durable_jobs: Mutex<BTreeMap<String, AlphaRuntimeV1BackgroundJob>>,
}

impl AlphaRuntimeV1Adapter {
    fn new(
        state_root: &Path,
        keys: Arc<OsArtifactKeyManager>,
        store: Arc<FilesystemSealedArtifactStore>,
    ) -> Result<Self, AlphaRuntimeV1Error> {
        ensure_safe_parent_directory(state_root)?;
        ensure_private_child_directory(state_root, BACKGROUND_JOBS_DIR)?;
        let alpha_dir = ensure_private_child_directory(state_root, ALPHA_RUNTIME_DIR)?;
        ensure_private_child_directory(&alpha_dir, RECEIPTS_DIR)?;
        Ok(Self {
            state_root: state_root.to_path_buf(),
            keys,
            store,
            newly_durable_jobs: Mutex::new(BTreeMap::new()),
        })
    }

    fn certified_routes(self: &Arc<Self>) -> Result<OutboxRouteRegistry, AlphaRuntimeV1Error> {
        let mut routes = OutboxRouteRegistry::default();
        routes.register_certified(CertifiedOutboxDestinationHandler::alpha_runtime_v1(
            self.clone(),
        )?)?;
        Ok(routes)
    }

    fn deliver_claim(
        &self,
        lease: &OutboxLease,
    ) -> Result<DurableDeliveryReceipt, DeliveryFailureReason> {
        let envelope = lease
            .public_payload
            .as_value()
            .map_err(|_| DeliveryFailureReason::InvalidVariant)
            .and_then(|value| {
                AlphaRuntimeV1Envelope::from_value(&value)
                    .map_err(|_| DeliveryFailureReason::InvalidVariant)
            })?;
        if lease.destination != Destination::AlphaRuntimeV1 {
            return Err(DeliveryFailureReason::MissingRoute);
        }
        let artifact_ref = ArtifactSealedArtifactRef::try_from(&lease.sealed_artifact_ref)
            .map_err(|_| DeliveryFailureReason::SealedTaskMismatch)?;
        let task = unseal_task_exact(&self.keys, &self.store, &artifact_ref, &envelope)
            .map_err(map_task_read_failure)?;
        let job = AlphaRuntimeV1BackgroundJob::from_lease(&self.state_root, lease, envelope);
        let publication = create_or_verify_job_and_receipt(&self.state_root, &job, &task)
            .map_err(map_durable_write_failure)?;
        if publication.job_created || publication.receipt_repaired {
            self.newly_durable_jobs
                .lock()
                .map_err(|_| DeliveryFailureReason::DurableStoreFailure)?
                .insert(lease.idempotency_key.as_str().to_string(), job);
        }
        DurableDeliveryReceipt::new(durable_receipt_value(lease.idempotency_key.as_str()))
            .map_err(|_| DeliveryFailureReason::DurableStoreFailure)
    }

    fn take_new_job(
        &self,
        idempotency_key: &crate::run_ledger::DownstreamIdempotencyKey,
    ) -> Option<AlphaRuntimeV1BackgroundJob> {
        self.newly_durable_jobs
            .lock()
            .ok()
            .and_then(|mut jobs| jobs.remove(idempotency_key.as_str()))
    }

    fn complete_with_evidence(
        &self,
        ledger: &RunLedger,
        mode: ResolvedExecutionMode,
        lease: &OutboxLease,
        completion: DeliveryCompletion,
    ) -> Result<DeliveryCompletionReceipt, AlphaRuntimeV1Error> {
        let private = json!({
            "attempt_count": lease.attempt_count,
            "completion": match &completion {
                DeliveryCompletion::Acknowledged(_) => "acknowledged",
                DeliveryCompletion::Nacked(_) => "nacked",
            },
            "delivery_failure": match &completion {
                DeliveryCompletion::Acknowledged(_) => Value::Null,
                DeliveryCompletion::Nacked(reason) => Value::String(reason.as_str().to_string()),
            },
            "delivery_receipt": match &completion {
                DeliveryCompletion::Acknowledged(receipt) => {
                    Value::String(receipt.as_str().to_string())
                }
                DeliveryCompletion::Nacked(_) => Value::Null,
            },
            "event_id": lease.event_id.to_string(),
            "idempotency_key": lease.idempotency_key.as_str(),
            "outbox_id": lease.outbox_id.to_string(),
            "run_id": lease.run_id.to_string(),
            "schema": ALPHA_RUNTIME_V1_DELIVERY_EVIDENCE_SCHEMA,
            "source_sealed_artifact_ref": SealedArtifactRefWire::from(&lease.sealed_artifact_ref),
        });
        let metadata_safe = json!({
            "identity": delivery_result_evidence_identity(lease, &completion),
            "private": private,
        });
        let exact_identity = delivery_result_exact_evidence_identity(lease, &completion);
        let exact_private = json!({
            "evidence_id": lease.outbox_id.to_string(),
            "evidence_subject": exact_identity["evidence_subject"].clone(),
            "payload": metadata_safe["private"].clone(),
            "phase": "result",
            "schema": RUNTIME_EVIDENCE_SCHEMA,
        });
        let exact_safe = json!({
            "identity": exact_identity,
            "private": exact_private,
        });
        let fingerprint_key = self.keys.active_fingerprint_key()?;
        // Delivery completion is never blocked merely because a closed exact
        // transcript is unsafe. Seal the existing credential-redacted evidence
        // instead; its typed metadata fidelity atomically downgrades the run.
        let (prepared, exact_private) = match prepare_exact_runtime_artifact_bound(
            ExactRuntimeEvidenceSchema::DeliveryResultV1,
            &exact_safe,
            &exact_safe["private"],
            &LedgerEvidenceRedactor,
            &fingerprint_key,
        ) {
            Ok(prepared) => (prepared, true),
            Err(_) => (
                prepare_artifact(&metadata_safe, &LedgerEvidenceRedactor, &fingerprint_key)?,
                false,
            ),
        };
        let pending =
            ArtifactSealer::new(self.keys.as_ref(), self.store.as_ref()).seal(prepared)?;
        pending.commit_with(|identity, reference| {
            let identity = AppendIdentity::try_from(&identity)?;
            let reference = SealedArtifactRef::try_from(&reference)?;
            let evidence = if exact_private {
                DeliveryEvidence::exact_private(identity, reference)
            } else {
                DeliveryEvidence::new(identity, reference)
            };
            Ok::<_, AlphaRuntimeV1Error>(ledger.complete_delivery_with_evidence(
                mode, lease, completion, evidence,
            )?)
        })
    }
}

impl crate::run_ledger::DurableOutboxDestinationHandler for AlphaRuntimeV1Adapter {
    fn destination(&self) -> Destination {
        Destination::AlphaRuntimeV1
    }

    fn deliver(
        &self,
        _idempotency_key: &crate::run_ledger::DownstreamIdempotencyKey,
        _public_payload: &CanonicalJson,
    ) -> Result<DurableDeliveryReceipt, DeliveryFailureReason> {
        // The certified registry invokes `deliver_leased`; a lease-free call
        // cannot satisfy the sealed-reference authorization boundary.
        Err(DeliveryFailureReason::HandlerRejected)
    }

    fn deliver_leased(
        &self,
        lease: &OutboxLease,
    ) -> Result<DurableDeliveryReceipt, DeliveryFailureReason> {
        self.deliver_claim(lease)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SealedArtifactRefWire {
    envelope_digest: String,
    key_id: String,
    schema_version: u32,
    encryption_algorithm: String,
    canonicalization_algorithm: String,
    canonicalization_version: u8,
    redactor_version: String,
}

impl From<&SealedArtifactRef> for SealedArtifactRefWire {
    fn from(value: &SealedArtifactRef) -> Self {
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
}

impl SealedArtifactRefWire {
    fn into_ledger(self) -> Result<SealedArtifactRef, AlphaRuntimeV1Error> {
        let artifact = ArtifactSealedArtifactRef {
            envelope_digest: self.envelope_digest,
            key_id: self.key_id,
            schema_version: self.schema_version,
            encryption_algorithm: self.encryption_algorithm,
            canonicalization_algorithm: self.canonicalization_algorithm,
            canonicalization_version: self.canonicalization_version,
            redactor_version: self.redactor_version,
        };
        Ok(SealedArtifactRef::try_from(&artifact)?)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AlphaRuntimeV1BackgroundJobWire {
    schema: String,
    run_id: String,
    event_id: String,
    idempotency_key: String,
    public_envelope: Value,
    sealed_artifact_ref: SealedArtifactRefWire,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AlphaRuntimeV1ReceiptFile {
    schema: String,
    idempotency_key: String,
    job_id: String,
    run_id: String,
    event_id: String,
    public_envelope: Value,
    sealed_artifact_ref: SealedArtifactRefWire,
}

impl AlphaRuntimeV1ReceiptFile {
    fn from_job(job: &AlphaRuntimeV1BackgroundJob) -> Self {
        Self {
            schema: ALPHA_RUNTIME_V1_RECEIPT_SCHEMA.to_string(),
            idempotency_key: job.idempotency_key.clone(),
            job_id: job.job_id.clone(),
            run_id: job.run_id.to_string(),
            event_id: job.event_id.to_string(),
            public_envelope: job.envelope.to_value(),
            sealed_artifact_ref: SealedArtifactRefWire::from(&job.sealed_artifact_ref),
        }
    }

    fn parse(value: Value) -> Result<Self, AlphaRuntimeV1Error> {
        let receipt: Self = serde_json::from_value(value).map_err(|error| {
            AlphaRuntimeV1Error::InvalidEnvelope(format!("dedupe receipt decode failed: {error}"))
        })?;
        if receipt.schema != ALPHA_RUNTIME_V1_RECEIPT_SCHEMA {
            return Err(AlphaRuntimeV1Error::InvalidEnvelope(
                "dedupe receipt schema is not allowlisted".to_string(),
            ));
        }
        validate_idempotency_key(&receipt.idempotency_key)?;
        if receipt.job_id != deterministic_job_id(&receipt.idempotency_key) {
            return Err(AlphaRuntimeV1Error::InvalidEnvelope(
                "dedupe receipt job id does not bind its idempotency key".to_string(),
            ));
        }
        RunId::from_str(&receipt.run_id).map_err(|error| {
            AlphaRuntimeV1Error::InvalidEnvelope(format!("invalid receipt run id: {error}"))
        })?;
        EventId::from_str(&receipt.event_id).map_err(|error| {
            AlphaRuntimeV1Error::InvalidEnvelope(format!("invalid receipt event id: {error}"))
        })?;
        AlphaRuntimeV1Envelope::from_value(&receipt.public_envelope)?;
        receipt.sealed_artifact_ref.clone().into_ledger()?;
        Ok(receipt)
    }

    fn matches_job(&self, job: &AlphaRuntimeV1BackgroundJob) -> bool {
        self.schema == ALPHA_RUNTIME_V1_RECEIPT_SCHEMA
            && self.idempotency_key == job.idempotency_key
            && self.job_id == job.job_id
            && self.run_id == job.run_id.to_string()
            && self.event_id == job.event_id.to_string()
            && AlphaRuntimeV1Envelope::from_value(&self.public_envelope)
                .map(|envelope| envelope == job.envelope)
                .unwrap_or(false)
            && self
                .sealed_artifact_ref
                .clone()
                .into_ledger()
                .map(|reference| reference == job.sealed_artifact_ref)
                .unwrap_or(false)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BoundTaskEnvelope {
    payload: PrivateTaskPayload,
    replay_fidelity: String,
    source_commitment: String,
    source_commitment_algorithm: String,
    source_commitment_version: u8,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrivateTaskPayload {
    schema: String,
    envelope: Value,
    task: String,
}

struct ExactLeaseArtifactAuthorization {
    expected: ArtifactSealedArtifactRef,
}

impl ExactArtifactAuthorization for ExactLeaseArtifactAuthorization {
    fn authorize_exact_read(
        &self,
        artifact: &ArtifactSealedArtifactRef,
    ) -> Result<(), SealedArtifactError> {
        if artifact == &self.expected {
            Ok(())
        } else {
            Err(SealedArtifactError::Unauthorized(
                "AlphaRuntimeV1 exact artifact authorization rejected a different reference"
                    .to_string(),
            ))
        }
    }
}

fn unseal_task_exact(
    keys: &OsArtifactKeyManager,
    store: &FilesystemSealedArtifactStore,
    artifact_ref: &ArtifactSealedArtifactRef,
    expected_envelope: &AlphaRuntimeV1Envelope,
) -> Result<String, AlphaRuntimeV1Error> {
    let authorization = ExactLeaseArtifactAuthorization {
        expected: artifact_ref.clone(),
    };
    let bytes = ArtifactSealer::new(keys, store).unseal_exact(artifact_ref, &authorization)?;
    let bound: BoundTaskEnvelope = serde_json::from_slice(bytes.as_slice()).map_err(|error| {
        AlphaRuntimeV1Error::InvalidEnvelope(format!(
            "sealed AlphaRuntimeV1 task decode failed: {error}"
        ))
    })?;
    if bound.replay_fidelity != "metadata_only"
        || bound.source_commitment_algorithm != "hmac-sha256"
        || bound.source_commitment_version != 1
        || !is_lowercase_sha256(&bound.source_commitment)
        || bound.payload.schema != ALPHA_RUNTIME_V1_PRIVATE_TASK_SCHEMA
    {
        return Err(AlphaRuntimeV1Error::InvalidEnvelope(
            "sealed AlphaRuntimeV1 task has an invalid private wrapper".to_string(),
        ));
    }
    let actual_envelope = AlphaRuntimeV1Envelope::from_value(&bound.payload.envelope)?;
    if &actual_envelope != expected_envelope {
        return Err(AlphaRuntimeV1Error::Invariant(
            "sealed AlphaRuntimeV1 task does not bind the public envelope".to_string(),
        ));
    }
    if bound.payload.task.trim().is_empty() {
        return Err(AlphaRuntimeV1Error::EmptyTask);
    }
    if bound.payload.task.len() > MAX_TASK_BYTES {
        return Err(AlphaRuntimeV1Error::TaskTooLarge);
    }
    if bound.payload.task.contains('\0') {
        return Err(AlphaRuntimeV1Error::TaskContainsNul);
    }
    Ok(bound.payload.task)
}

fn unseal_task(
    keys: &OsArtifactKeyManager,
    store: &FilesystemSealedArtifactStore,
    reference: &SealedArtifactRef,
    envelope: &AlphaRuntimeV1Envelope,
) -> Result<String, AlphaRuntimeV1Error> {
    let artifact_ref = ArtifactSealedArtifactRef::try_from(reference)?;
    unseal_task_exact(keys, store, &artifact_ref, envelope)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DurablePublication {
    job_created: bool,
    receipt_repaired: bool,
}

fn create_or_verify_job_and_receipt(
    state_root: &Path,
    job: &AlphaRuntimeV1BackgroundJob,
    _task: &str,
) -> Result<DurablePublication, AlphaRuntimeV1Error> {
    // `_task` has already been exact-unsealed and bound above. It intentionally
    // never enters the status record, receipt, log metadata, or public ledger.
    let jobs_dir = ensure_private_child_directory(state_root, BACKGROUND_JOBS_DIR)?;
    let expected_status_path = jobs_dir.join(format!("{}.json", job.job_id));
    if expected_status_path != job.status_path {
        return Err(AlphaRuntimeV1Error::UnsafePath(job.status_path.clone()));
    }
    let job_created = match read_private_json_if_exists(&job.status_path)? {
        Some(value) => {
            let existing = AlphaRuntimeV1BackgroundJob::from_status_value(
                state_root,
                &job.status_path,
                &value,
            )?
            .ok_or_else(|| AlphaRuntimeV1Error::PublicationConflict(job.status_path.clone()))?;
            if !existing.same_binding(job) {
                return Err(AlphaRuntimeV1Error::PublicationConflict(
                    job.status_path.clone(),
                ));
            }
            false
        }
        None => {
            let bytes = serde_json_canonicalizer::to_vec(&job.status_value())?;
            match publish_immutable(&jobs_dir, &job.status_path, &bytes)? {
                ImmutablePublication::Created => true,
                ImmutablePublication::AlreadyExists => {
                    let existing =
                        read_private_json_if_exists(&job.status_path)?.ok_or_else(|| {
                            AlphaRuntimeV1Error::PublicationConflict(job.status_path.clone())
                        })?;
                    let existing = AlphaRuntimeV1BackgroundJob::from_status_value(
                        state_root,
                        &job.status_path,
                        &existing,
                    )?
                    .ok_or_else(|| {
                        AlphaRuntimeV1Error::PublicationConflict(job.status_path.clone())
                    })?;
                    if !existing.same_binding(job) {
                        return Err(AlphaRuntimeV1Error::PublicationConflict(
                            job.status_path.clone(),
                        ));
                    }
                    false
                }
            }
        }
    };
    let receipt_repaired = create_or_verify_receipt(state_root, job)?;
    Ok(DurablePublication {
        job_created,
        receipt_repaired,
    })
}

fn create_or_verify_receipt(
    state_root: &Path,
    job: &AlphaRuntimeV1BackgroundJob,
) -> Result<bool, AlphaRuntimeV1Error> {
    let alpha_dir = ensure_private_child_directory(state_root, ALPHA_RUNTIME_DIR)?;
    let receipts_dir = ensure_private_child_directory(&alpha_dir, RECEIPTS_DIR)?;
    let receipt_path = receipts_dir.join(format!("{}.json", job.idempotency_key));
    let expected = AlphaRuntimeV1ReceiptFile::from_job(job);
    match read_private_json_if_exists(&receipt_path)? {
        Some(value) => {
            if !AlphaRuntimeV1ReceiptFile::parse(value)?.matches_job(job) {
                return Err(AlphaRuntimeV1Error::PublicationConflict(receipt_path));
            }
            Ok(false)
        }
        None => {
            let bytes = serde_json_canonicalizer::to_vec(&expected)?;
            match publish_immutable(&receipts_dir, &receipt_path, &bytes)? {
                ImmutablePublication::Created => Ok(true),
                ImmutablePublication::AlreadyExists => {
                    let existing =
                        read_private_json_if_exists(&receipt_path)?.ok_or_else(|| {
                            AlphaRuntimeV1Error::PublicationConflict(receipt_path.clone())
                        })?;
                    if !AlphaRuntimeV1ReceiptFile::parse(existing)?.matches_job(job) {
                        return Err(AlphaRuntimeV1Error::PublicationConflict(receipt_path));
                    }
                    Ok(false)
                }
            }
        }
    }
}

fn read_receipt(
    state_root: &Path,
    idempotency_key: &str,
) -> Result<AlphaRuntimeV1ReceiptFile, AlphaRuntimeV1Error> {
    validate_idempotency_key(idempotency_key)?;
    let alpha_dir = ensure_private_child_directory(state_root, ALPHA_RUNTIME_DIR)?;
    let receipts_dir = ensure_private_child_directory(&alpha_dir, RECEIPTS_DIR)?;
    let path = receipts_dir.join(format!("{idempotency_key}.json"));
    let value = read_private_json_if_exists(&path)?.ok_or_else(|| {
        AlphaRuntimeV1Error::Invariant(
            "queued AlphaRuntimeV1 job has no durable receipt".to_string(),
        )
    })?;
    AlphaRuntimeV1ReceiptFile::parse(value)
}

fn runtime_evidence(
    evidence_id: &str,
    evidence_subject: &str,
    artifact_kind: &str,
    outcome: &str,
    private: &Value,
) -> Value {
    json!({
        "identity": {
            "artifact_kind": artifact_kind,
            "evidence_id": evidence_id,
            "evidence_subject": evidence_subject,
            "outcome": outcome,
            "replay_fidelity": "credential_redacted",
            "schema": RUNTIME_EVIDENCE_SCHEMA,
        },
        "private": private,
    })
}

fn finalize_enqueue_failure(
    ledger: &RunLedger,
    mode: ResolvedExecutionMode,
    owner_lease: &crate::run_ledger::RunOwnerLease,
) -> Result<RunStatus, AlphaRuntimeV1Error> {
    Ok(ledger.finalize_run_exit(
        mode,
        owner_lease,
        RunExit::new(
            TerminalReason::ExecutionFailure,
            CanonicalJson::from_value(&json!({
                "schema": "hermes.alpha-runtime-v1/run-exit/v1",
                "status": "enqueue_failed",
            }))?,
        ),
    )?)
}

fn classify_outbox_state(summary: &mut AlphaRuntimeV1DispatchSummary, state: OutboxState) {
    match state {
        OutboxState::Acknowledged => {
            summary.acknowledged = summary.acknowledged.saturating_add(1);
        }
        OutboxState::Ready | OutboxState::Leased => {
            summary.retryable = summary.retryable.saturating_add(1);
        }
        OutboxState::DeadLettered => {
            summary.dead_lettered = summary.dead_lettered.saturating_add(1);
        }
    }
}

fn canonical_digest(value: &Value) -> Result<Sha256Digest, AlphaRuntimeV1Error> {
    let bytes = serde_json_canonicalizer::to_vec(value)?;
    Ok(Sha256Digest::new(hex::encode(Sha256::digest(bytes)))?)
}

fn durable_receipt_value(idempotency_key: &str) -> String {
    format!("alpha-runtime-v1:{idempotency_key}")
}

fn deterministic_job_id(idempotency_key: &str) -> String {
    format!("alpha-runtime-v1-{idempotency_key}")
}

fn required_string(object: &Map<String, Value>, key: &str) -> Result<String, AlphaRuntimeV1Error> {
    object
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| AlphaRuntimeV1Error::InvalidEnvelope(format!("missing {key}")))
}

fn validate_idempotency_key(value: &str) -> Result<(), AlphaRuntimeV1Error> {
    if is_lowercase_sha256(value) {
        Ok(())
    } else {
        Err(AlphaRuntimeV1Error::InvalidEnvelope(
            "idempotency key must be a lowercase SHA-256 digest".to_string(),
        ))
    }
}

fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn map_task_read_failure(error: AlphaRuntimeV1Error) -> DeliveryFailureReason {
    match error {
        AlphaRuntimeV1Error::InvalidEnvelope(_)
        | AlphaRuntimeV1Error::Invariant(_)
        | AlphaRuntimeV1Error::EmptyTask
        | AlphaRuntimeV1Error::TaskTooLarge
        | AlphaRuntimeV1Error::TaskContainsNul => DeliveryFailureReason::SealedTaskMismatch,
        AlphaRuntimeV1Error::Artifact(
            SealedArtifactError::Unauthorized(_)
            | SealedArtifactError::InvalidEnvelope(_)
            | SealedArtifactError::CasConflict(_),
        ) => DeliveryFailureReason::SealedTaskMismatch,
        _ => DeliveryFailureReason::DurableStoreFailure,
    }
}

fn map_durable_write_failure(error: AlphaRuntimeV1Error) -> DeliveryFailureReason {
    match error {
        AlphaRuntimeV1Error::PublicationConflict(_) | AlphaRuntimeV1Error::UnsafePath(_) => {
            DeliveryFailureReason::HandlerRejected
        }
        AlphaRuntimeV1Error::InvalidEnvelope(_) | AlphaRuntimeV1Error::Invariant(_) => {
            DeliveryFailureReason::HandlerRejected
        }
        _ => DeliveryFailureReason::DurableStoreFailure,
    }
}

fn ensure_safe_parent_directory(path: &Path) -> Result<(), AlphaRuntimeV1Error> {
    if !path.exists() {
        fs::create_dir_all(path).map_err(|source| AlphaRuntimeV1Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
    }
    let metadata = fs::symlink_metadata(path).map_err(|source| AlphaRuntimeV1Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || !is_owned_by_current_user(&metadata)
    {
        return Err(AlphaRuntimeV1Error::UnsafePath(path.to_path_buf()));
    }
    Ok(())
}

fn ensure_private_child_directory(
    parent: &Path,
    name: &str,
) -> Result<PathBuf, AlphaRuntimeV1Error> {
    ensure_safe_parent_directory(parent)?;
    let path = parent.join(name);
    match fs::create_dir(&path) {
        Ok(()) => set_private_directory_permissions(&path)?,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(source) => {
            return Err(AlphaRuntimeV1Error::Io {
                path: path.clone(),
                source,
            })
        }
    }
    let metadata = fs::symlink_metadata(&path).map_err(|source| AlphaRuntimeV1Error::Io {
        path: path.clone(),
        source,
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() || !is_owner_only(&metadata) {
        return Err(AlphaRuntimeV1Error::UnsafePath(path));
    }
    Ok(path)
}

fn read_private_json_if_exists(path: &Path) -> Result<Option<Value>, AlphaRuntimeV1Error> {
    let bytes = match read_private_file_if_exists(path)? {
        Some(bytes) => bytes,
        None => return Ok(None),
    };
    let value = serde_json::from_slice(&bytes).map_err(|error| {
        AlphaRuntimeV1Error::InvalidEnvelope(format!("durable JSON decode failed: {error}"))
    })?;
    Ok(Some(value))
}

fn read_private_file_if_exists(path: &Path) -> Result<Option<Vec<u8>>, AlphaRuntimeV1Error> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(AlphaRuntimeV1Error::Io {
                path: path.to_path_buf(),
                source,
            })
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() || !is_owner_only(&metadata) {
        return Err(AlphaRuntimeV1Error::UnsafePath(path.to_path_buf()));
    }
    let bytes = fs::read(path).map_err(|source| AlphaRuntimeV1Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    Ok(Some(bytes))
}

enum ImmutablePublication {
    Created,
    AlreadyExists,
}

fn publish_immutable(
    parent: &Path,
    destination: &Path,
    bytes: &[u8],
) -> Result<ImmutablePublication, AlphaRuntimeV1Error> {
    let filename = destination
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AlphaRuntimeV1Error::UnsafePath(destination.to_path_buf()))?;
    let temporary = parent.join(format!(".{filename}.{}.tmp", uuid::Uuid::new_v4()));
    let mut file = create_private_new_file(&temporary)?;
    file.write_all(bytes)
        .map_err(|source| AlphaRuntimeV1Error::Io {
            path: temporary.clone(),
            source,
        })?;
    file.sync_all().map_err(|source| AlphaRuntimeV1Error::Io {
        path: temporary.clone(),
        source,
    })?;
    drop(file);
    let publication = match fs::hard_link(&temporary, destination) {
        Ok(()) => ImmutablePublication::Created,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            ImmutablePublication::AlreadyExists
        }
        Err(source) => {
            let _ = fs::remove_file(&temporary);
            return Err(AlphaRuntimeV1Error::Io {
                path: destination.to_path_buf(),
                source,
            });
        }
    };
    fs::remove_file(&temporary).map_err(|source| AlphaRuntimeV1Error::Io {
        path: temporary,
        source,
    })?;
    sync_directory(parent)?;
    Ok(publication)
}

fn create_private_new_file(path: &Path) -> Result<File, AlphaRuntimeV1Error> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    options
        .open(path)
        .map_err(|source| AlphaRuntimeV1Error::Io {
            path: path.to_path_buf(),
            source,
        })
}

fn sync_directory(path: &Path) -> Result<(), AlphaRuntimeV1Error> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|directory| directory.sync_all())
            .map_err(|source| AlphaRuntimeV1Error::Io {
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

fn set_private_directory_permissions(path: &Path) -> Result<(), AlphaRuntimeV1Error> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| {
            AlphaRuntimeV1Error::Io {
                path: path.to_path_buf(),
                source,
            }
        })
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

#[cfg(unix)]
fn is_owned_by_current_user(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.uid() == unsafe { libc::geteuid() }
}

#[cfg(not(unix))]
fn is_owned_by_current_user(_metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(unix)]
fn is_owner_only(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.mode() & 0o077 == 0 && is_owned_by_current_user(metadata)
}

#[cfg(not(unix))]
fn is_owner_only(_metadata: &fs::Metadata) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use std::fs;

    use hermes_core::execution_mode::{ExecutionMode, ExecutionModeSource, ResolvedExecutionMode};
    use rusqlite::params;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;
    use crate::run_runtime::{EphemeralRunRuntimeFactory, RunRuntime};

    fn mode() -> ResolvedExecutionMode {
        ResolvedExecutionMode {
            mode: ExecutionMode::Manual,
            source: ExecutionModeSource::BuiltInDefault,
        }
    }

    fn request(task: &str) -> AlphaRuntimeV1Request {
        AlphaRuntimeV1Request::new("primary-objective-loop", "manual", task).unwrap()
    }

    #[test]
    fn enqueue_dispatch_seals_task_creates_job_and_acks_once() {
        let root = TempDir::new().unwrap();
        let runtime = RunRuntime::open_for_ephemeral_testing(root.path()).unwrap();
        let task = "secret mission task: do not persist publicly";
        let receipt = runtime
            .enqueue_alpha_runtime_v1(mode(), request(task))
            .unwrap();

        let before = runtime.ledger().outbox_for_run(receipt.run_id).unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].state, OutboxState::Ready);
        assert!(!before[0].public_payload.as_str().contains(task));

        let dispatched = runtime.dispatch_alpha_runtime_v1(mode(), 1).unwrap();
        assert_eq!(dispatched.claimed, 1);
        assert_eq!(dispatched.acknowledged, 1);
        assert_eq!(dispatched.retryable, 0);
        assert_eq!(dispatched.dead_lettered, 0);
        let job = dispatched.newly_durable_jobs().first().unwrap().clone();
        let status = fs::read_to_string(job.status_path()).unwrap();
        assert!(status.contains(SEALED_TASK_PLACEHOLDER));
        assert!(!status.contains(task));
        let receipt_path = root
            .path()
            .join(ALPHA_RUNTIME_DIR)
            .join(RECEIPTS_DIR)
            .join(format!("{}.json", job.idempotency_key));
        assert!(!fs::read_to_string(receipt_path).unwrap().contains(task));
        assert_eq!(
            runtime
                .resolve_alpha_runtime_v1_background_job_task(&job)
                .unwrap(),
            task
        );

        let exported = runtime.publish_run_export(receipt.run_id).unwrap();
        assert!(!fs::read_to_string(exported.path).unwrap().contains(task));
        let after = runtime.ledger().outbox_for_run(receipt.run_id).unwrap();
        assert_eq!(after[0].state, OutboxState::Acknowledged);
        let duplicate = runtime.dispatch_alpha_runtime_v1(mode(), 1).unwrap();
        assert_eq!(duplicate.claimed, 0);
        assert!(duplicate.empty);
    }

    #[test]
    fn crash_after_job_before_receipt_repairs_without_second_job() {
        let root = TempDir::new().unwrap();
        let runtime = RunRuntime::open_for_ephemeral_testing(root.path()).unwrap();
        let task = "repair this sealed task";
        let _receipt = runtime
            .enqueue_alpha_runtime_v1(mode(), request(task))
            .unwrap();
        let dispatched = runtime.dispatch_alpha_runtime_v1(mode(), 1).unwrap();
        let job = dispatched.newly_durable_jobs().first().unwrap().clone();
        let status_before = fs::read(job.status_path()).unwrap();
        let receipt_path = root
            .path()
            .join(ALPHA_RUNTIME_DIR)
            .join(RECEIPTS_DIR)
            .join(format!("{}.json", job.idempotency_key));
        fs::remove_file(&receipt_path).unwrap();

        let repaired = create_or_verify_job_and_receipt(root.path(), &job, task).unwrap();
        assert!(!repaired.job_created);
        assert!(repaired.receipt_repaired);
        assert_eq!(fs::read(job.status_path()).unwrap(), status_before);
        assert!(receipt_path.is_file());
        let duplicate = create_or_verify_job_and_receipt(root.path(), &job, task).unwrap();
        assert!(!duplicate.job_created);
        assert!(!duplicate.receipt_repaired);
    }

    #[test]
    fn restarted_runtime_dispatches_pending_sealed_delivery() {
        let root = TempDir::new().unwrap();
        let factory = EphemeralRunRuntimeFactory::default();
        let initial = factory.open(root.path()).unwrap();
        let receipt = initial
            .enqueue_alpha_runtime_v1(mode(), request("resume after runtime restart"))
            .unwrap();
        drop(initial);

        let restarted = factory.open(root.path()).unwrap();
        let dispatched = restarted.dispatch_alpha_runtime_v1(mode(), 1).unwrap();
        assert_eq!(dispatched.claimed, 1);
        assert_eq!(dispatched.acknowledged, 1);
        assert_eq!(
            restarted.ledger().outbox_for_run(receipt.run_id).unwrap()[0].state,
            OutboxState::Acknowledged
        );
    }

    #[test]
    fn missing_or_invalid_variant_is_nacked_until_dead_lettered() {
        for payload in [
            json!({
                "schema": ALPHA_RUNTIME_V1_ENVELOPE_SCHEMA,
                "loop_id": "primary-objective-loop",
                "event_type": "manual",
                "variant": "not-allowlisted",
            }),
            json!({
                "schema": ALPHA_RUNTIME_V1_ENVELOPE_SCHEMA,
                "loop_id": "primary-objective-loop",
                "event_type": "manual",
            }),
        ] {
            let root = TempDir::new().unwrap();
            let runtime = RunRuntime::open_for_ephemeral_testing(root.path()).unwrap();
            let receipt = runtime
                .enqueue_alpha_runtime_v1(mode(), request("sealed task for invalid route"))
                .unwrap();
            let payload = CanonicalJson::from_value(&payload).unwrap();
            runtime
                .ledger()
                .connection_factory()
                .open()
                .unwrap()
                .execute(
                    "UPDATE session_run_outbox SET public_payload_jcs = ?1 WHERE outbox_id = ?2",
                    params![payload.as_str(), receipt.outbox_id.to_string()],
                )
                .unwrap();

            let dispatched = runtime.dispatch_alpha_runtime_v1(mode(), 3).unwrap();
            assert_eq!(dispatched.claimed, 3);
            assert_eq!(dispatched.retryable, 2);
            assert_eq!(dispatched.dead_lettered, 1);
            assert_eq!(dispatched.acknowledged, 0);
            assert_eq!(
                runtime.ledger().outbox_for_run(receipt.run_id).unwrap()[0].state,
                OutboxState::DeadLettered
            );
        }
    }
}
