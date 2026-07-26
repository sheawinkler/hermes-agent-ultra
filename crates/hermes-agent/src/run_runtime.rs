use hermes_environments::ExecutionContainmentContext;
use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::future::Future;
use std::io::Write;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;

use hermes_core::execution_mode::{ExecutionMode, ResolvedExecutionMode};
use hermes_core::{AgentError, Message, ToolError, ToolSchema};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tokio::sync::{watch, Mutex, RwLock};

use crate::agent_loop::{AgentConfig, ToolEntry, ToolExecutionTrust, ToolRegistry};
use crate::alpha_runtime_v1::{
    self, AlphaRuntimeV1BackgroundJob, AlphaRuntimeV1DispatchSummary, AlphaRuntimeV1EnqueueReceipt,
    AlphaRuntimeV1Error, AlphaRuntimeV1Request,
};
use crate::artifact_lifecycle::{
    ArtifactAppendCoordinator, ArtifactAppendOutcome, ArtifactAppendRequest,
    ReplayCriticalArtifactAppend,
};
use crate::deterministic_replay::{BoundExactArtifactReader, VerifiedRunReplayPlan};
use crate::exact_replay_lifecycle::{ExactReplayLifecycleCoordinator, ExactReplayLifecycleError};
use crate::run_export::{
    canonical_export_bytes_from_snapshot, export_run, verify_run_export, RunExportError,
    RunExportReceipt,
};
use crate::run_ledger::{
    ActionId, AppendKey, AppendReceipt, ArtifactDegradationReason, BranchId, CanonicalJson,
    DigestAbsence, EffectIntent, EventId, LedgerConnectionFactory, LedgerError, ModelId,
    NonResumableEffectLease, ObjectiveId, ProvenanceDigest, ProviderId, ReplayAppendContract,
    ReplayOperationKind, ReplayOperationPhase, RunExecutionCertification, RunExit, RunHeader,
    RunId, RunLedger, RunLedgerConfig, RunOwnerId, RunOwnerLease, RunProvenance, RunStart,
    RunStatus, SessionId, SessionRunEvent, Sha256Digest, TaskId, TenantId, TerminalReason,
};
use crate::sealed_artifact::{
    prepare_artifact_bound, ArtifactCredentialStore, EphemeralArtifactCredentialStore,
    ExactRuntimeEvidenceSchema, FilesystemSealedArtifactStore, LedgerEvidenceRedactor,
    OsArtifactKeyManager, OsArtifactKeyManagerConfig, SealedArtifactError,
    SystemArtifactCredentialStore,
};

const OWNER_LEASE_DURATION: Duration = Duration::from_secs(60);
const OWNER_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(10);
const RUNTIME_EVIDENCE_SCHEMA: &str = "hermes.runtime-evidence/v1";
const RUN_RECORD_INITIALIZATION_MARKER: &str = "initialization-v1.json";
const RUN_RECORD_INITIALIZATION_MARKER_BYTES: &[u8] =
    br#"{"schema":"hermes.agent-run-record-initialization/v1","version":1}"#;

#[derive(Debug, thiserror::Error)]
pub enum RunRuntimeError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    Artifact(#[from] SealedArtifactError),
    #[error(transparent)]
    Export(#[from] RunExportError),
    #[error(transparent)]
    AlphaRuntimeV1(#[from] AlphaRuntimeV1Error),
    #[error(transparent)]
    ExactReplayLifecycle(#[from] ExactReplayLifecycleError),
    #[error("run runtime I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("run runtime canonicalization failed: {0}")]
    Canonicalization(#[from] serde_json::Error),
    #[error("run owner heartbeat failed: {0}")]
    Heartbeat(String),
    #[error("run {run_id} was already finalized")]
    AlreadyFinalized { run_id: RunId },
    #[error("verified replay plan tenant does not match this runtime's authoritative run scope")]
    ReplayPlanTenantMismatch,
    #[error("verified replay plan does not equal this runtime's authoritative ledger snapshot")]
    ReplayPlanSnapshotMismatch,
    #[error(
        "execution containment context does not match resolved mode (expected {expected}, received {received})"
    )]
    ExecutionContainmentModeMismatch {
        expected: ExecutionMode,
        received: ExecutionMode,
    },
    #[error(
        "manual append {append_key:?} degraded because {reason:?}; audit event {audit_event_id} was committed"
    )]
    DegradedManualAppend {
        append_key: AppendKey,
        reason: ArtifactDegradationReason,
        audit_event_id: EventId,
    },
    #[error("invalid effective tool contract: {0}")]
    InvalidToolContract(String),
    #[error("run evidence persistence failed: {0}")]
    Evidence(String),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum GovernedToolExecutionError {
    #[error(transparent)]
    Outcome(#[from] ToolError),
    #[error(transparent)]
    Runtime(#[from] RunRuntimeError),
}

#[derive(Debug, thiserror::Error)]
pub(crate) enum GovernedOAuthRefreshError {
    #[error(transparent)]
    Agent(#[from] AgentError),
    #[error(transparent)]
    Runtime(#[from] RunRuntimeError),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(transparent)]
    Serialization(#[from] serde_json::Error),
}

impl GovernedOAuthRefreshError {
    pub(crate) fn into_agent_error(self) -> AgentError {
        match self {
            Self::Agent(error) => error,
            Self::Runtime(error) => {
                AgentError::Io(format!("governed OAuth refresh runtime failure: {error}"))
            }
            Self::Ledger(error) => {
                AgentError::Io(format!("governed OAuth refresh ledger failure: {error}"))
            }
            Self::Serialization(error) => AgentError::Io(format!(
                "governed OAuth refresh serialization failure: {error}"
            )),
        }
    }
}

impl From<LedgerError> for GovernedToolExecutionError {
    fn from(error: LedgerError) -> Self {
        Self::Runtime(RunRuntimeError::Ledger(error))
    }
}

impl From<serde_json::Error> for GovernedToolExecutionError {
    fn from(error: serde_json::Error) -> Self {
        Self::Runtime(RunRuntimeError::Canonicalization(error))
    }
}

#[derive(Debug, Clone)]
pub struct RunCompletion {
    pub run_id: RunId,
    pub export: RunExportReceipt,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRunSelection {
    Latest,
    All,
}

/// Verified public ledger projection. This is safe for inspection but is not
/// exact replay: encrypted private artifacts remain in the authorized CAS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunInspection {
    pub run_id: RunId,
    pub header: RunHeader,
    pub status: RunStatus,
    pub event_count: u64,
    pub chain_head: String,
    pub events: Vec<SessionRunEvent>,
}

/// Borrowed, immutable start evidence derived from one prepared agent-loop
/// bundle. The runtime seals these references before the loop moves them into
/// its execution context; callers cannot rebuild a divergent start contract.
pub(crate) struct PreparedRunStartEvidence<'a> {
    initial_messages: &'a [Message],
    advertised_tools: &'a [ToolSchema],
    system_prompt: PreparedSystemPromptEvidence<'a>,
}

impl<'a> PreparedRunStartEvidence<'a> {
    pub(crate) fn new(
        initial_messages: &'a [Message],
        advertised_tools: &'a [ToolSchema],
        system_prompt: PreparedSystemPromptEvidence<'a>,
    ) -> Self {
        Self {
            initial_messages,
            advertised_tools,
            system_prompt,
        }
    }

    #[cfg(test)]
    pub(crate) fn unavailable_for_nonexecuting_test(
        initial_messages: &'a [Message],
        advertised_tools: &'a [ToolSchema],
    ) -> Self {
        Self::new(
            initial_messages,
            advertised_tools,
            PreparedSystemPromptEvidence::Unavailable {
                reason: "nonexecuting_test_without_prompt_context",
            },
        )
    }
}

/// The only prompt evidence shapes a run may admit. Fresh skills provenance is
/// available only when it was actually injected while building the prompt.
pub(crate) enum PreparedSystemPromptEvidence<'a> {
    Stored {
        content: &'a str,
    },
    Fresh {
        content: &'a str,
        injected_skills_block: Option<&'a str>,
    },
    Unavailable {
        reason: &'static str,
    },
}

/// Immutable, closed trigger metadata supplied by the execution ingress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunInvocationContext {
    trigger: RunTrigger,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum RunTrigger {
    Direct,
    CronManual {
        job_id: String,
        dispatch_time_ms: i64,
    },
    CronScheduled {
        job_id: String,
        source: CronScheduleSource,
        schedule_time_ms: i64,
        dispatch_time_ms: i64,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CronScheduleSource {
    Local,
    Managed,
}

#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum RunInvocationContextError {
    #[error("cron job id must contain 1 to 256 public identifier characters")]
    InvalidCronJobId,
    #[error("cron trigger timestamps must be non-negative Unix milliseconds")]
    InvalidTimestamp,
}

impl Default for RunInvocationContext {
    fn default() -> Self {
        Self {
            trigger: RunTrigger::Direct,
        }
    }
}

impl RunInvocationContext {
    pub fn cron_manual(
        job_id: impl Into<String>,
        dispatch_time_ms: i64,
    ) -> Result<Self, RunInvocationContextError> {
        let job_id = validated_cron_job_id(job_id.into())?;
        if dispatch_time_ms < 0 {
            return Err(RunInvocationContextError::InvalidTimestamp);
        }
        Ok(Self {
            trigger: RunTrigger::CronManual {
                job_id,
                dispatch_time_ms,
            },
        })
    }

    pub fn cron_scheduled(
        job_id: impl Into<String>,
        source: CronScheduleSource,
        schedule_time_ms: i64,
        dispatch_time_ms: i64,
    ) -> Result<Self, RunInvocationContextError> {
        let job_id = validated_cron_job_id(job_id.into())?;
        if schedule_time_ms < 0 || dispatch_time_ms < 0 {
            return Err(RunInvocationContextError::InvalidTimestamp);
        }
        Ok(Self {
            trigger: RunTrigger::CronScheduled {
                job_id,
                source,
                schedule_time_ms,
                dispatch_time_ms,
            },
        })
    }

    fn public_payload(&self) -> Value {
        match &self.trigger {
            RunTrigger::Direct => json!({ "kind": "direct" }),
            RunTrigger::CronManual {
                job_id,
                dispatch_time_ms,
            } => json!({
                "dispatch_time_ms": dispatch_time_ms,
                "job_id": job_id,
                "kind": "cron_manual",
            }),
            RunTrigger::CronScheduled {
                job_id,
                source,
                schedule_time_ms,
                dispatch_time_ms,
            } => json!({
                "dispatch_time_ms": dispatch_time_ms,
                "job_id": job_id,
                "kind": "cron_scheduled",
                "schedule_source": match source {
                    CronScheduleSource::Local => "local",
                    CronScheduleSource::Managed => "managed",
                },
                "schedule_time_ms": schedule_time_ms,
            }),
        }
    }
}

fn validated_cron_job_id(value: String) -> Result<String, RunInvocationContextError> {
    let trimmed = value.trim();
    if trimmed.is_empty()
        || trimmed.len() > 256
        || !trimmed.chars().all(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | ':' | '/' | '@' | '+' | '-')
        })
    {
        return Err(RunInvocationContextError::InvalidCronJobId);
    }
    Ok(trimmed.to_string())
}
/// Reusable in-memory credential context for restart/recovery tests.
///
/// Every runtime opened by one factory shares sealing and fingerprint keys,
/// while remaining isolated from the OS credential store.
#[doc(hidden)]
#[derive(Clone, Default)]
pub struct EphemeralRunRuntimeFactory {
    credentials: Arc<EphemeralArtifactCredentialStore>,
}

impl EphemeralRunRuntimeFactory {
    pub fn open(&self, state_root: impl AsRef<Path>) -> Result<RunRuntime, RunRuntimeError> {
        RunRuntime::open_with_credential_store(state_root.as_ref(), self.credentials.clone())
    }
}

/// Immutable trusted replay ingress scope. It is internal-only: callers may
/// not mint a tenant/session/branch authorization by choosing constructor
/// parameters. The current local CLI runtime is tenant `local`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ReplayAccessScope {
    tenant_id: TenantId,
    session_id: Option<SessionId>,
    branch_id: Option<BranchId>,
}

impl ReplayAccessScope {
    fn local() -> Self {
        Self {
            tenant_id: TenantId::new("local").expect("static local tenant id is valid"),
            session_id: None,
            branch_id: None,
        }
    }

    fn permits(&self, plan: &VerifiedRunReplayPlan) -> bool {
        self.tenant_id == *plan.tenant_id()
            && self
                .session_id
                .as_ref()
                .map_or(true, |session_id| session_id == plan.session_id())
            && self
                .branch_id
                .as_ref()
                .map_or(true, |branch_id| branch_id == plan.branch_id())
    }
}

pub struct RunRuntime {
    state_root: PathBuf,
    replay_access_scope: ReplayAccessScope,
    ledger: Arc<RunLedger>,
    keys: Arc<OsArtifactKeyManager>,
    session_keys: Arc<OsArtifactKeyManager>,
    store: Arc<FilesystemSealedArtifactStore>,
    exports_dir: PathBuf,
}

impl RunRuntime {
    /// Open the production run-record runtime rooted in an owner-only state
    /// directory. A truly empty state explicitly bootstraps its two key
    /// managers; every existing state resolves its recorded key lineage and
    /// fails closed if the required credentials are absent or malformed.
    pub fn open(state_root: impl AsRef<Path>) -> Result<Self, RunRuntimeError> {
        Self::open_with_credential_store(
            state_root.as_ref(),
            Arc::new(SystemArtifactCredentialStore),
        )
    }

    /// Open an isolated runtime whose sealing credentials live only in memory.
    ///
    /// Each call creates a fresh credential context. Tests that reopen the same
    /// state root must instead share an [`EphemeralRunRuntimeFactory`].
    pub fn open_for_ephemeral_testing(
        state_root: impl AsRef<Path>,
    ) -> Result<Self, RunRuntimeError> {
        EphemeralRunRuntimeFactory::default().open(state_root)
    }

    fn open_with_credential_store(
        state_root: &Path,
        credentials: Arc<dyn ArtifactCredentialStore>,
    ) -> Result<Self, RunRuntimeError> {
        let root = state_root.join("run-record-v1");
        let artifact_dir = root.join("artifacts");
        let exports_dir = root.join("exports");
        for directory in [&root, &artifact_dir, &exports_dir] {
            ensure_private_directory(directory)?;
        }

        let database_path = state_root.join("sessions.db");
        ensure_private_file(&database_path)?;
        let factory = LedgerConnectionFactory::new(&database_path);
        factory.migrate_and_cut_over_v0_to_v1()?;
        ensure_private_file(&database_path)?;
        let ledger = Arc::new(RunLedger::open_with_config(
            factory,
            RunLedgerConfig {
                owner_lease_duration: OWNER_LEASE_DURATION,
                ..RunLedgerConfig::default()
            },
        )?);
        ledger.recover_stale_runs()?;

        let store = Arc::new(FilesystemSealedArtifactStore::open(&artifact_dir)?);
        let key_config = OsArtifactKeyManagerConfig::new(
            "hermes-agent-run-record",
            "run-record-sealing-v1",
            "run-record-fingerprint-v1",
            1,
            &store,
        )?;
        let keys = Arc::new(OsArtifactKeyManager::with_store(
            key_config,
            credentials.clone(),
        ));

        let session_key_config = OsArtifactKeyManagerConfig::new(
            "hermes-agent-run-record",
            "run-record-sealing-v1",
            "run-session-pseudonym-v1",
            1,
            &store,
        )?;
        let session_keys = Arc::new(OsArtifactKeyManager::with_store(
            session_key_config,
            credentials,
        ));

        let initialization_marker_exists = run_record_initialization_marker_exists(&root)?;
        let has_committed_runs = !ledger.recent_run_ids(1)?.is_empty();
        let has_sealed_references = !ledger.referenced_sealed_envelope_digests()?.is_empty();
        if !initialization_marker_exists && !has_committed_runs && !has_sealed_references {
            // A database created before the first credential write is not
            // lineage. This branch resumes that interrupted first install,
            // while the key-manager markers make create-if-absent idempotent.
            keys.provision_active_keys_for_store(&store)?;
            session_keys.provision_active_keys_for_store(&store)?;
            write_run_record_initialization_marker(&root)?;
        } else {
            keys.resolve_active_keys_for_store(&store)?;
            session_keys.resolve_active_keys_for_store(&store)?;
        }

        Ok(Self {
            state_root: state_root.to_path_buf(),
            replay_access_scope: ReplayAccessScope::local(),
            ledger,
            keys,
            session_keys,
            store,
            exports_dir,
        })
    }

    pub fn ledger(&self) -> &Arc<RunLedger> {
        &self.ledger
    }

    #[cfg(test)]
    /// Build the authorization-gated exact replay facade without exposing
    /// sealing keys or store operations to callers.
    pub(crate) fn exact_replay_lifecycle(&self) -> ExactReplayLifecycleCoordinator<'_> {
        ExactReplayLifecycleCoordinator::new(
            self.ledger.as_ref(),
            self.keys.as_ref(),
            self.store.as_ref(),
        )
    }

    /// Bind one verified plan to the immutable trusted ingress scope and one
    /// authoritative ledger snapshot. The snapshot is rebuilt as canonical
    /// export bytes before this capability exists, rejecting self-rehashed
    /// same-run documents as well as cross-tenant plans. Lease-owner
    /// provenance is never an authorization input.
    pub fn bind_verified_replay_plan(
        &self,
        plan: &VerifiedRunReplayPlan,
    ) -> Result<BoundExactArtifactReader<'_>, RunRuntimeError> {
        if !self.replay_access_scope.permits(plan) {
            return Err(RunRuntimeError::ReplayPlanTenantMismatch);
        }
        let snapshot = self.ledger.export_snapshot(plan.summary().run_id)?;
        let canonical_snapshot = canonical_export_bytes_from_snapshot(&snapshot)?;
        if canonical_snapshot.as_slice() != plan.canonical_bytes() {
            return Err(RunRuntimeError::ReplayPlanSnapshotMismatch);
        }
        Ok(BoundExactArtifactReader::from_verified_plan(
            self.ledger.as_ref(),
            self.keys.as_ref(),
            self.store.as_ref(),
            plan,
        ))
    }

    /// Synchronously terminalize every expired owner lease. Status callers
    /// may invoke this directly; manual admission invokes it before creating
    /// a new run.
    pub fn reconcile_stale_runs(&self) -> Result<Vec<RunId>, RunRuntimeError> {
        Ok(self.ledger.recover_stale_runs()?)
    }

    /// Enqueue one closed AlphaRuntimeV1 request. The task remains only in its
    /// sealed artifact; the authoritative run is finalized pending durable
    /// outbox delivery before this method returns.
    pub fn enqueue_alpha_runtime_v1(
        &self,
        mode: ResolvedExecutionMode,
        request: AlphaRuntimeV1Request,
    ) -> Result<AlphaRuntimeV1EnqueueReceipt, RunRuntimeError> {
        Ok(alpha_runtime_v1::enqueue(
            self.ledger.as_ref(),
            self.keys.as_ref(),
            self.store.as_ref(),
            mode,
            request,
        )?)
    }

    /// Claim and dispatch bounded AlphaRuntimeV1 work through the only
    /// certified production adapter. The summary never conflates retries,
    /// dead-lettering, rejection, and ACKs.
    pub fn dispatch_alpha_runtime_v1(
        &self,
        mode: ResolvedExecutionMode,
        max_claims: usize,
    ) -> Result<AlphaRuntimeV1DispatchSummary, RunRuntimeError> {
        Ok(alpha_runtime_v1::dispatch(
            self.ledger.as_ref(),
            Arc::clone(&self.keys),
            Arc::clone(&self.store),
            &self.state_root,
            mode,
            max_claims,
        )?)
    }

    /// Exact-unseal a sealed AlphaRuntimeV1 task only after the mutable queued
    /// job has been bound back to its immutable durable receipt.
    pub fn resolve_alpha_runtime_v1_background_job_task(
        &self,
        job: &AlphaRuntimeV1BackgroundJob,
    ) -> Result<String, RunRuntimeError> {
        Ok(alpha_runtime_v1::resolve_background_job_task(
            self.keys.as_ref(),
            self.store.as_ref(),
            &self.state_root,
            job,
        )?)
    }
    pub fn inspect_run(&self, run_id: RunId) -> Result<RunInspection, RunRuntimeError> {
        let snapshot = self.ledger.export_snapshot(run_id)?;
        let event_count =
            u64::try_from(snapshot.events.len()).map_err(|_| LedgerError::NumericOverflow)?;
        Ok(RunInspection {
            run_id,
            header: snapshot.header,
            status: snapshot.status,
            event_count,
            chain_head: snapshot.head_hash,
            events: snapshot.events,
        })
    }

    pub fn session_runs(
        &self,
        session_id: &str,
        selection: SessionRunSelection,
    ) -> Result<Vec<RunInspection>, RunRuntimeError> {
        let session_id = self.persisted_session_id(session_id)?;
        let run_ids = self
            .ledger
            .run_ids_for_session(&session_id, selection == SessionRunSelection::Latest)?;
        run_ids
            .into_iter()
            .map(|run_id| self.inspect_run(run_id))
            .collect()
    }

    pub fn recent_runs(&self, limit: usize) -> Result<Vec<RunInspection>, RunRuntimeError> {
        self.ledger
            .recent_run_ids(limit)?
            .into_iter()
            .map(|run_id| self.inspect_run(run_id))
            .collect()
    }

    pub fn publish_run_export(&self, run_id: RunId) -> Result<RunExportReceipt, RunRuntimeError> {
        self.ledger.verify_run_chain(run_id)?;
        let receipt = export_run(&self.ledger, &self.exports_dir, run_id)?;
        let verified = verify_run_export(&receipt.path)?;
        if verified.run_id != run_id || verified.digest != receipt.digest {
            return Err(RunRuntimeError::Export(RunExportError::InvalidExport(
                "published run export verification did not match its receipt".to_string(),
            )));
        }
        Ok(receipt)
    }

    fn persisted_session_id(&self, session_id: &str) -> Result<SessionId, RunRuntimeError> {
        let session_id = session_id.trim();
        if session_id.is_empty() {
            return Err(RunRuntimeError::Evidence(
                "session run lookup requires a non-empty session id".to_string(),
            ));
        }
        let session_fingerprint_key = self.session_keys.active_fingerprint_key()?;
        let private_session = json!({ "session_id": session_id });
        let safe_session = safe_runtime_artifact(
            "session-identity",
            "run-session",
            "request",
            "not_applicable",
            &private_session,
        )?;
        let identity = prepare_artifact_bound(
            &safe_session,
            &private_session,
            &LedgerEvidenceRedactor,
            &session_fingerprint_key,
        )?
        .into_identity();
        Ok(SessionId::new(format!(
            "session-{}",
            identity.private_fingerprint
        ))?)
    }

    pub(crate) fn start_run(
        self: &Arc<Self>,
        config: &AgentConfig,
        containment: &ExecutionContainmentContext,
        tools: &ToolRegistry,
        prepared: PreparedRunStartEvidence<'_>,
        invocation_context: &RunInvocationContext,
        parent_run_id: Option<RunId>,
    ) -> Result<Arc<ActiveRun>, RunRuntimeError> {
        let mode = config.execution_mode;
        if containment.mode() != mode {
            return Err(RunRuntimeError::ExecutionContainmentModeMismatch {
                expected: mode.mode,
                received: containment.mode().mode,
            });
        }
        let execution_certification =
            RunExecutionCertification::from_containment_context(containment)?;
        let containment_provenance =
            configured_execution_containment_provenance(&execution_certification);
        let run_id = RunId::new();
        self.reconcile_stale_runs()?;
        let fingerprint_key = self.keys.active_fingerprint_key()?;
        let session_id = match config
            .session_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
        {
            Some(value) => self.persisted_session_id(value)?,
            None => SessionId::new(format!("session-{run_id}"))?,
        };
        let (provider, model) = config
            .model
            .split_once(':')
            .map(|(provider, model)| (provider, model))
            .unwrap_or(("runtime", config.model.as_str()));

        let config_projection = public_config_projection(config);
        let config_digest = digest(&config_projection)?;
        let exact_tool_contract = prepared
            .advertised_tools
            .iter()
            .map(|schema| AdvertisedToolContract {
                schema,
                execution_trust: tools
                    .execution_trust_for(&schema.name)
                    .unwrap_or("unregistered"),
            })
            .collect::<Vec<_>>();
        let mut effective_tools = prepared.advertised_tools.to_vec();
        effective_tools.sort_by(|left, right| left.name.cmp(&right.name));
        for duplicate in effective_tools.windows(2) {
            if duplicate[0].name == duplicate[1].name
                && digest(&duplicate[0])? != digest(&duplicate[1])?
            {
                return Err(RunRuntimeError::InvalidToolContract(format!(
                    "conflicting schemas advertise duplicate tool name '{}'",
                    duplicate[0].name
                )));
            }
        }
        effective_tools.dedup_by(|left, right| left.name == right.name);
        let canonical_tool_contract = effective_tools
            .iter()
            .map(|schema| AdvertisedToolContract {
                schema,
                execution_trust: tools
                    .execution_trust_for(&schema.name)
                    .unwrap_or("unregistered"),
            })
            .collect::<Vec<_>>();
        let tool_digest = ProvenanceDigest::present(digest(&canonical_tool_contract)?);
        let (system_prompt, system_prompt_source, injected_skills_block) =
            match &prepared.system_prompt {
                PreparedSystemPromptEvidence::Stored { content } => {
                    (Value::String((*content).to_string()), "stored", None)
                }
                PreparedSystemPromptEvidence::Fresh {
                    content,
                    injected_skills_block,
                } => (
                    Value::String((*content).to_string()),
                    "fresh",
                    *injected_skills_block,
                ),
                PreparedSystemPromptEvidence::Unavailable { reason: _reason } => {
                    (Value::Null, "unavailable", None)
                }
            };
        let private_start_evidence = json!({
            "advertised_tools": prepared.advertised_tools,
            "config_projection": config_projection,
            "execution_containment": containment_provenance.clone(),
            "injected_skills_block": injected_skills_block,
            "messages": prepared.initial_messages,
            "schema": "hermes.agent-run-start-private/v1",
            "system_prompt": system_prompt,
            "system_prompt_source": system_prompt_source,
            "tool_contract": exact_tool_contract,
        });
        let safe_start_evidence = safe_runtime_artifact(
            "run-start",
            "run-provenance",
            "request",
            "not_applicable",
            &private_start_evidence,
        )?;
        let private_provenance_commitment = prepare_artifact_bound(
            &safe_start_evidence,
            &private_start_evidence,
            &LedgerEvidenceRedactor,
            &fingerprint_key,
        )?
        .into_identity()
        .private_fingerprint;
        let sandbox_digest = containment_provenance
            .configuration_digest
            .as_deref()
            .map(|digest| Sha256Digest::new(digest.to_string()))
            .transpose()?
            .map(ProvenanceDigest::present)
            .unwrap_or_else(|| ProvenanceDigest::absent(DigestAbsence::NotApplicable));
        let policy_digest = ProvenanceDigest::present(digest(&agent_loop_effect_policy(
            mode,
            &containment_provenance,
        ))?);
        let provenance = RunProvenance::new(
            session_id,
            parent_run_id,
            BranchId::new("main")?,
            TenantId::new("local")?,
            ActionId::new("agent-loop")?,
            ObjectiveId::new(format!("session-{run_id}"))?,
            TaskId::new(format!("run-{run_id}"))?,
            ModelId::new(bounded_identifier(model, "model")?)?,
            ProviderId::new(bounded_identifier(provider, "provider")?)?,
            config_digest,
            ProvenanceDigest::absent(DigestAbsence::Unavailable),
            ProvenanceDigest::absent(DigestAbsence::Unavailable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            tool_digest,
            ProvenanceDigest::absent(DigestAbsence::Unavailable),
            policy_digest,
            sandbox_digest,
        );
        let start_payload = CanonicalJson::from_value(&json!({
            "message_count": prepared.initial_messages.len(),
            "private_provenance_commitment": private_provenance_commitment,
            "execution_containment": containment_provenance,
            "schema": "hermes.agent-run-start/v4",
            "surface": public_surface(config.platform.as_deref()),
            "system_prompt_source": system_prompt_source,
            "trigger": invocation_context.public_payload(),
        }))?;
        let owner_id = RunOwnerId::new(format!("agent-loop-{}", uuid::Uuid::new_v4()))?;
        let coordinator = ExactReplayLifecycleCoordinator::new(
            self.ledger.as_ref(),
            self.keys.as_ref(),
            self.store.as_ref(),
        );
        let created = coordinator.create_run_and_start_bound_artifact(
            mode,
            owner_id,
            RunStart::new(run_id, provenance, start_payload)
                .with_execution_containment_context(containment)?,
            &safe_start_evidence,
            &private_start_evidence,
            &LedgerEvidenceRedactor,
        )?;
        Ok(ActiveRun::start(
            Arc::clone(self),
            mode,
            created.owner_lease,
        ))
    }
}

struct ActiveRunState {
    runtime: Arc<RunRuntime>,
    mode: ResolvedExecutionMode,
    run_id: RunId,
    lease: Mutex<RunOwnerLease>,
    heartbeat_failure: StdMutex<Option<String>>,
    effect_poisoned: Arc<AtomicBool>,
    effect_gate: Arc<Mutex<()>>,
    lifecycle_gate: Arc<RwLock<()>>,
    oauth_refresh_providers: Mutex<HashSet<String>>,
    next_llm_attempt: AtomicU64,
    terminal_committed: AtomicBool,
    finalized: AtomicBool,
}

pub(crate) struct LlmAttempt {
    call_id: String,
    effect_lease: NonResumableEffectLease,
    poison_guard: EffectPoisonGuard,
    _effect_permit: tokio::sync::OwnedMutexGuard<()>,
    _lifecycle_permit: tokio::sync::OwnedRwLockReadGuard<()>,
}

struct EffectPoisonGuard {
    poisoned: Arc<AtomicBool>,
    armed: bool,
}

impl EffectPoisonGuard {
    fn new(poisoned: Arc<AtomicBool>) -> Self {
        Self {
            poisoned,
            armed: true,
        }
    }

    fn poison(&mut self) {
        self.poisoned.store(true, Ordering::SeqCst);
        self.armed = false;
    }

    fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for EffectPoisonGuard {
    fn drop(&mut self) {
        if self.armed {
            self.poisoned.store(true, Ordering::SeqCst);
        }
    }
}
pub struct ActiveRun {
    state: Arc<ActiveRunState>,
    heartbeat_stop: watch::Sender<bool>,
    finish_lock: Mutex<()>,
    heartbeat_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl ActiveRun {
    fn start(
        runtime: Arc<RunRuntime>,
        mode: ResolvedExecutionMode,
        owner_lease: RunOwnerLease,
    ) -> Arc<Self> {
        let run_id = owner_lease.run_id;
        let state = Arc::new(ActiveRunState {
            runtime,
            mode,
            run_id,
            lease: Mutex::new(owner_lease),
            heartbeat_failure: StdMutex::new(None),
            effect_poisoned: Arc::new(AtomicBool::new(false)),
            effect_gate: Arc::new(Mutex::new(())),
            lifecycle_gate: Arc::new(RwLock::new(())),
            oauth_refresh_providers: Mutex::new(HashSet::new()),
            next_llm_attempt: AtomicU64::new(0),
            terminal_committed: AtomicBool::new(false),
            finalized: AtomicBool::new(false),
        });
        let (heartbeat_stop, mut stop_rx) = watch::channel(false);
        let heartbeat_state = Arc::clone(&state);
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    changed = stop_rx.changed() => {
                        if changed.is_err() || *stop_rx.borrow() {
                            break;
                        }
                    }
                    () = tokio::time::sleep(OWNER_HEARTBEAT_INTERVAL) => {
                        let mut lease = heartbeat_state.lease.lock().await;
                        match heartbeat_state.runtime.ledger.heartbeat_run_owner(
                            heartbeat_state.mode,
                            &lease,
                        ) {
                            Ok(renewed) => *lease = renewed,
                            Err(error) => {
                                let mut failure = heartbeat_state
                                    .heartbeat_failure
                                    .lock()
                                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                                *failure = Some(error.to_string());
                                break;
                            }
                        }
                    }
                }
            }
        });
        Arc::new(Self {
            state,
            heartbeat_stop,
            finish_lock: Mutex::new(()),
            heartbeat_task: Mutex::new(Some(task)),
        })
    }

    pub fn run_id(&self) -> RunId {
        self.state.run_id
    }

    pub(crate) async fn begin_llm_attempt(
        &self,
        provider: &str,
        model: &str,
        messages: &[Message],
        tools: &[ToolSchema],
        max_tokens: Option<u32>,
        temperature: Option<f64>,
        extra_body: Option<&Value>,
        stream: bool,
    ) -> Result<LlmAttempt, RunRuntimeError> {
        let lifecycle_permit = Arc::clone(&self.state.lifecycle_gate).read_owned().await;
        let effect_permit = Arc::clone(&self.state.effect_gate).lock_owned().await;
        self.ensure_effect_start_allowed()?;
        let attempt = self.state.next_llm_attempt.fetch_add(1, Ordering::SeqCst);
        let call_id = format!("llm-attempt-{attempt}");
        let evidence = bound_exact_runtime_evidence(
            &call_id,
            "llm-request",
            "request",
            json!({
                "extra_body": extra_body,
                "max_tokens": max_tokens,
                "messages": messages,
                "model": model,
                "provider": provider,
                "stream": stream,
                "temperature": temperature,
                "tools": tools,
            }),
        );
        let safe = safe_runtime_artifact(
            &call_id,
            "llm-request",
            "request",
            "not_applicable",
            &evidence,
        )?;
        let payload = CanonicalJson::from_value(&json!({
            "attempt": attempt,
            "model": bounded_identifier(model, "model")?,
            "provider": bounded_identifier(provider, "provider")?,
            "schema": "hermes.llm-request/v1",
            "stream": stream,
        }))?;
        let receipt = self
            .append_bound_artifact(
                llm_request_append_key(self.state.run_id, &call_id)?,
                EffectIntent::LocalOneShotV1,
                payload,
                &safe,
                &evidence,
                Some(ReplayCriticalArtifactAppend::new(
                    ExactRuntimeEvidenceSchema::LlmRequestV1,
                    ReplayAppendContract::own_event(
                        ReplayOperationKind::Model,
                        ReplayOperationPhase::Request,
                    ),
                )),
            )
            .await?;
        let certified = CertifiedEffectIntent::llm(&receipt)?;
        let effect_lease = {
            let owner = self.lock_healthy_lease().await?;
            self.state.runtime.ledger.begin_nonresumable_effect(
                self.state.mode,
                &owner,
                &certified,
            )?
        };
        Ok(LlmAttempt {
            call_id,
            effect_lease,
            poison_guard: EffectPoisonGuard::new(Arc::clone(&self.state.effect_poisoned)),
            _effect_permit: effect_permit,
            _lifecycle_permit: lifecycle_permit,
        })
    }

    pub(crate) async fn record_llm_result(
        &self,
        attempt: LlmAttempt,
        response: &hermes_core::LlmResponse,
    ) -> Result<(), RunRuntimeError> {
        self.record_llm_attempt_outcome(attempt, "returned", json!({ "response": response }))
            .await
    }

    pub(crate) async fn record_llm_stream_result(
        &self,
        attempt: LlmAttempt,
        response: &hermes_core::LlmResponse,
        stream_digest: &str,
        chunk_count: u64,
        interrupted: bool,
    ) -> Result<(), RunRuntimeError> {
        let outcome = if interrupted {
            "handler_error"
        } else {
            "returned"
        };
        self.record_llm_attempt_outcome(
            attempt,
            outcome,
            json!({
                "response": response,
                "stream": {
                    "chunk_count": chunk_count,
                    "sha256": stream_digest,
                },
            }),
        )
        .await
    }

    pub(crate) async fn record_llm_error(
        &self,
        attempt: LlmAttempt,
        error: &str,
    ) -> Result<(), RunRuntimeError> {
        self.record_llm_attempt_outcome(attempt, "handler_error", json!({ "error": error }))
            .await
    }

    pub(crate) async fn record_llm_uncertain(
        &self,
        attempt: LlmAttempt,
        error: &str,
    ) -> Result<(), RunRuntimeError> {
        self.record_llm_uncertain_outcome(attempt, json!({ "error": error }))
            .await
    }

    pub(crate) async fn record_llm_stream_error(
        &self,
        attempt: LlmAttempt,
        error: &str,
        stream_digest: &str,
        chunk_count: u64,
    ) -> Result<(), RunRuntimeError> {
        self.record_llm_attempt_outcome(
            attempt,
            "handler_error",
            json!({
                "error": error,
                "stream": {
                    "chunk_count": chunk_count,
                    "sha256": stream_digest,
                },
            }),
        )
        .await
    }

    pub(crate) async fn record_llm_stream_uncertain(
        &self,
        attempt: LlmAttempt,
        error: &str,
        stream_digest: &str,
        chunk_count: u64,
    ) -> Result<(), RunRuntimeError> {
        self.record_llm_uncertain_outcome(
            attempt,
            json!({
                "error": error,
                "stream": {
                    "chunk_count": chunk_count,
                    "sha256": stream_digest,
                },
            }),
        )
        .await
    }

    async fn record_llm_uncertain_outcome(
        &self,
        attempt: LlmAttempt,
        result: Value,
    ) -> Result<(), RunRuntimeError> {
        let mut attempt = attempt;
        attempt.poison_guard.poison();
        let call_id = attempt.call_id;
        let effect_lease = attempt.effect_lease;
        let evidence = bound_exact_runtime_evidence(&call_id, "llm-response", "result", result);
        let safe = safe_runtime_artifact(
            &call_id,
            "llm-response",
            "result",
            "effect_non_resumable_unknown",
            &evidence,
        )?;
        let payload = CanonicalJson::from_value(&json!({
            "outcome": "effect_non_resumable_unknown",
            "schema": "hermes.llm-uncertain/v1",
            "source_event_id": effect_lease.event_id,
        }))?;
        self.append_bound_artifact(
            llm_uncertain_append_key(self.state.run_id, &call_id)?,
            EffectIntent::None,
            payload,
            &safe,
            &evidence,
            Some(ReplayCriticalArtifactAppend::new(
                ExactRuntimeEvidenceSchema::LlmResultV1,
                ReplayAppendContract::resolve_source(
                    ReplayOperationKind::Model,
                    ReplayOperationPhase::Result,
                    effect_lease.event_id,
                ),
            )),
        )
        .await?;
        Ok(())
    }

    async fn record_llm_attempt_outcome(
        &self,
        attempt: LlmAttempt,
        outcome: &str,
        result: Value,
    ) -> Result<(), RunRuntimeError> {
        let mut attempt = attempt;
        let call_id = attempt.call_id;
        let effect_lease = attempt.effect_lease;
        let evidence = bound_exact_runtime_evidence(&call_id, "llm-response", "result", result);
        let safe = safe_runtime_artifact(&call_id, "llm-response", "result", outcome, &evidence)?;
        let observed = RunLedger::observed_nonresumable_attempt(effect_lease, outcome)?;
        let payload = self
            .state
            .runtime
            .ledger
            .persist_nonresumable_result(self.state.mode, &observed)?;
        let receipt = self
            .append_bound_artifact(
                observed.result_append_key().clone(),
                EffectIntent::None,
                payload,
                &safe,
                &evidence,
                Some(ReplayCriticalArtifactAppend::new(
                    ExactRuntimeEvidenceSchema::LlmResultV1,
                    ReplayAppendContract::resolve_source(
                        ReplayOperationKind::Model,
                        ReplayOperationPhase::Result,
                        observed.event_id(),
                    ),
                )),
            )
            .await?;
        {
            let owner = self.lock_healthy_lease().await?;
            self.state.runtime.ledger.complete_nonresumable_effect(
                self.state.mode,
                &observed,
                receipt.event_id,
            )?;
            drop(owner);
        }
        attempt.poison_guard.disarm();
        Ok(())
    }

    /// Execute one vault refresh after its intent is durably certified. A
    /// provider is attempted at most once per run; cancellation or a panic
    /// after intent leaves the durable effect executing so finalization can
    /// choose the typed unknown outcome from ledger state.
    pub(crate) async fn execute_oauth_refresh_once<F, Fut>(
        &self,
        provider: &str,
        operation: F,
    ) -> Result<(), GovernedOAuthRefreshError>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = Result<Value, AgentError>>,
    {
        let provider = bounded_identifier(provider, "oauth provider")?;
        {
            let mut attempted = self.state.oauth_refresh_providers.lock().await;
            if !attempted.insert(provider.clone()) {
                return Ok(());
            }
        }

        let _lifecycle_permit = Arc::clone(&self.state.lifecycle_gate).read_owned().await;
        let _effect_permit = Arc::clone(&self.state.effect_gate).lock_owned().await;
        self.ensure_effect_start_allowed()?;
        let invocation_key = format!(
            "oauth-refresh-{}",
            scoped_key_hash(self.state.run_id, "oauth-refresh", &provider)
        );
        let intent_evidence = bound_exact_runtime_evidence(
            &invocation_key,
            "oauth-refresh",
            "request",
            json!({ "provider": provider }),
        );
        let intent_safe = safe_runtime_artifact(
            &invocation_key,
            "oauth-refresh",
            "request",
            "not_applicable",
            &intent_evidence,
        )?;
        let intent_payload = CanonicalJson::from_value(&json!({
            "provider": provider,
            "schema": "hermes.oauth-refresh-intent/v1",
        }))?;
        let intent_receipt = self
            .append_bound_artifact(
                intent_append_key(self.state.run_id, &invocation_key)?,
                EffectIntent::LocalOneShotV1,
                intent_payload,
                &intent_safe,
                &intent_evidence,
                Some(ReplayCriticalArtifactAppend::new(
                    ExactRuntimeEvidenceSchema::OauthRequestV1,
                    ReplayAppendContract::own_event(
                        ReplayOperationKind::Oauth,
                        ReplayOperationPhase::Request,
                    ),
                )),
            )
            .await?;
        let certified = CertifiedEffectIntent::from_receipt(&intent_receipt)?;
        let effect_lease = {
            let owner = self.lock_healthy_lease().await?;
            self.state.runtime.ledger.begin_nonresumable_effect(
                self.state.mode,
                &owner,
                &certified,
            )?
        };
        let mut poison_guard = EffectPoisonGuard::new(Arc::clone(&self.state.effect_poisoned));
        let operation_result = operation().await;
        let (outcome, result_evidence) = match &operation_result {
            Ok(resolution) => (
                "returned",
                bound_exact_runtime_evidence(
                    &invocation_key,
                    "oauth-refresh",
                    "result",
                    json!({
                        "provider": provider,
                        "resolution": resolution,
                    }),
                ),
            ),
            Err(error) => (
                "handler_error",
                bound_exact_runtime_evidence(
                    &invocation_key,
                    "oauth-refresh",
                    "result",
                    json!({
                        "error_class": oauth_refresh_error_class(error),
                        "provider": provider,
                    }),
                ),
            ),
        };
        let result_safe = safe_runtime_artifact(
            &invocation_key,
            "oauth-refresh",
            "result",
            outcome,
            &result_evidence,
        )?;
        let observed = RunLedger::observed_nonresumable_attempt(effect_lease, outcome)?;
        let result_payload = self
            .state
            .runtime
            .ledger
            .persist_nonresumable_result(self.state.mode, &observed)?;
        let result_receipt = self
            .append_bound_artifact(
                observed.result_append_key().clone(),
                EffectIntent::None,
                result_payload,
                &result_safe,
                &result_evidence,
                Some(ReplayCriticalArtifactAppend::new(
                    ExactRuntimeEvidenceSchema::OauthResultV1,
                    ReplayAppendContract::resolve_source(
                        ReplayOperationKind::Oauth,
                        ReplayOperationPhase::Result,
                        observed.event_id(),
                    ),
                )),
            )
            .await?;
        {
            let _owner = self.lock_healthy_lease().await?;
            self.state.runtime.ledger.complete_nonresumable_effect(
                self.state.mode,
                &observed,
                result_receipt.event_id,
            )?;
        }
        poison_guard.disarm();
        operation_result.map(|_| ()).map_err(Into::into)
    }

    pub(crate) async fn execute_tool(
        &self,
        tool_call_id: &str,
        invocation_key: &str,
        tool_name: &str,
        params: Value,
        entry: ToolEntry,
    ) -> Result<String, GovernedToolExecutionError> {
        if entry.execution_trust == ToolExecutionTrust::Untrusted {
            return Err(ToolError::ExecutionFailed(format!(
                "untrusted dynamic tool '{tool_name}' is unavailable until a policy-allowlisted executor is configured"
            ))
            .into());
        }

        let effect_intent = match entry.execution_trust {
            ToolExecutionTrust::VettedLocalOneShot => EffectIntent::LocalOneShotV1,
            ToolExecutionTrust::Observation => EffectIntent::None,
            ToolExecutionTrust::Untrusted => unreachable!("untrusted tools fail closed"),
        };
        let _lifecycle_permit = Arc::clone(&self.state.lifecycle_gate).read_owned().await;
        let _effect_permit = if effect_intent == EffectIntent::LocalOneShotV1 {
            Some(Arc::clone(&self.state.effect_gate).lock_owned().await)
        } else {
            None
        };
        if effect_intent == EffectIntent::LocalOneShotV1 {
            self.ensure_effect_start_allowed()?;
        } else {
            self.ensure_operation_start_allowed()?;
        }
        let intent_evidence = bound_exact_runtime_evidence(
            invocation_key,
            "tool-invocation",
            "request",
            json!({
                "arguments": params.clone(),
                "call_id": invocation_key,
                "effect_intent": effect_intent.as_str(),
                "execution_trust": entry.execution_trust.as_str(),
                "tool_name": tool_name,
            }),
        );
        let intent_payload = CanonicalJson::from_value(&json!({
            "invocation_slot": invocation_key,
            "schema": "hermes.tool-intent/v1",
            "schema_version": 1,
            "trust": entry.execution_trust.as_str(),
        }))?;
        let intent_safe = safe_runtime_artifact(
            invocation_key,
            "tool-invocation",
            "request",
            "not_applicable",
            &intent_evidence,
        )?;
        let intent_receipt = self
            .append_bound_artifact(
                intent_append_key(self.state.run_id, invocation_key)?,
                effect_intent,
                intent_payload,
                &intent_safe,
                &intent_evidence,
                Some(ReplayCriticalArtifactAppend::new(
                    ExactRuntimeEvidenceSchema::ToolIntentV1,
                    ReplayAppendContract::own_event(
                        ReplayOperationKind::Tool,
                        ReplayOperationPhase::Request,
                    ),
                )),
            )
            .await?;

        if effect_intent == EffectIntent::LocalOneShotV1 {
            let certified = CertifiedEffectIntent::vetted_tool(&intent_receipt, &entry)?;
            let lease = {
                let owner = self.lock_healthy_lease().await?;
                self.state.runtime.ledger.begin_nonresumable_effect(
                    self.state.mode,
                    &owner,
                    &certified,
                )?
            };
            let mut poison_guard = EffectPoisonGuard::new(Arc::clone(&self.state.effect_poisoned));
            let observed = ObservedToolResult::invoke(entry, params);
            if observed.is_unknown() {
                poison_guard.poison();
                let evidence = bound_exact_runtime_evidence(
                    invocation_key,
                    "tool-invocation",
                    "result",
                    observed.exact_replay_evidence(
                        invocation_key,
                        tool_name,
                        "effect_non_resumable_unknown",
                    ),
                );
                let result_safe = safe_runtime_artifact(
                    invocation_key,
                    "tool-invocation",
                    "result",
                    "effect_non_resumable_unknown",
                    &evidence,
                )?;
                let payload = CanonicalJson::from_value(&json!({
                    "outcome": "effect_non_resumable_unknown",
                    "schema": "hermes.tool-uncertain/v1",
                    "source_event_id": lease.event_id,
                }))?;
                self.append_bound_artifact(
                    tool_uncertain_append_key(self.state.run_id, invocation_key)?,
                    EffectIntent::None,
                    payload,
                    &result_safe,
                    &evidence,
                    Some(ReplayCriticalArtifactAppend::new(
                        ExactRuntimeEvidenceSchema::ToolResultV1,
                        ReplayAppendContract::resolve_source(
                            ReplayOperationKind::Tool,
                            ReplayOperationPhase::Result,
                            lease.event_id,
                        ),
                    )),
                )
                .await?;
                return observed.into_result().map_err(Into::into);
            }
            let outcome = observed.outcome();
            let evidence = bound_exact_runtime_evidence(
                invocation_key,
                "tool-invocation",
                "result",
                observed.exact_replay_evidence(invocation_key, tool_name, outcome),
            );
            let result_safe = safe_runtime_artifact(
                invocation_key,
                "tool-invocation",
                "result",
                outcome,
                &evidence,
            )?;
            let attempt = RunLedger::observed_nonresumable_attempt(lease, outcome)?;
            let result_payload = self
                .state
                .runtime
                .ledger
                .persist_nonresumable_result(self.state.mode, &attempt)?;
            let result_receipt = self
                .append_bound_artifact(
                    attempt.result_append_key().clone(),
                    EffectIntent::None,
                    result_payload,
                    &result_safe,
                    &evidence,
                    Some(ReplayCriticalArtifactAppend::new(
                        ExactRuntimeEvidenceSchema::ToolResultV1,
                        ReplayAppendContract::resolve_source(
                            ReplayOperationKind::Tool,
                            ReplayOperationPhase::Result,
                            attempt.event_id(),
                        ),
                    )),
                )
                .await?;
            {
                let owner = self.lock_healthy_lease().await?;
                self.state.runtime.ledger.complete_nonresumable_effect(
                    self.state.mode,
                    &attempt,
                    result_receipt.event_id,
                )?;
                drop(owner);
            }
            poison_guard.disarm();
            return observed.into_result().map_err(Into::into);
        }

        let observed = ObservedToolResult::invoke(entry, params);
        let evidence = bound_exact_runtime_evidence(
            invocation_key,
            "tool-invocation",
            "result",
            observed.exact_replay_evidence(invocation_key, tool_name, observed.outcome()),
        );
        let result_safe = safe_runtime_artifact(
            invocation_key,
            "tool-invocation",
            "result",
            observed.outcome(),
            &evidence,
        )?;
        let result_payload = CanonicalJson::from_value(&json!({
            "outcome": observed.outcome(),
            "schema": "hermes.tool-result/v1",
            "source_event_id": intent_receipt.event_id,
        }))?;
        self.append_bound_artifact(
            result_append_key(self.state.run_id, invocation_key)?,
            EffectIntent::None,
            result_payload,
            &result_safe,
            &evidence,
            Some(ReplayCriticalArtifactAppend::new(
                ExactRuntimeEvidenceSchema::ToolResultV1,
                ReplayAppendContract::resolve_source(
                    ReplayOperationKind::Tool,
                    ReplayOperationPhase::Result,
                    intent_receipt.event_id,
                ),
            )),
        )
        .await?;
        observed.into_result().map_err(Into::into)
    }

    pub(crate) async fn execute_delegation<F>(
        &self,
        tool_call_id: &str,
        invocation_key: &str,
        request_evidence: Value,
        operation: F,
    ) -> Result<String, RunRuntimeError>
    where
        F: Future<Output = String>,
    {
        let _lifecycle_permit = Arc::clone(&self.state.lifecycle_gate).read_owned().await;
        let _effect_permit = Arc::clone(&self.state.effect_gate).lock_owned().await;
        self.ensure_effect_start_allowed()?;
        let tool_name = "delegate_task";
        let request_payload =
            closed_delegation_request_payload(tool_call_id, invocation_key, &request_evidence);
        let request_evidence =
            bound_exact_runtime_evidence(tool_call_id, "delegation", "request", request_payload);
        let intent_payload = CanonicalJson::from_value(&json!({
            "invocation_slot": invocation_key,
            "schema": "hermes.delegation-intent/v1",
            "schema_version": 1,
        }))?;
        let intent_safe = safe_runtime_artifact(
            tool_call_id,
            "delegation",
            "request",
            "not_applicable",
            &request_evidence,
        )?;
        let intent_receipt = self
            .append_bound_artifact(
                intent_append_key(self.state.run_id, invocation_key)?,
                EffectIntent::LocalOneShotV1,
                intent_payload,
                &intent_safe,
                &request_evidence,
                Some(ReplayCriticalArtifactAppend::new(
                    ExactRuntimeEvidenceSchema::DelegationRequestV1,
                    ReplayAppendContract::own_event(
                        ReplayOperationKind::Delegation,
                        ReplayOperationPhase::Request,
                    ),
                )),
            )
            .await?;
        let certified = CertifiedEffectIntent::delegation(&intent_receipt)?;
        let lease = {
            let owner = self.lock_healthy_lease().await?;
            self.state.runtime.ledger.begin_nonresumable_effect(
                self.state.mode,
                &owner,
                &certified,
            )?
        };
        let mut poison_guard = EffectPoisonGuard::new(Arc::clone(&self.state.effect_poisoned));

        let output = operation.await;
        let evidence = bound_exact_runtime_evidence(
            tool_call_id,
            "delegation",
            "result",
            json!({
                "call_id": tool_call_id,
                "invocation_key": invocation_key,
                "outcome": "returned",
                "response": output,
                "tool_name": tool_name,
            }),
        );
        let result_safe =
            safe_runtime_artifact(tool_call_id, "delegation", "result", "returned", &evidence)?;
        let attempt = RunLedger::observed_nonresumable_attempt(lease, "returned")?;
        let result_payload = self
            .state
            .runtime
            .ledger
            .persist_nonresumable_result(self.state.mode, &attempt)?;
        let result_receipt = self
            .append_bound_artifact(
                attempt.result_append_key().clone(),
                EffectIntent::None,
                result_payload,
                &result_safe,
                &evidence,
                Some(ReplayCriticalArtifactAppend::new(
                    ExactRuntimeEvidenceSchema::DelegationResultV1,
                    ReplayAppendContract::resolve_source(
                        ReplayOperationKind::Delegation,
                        ReplayOperationPhase::Result,
                        attempt.event_id(),
                    ),
                )),
            )
            .await?;
        {
            let _owner = self.lock_healthy_lease().await?;
            self.state.runtime.ledger.complete_nonresumable_effect(
                self.state.mode,
                &attempt,
                result_receipt.event_id,
            )?;
        }
        poison_guard.disarm();
        Ok(output)
    }

    pub(crate) async fn record_agent_outcome(
        &self,
        outcome: &str,
        evidence_text: &str,
    ) -> Result<(), RunRuntimeError> {
        let _lifecycle_permit = Arc::clone(&self.state.lifecycle_gate).read_owned().await;
        self.ensure_operation_start_allowed()?;
        let tool_call_id = format!("run-output-{}", self.run_id_async().await);
        let evidence = bound_exact_runtime_evidence(
            &tool_call_id,
            "agent-output",
            "result",
            json!({ "response": evidence_text }),
        );
        let safe =
            safe_runtime_artifact(&tool_call_id, "agent-output", "result", outcome, &evidence)?;
        let payload = CanonicalJson::from_value(&json!({
            "outcome": outcome,
            "schema": "hermes.agent-output/v1",
        }))?;
        self.append_bound_artifact(
            output_append_key(self.state.run_id, &tool_call_id)?,
            EffectIntent::None,
            payload,
            &safe,
            &evidence,
            Some(ReplayCriticalArtifactAppend::new(
                ExactRuntimeEvidenceSchema::AgentOutputV1,
                ReplayAppendContract::own_event(
                    ReplayOperationKind::AgentOutput,
                    ReplayOperationPhase::Result,
                ),
            )),
        )
        .await?;
        Ok(())
    }

    pub(crate) async fn finish(
        &self,
        terminal_reason: TerminalReason,
    ) -> Result<RunCompletion, RunRuntimeError> {
        let _finish_guard = self.finish_lock.lock().await;
        let _lifecycle_permit = Arc::clone(&self.state.lifecycle_gate).write_owned().await;
        let _effect_permit = Arc::clone(&self.state.effect_gate).lock_owned().await;
        if self.state.finalized.load(Ordering::SeqCst) {
            return Err(RunRuntimeError::AlreadyFinalized {
                run_id: self.state.run_id,
            });
        }

        if !self.state.terminal_committed.load(Ordering::SeqCst) {
            self.ensure_healthy()?;
            let outcome = terminal_reason.outcome();
            let payload = CanonicalJson::from_value(&json!({
                "outcome": outcome,
                "schema": "hermes.agent-run-exit/v1",
            }))?;
            let lease = self.state.lease.lock().await;
            self.state.runtime.ledger.finalize_run_exit(
                self.state.mode,
                &lease,
                RunExit::new(terminal_reason, payload),
            )?;
            self.state.terminal_committed.store(true, Ordering::SeqCst);
            drop(lease);
            let _ = self.heartbeat_stop.send(true);
            if let Some(task) = self.heartbeat_task.lock().await.take() {
                let _ = task.await;
            }
        }
        let run_id = self.state.run_id;
        self.state.runtime.ledger.verify_run_chain(run_id)?;
        let export = export_run(
            &self.state.runtime.ledger,
            &self.state.runtime.exports_dir,
            run_id,
        )?;
        let verified = verify_run_export(&export.path)?;
        if verified.run_id != run_id || verified.digest != export.digest {
            return Err(RunRuntimeError::Export(RunExportError::InvalidExport(
                "published export verification did not match its receipt".to_string(),
            )));
        }
        self.state.finalized.store(true, Ordering::SeqCst);
        Ok(RunCompletion { run_id, export })
    }

    async fn append_bound_artifact(
        &self,
        append_key: AppendKey,
        effect_intent: EffectIntent,
        public_payload: CanonicalJson,
        safe_artifact: &Value,
        private_evidence: &Value,
        replay: Option<ReplayCriticalArtifactAppend>,
    ) -> Result<AppendReceipt, RunRuntimeError> {
        let owner = self.lock_healthy_lease().await?;
        let redactor = LedgerEvidenceRedactor;
        let coordinator = ArtifactAppendCoordinator::new_with_filesystem_admission(
            &self.state.runtime.ledger,
            &self.state.runtime.keys,
            self.state.runtime.store.as_ref(),
        );
        let source_append_key = append_key.clone();
        let request = ArtifactAppendRequest {
            mode: self.state.mode,
            owner_lease: &owner,
            append_key,
            effect_intent,
            public_payload,
            raw_artifact: safe_artifact,
            redactor: &redactor,
        };
        let appended = match replay {
            Some(replay) => {
                coordinator.append_bound_replay_critical(&request, private_evidence, replay)
            }
            None => coordinator.append_bound(&request, private_evidence),
        };
        match appended {
            Ok(ArtifactAppendOutcome::Committed(receipt)) => Ok(receipt),
            Ok(ArtifactAppendOutcome::InFlight { event_id, .. }) => Err(RunRuntimeError::Evidence(
                format!("run evidence append is already in flight for event {event_id}"),
            )),
            Ok(ArtifactAppendOutcome::ManualDegraded { reason, receipt }) => {
                Err(RunRuntimeError::DegradedManualAppend {
                    append_key: source_append_key,
                    reason,
                    audit_event_id: receipt.event_id,
                })
            }
            Err(error) => Err(RunRuntimeError::Evidence(error.to_string())),
        }
    }

    async fn lock_healthy_lease(
        &self,
    ) -> Result<tokio::sync::MutexGuard<'_, RunOwnerLease>, RunRuntimeError> {
        self.ensure_healthy()?;
        let lease = self.state.lease.lock().await;
        self.ensure_healthy()?;
        Ok(lease)
    }

    fn ensure_healthy(&self) -> Result<(), RunRuntimeError> {
        let failure = self
            .state
            .heartbeat_failure
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(error) = failure.as_ref() {
            return Err(RunRuntimeError::Heartbeat(error.clone()));
        }
        Ok(())
    }

    fn ensure_operation_start_allowed(&self) -> Result<(), RunRuntimeError> {
        self.ensure_healthy()?;
        if self.state.terminal_committed.load(Ordering::SeqCst)
            || self.state.finalized.load(Ordering::SeqCst)
        {
            return Err(RunRuntimeError::AlreadyFinalized {
                run_id: self.state.run_id,
            });
        }
        Ok(())
    }

    fn ensure_effect_start_allowed(&self) -> Result<(), RunRuntimeError> {
        self.ensure_operation_start_allowed()?;
        if self.state.effect_poisoned.load(Ordering::SeqCst) {
            return Err(RunRuntimeError::Evidence(
                "run has an unresolved non-resumable effect; new effects are blocked".to_string(),
            ));
        }
        Ok(())
    }

    async fn run_id_async(&self) -> RunId {
        self.state.run_id
    }
}

impl Drop for ActiveRun {
    fn drop(&mut self) {
        let _ = self.heartbeat_stop.send(true);
    }
}

pub(crate) struct CertifiedEffectIntent {
    source_event_id: EventId,
    source_event_hash: Sha256Digest,
}

impl CertifiedEffectIntent {
    fn from_receipt(intent_receipt: &AppendReceipt) -> Result<Self, LedgerError> {
        if intent_receipt.sealed_artifact_ref.is_none() {
            return Err(LedgerError::IntegrityViolation(
                "non-resumable effect lacks sealed intent evidence".to_string(),
            ));
        }
        Ok(Self {
            source_event_id: intent_receipt.event_id,
            source_event_hash: Sha256Digest::new(intent_receipt.event_hash.clone())?,
        })
    }

    #[cfg(test)]
    pub(crate) fn from_receipt_for_test(
        intent_receipt: &AppendReceipt,
    ) -> Result<Self, LedgerError> {
        Self::from_receipt(intent_receipt)
    }

    fn vetted_tool(intent_receipt: &AppendReceipt, entry: &ToolEntry) -> Result<Self, LedgerError> {
        if entry.execution_trust != ToolExecutionTrust::VettedLocalOneShot {
            return Err(LedgerError::IntegrityViolation(
                "runtime tool effect lacks vetted registration".to_string(),
            ));
        }
        Self::from_receipt(intent_receipt)
    }

    fn delegation(intent_receipt: &AppendReceipt) -> Result<Self, LedgerError> {
        Self::from_receipt(intent_receipt)
    }

    fn llm(intent_receipt: &AppendReceipt) -> Result<Self, LedgerError> {
        Self::from_receipt(intent_receipt)
    }

    pub(crate) const fn source_event_id(&self) -> EventId {
        self.source_event_id
    }

    pub(crate) fn source_event_hash(&self) -> &Sha256Digest {
        &self.source_event_hash
    }
}

enum ObservedToolResult {
    Returned(String),
    HandlerError(ToolError),
    Panicked,
}

impl ObservedToolResult {
    fn invoke(entry: ToolEntry, params: Value) -> Self {
        match catch_unwind(AssertUnwindSafe(|| (entry.handler)(params))) {
            Ok(Ok(output)) => Self::Returned(output),
            Ok(Err(error)) => Self::HandlerError(error),
            Err(_) => Self::Panicked,
        }
    }

    const fn is_unknown(&self) -> bool {
        matches!(
            self,
            Self::Panicked
                | Self::HandlerError(ToolError::Timeout(_) | ToolError::OutcomeUnknown(_))
        )
    }

    const fn outcome(&self) -> &'static str {
        match self {
            Self::Returned(_) => "returned",
            Self::HandlerError(_) | Self::Panicked => "handler_error",
        }
    }

    fn evidence(&self) -> Value {
        match self {
            Self::Returned(output) => json!({ "returned": output }),
            Self::HandlerError(error) => json!({ "handler_error": error.to_string() }),
            Self::Panicked => json!({ "panic": "tool handler panicked after invocation began" }),
        }
    }

    /// Closed result schema paired with the exact public tool event. The
    /// caller supplies the public outcome so a mismatched observation cannot
    /// be silently reclassified while constructing evidence.
    fn exact_replay_evidence(&self, call_id: &str, tool_name: &str, outcome: &str) -> Value {
        match (outcome, self) {
            ("returned", Self::Returned(value)) => json!({
                "call_id": call_id,
                "outcome": outcome,
                "tool_name": tool_name,
                "value": value,
            }),
            ("handler_error" | "effect_non_resumable_unknown", Self::HandlerError(error)) => {
                json!({
                    "call_id": call_id,
                    "error": error.to_string(),
                    "outcome": outcome,
                    "tool_name": tool_name,
                })
            }
            ("effect_non_resumable_unknown", Self::Panicked) => json!({
                "call_id": call_id,
                "error": "tool handler panicked after invocation began",
                "outcome": outcome,
                "tool_name": tool_name,
            }),
            (_, Self::Panicked) => json!({
                "call_id": call_id,
                "error": "tool handler panicked after invocation began",
                "outcome": "handler_error",
                "tool_name": tool_name,
            }),
            (_, Self::Returned(value)) => json!({
                "call_id": call_id,
                "outcome": "returned",
                "tool_name": tool_name,
                "value": value,
            }),
            (_, Self::HandlerError(error)) => json!({
                "call_id": call_id,
                "error": error.to_string(),
                "outcome": "handler_error",
                "tool_name": tool_name,
            }),
        }
    }

    fn into_result(self) -> Result<String, ToolError> {
        match self {
            Self::Returned(output) => Ok(output),
            Self::HandlerError(error) => Err(error),
            Self::Panicked => Err(ToolError::ExecutionFailed(
                "tool handler panicked after invocation began; outcome is unknown".to_string(),
            )),
        }
    }
}

#[derive(Serialize)]
struct AdvertisedToolContract<'a> {
    schema: &'a ToolSchema,
    execution_trust: &'static str,
}

/// Public, configuration-only evidence of the containment policy that
/// admitted a run. It deliberately does not claim an executed command.
#[derive(Clone, Serialize)]
struct ConfiguredExecutionContainmentProvenance {
    schema: &'static str,
    admission: &'static str,
    backend_kind: Option<&'static str>,
    scope: Option<&'static str>,
    policy_version: Option<u16>,
    configuration_digest: Option<String>,
}

fn configured_execution_containment_provenance(
    certification: &RunExecutionCertification,
) -> ConfiguredExecutionContainmentProvenance {
    match certification {
        RunExecutionCertification::ManualHost => ConfiguredExecutionContainmentProvenance {
            schema: "hermes.execution-containment-provenance/v1",
            admission: "manual_host",
            backend_kind: None,
            scope: None,
            policy_version: None,
            configuration_digest: None,
        },
        RunExecutionCertification::CertifiedBackendConfigured { .. } => {
            ConfiguredExecutionContainmentProvenance {
                schema: "hermes.execution-containment-provenance/v1",
                admission: "certified_backend_configured",
                backend_kind: certification.backend_kind().map(|kind| kind.as_str()),
                scope: certification.scope().map(|scope| scope.as_str()),
                policy_version: certification.policy_version(),
                configuration_digest: certification
                    .configuration_digest()
                    .map(|digest| digest.as_str().to_string()),
            }
        }
    }
}

#[derive(Serialize)]
struct AgentLoopEffectPolicy {
    schema: &'static str,
    resolved_execution_mode: ResolvedExecutionMode,
    execution_containment: ConfiguredExecutionContainmentProvenance,
    permitted_effect_intents: [EffectIntent; 2],
}

fn agent_loop_effect_policy(
    mode: ResolvedExecutionMode,
    containment: &ConfiguredExecutionContainmentProvenance,
) -> AgentLoopEffectPolicy {
    AgentLoopEffectPolicy {
        schema: "hermes.agent-loop-effect-policy/v2",
        resolved_execution_mode: mode,
        execution_containment: containment.clone(),
        permitted_effect_intents: [EffectIntent::None, EffectIntent::LocalOneShotV1],
    }
}

fn public_config_projection(config: &AgentConfig) -> Value {
    json!({
        "api_mode": config.api_mode,
        "background_review_enabled": config.background_review_enabled,
        "background_review_metrics_enabled": config.background_review_metrics_enabled,
        "budget": config.budget,
        "checkpoint_interval_turns": config.checkpoint_interval_turns,
        "code_index_enabled": config.code_index_enabled,
        "code_index_max_files": config.code_index_max_files,
        "code_index_max_symbols": config.code_index_max_symbols,
        "execution_mode": config.execution_mode,
        "lsp_context_enabled": config.lsp_context_enabled,
        "lsp_context_max_chars": config.lsp_context_max_chars,
        "max_concurrent_delegates": config.max_concurrent_delegates,
        "max_cost_usd": config.max_cost_usd,
        "max_delegate_depth": config.max_delegate_depth,
        "max_tokens": config.max_tokens,
        "max_turns": config.max_turns,
        "memory_flush_interval": config.memory_flush_interval,
        "memory_nudge_interval": config.memory_nudge_interval,
        "preflight_context_compress": config.preflight_context_compress,
        "quiet_mode": config.quiet_mode,
        "rollback_on_tool_error_threshold": config.rollback_on_tool_error_threshold,
        "skill_creation_nudge_interval": config.skill_creation_nudge_interval,
        "skip_context_files": config.skip_context_files,
        "skip_memory": config.skip_memory,
        "stream": config.stream,
    })
}

fn public_surface(surface: Option<&str>) -> &'static str {
    match surface
        .map(str::trim)
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("acp") => "acp",
        Some("cli") => "cli",
        Some("cron") => "cron",
        Some("discord") => "discord",
        Some("gateway") => "gateway",
        Some("http") => "http",
        Some("slack") => "slack",
        Some("telegram") => "telegram",
        Some("tui") => "tui",
        Some("whatsapp") => "whatsapp",
        None | Some("") | Some("agent") => "agent",
        Some(_) => "custom",
    }
}

fn bound_runtime_evidence(evidence_id: &str, evidence_subject: &str, payload: Value) -> Value {
    json!({
        "evidence_id": evidence_id,
        "evidence_subject": evidence_subject,
        "payload": payload,
    })
}

/// Closed private transcript wrapper for the only runtime schemas allowed to
/// contribute to exact replay. Its identity fields intentionally duplicate the
/// public identity so the sealed-artifact validator can reject substitutions.
fn bound_exact_runtime_evidence(
    evidence_id: &str,
    evidence_subject: &str,
    phase: &str,
    payload: Value,
) -> Value {
    json!({
        "evidence_id": evidence_id,
        "evidence_subject": evidence_subject,
        "payload": payload,
        "phase": phase,
        "schema": RUNTIME_EVIDENCE_SCHEMA,
    })
}

/// Preserve the caller's payload verbatim unless it is the closed
/// `delegate_task` request shape. Returning the original invalid/unknown
/// payload causes the exact classifier to fail closed while the normal
/// redacted metadata artifact still records honest evidence.
fn closed_delegation_request_payload(
    tool_call_id: &str,
    invocation_key: &str,
    raw: &Value,
) -> Value {
    let Some(fields) = raw.as_object() else {
        return raw.clone();
    };
    if fields.keys().any(|key| {
        !matches!(
            key.as_str(),
            "background" | "context" | "model" | "task" | "toolset"
        )
    }) {
        return raw.clone();
    }
    let Some(task) = fields.get("task").and_then(Value::as_str) else {
        return raw.clone();
    };
    if fields
        .get("background")
        .is_some_and(|value| !value.is_boolean())
        || fields
            .get("context")
            .is_some_and(|value| !value.is_string() && !value.is_null())
        || fields
            .get("model")
            .is_some_and(|value| !value.is_string() && !value.is_null())
        || fields
            .get("toolset")
            .is_some_and(|value| !value.is_string() && !value.is_null())
    {
        return raw.clone();
    }
    json!({
        "background": fields.get("background").cloned().unwrap_or(Value::Bool(false)),
        "call_id": tool_call_id,
        "context": fields.get("context").cloned().unwrap_or(Value::Null),
        "invocation_key": invocation_key,
        "model": fields.get("model").cloned().unwrap_or(Value::Null),
        "task": task,
        "tool_name": "delegate_task",
        "toolset": fields.get("toolset").cloned().unwrap_or(Value::Null),
    })
}

/// Collapse operation errors to a fixed credential-free class before they
/// enter an exact OAuth transcript. The original error remains available only
/// through the regular redacted metadata path when exact preparation rejects.
fn oauth_refresh_error_class(error: &AgentError) -> &'static str {
    match error {
        AgentError::AuthFailed(_) => "auth_failed",
        AgentError::Interrupted { .. } | AgentError::Timeout(_) => "credential_unavailable",
        AgentError::LlmApi(_)
        | AgentError::ToolExecution(_)
        | AgentError::Config(_)
        | AgentError::PolicyBlocked(_)
        | AgentError::Gateway(_)
        | AgentError::MaxTurnsExceeded
        | AgentError::InvalidToolCall(_)
        | AgentError::ContextTooLong
        | AgentError::RateLimited { .. }
        | AgentError::Io(_) => "operation_failed",
    }
}

fn safe_runtime_artifact(
    evidence_id: &str,
    evidence_subject: &str,
    artifact_kind: &str,
    outcome: &str,
    private_evidence: &Value,
) -> Result<Value, serde_json::Error> {
    Ok(json!({
        "identity": {
            "artifact_kind": artifact_kind,
            "outcome": outcome,
            "replay_fidelity": "credential_redacted",
            "schema": RUNTIME_EVIDENCE_SCHEMA,
            "evidence_id": evidence_id,
            "evidence_subject": evidence_subject,
        },
        "private": private_evidence,
    }))
}

fn llm_request_append_key(run_id: RunId, call_id: &str) -> Result<AppendKey, LedgerError> {
    AppendKey::new(format!(
        "llm-request-{}",
        scoped_key_hash(run_id, "llm-request", call_id)
    ))
}

fn llm_uncertain_append_key(run_id: RunId, call_id: &str) -> Result<AppendKey, LedgerError> {
    AppendKey::new(format!(
        "llm-uncertain-{}",
        scoped_key_hash(run_id, "llm-uncertain", call_id)
    ))
}
fn tool_uncertain_append_key(
    run_id: RunId,
    invocation_key: &str,
) -> Result<AppendKey, LedgerError> {
    AppendKey::new(format!(
        "tool-uncertain-{}",
        scoped_key_hash(run_id, "tool-uncertain", invocation_key)
    ))
}

fn intent_append_key(run_id: RunId, tool_call_id: &str) -> Result<AppendKey, LedgerError> {
    AppendKey::new(format!(
        "tool-intent-{}",
        scoped_key_hash(run_id, "intent", tool_call_id)
    ))
}

fn result_append_key(run_id: RunId, tool_call_id: &str) -> Result<AppendKey, LedgerError> {
    AppendKey::new(format!(
        "tool-result-{}",
        scoped_key_hash(run_id, "result", tool_call_id)
    ))
}

fn output_append_key(run_id: RunId, tool_call_id: &str) -> Result<AppendKey, LedgerError> {
    AppendKey::new(format!(
        "agent-output-{}",
        scoped_key_hash(run_id, "output", tool_call_id)
    ))
}

fn scoped_key_hash(run_id: RunId, phase: &str, invocation_slot: &str) -> String {
    let run_id = run_id.to_string();
    let mut hasher = Sha256::new();
    for value in [run_id.as_str(), phase, invocation_slot] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    hex::encode(hasher.finalize())
}

fn digest<T: Serialize>(value: &T) -> Result<Sha256Digest, RunRuntimeError> {
    let bytes = serde_json_canonicalizer::to_vec(value)?;
    Ok(Sha256Digest::new(hex::encode(Sha256::digest(bytes)))?)
}

fn bounded_identifier(value: &str, label: &str) -> Result<String, RunRuntimeError> {
    let trimmed = value.trim();
    let valid = !trimmed.is_empty()
        && trimmed.len() <= 256
        && trimmed.chars().all(|ch| {
            ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | ':' | '/' | '@' | '+')
        });
    if valid {
        Ok(trimmed.to_string())
    } else {
        Err(RunRuntimeError::InvalidToolContract(format!(
            "{label} identity must contain 1 to 256 public identifier characters"
        )))
    }
}

fn tool_ledger_error(error: LedgerError) -> ToolError {
    ToolError::ExecutionFailed(format!("run ledger rejected tool execution: {error}"))
}

fn tool_runtime_error(error: impl std::fmt::Display) -> ToolError {
    ToolError::ExecutionFailed(format!("run runtime rejected tool execution: {error}"))
}

fn run_record_initialization_marker_path(root: &Path) -> PathBuf {
    root.join(RUN_RECORD_INITIALIZATION_MARKER)
}

fn run_record_initialization_marker_exists(root: &Path) -> Result<bool, RunRuntimeError> {
    let marker = run_record_initialization_marker_path(root);
    let metadata = match fs::symlink_metadata(&marker) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(SealedArtifactError::KeyUnavailable(
            "run-record initialization marker must be a regular file".to_string(),
        )
        .into());
    }
    let bytes = fs::read(&marker)?;
    if bytes == RUN_RECORD_INITIALIZATION_MARKER_BYTES {
        Ok(true)
    } else {
        Err(SealedArtifactError::KeyUnavailable(
            "run-record initialization marker is invalid".to_string(),
        )
        .into())
    }
}

fn write_run_record_initialization_marker(root: &Path) -> Result<(), RunRuntimeError> {
    if run_record_initialization_marker_exists(root)? {
        return Ok(());
    }
    let destination = run_record_initialization_marker_path(root);
    let temporary = root.join(format!(
        ".{RUN_RECORD_INITIALIZATION_MARKER}.{}.tmp",
        uuid::Uuid::new_v4()
    ));
    #[cfg(unix)]
    let mut file = {
        use std::os::unix::fs::OpenOptionsExt;
        OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&temporary)?
    };
    #[cfg(not(unix))]
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;

    let result = (|| -> Result<(), RunRuntimeError> {
        file.write_all(RUN_RECORD_INITIALIZATION_MARKER_BYTES)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temporary, &destination)?;
        #[cfg(unix)]
        {
            std::fs::File::open(root)?.sync_all()?;
        }
        if !run_record_initialization_marker_exists(root)? {
            return Err(SealedArtifactError::KeyUnavailable(
                "run-record initialization marker disappeared after publication".to_string(),
            )
            .into());
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(temporary);
    }
    result
}

fn ensure_private_directory(path: &Path) -> std::io::Result<()> {
    if !path.exists() {
        fs::create_dir_all(path)?;
    }
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "run runtime directory must be a real directory",
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn ensure_private_file(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
        OpenOptions::new()
            .create(true)
            .append(true)
            .mode(0o600)
            .open(path)?;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    {
        OpenOptions::new().create(true).append(true).open(path)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sealed_artifact::{
        ArtifactCredentialProvisionGuard, ArtifactCredentialStore, ArtifactKeyResolver,
        ExactArtifactAuthorization,
    };
    use hermes_core::execution_mode::ExecutionModeSource;
    use zeroize::Zeroizing;
    fn test_start_evidence<'a>(
        messages: &'a [Message],
        advertised_tools: &'a [ToolSchema],
    ) -> PreparedRunStartEvidence<'a> {
        PreparedRunStartEvidence::new(
            messages,
            advertised_tools,
            PreparedSystemPromptEvidence::Fresh {
                content: "runtime test system prompt",
                injected_skills_block: None,
            },
        )
    }

    fn test_manual_containment(mode: &ResolvedExecutionMode) -> ExecutionContainmentContext {
        ExecutionContainmentContext::manual(*mode)
            .expect("manual tests require an explicit host-local context")
    }

    fn start_commitment(inspection: &RunInspection) -> String {
        let start = inspection
            .events
            .iter()
            .find(|event| event.kind == crate::run_ledger::SessionRunEventKind::RunStart)
            .expect("run start event");
        start
            .public_payload
            .as_value()
            .expect("canonical start payload")
            .get("payload")
            .and_then(serde_json::Value::as_object)
            .and_then(|payload| payload.get("private_provenance_commitment"))
            .and_then(serde_json::Value::as_str)
            .expect("keyed private start commitment")
            .to_string()
    }

    fn assert_sqlite_has_no_private_text(
        connection: &rusqlite::Connection,
        private_values: &[&str],
    ) {
        let mut statement = connection
            .prepare(
                "SELECT name FROM sqlite_master \
                 WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
            )
            .expect("enumerate ledger tables");
        let tables = statement
            .query_map([], |row| row.get::<_, String>(0))
            .expect("read ledger table names")
            .collect::<Result<Vec<_>, _>>()
            .expect("collect ledger table names");
        for table in tables {
            let quoted_table = format!("\"{}\"", table.replace('"', "\"\""));
            let mut columns_statement = connection
                .prepare(&format!("PRAGMA table_info({quoted_table})"))
                .expect("enumerate ledger columns");
            let columns = columns_statement
                .query_map([], |row| row.get::<_, String>(1))
                .expect("read ledger column names")
                .collect::<Result<Vec<_>, _>>()
                .expect("collect ledger column names");
            for column in columns {
                let quoted_column = format!("\"{}\"", column.replace('"', "\"\""));
                let query = format!(
                    "SELECT COUNT(*) FROM {quoted_table} \
                     WHERE instr(CAST({quoted_column} AS TEXT), ?1) > 0"
                );
                for private_value in private_values {
                    let count: i64 = connection
                        .query_row(&query, [private_value], |row| row.get(0))
                        .expect("scan public SQLite column");
                    assert_eq!(
                        count, 0,
                        "private value appeared in SQLite table {table}, column {column}"
                    );
                }
            }
        }
    }

    fn assert_artifact_tree_has_no_private_text(root: &Path, private_values: &[&str]) {
        let entries = std::fs::read_dir(root).expect("read private artifact directory");
        for entry in entries {
            let path = entry.expect("artifact directory entry").path();
            if path.is_dir() {
                assert_artifact_tree_has_no_private_text(&path, private_values);
                continue;
            }
            let bytes = std::fs::read(&path).expect("read sealed artifact metadata");
            for private_value in private_values {
                assert!(
                    !bytes
                        .windows(private_value.len())
                        .any(|window| window == private_value.as_bytes()),
                    "private value appeared in sealed artifact bytes at {}",
                    path.display()
                );
            }
        }
    }

    struct AllowExactStartRead;

    impl ExactArtifactAuthorization for AllowExactStartRead {
        fn authorize_exact_read(
            &self,
            _artifact: &crate::sealed_artifact::SealedArtifactRef,
        ) -> Result<(), SealedArtifactError> {
            Ok(())
        }
    }

    struct CorruptCredentialGuard;

    impl ArtifactCredentialProvisionGuard for CorruptCredentialGuard {}

    struct CorruptCredentialStore;

    impl ArtifactCredentialStore for CorruptCredentialStore {
        fn load_secret(
            &self,
            _service: &str,
            _account: &str,
        ) -> Result<Option<Zeroizing<Vec<u8>>>, SealedArtifactError> {
            Ok(Some(Zeroizing::new(vec![0])))
        }

        fn store_secret(
            &self,
            _service: &str,
            _account: &str,
            _secret: &[u8],
        ) -> Result<(), SealedArtifactError> {
            Ok(())
        }

        fn acquire_provision_lock(
            &self,
            _service: &str,
            _store_namespace: &str,
        ) -> Result<Box<dyn ArtifactCredentialProvisionGuard>, SealedArtifactError> {
            Ok(Box::new(CorruptCredentialGuard))
        }
    }

    #[tokio::test]
    async fn resolved_nonmanual_modes_are_rejected_before_run_start_or_effect() {
        let state = tempfile::tempdir().expect("runtime state directory");
        let runtime = Arc::new(
            RunRuntime::open_for_ephemeral_testing(state.path()).expect("ephemeral runtime"),
        );
        let tools = ToolRegistry::new();
        let invocation = RunInvocationContext::default();
        let mut config = AgentConfig::default();

        config.execution_mode = ResolvedExecutionMode {
            mode: ExecutionMode::Manual,
            source: ExecutionModeSource::BuiltInDefault,
        };
        let active = runtime
            .start_run(
                &config,
                &test_manual_containment(&config.execution_mode),
                &tools,
                test_start_evidence(&[], &[]),
                &invocation,
                None,
            )
            .expect("manual mode starts");
        active
            .finish(TerminalReason::Completed)
            .await
            .expect("manual run finalizes");

        for mode in [
            ExecutionMode::Reviewed,
            ExecutionMode::AutonomousLocal,
            ExecutionMode::Ci,
            ExecutionMode::RemoteWorker,
        ] {
            let resolved = hermes_core::execution_mode::resolve_execution_mode(
                ExecutionMode::Manual,
                None,
                None,
                None,
                Some(mode.as_str()),
            )
            .expect("closed nonmanual CLI mode resolves");
            assert_eq!(resolved.mode, mode);
            assert_eq!(resolved.source, ExecutionModeSource::Cli);
            let rejection = ExecutionContainmentContext::manual(resolved)
                .expect_err("closed nonmanual mode must require a certified context");
            assert_eq!(
                rejection,
                hermes_environments::ExecutionContainmentAdmissionError::MissingCertificate {
                    mode
                }
            );
            config.execution_mode = resolved;
            let manual_mode = AgentConfig::default().execution_mode;
            let manual_containment = test_manual_containment(&manual_mode);
            match runtime.start_run(
                &config,
                &manual_containment,
                &tools,
                test_start_evidence(&[], &[]),
                &invocation,
                None,
            ) {
                Err(RunRuntimeError::ExecutionContainmentModeMismatch { expected, received }) => {
                    assert_eq!(expected, mode);
                    assert_eq!(received, ExecutionMode::Manual);
                }
                Err(other) => panic!("unexpected non-manual mode error: {other}"),
                Ok(_) => panic!("non-manual mode must not start an authoritative run"),
            }
        }
        let connection = runtime.ledger().connection_factory().open().unwrap();
        let persisted_runs: i64 = connection
            .query_row("SELECT COUNT(*) FROM session_runs", [], |row| row.get(0))
            .unwrap();
        assert_eq!(persisted_runs, 1);
    }

    #[tokio::test]
    async fn start_run_reconciles_expired_prior_owner_before_new_manual_admission() {
        let state = tempfile::tempdir().expect("runtime state directory");
        let factory = EphemeralRunRuntimeFactory::default();
        let runtime_a = Arc::new(factory.open(state.path()).expect("runtime A"));
        let config = AgentConfig::default();
        let tools = ToolRegistry::new();
        let messages = vec![Message::user("runtime A input")];
        let advertised = Vec::<ToolSchema>::new();
        let active_a = runtime_a
            .start_run(
                &config,
                &test_manual_containment(&config.execution_mode),
                &tools,
                test_start_evidence(&messages, &advertised),
                &RunInvocationContext::default(),
                None,
            )
            .expect("runtime A starts");
        let stale_run_id = active_a.run_id();
        let heartbeat_task = active_a
            .heartbeat_task
            .lock()
            .await
            .take()
            .expect("test simulates runtime A process death");
        heartbeat_task.abort();
        let _ = heartbeat_task.await;
        drop(active_a);
        drop(runtime_a);

        let runtime_b = Arc::new(factory.open(state.path()).expect("runtime B opens"));
        assert!(
            runtime_b
                .inspect_run(stale_run_id)
                .expect("inspect live A after B opens")
                .status
                .exit_terminal_reason
                .is_none(),
            "B opening while A's lease is live must not recover A"
        );
        let live_b = runtime_b
            .start_run(
                &config,
                &test_manual_containment(&config.execution_mode),
                &tools,
                test_start_evidence(&messages, &advertised),
                &RunInvocationContext::default(),
                None,
            )
            .expect("B can admit a separate manual run while A remains live");
        assert!(runtime_b
            .inspect_run(stale_run_id)
            .expect("inspect live A after B start")
            .status
            .exit_terminal_reason
            .is_none());
        live_b
            .finish(TerminalReason::Completed)
            .await
            .expect("first B run finalizes");

        let connection = runtime_b
            .ledger()
            .connection_factory()
            .open()
            .expect("open owner lease fixture database");
        connection
            .execute(
                "UPDATE session_runs SET owner_lease_expires_at_ms = 0 WHERE run_id = ?1",
                [stale_run_id.to_string()],
            )
            .expect("expire A lease without sleeping");

        let recovered_b = runtime_b
            .start_run(
                &config,
                &test_manual_containment(&config.execution_mode),
                &tools,
                test_start_evidence(&messages, &advertised),
                &RunInvocationContext::default(),
                None,
            )
            .expect("B reconciles A before admitting its next run");
        let recovered_a = runtime_b
            .inspect_run(stale_run_id)
            .expect("inspect recovered A");
        assert_eq!(
            recovered_a.status.exit_terminal_reason,
            Some(TerminalReason::ProcessDeath)
        );
        assert_eq!(
            recovered_a
                .events
                .iter()
                .filter(|event| event.kind == crate::run_ledger::SessionRunEventKind::RunExit)
                .count(),
            1,
            "lease recovery must append one terminal exit"
        );
        recovered_b
            .finish(TerminalReason::Completed)
            .await
            .expect("recovery-admitted B run finalizes");

        let later_b = runtime_b
            .start_run(
                &config,
                &test_manual_containment(&config.execution_mode),
                &tools,
                test_start_evidence(&messages, &advertised),
                &RunInvocationContext::default(),
                None,
            )
            .expect("later B start does not duplicate A recovery");
        let settled_a = runtime_b
            .inspect_run(stale_run_id)
            .expect("inspect settled A");
        assert_eq!(
            settled_a
                .events
                .iter()
                .filter(|event| event.kind == crate::run_ledger::SessionRunEventKind::RunExit)
                .count(),
            1,
            "already terminal A cannot be recovered twice"
        );
        later_b
            .finish(TerminalReason::Completed)
            .await
            .expect("later B run finalizes");
    }

    #[tokio::test]
    async fn start_evidence_is_sealed_keyed_and_private_values_never_enter_sqlite() {
        let state = tempfile::tempdir().expect("runtime state directory");
        let runtime = Arc::new(
            RunRuntime::open_for_ephemeral_testing(state.path()).expect("ephemeral runtime"),
        );
        let tools = ToolRegistry::new();
        let private_prompt = "PRIVATE_START_PROMPT_SENTINEL_20260720";
        let private_input = "PRIVATE_START_INPUT_SENTINEL_20260720";
        let delegation_key = "sk-DELEGATION_CONFIG_SENTINEL_1234567890";
        let provider_key = "sk-PROVIDER_CONFIG_SENTINEL_1234567890";
        let mut config = AgentConfig::default();
        config.delegation_api_key = Some(delegation_key.to_string());
        config.runtime_providers.insert(
            "openai".to_string(),
            crate::agent_loop::RuntimeProviderConfig {
                api_key: Some(provider_key.to_string()),
                ..crate::agent_loop::RuntimeProviderConfig::default()
            },
        );
        let first_messages = vec![Message::user(private_input)];
        let first = runtime
            .start_run(
                &config,
                &test_manual_containment(&config.execution_mode),
                &tools,
                PreparedRunStartEvidence::new(
                    &first_messages,
                    &[],
                    PreparedSystemPromptEvidence::Fresh {
                        content: private_prompt,
                        injected_skills_block: Some("PRIVATE_SKILLS_SENTINEL_20260720"),
                    },
                ),
                &RunInvocationContext::default(),
                None,
            )
            .expect("private start evidence admits");
        let first_inspection = runtime
            .inspect_run(first.run_id())
            .expect("inspect first private run");
        let start_event = first_inspection
            .events
            .iter()
            .find(|event| event.kind == crate::run_ledger::SessionRunEventKind::RunStart)
            .expect("run start event");
        let sealed_reference = start_event
            .sealed_artifact_ref
            .as_ref()
            .expect("authoritative start must carry a sealed exact-evidence reference");
        let coordinator = ExactReplayLifecycleCoordinator::new(
            runtime.ledger().as_ref(),
            runtime.keys.as_ref(),
            runtime.store.as_ref(),
        );
        let exact = coordinator
            .read_exact(
                first.run_id(),
                start_event.event_id,
                sealed_reference,
                &AllowExactStartRead,
            )
            .expect("authorized start evidence read");
        let exact: Value =
            serde_json::from_slice(exact.as_slice()).expect("exact prepared start evidence");
        assert_eq!(exact["system_prompt"], private_prompt);
        assert_eq!(
            exact["messages"],
            serde_json::to_value(&first_messages).expect("serialize exact input")
        );
        assert_eq!(
            exact["injected_skills_block"],
            "PRIVATE_SKILLS_SENTINEL_20260720"
        );
        assert_eq!(exact["schema"], "hermes.agent-run-start-private/v1");
        assert!(exact.get("config").is_none());
        assert_eq!(exact["system_prompt_source"], "fresh");
        assert!(exact.get("config_projection").is_some());
        assert!(!exact.to_string().contains(delegation_key));
        assert!(!exact.to_string().contains(provider_key));
        let first_commitment = start_commitment(&first_inspection);
        assert_eq!(first_commitment.len(), 64);
        assert!(
            first_commitment
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit()),
            "the typed start payload round-trips a keyed commitment string"
        );
        first
            .finish(TerminalReason::Completed)
            .await
            .expect("first private run finalizes");

        let changed_input = format!("{private_input} changed");
        let changed_messages = vec![Message::user(changed_input.clone())];
        let changed = runtime
            .start_run(
                &config,
                &test_manual_containment(&config.execution_mode),
                &tools,
                PreparedRunStartEvidence::new(
                    &changed_messages,
                    &[],
                    PreparedSystemPromptEvidence::Fresh {
                        content: private_prompt,
                        injected_skills_block: Some("PRIVATE_SKILLS_SENTINEL_20260720"),
                    },
                ),
                &RunInvocationContext::default(),
                None,
            )
            .expect("changed private start evidence admits");
        let changed_inspection = runtime
            .inspect_run(changed.run_id())
            .expect("inspect changed private run");
        assert_ne!(
            first_commitment,
            start_commitment(&changed_inspection),
            "changing one prepared private byte must change the keyed commitment"
        );
        changed
            .finish(TerminalReason::Completed)
            .await
            .expect("changed private run finalizes");

        let changed_prompt = format!("{private_prompt} changed");
        let prompt_changed = runtime
            .start_run(
                &config,
                &test_manual_containment(&config.execution_mode),
                &tools,
                PreparedRunStartEvidence::new(
                    &first_messages,
                    &[],
                    PreparedSystemPromptEvidence::Fresh {
                        content: &changed_prompt,
                        injected_skills_block: Some("PRIVATE_SKILLS_SENTINEL_20260720"),
                    },
                ),
                &RunInvocationContext::default(),
                None,
            )
            .expect("prompt-mutated private start evidence admits");
        assert_ne!(
            first_commitment,
            start_commitment(
                &runtime
                    .inspect_run(prompt_changed.run_id())
                    .expect("inspect prompt-mutated private run")
            ),
            "changing prepared prompt bytes must change the keyed commitment"
        );
        prompt_changed
            .finish(TerminalReason::Completed)
            .await
            .expect("prompt-mutated private run finalizes");

        let changed_skills = "PRIVATE_SKILLS_SENTINEL_20260720 changed";
        let skills_changed = runtime
            .start_run(
                &config,
                &test_manual_containment(&config.execution_mode),
                &tools,
                PreparedRunStartEvidence::new(
                    &first_messages,
                    &[],
                    PreparedSystemPromptEvidence::Fresh {
                        content: private_prompt,
                        injected_skills_block: Some(changed_skills),
                    },
                ),
                &RunInvocationContext::default(),
                None,
            )
            .expect("skills-mutated private start evidence admits");
        assert_ne!(
            first_commitment,
            start_commitment(
                &runtime
                    .inspect_run(skills_changed.run_id())
                    .expect("inspect skills-mutated private run")
            ),
            "changing prepared injected-skills bytes must change the keyed commitment"
        );
        skills_changed
            .finish(TerminalReason::Completed)
            .await
            .expect("skills-mutated private run finalizes");

        let private_tool_schema = "PRIVATE_TOOL_SCHEMA_SENTINEL_20260720";
        let changed_tools = vec![ToolSchema::new(
            "private-tool",
            private_tool_schema,
            hermes_core::JsonSchema::new("object"),
        )];
        let tools_changed = runtime
            .start_run(
                &config,
                &test_manual_containment(&config.execution_mode),
                &tools,
                PreparedRunStartEvidence::new(
                    &first_messages,
                    &changed_tools,
                    PreparedSystemPromptEvidence::Fresh {
                        content: private_prompt,
                        injected_skills_block: Some("PRIVATE_SKILLS_SENTINEL_20260720"),
                    },
                ),
                &RunInvocationContext::default(),
                None,
            )
            .expect("tools-mutated private start evidence admits");
        assert_ne!(
            first_commitment,
            start_commitment(
                &runtime
                    .inspect_run(tools_changed.run_id())
                    .expect("inspect tools-mutated private run")
            ),
            "changing prepared tool-schema bytes must change the keyed commitment"
        );
        tools_changed
            .finish(TerminalReason::Completed)
            .await
            .expect("tools-mutated private run finalizes");

        let prompt_sha = hex::encode(Sha256::digest(private_prompt.as_bytes()));
        let input_sha = hex::encode(Sha256::digest(private_input.as_bytes()));
        let changed_input_sha = hex::encode(Sha256::digest(changed_input.as_bytes()));
        let delegation_key_sha = hex::encode(Sha256::digest(delegation_key.as_bytes()));
        let provider_key_sha = hex::encode(Sha256::digest(provider_key.as_bytes()));
        let changed_prompt_sha = hex::encode(Sha256::digest(changed_prompt.as_bytes()));
        let changed_skills_sha = hex::encode(Sha256::digest(changed_skills.as_bytes()));
        let private_tool_schema_sha = hex::encode(Sha256::digest(private_tool_schema.as_bytes()));
        let connection = runtime
            .ledger()
            .connection_factory()
            .open()
            .expect("open runtime SQLite database");
        assert_sqlite_has_no_private_text(
            &connection,
            &[
                private_prompt,
                private_input,
                "PRIVATE_SKILLS_SENTINEL_20260720",
                &changed_prompt,
                changed_skills,
                private_tool_schema,
                delegation_key,
                provider_key,
                &prompt_sha,
                &input_sha,
                &changed_input,
                &changed_input_sha,
                &changed_prompt_sha,
                &changed_skills_sha,
                &private_tool_schema_sha,
                &delegation_key_sha,
                &provider_key_sha,
            ],
        );
        let private_values = [
            private_prompt,
            private_input,
            "PRIVATE_SKILLS_SENTINEL_20260720",
            &changed_prompt,
            changed_skills,
            private_tool_schema,
            delegation_key,
            provider_key,
            &prompt_sha,
            &input_sha,
            &changed_input,
            &changed_input_sha,
            &changed_prompt_sha,
            &changed_skills_sha,
            &private_tool_schema_sha,
            &delegation_key_sha,
            &provider_key_sha,
        ];
        assert_artifact_tree_has_no_private_text(
            &state.path().join("run-record-v1").join("artifacts"),
            &private_values,
        );
        for run_id in [
            first.run_id(),
            changed.run_id(),
            prompt_changed.run_id(),
            skills_changed.run_id(),
            tools_changed.run_id(),
        ] {
            let export = runtime
                .publish_run_export(run_id)
                .expect("public run export");
            let export_bytes = std::fs::read(export.path).expect("read public run export");
            for private_value in private_values {
                assert!(
                    !export_bytes
                        .windows(private_value.len())
                        .any(|window| window == private_value.as_bytes()),
                    "private value appeared in public run export"
                );
            }
        }
    }

    #[tokio::test]
    async fn sealed_start_prompt_source_is_closed_and_consistent() {
        let state = tempfile::tempdir().expect("runtime state directory");
        let runtime = Arc::new(
            RunRuntime::open_for_ephemeral_testing(state.path()).expect("ephemeral runtime"),
        );
        let config = AgentConfig::default();
        let tools = ToolRegistry::new();
        let advertised = vec![ToolSchema::new(
            "sealed-tool",
            "Exact advertised schema",
            hermes_core::JsonSchema::new("object"),
        )];
        let exact_for = |active: &Arc<ActiveRun>| {
            let inspection = runtime
                .inspect_run(active.run_id())
                .expect("inspect started run");
            let event = inspection
                .events
                .iter()
                .find(|event| event.kind == crate::run_ledger::SessionRunEventKind::RunStart)
                .expect("run start event");
            let coordinator = ExactReplayLifecycleCoordinator::new(
                runtime.ledger().as_ref(),
                runtime.keys.as_ref(),
                runtime.store.as_ref(),
            );
            let bytes = coordinator
                .read_exact(
                    active.run_id(),
                    event.event_id,
                    event
                        .sealed_artifact_ref
                        .as_ref()
                        .expect("sealed start reference"),
                    &AllowExactStartRead,
                )
                .expect("authorized exact start read");
            serde_json::from_slice::<Value>(bytes.as_slice()).expect("closed start object")
        };

        let fresh_messages = vec![Message::user("fresh")];
        let fresh = runtime
            .start_run(
                &config,
                &test_manual_containment(&config.execution_mode),
                &tools,
                PreparedRunStartEvidence::new(
                    &fresh_messages,
                    &advertised,
                    PreparedSystemPromptEvidence::Fresh {
                        content: "fresh system",
                        injected_skills_block: None,
                    },
                ),
                &RunInvocationContext::default(),
                None,
            )
            .expect("fresh start");
        let fresh_exact = exact_for(&fresh);
        assert_eq!(fresh_exact["system_prompt_source"], "fresh");
        assert_eq!(fresh_exact["system_prompt"], "fresh system");
        assert!(fresh_exact["injected_skills_block"].is_null());
        assert_eq!(
            fresh_exact["advertised_tools"],
            serde_json::to_value(&advertised).expect("serialize exact advertised tools")
        );
        fresh
            .finish(TerminalReason::Completed)
            .await
            .expect("fresh finish");

        let stored_messages = vec![Message::user("stored")];
        let stored = runtime
            .start_run(
                &config,
                &test_manual_containment(&config.execution_mode),
                &tools,
                PreparedRunStartEvidence::new(
                    &stored_messages,
                    &[],
                    PreparedSystemPromptEvidence::Stored {
                        content: "stored system",
                    },
                ),
                &RunInvocationContext::default(),
                None,
            )
            .expect("stored start");
        let stored_exact = exact_for(&stored);
        assert_eq!(stored_exact["system_prompt_source"], "stored");
        assert_eq!(stored_exact["system_prompt"], "stored system");
        assert!(
            stored_exact["injected_skills_block"].is_null(),
            "stored prompts must not claim fresh injected-skills provenance"
        );
        stored
            .finish(TerminalReason::Completed)
            .await
            .expect("stored finish");

        let unavailable = runtime
            .start_run(
                &config,
                &test_manual_containment(&config.execution_mode),
                &tools,
                PreparedRunStartEvidence::unavailable_for_nonexecuting_test(&[], &[]),
                &RunInvocationContext::default(),
                None,
            )
            .expect("unavailable blocked start");
        let unavailable_exact = exact_for(&unavailable);
        assert_eq!(unavailable_exact["system_prompt_source"], "unavailable");
        assert!(unavailable_exact["system_prompt"].is_null());
        assert!(unavailable_exact["injected_skills_block"].is_null());
        assert_eq!(unavailable_exact["messages"], serde_json::json!([]));
        let unavailable_header = runtime
            .inspect_run(unavailable.run_id())
            .expect("inspect unavailable run")
            .header;
        assert_eq!(
            unavailable_header.provenance.prompt_digest(),
            &ProvenanceDigest::absent(DigestAbsence::Unavailable)
        );
        unavailable
            .finish(TerminalReason::PolicyBlocked)
            .await
            .expect("unavailable finish");
    }

    #[tokio::test]
    async fn start_run_commits_authoritative_tool_and_policy_digests() {
        let state = tempfile::tempdir().expect("runtime state directory");
        let runtime = Arc::new(
            RunRuntime::open_for_ephemeral_testing(state.path()).expect("ephemeral runtime"),
        );
        let mut tools = ToolRegistry::new();
        let alpha = ToolSchema::new(
            "alpha",
            "Observed alpha tool",
            hermes_core::JsonSchema::new("object"),
        );
        let beta = ToolSchema::new(
            "beta",
            "Untrusted beta tool",
            hermes_core::JsonSchema::new("object"),
        );
        tools.register_test_observation(
            "alpha",
            alpha.clone(),
            Arc::new(|_params| Ok("observed".to_string())),
        );
        tools.register(
            "beta",
            beta.clone(),
            Arc::new(|_params| Ok("untrusted".to_string())),
        );
        let advertised = vec![beta.clone(), alpha.clone()];
        let config = AgentConfig::default();
        let active = runtime
            .start_run(
                &config,
                &test_manual_containment(&config.execution_mode),
                &tools,
                test_start_evidence(&[], &advertised),
                &RunInvocationContext::default(),
                None,
            )
            .expect("manual mode starts");

        let inspection = runtime
            .inspect_run(active.run_id())
            .expect("inspect started run");
        let expected_tool_contract = vec![
            AdvertisedToolContract {
                schema: &alpha,
                execution_trust: "observation",
            },
            AdvertisedToolContract {
                schema: &beta,
                execution_trust: "untrusted",
            },
        ];
        let manual_containment =
            configured_execution_containment_provenance(&RunExecutionCertification::ManualHost);
        let provenance = &inspection.header.provenance;
        assert_eq!(
            provenance.tool_digest(),
            &ProvenanceDigest::present(digest(&expected_tool_contract).expect("tool digest"))
        );
        assert_eq!(
            provenance.policy_digest(),
            &ProvenanceDigest::present(
                digest(&agent_loop_effect_policy(
                    config.execution_mode,
                    &manual_containment
                ))
                .expect("policy digest")
            )
        );
        assert_eq!(
            provenance.prompt_digest(),
            &ProvenanceDigest::absent(DigestAbsence::Unavailable)
        );
        assert_eq!(
            provenance.input_digest(),
            &ProvenanceDigest::absent(DigestAbsence::Unavailable)
        );
        assert_eq!(
            provenance.skill_digest(),
            &ProvenanceDigest::absent(DigestAbsence::Unavailable)
        );
        assert_eq!(
            provenance.output_digest(),
            &ProvenanceDigest::absent(DigestAbsence::NotApplicable)
        );

        active
            .finish(TerminalReason::Completed)
            .await
            .expect("started run finalizes");
    }

    #[test]
    fn tool_and_policy_digest_change_with_closed_contract_inputs() {
        let alpha = ToolSchema::new(
            "alpha",
            "Observed alpha tool",
            hermes_core::JsonSchema::new("object"),
        );
        let changed_alpha = ToolSchema::new(
            "alpha",
            "Changed alpha tool",
            hermes_core::JsonSchema::new("object"),
        );
        let baseline = [AdvertisedToolContract {
            schema: &alpha,
            execution_trust: "observation",
        }];
        let changed_schema = [AdvertisedToolContract {
            schema: &changed_alpha,
            execution_trust: "observation",
        }];
        let changed_trust = [AdvertisedToolContract {
            schema: &alpha,
            execution_trust: "untrusted",
        }];
        assert_ne!(
            digest(&baseline).expect("baseline tool digest"),
            digest(&changed_schema).expect("changed schema digest")
        );
        assert_ne!(
            digest(&baseline).expect("baseline tool digest"),
            digest(&changed_trust).expect("changed trust digest")
        );

        let default_mode = ResolvedExecutionMode {
            mode: ExecutionMode::Manual,
            source: ExecutionModeSource::BuiltInDefault,
        };
        let cli_mode = ResolvedExecutionMode {
            mode: ExecutionMode::Manual,
            source: ExecutionModeSource::Cli,
        };
        let manual_containment =
            configured_execution_containment_provenance(&RunExecutionCertification::ManualHost);
        assert_ne!(
            digest(&agent_loop_effect_policy(default_mode, &manual_containment))
                .expect("default policy digest"),
            digest(&agent_loop_effect_policy(cli_mode, &manual_containment))
                .expect("CLI policy digest")
        );
        assert_eq!(
            serde_json::to_value(agent_loop_effect_policy(default_mode, &manual_containment,))
                .expect("serialize closed policy"),
            json!({
                "schema": "hermes.agent-loop-effect-policy/v2",
                "resolved_execution_mode": {
                    "mode": "manual",
                    "source": "built_in_default",
                },
                "execution_containment": {
                    "schema": "hermes.execution-containment-provenance/v1",
                    "admission": "manual_host",
                    "backend_kind": null,
                    "scope": null,
                    "policy_version": null,
                    "configuration_digest": null,
                },
                "permitted_effect_intents": ["none", "local_one_shot_v1"],
            })
        );
    }

    #[tokio::test]
    async fn manual_store_outage_returns_a_typed_degradation_error() {
        let state = tempfile::tempdir().expect("runtime state directory");
        let runtime = Arc::new(
            RunRuntime::open_for_ephemeral_testing(state.path()).expect("ephemeral runtime"),
        );
        let tools = ToolRegistry::new();
        let config = AgentConfig::default();
        let active = runtime
            .start_run(
                &config,
                &test_manual_containment(&config.execution_mode),
                &tools,
                test_start_evidence(&[], &[]),
                &RunInvocationContext::default(),
                None,
            )
            .expect("manual mode starts");

        let artifact_root = state.path().join("run-record-v1").join("artifacts");
        let unavailable_root = state
            .path()
            .join("run-record-v1")
            .join("artifacts-unavailable");
        std::fs::rename(&artifact_root, &unavailable_root)
            .expect("temporarily make the artifact store unavailable");

        let private_evidence = bound_runtime_evidence(
            "manual-store-outage",
            "llm-request",
            json!({"request": "sealed"}),
        );
        let safe_artifact = safe_runtime_artifact(
            "manual-store-outage",
            "llm-request",
            "request",
            "not_applicable",
            &private_evidence,
        )
        .unwrap();
        let append_key = AppendKey::new("manual-store-outage-runtime").unwrap();
        let result = active
            .append_bound_artifact(
                append_key.clone(),
                EffectIntent::None,
                CanonicalJson::from_value(&json!({"schema": "runtime-test/v1"})).unwrap(),
                &safe_artifact,
                &private_evidence,
                None,
            )
            .await;
        std::fs::rename(&unavailable_root, &artifact_root)
            .expect("restore the artifact store after the outage");

        match result {
            Err(RunRuntimeError::DegradedManualAppend {
                append_key: actual_key,
                reason: ArtifactDegradationReason::ArtifactStoreUnavailable,
                ..
            }) => assert_eq!(actual_key, append_key),
            other => panic!("expected typed manual store degradation, got {other:?}"),
        }
        assert_eq!(
            runtime
                .ledger()
                .events_for_run(active.run_id())
                .unwrap()
                .len(),
            2,
            "the degradation audit is the only post-start event"
        );
        assert!(runtime
            .ledger()
            .outbox_for_run(active.run_id())
            .unwrap()
            .is_empty());
        active
            .finish(TerminalReason::ExecutionFailure)
            .await
            .expect("degraded manual run finalizes as failure");
    }

    #[tokio::test]
    async fn finish_uses_durable_result_state_after_result_event_link_failure() {
        let state = tempfile::tempdir().expect("runtime state directory");
        let runtime = Arc::new(
            RunRuntime::open_for_ephemeral_testing(state.path()).expect("ephemeral runtime"),
        );
        let active = runtime
            .start_run(
                &AgentConfig::default(),
                &test_manual_containment(&AgentConfig::default().execution_mode),
                &ToolRegistry::new(),
                test_start_evidence(&[], &[]),
                &RunInvocationContext::default(),
                None,
            )
            .expect("manual mode starts");
        let attempt = active
            .begin_llm_attempt("provider", "model", &[], &[], None, None, None, false)
            .await
            .expect("recorded request and effect lease");
        let LlmAttempt {
            call_id,
            effect_lease,
            poison_guard,
            _effect_permit,
            _lifecycle_permit,
        } = attempt;
        let evidence = bound_runtime_evidence(
            &call_id,
            "llm-response",
            json!({"response": "committed before link failure"}),
        );
        let safe = safe_runtime_artifact(&call_id, "llm-response", "result", "returned", &evidence)
            .expect("safe result evidence");
        let observed =
            RunLedger::observed_nonresumable_attempt(effect_lease, "returned").expect("observed");
        let result_payload = runtime
            .ledger()
            .persist_nonresumable_result(AgentConfig::default().execution_mode, &observed)
            .expect("persisted result payload");
        let result_receipt = active
            .append_bound_artifact(
                observed.result_append_key().clone(),
                EffectIntent::None,
                result_payload,
                &safe,
                &evidence,
                Some(ReplayCriticalArtifactAppend::new(
                    ExactRuntimeEvidenceSchema::LlmResultV1,
                    ReplayAppendContract::resolve_source(
                        ReplayOperationKind::Model,
                        ReplayOperationPhase::Result,
                        observed.event_id(),
                    ),
                )),
            )
            .await
            .expect("result event committed");
        let owner = active.lock_healthy_lease().await.expect("current owner");
        assert!(
            runtime
                .ledger()
                .complete_nonresumable_effect(
                    AgentConfig::default().execution_mode,
                    &observed,
                    EventId::new(),
                )
                .is_err(),
            "a mismatched result-event link must fail after the result event commits"
        );
        drop(owner);
        assert!(runtime
            .ledger()
            .events_for_run(active.run_id())
            .expect("read committed events")
            .iter()
            .any(|event| event.event_id == result_receipt.event_id));
        drop(_effect_permit);
        drop(_lifecycle_permit);
        drop(poison_guard);
        assert!(
            active.state.effect_poisoned.load(Ordering::SeqCst),
            "the failed link remains conservatively poisoned in memory"
        );

        active
            .finish(TerminalReason::Completed)
            .await
            .expect("durable terminalization resolves the committed result");
        let finalized = runtime
            .inspect_run(active.run_id())
            .expect("inspect finalized run");
        assert_eq!(
            finalized.status.final_outcome,
            Some(crate::run_ledger::RunOutcome::Success)
        );
        assert_eq!(
            finalized
                .events
                .iter()
                .filter(|event| event.kind == crate::run_ledger::SessionRunEventKind::RunExit)
                .count(),
            1,
            "durable finalization must append exactly one exit after link failure"
        );
        assert!(matches!(
            active
                .begin_llm_attempt("provider", "model", &[], &[], None, None, None, false)
                .await,
            Err(RunRuntimeError::AlreadyFinalized { .. })
        ));
    }

    #[test]
    fn fresh_runtime_initializes_both_key_lineages_and_reopens() {
        let state = tempfile::tempdir().expect("runtime state directory");
        let factory = EphemeralRunRuntimeFactory::default();

        let initial = factory.open(state.path()).expect("fresh state initializes");
        assert!(initial.keys.active_key().is_ok());
        assert!(initial.keys.active_fingerprint_key().is_ok());
        assert!(initial.session_keys.active_key().is_ok());
        assert!(initial.session_keys.active_fingerprint_key().is_ok());
        assert!(
            run_record_initialization_marker_exists(&state.path().join("run-record-v1"))
                .expect("read initialization marker")
        );
        drop(initial);

        let reopened = factory
            .open(state.path())
            .expect("existing state resolves the original key lineages");
        assert!(reopened.keys.active_key().is_ok());
        assert!(reopened.session_keys.active_fingerprint_key().is_ok());
    }

    #[test]
    fn existing_unrelated_home_bootstraps_run_record_key_lineages() {
        let state = tempfile::tempdir().expect("runtime state directory");
        std::fs::write(state.path().join("config.yaml"), b"model: test-model\n")
            .expect("write unrelated configuration");
        std::fs::create_dir(state.path().join("sessions"))
            .expect("create unrelated session directory");

        let runtime = EphemeralRunRuntimeFactory::default()
            .open(state.path())
            .expect("first run-record install upgrades an existing Hermes home");
        assert!(runtime.keys.active_key().is_ok());
        assert!(runtime.session_keys.active_fingerprint_key().is_ok());
        assert!(
            run_record_initialization_marker_exists(&state.path().join("run-record-v1"))
                .expect("read initialization marker")
        );
    }

    #[test]
    fn interrupted_first_install_after_ledger_migration_resumes_key_provisioning() {
        let state = tempfile::tempdir().expect("runtime state directory");
        let root = state.path().join("run-record-v1");
        let artifact_dir = root.join("artifacts");
        let exports_dir = root.join("exports");
        for directory in [&root, &artifact_dir, &exports_dir] {
            ensure_private_directory(directory).expect("prepare interrupted install directory");
        }
        let database_path = state.path().join("sessions.db");
        ensure_private_file(&database_path).expect("prepare interrupted install database");
        LedgerConnectionFactory::new(&database_path)
            .migrate_and_cut_over_v0_to_v1()
            .expect("complete ledger migration before interruption");

        let factory = EphemeralRunRuntimeFactory::default();
        let store = FilesystemSealedArtifactStore::open(&artifact_dir)
            .expect("open protected artifact store");
        let key_config = OsArtifactKeyManagerConfig::new(
            "hermes-agent-run-record",
            "run-record-sealing-v1",
            "run-record-fingerprint-v1",
            1,
            &store,
        )
        .expect("build sealing key configuration");
        OsArtifactKeyManager::with_store(key_config, factory.credentials.clone())
            .provision_active_keys_for_store(&store)
            .expect("simulate interruption after the first key lineage provisioned");
        assert!(!run_record_initialization_marker_exists(&root)
            .expect("initialization marker has not been committed"));
        drop(store);

        let resumed = factory
            .open(state.path())
            .expect("resume first install without regenerating the first key lineage");
        assert!(resumed.keys.active_key().is_ok());
        assert!(resumed.session_keys.active_key().is_ok());
        assert!(run_record_initialization_marker_exists(&root)
            .expect("resumed install commits initialization marker"));
    }

    #[test]
    fn existing_runtime_refuses_missing_credentials_on_reopen() {
        let state = tempfile::tempdir().expect("runtime state directory");
        let factory = EphemeralRunRuntimeFactory::default();
        let initial = factory.open(state.path()).expect("fresh state initializes");
        drop(initial);

        let error = match EphemeralRunRuntimeFactory::default().open(state.path()) {
            Err(error) => error,
            Ok(_) => panic!("a new empty credential store cannot reopen existing state"),
        };
        assert!(matches!(
            error,
            RunRuntimeError::Artifact(SealedArtifactError::KeyUnavailable(_))
        ));
    }

    #[test]
    fn existing_runtime_refuses_corrupt_credentials_on_reopen() {
        let state = tempfile::tempdir().expect("runtime state directory");
        let factory = EphemeralRunRuntimeFactory::default();
        let initial = factory.open(state.path()).expect("fresh state initializes");
        drop(initial);

        let error = match RunRuntime::open_with_credential_store(
            state.path(),
            Arc::new(CorruptCredentialStore),
        ) {
            Err(error) => error,
            Ok(_) => panic!("corrupt credentials cannot reopen an existing state"),
        };
        assert!(matches!(
            error,
            RunRuntimeError::Artifact(SealedArtifactError::KeyUnavailable(_))
        ));
    }
}
