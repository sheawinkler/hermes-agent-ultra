use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hermes_core::execution_mode::{ExecutionMode, ResolvedExecutionMode};
use hermes_environments::{
    VerifiedIsolatedBackendKind, VerifiedIsolatedBackendScope, CLOSED_EXECUTION_POLICY_VERSION,
};
use rusqlite::{params, Connection, OptionalExtension, Row, Transaction, TransactionBehavior};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::connection::LedgerConnectionFactory;
use super::error::{LedgerError, LedgerResult};
use super::policy::{
    sha256_hex, validate_payload, DeliveryManifest, DispatchPolicyV1, OutboxRouteRegistry,
};
use super::types::{
    execution_mode_from_db, execution_mode_source_to_db, execution_mode_to_db, AppendIdentity,
    AppendKey, AppendLease, AppendReceipt, AppendRequest, AppendReservation,
    ArtifactDegradationReason, ArtifactPhysicalState, ArtifactReferenceState,
    ArtifactReferenceStatus, ArtifactStateReason, ArtifactStateTransition,
    BranchReplaySourceSnapshot, CanonicalJson,
    ClaimNext, CreatedRun, DeliveryCompletion, DeliveryCompletionReceipt, DeliveryEvidence,
    DeliveryFailureReason, Destination, DigestAbsence, DownstreamIdempotencyKey,
    DurableDeliveryReceipt, EffectIntent, EventId, ExactReplayState, ExactReplayStatus, LeaseToken,
    NonResumableEffectAttempt, NonResumableEffectLease, NonResumableEffectRecord,
    NonResumableEffectSnapshot, NonResumableEffectState, NonResumableIdempotencyKey, OutboxLease,
    OutboxRecord, OutboxRowId, OutboxState, PrivateFingerprint, ProvenanceDigest,
    ReplayAppendContract, ReplayArtifactFidelity, ReplayObligationFidelity,
    ReplayObligationSnapshot, ReplayOperationKind, ReplayOperationOutcome, ReplayOperationPhase,
    RunExecutionCertification, RunExit, RunHeader, RunId, RunOutcome, RunOwnerId, RunOwnerLease,
    RunProvenance, RunStart, RunStatus, SealedArtifactRef, SessionId, SessionRunEvent,
    SessionRunEventKind, Sha256Digest, StaleRunRecovery, TerminalReason,
    CANONICALIZATION_ALGORITHM, CANONICALIZATION_VERSION,
};
use crate::run_export::{LedgerManifestSnapshot, LedgerOutboxSnapshot, LedgerRunSnapshot};
use crate::run_runtime::CertifiedEffectIntent;

const MAX_LEASE_DURATION: Duration = Duration::from_secs(24 * 60 * 60);
const DEFAULT_APPEND_LEASE: Duration = Duration::from_secs(30);
const DEFAULT_OUTBOX_LEASE: Duration = Duration::from_secs(30);
const DEFAULT_OWNER_LEASE: Duration = Duration::from_secs(30);
const NONRESUMABLE_EFFECT_IDEMPOTENCY_DOMAIN: &[u8] = b"hermes-local-one-shot-v1";
const NONRESUMABLE_CLAIM_BINDING_DOMAIN: &[u8] = b"hermes-local-one-shot-claim-v1";
const NONRESUMABLE_RESULT_APPEND_KEY_DOMAIN: &[u8] = b"hermes-local-one-shot-result-v1";
const NONRESUMABLE_RESULT_SCHEMA: &str = "hermes.session-run.nonresumable-result";
const DEFAULT_MAX_DELIVERY_ATTEMPTS: u32 = 3;
const CHAIN_GENESIS_DOMAIN: &[u8] = b"hermes.session-run.chain.genesis/v1";

#[derive(Debug, Clone)]
struct TerminalArtifactDegradation {
    audit_key: AppendKey,
    reason: ArtifactDegradationReason,
    public_context: CanonicalJson,
}
pub(crate) const RUN_START_BINDING_SCHEMA: &str = "hermes.session-run.start-binding";
pub(crate) const RUN_EXIT_BINDING_SCHEMA: &str = "hermes.session-run.exit-binding";
pub(crate) const RUN_EVENT_BINDING_SCHEMA_VERSION: u32 = 1;
/// Schema emitted by all new delivery-result events.
pub(crate) const DELIVERY_RESULT_BINDING_SCHEMA: &str = "hermes.session-run.delivery-result/v2";
/// Historical delivery-result schema emitted before sealed result evidence was
/// introduced. It is read-only compatibility input and must never be written.
pub(crate) const LEGACY_DELIVERY_RESULT_BINDING_SCHEMA: &str =
    "hermes.session-run.delivery-result/v1";
/// Closed public identity schema for sealed delivery-result evidence.
///
/// The schema is intentionally the existing runtime-evidence envelope schema
/// accepted by `LedgerEvidenceRedactor`; its exact fields are validated against
/// the leased outbox record before any completion is committed.
pub(crate) const DELIVERY_RESULT_EVIDENCE_IDENTITY_SCHEMA: &str = "hermes.runtime-evidence/v1";
pub(crate) const DELIVERY_RESULT_EVIDENCE_SUBJECT_DOMAIN: &str = "hermes.session-run.outbox";
const TERMINAL_ARTIFACT_DEGRADATION_SCHEMA: &str =
    "hermes.session-run.terminal-artifact-degradation";

/// Derive the sole public identity accepted for one leased delivery result.
/// It contains no task text, replay payload, or credential-bearing material.
pub(crate) fn delivery_result_evidence_identity(
    lease: &OutboxLease,
    completion: &DeliveryCompletion,
) -> Value {
    let outcome = match completion {
        DeliveryCompletion::Acknowledged(_) => "returned",
        DeliveryCompletion::Nacked(_) => "handler_error",
    };
    delivery_result_evidence_identity_for(
        &lease.outbox_id,
        lease.destination,
        &lease.idempotency_key,
        outcome,
    )
}

/// Derive the closed identity accepted only for a credential-free exact
/// delivery result artifact. This identity is intentionally distinct from the
/// legacy credential-redacted metadata evidence identity.
pub(crate) fn delivery_result_exact_evidence_identity(
    lease: &OutboxLease,
    completion: &DeliveryCompletion,
) -> Value {
    let outcome = match completion {
        DeliveryCompletion::Acknowledged(_) => "returned",
        DeliveryCompletion::Nacked(_) => "handler_error",
    };
    delivery_result_exact_evidence_identity_for(
        &lease.outbox_id,
        lease.destination,
        &lease.idempotency_key,
        outcome,
    )
}

/// Build the closed identity from immutable outbox fields for offline
/// verification. Only `returned` and `handler_error` are valid outcomes.
pub(crate) fn delivery_result_evidence_identity_for(
    outbox_id: &OutboxRowId,
    destination: Destination,
    idempotency_key: &DownstreamIdempotencyKey,
    outcome: &str,
) -> Value {
    debug_assert!(matches!(outcome, "returned" | "handler_error"));
    json!({
        "artifact_kind": "result",
        "evidence_id": outbox_id.to_string(),
        "evidence_subject": format!(
            "{}/{}/{}",
            DELIVERY_RESULT_EVIDENCE_SUBJECT_DOMAIN,
            destination.as_str(),
            idempotency_key.as_str(),
        ),
        "outcome": outcome,
        "replay_fidelity": "credential_redacted",
        "schema": DELIVERY_RESULT_EVIDENCE_IDENTITY_SCHEMA,
    })
}

/// Build the exact-private delivery identity from immutable outbox fields.
/// The associated sealed payload must separately pass the closed exact
/// runtime-evidence validator before this identity can be admitted.
pub(crate) fn delivery_result_exact_evidence_identity_for(
    outbox_id: &OutboxRowId,
    destination: Destination,
    idempotency_key: &DownstreamIdempotencyKey,
    outcome: &str,
) -> Value {
    debug_assert!(matches!(outcome, "returned" | "handler_error"));
    json!({
        "artifact_kind": "result",
        "evidence_id": outbox_id.to_string(),
        "evidence_subject": format!(
            "{}/{}/{}",
            DELIVERY_RESULT_EVIDENCE_SUBJECT_DOMAIN,
            destination.as_str(),
            idempotency_key.as_str(),
        ),
        "outcome": outcome,
        "replay_fidelity": "exact_private",
        "schema": DELIVERY_RESULT_EVIDENCE_IDENTITY_SCHEMA,
    })
}

fn validate_delivery_evidence_binding(
    lease: &OutboxLease,
    completion: &DeliveryCompletion,
    evidence: &DeliveryEvidence,
) -> LedgerResult<()> {
    if evidence.sealed_artifact_ref == lease.sealed_artifact_ref {
        return Err(LedgerError::IntegrityViolation(
            "delivery result evidence must not reuse the source sealed artifact".to_string(),
        ));
    }
    let actual = evidence.identity.public_identity.as_value()?;
    let expected = match evidence.replay_fidelity {
        ReplayArtifactFidelity::ExactPrivate => {
            delivery_result_exact_evidence_identity(lease, completion)
        }
        ReplayArtifactFidelity::MetadataOnly => delivery_result_evidence_identity(lease, completion),
    };
    if actual != expected {
        return Err(LedgerError::IntegrityViolation(
            "delivery result evidence identity does not bind the leased outbox completion"
                .to_string(),
        ));
    }
    Ok(())
}

fn validate_delivery_result_storage_shape(
    payload: &CanonicalJson,
    has_evidence: bool,
) -> LedgerResult<()> {
    let value = payload.as_value()?;
    let object = value
        .as_object()
        .ok_or_else(|| LedgerError::InvalidStoredState {
            kind: "delivery result payload",
            value: "not an object".to_string(),
        })?;
    let schema = object
        .get("schema")
        .and_then(Value::as_str)
        .ok_or_else(|| LedgerError::InvalidStoredState {
            kind: "delivery result payload",
            value: "missing schema".to_string(),
        })?;
    match schema {
        LEGACY_DELIVERY_RESULT_BINDING_SCHEMA if !has_evidence => Ok(()),
        LEGACY_DELIVERY_RESULT_BINDING_SCHEMA => Err(LedgerError::InvalidStoredState {
            kind: "delivery result evidence",
            value: "legacy v1 delivery result must not carry result evidence".to_string(),
        }),
        DELIVERY_RESULT_BINDING_SCHEMA if has_evidence => Ok(()),
        DELIVERY_RESULT_BINDING_SCHEMA => {
            let reason = object
                .get("delivery_error")
                .and_then(Value::as_str)
                .and_then(|value| DeliveryFailureReason::from_db(value).ok());
            if matches!(
                reason,
                Some(
                    DeliveryFailureReason::MissingRoute
                        | DeliveryFailureReason::InvalidPayload
                        | DeliveryFailureReason::AttemptsExhausted
                )
            ) {
                Ok(())
            } else {
                Err(LedgerError::InvalidStoredState {
                    kind: "delivery result evidence",
                    value: "v2 external delivery result lacks sealed evidence".to_string(),
                })
            }
        }
        _ => Err(LedgerError::InvalidStoredState {
            kind: "delivery result payload schema",
            value: schema.to_string(),
        }),
    }
}

pub trait LedgerClock: Send + Sync {
    fn now_unix_ms(&self) -> LedgerResult<i64>;
}

#[derive(Debug, Default)]
pub struct SystemLedgerClock;

impl LedgerClock for SystemLedgerClock {
    fn now_unix_ms(&self) -> LedgerResult<i64> {
        let duration = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| LedgerError::Clock)?;
        i64::try_from(duration.as_millis()).map_err(|_| LedgerError::Clock)
    }
}

#[derive(Debug, Clone)]
pub struct RunLedgerConfig {
    pub append_lease_duration: Duration,
    pub outbox_lease_duration: Duration,
    pub owner_lease_duration: Duration,
    pub max_delivery_attempts: u32,
}

impl Default for RunLedgerConfig {
    fn default() -> Self {
        Self {
            append_lease_duration: DEFAULT_APPEND_LEASE,
            outbox_lease_duration: DEFAULT_OUTBOX_LEASE,
            owner_lease_duration: DEFAULT_OWNER_LEASE,
            max_delivery_attempts: DEFAULT_MAX_DELIVERY_ATTEMPTS,
        }
    }
}

impl RunLedgerConfig {
    fn validate(&self) -> LedgerResult<()> {
        for duration in [
            self.append_lease_duration,
            self.outbox_lease_duration,
            self.owner_lease_duration,
        ] {
            if duration.is_zero() || duration > MAX_LEASE_DURATION {
                return Err(LedgerError::InvalidLeaseDuration {
                    milliseconds: duration.as_millis(),
                });
            }
        }
        if !(1..=1_000).contains(&self.max_delivery_attempts) {
            return Err(LedgerError::InvalidMaximumDeliveryAttempts {
                attempts: self.max_delivery_attempts,
            });
        }
        Ok(())
    }
}

#[derive(Clone)]
pub struct RunLedger {
    factory: LedgerConnectionFactory,
    config: RunLedgerConfig,
    clock: Arc<dyn LedgerClock>,
}

impl RunLedger {
    pub fn open(factory: LedgerConnectionFactory) -> LedgerResult<Self> {
        Self::open_with_config(factory, RunLedgerConfig::default())
    }

    pub fn open_with_config(
        factory: LedgerConnectionFactory,
        config: RunLedgerConfig,
    ) -> LedgerResult<Self> {
        Self::open_with_clock(factory, config, Arc::new(SystemLedgerClock))
    }

    pub(crate) fn open_with_clock(
        factory: LedgerConnectionFactory,
        config: RunLedgerConfig,
        clock: Arc<dyn LedgerClock>,
    ) -> LedgerResult<Self> {
        config.validate()?;
        factory.validate_cutover()?;
        Ok(Self {
            factory,
            config,
            clock,
        })
    }

    pub(crate) fn connection_factory(&self) -> &LedgerConnectionFactory {
        &self.factory
    }

    pub fn create_run_and_start(
        &self,
        mode: ResolvedExecutionMode,
        owner_id: RunOwnerId,
        start: RunStart,
    ) -> LedgerResult<CreatedRun> {
        self.create_run_and_start_inner(mode, owner_id, start, None)
    }

    /// Commit a new run-start event with one already-published exact artifact.
    ///
    /// Callers must retain the filesystem publication guard through this call:
    /// the required order is private CAS store guard first, then this ledger
    /// transaction. The ledger never acquires a store lock.
    /// Commit a new RunStart event with one already-published bound artifact.
    ///
    /// Exactness is determined by the preparation classifier, never by
    /// physical CAS availability alone.
    pub(crate) fn create_run_and_start_with_bound_artifact(
        &self,
        mode: ResolvedExecutionMode,
        owner_id: RunOwnerId,
        start: RunStart,
        identity: AppendIdentity,
        sealed_artifact_ref: SealedArtifactRef,
        fidelity: ReplayArtifactFidelity,
    ) -> LedgerResult<CreatedRun> {
        self.create_run_and_start_inner(
            mode,
            owner_id,
            start,
            Some((identity, sealed_artifact_ref, fidelity)),
        )
    }

    /// Compatibility entry point for callers that have already passed the
    /// closed exact-private preparation gate.
    pub(crate) fn create_run_and_start_with_sealed_artifact(
        &self,
        mode: ResolvedExecutionMode,
        owner_id: RunOwnerId,
        start: RunStart,
        identity: AppendIdentity,
        sealed_artifact_ref: SealedArtifactRef,
    ) -> LedgerResult<CreatedRun> {
        self.create_run_and_start_with_bound_artifact(
            mode,
            owner_id,
            start,
            identity,
            sealed_artifact_ref,
            ReplayArtifactFidelity::ExactPrivate,
        )
    }

    /// Atomically admit one effect-free child run from a verified source
    /// snapshot. The source is revalidated under the same immediate
    /// transaction that inserts the child, so no source-chain, artifact-state,
    /// obligation, or outbox change can race the lineage decision.
    ///
    /// This path intentionally takes no source event, outbox row, append key,
    /// or idempotency key to copy. The new child receives a freshly minted
    /// run-start event and an empty manifest.
    pub(crate) fn create_branch_run_from_verified_source(
        &self,
        source: &BranchReplaySourceSnapshot,
        mode: ResolvedExecutionMode,
        owner_id: RunOwnerId,
        start: RunStart,
    ) -> LedgerResult<CreatedRun> {
        validate_branch_replay_child_lineage(source, &start)?;
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_branch_replay_source_snapshot_tx(&transaction, source)?;
        let created = self.create_run_and_start_tx(&transaction, mode, owner_id, start, None)?;
        transaction.commit()?;
        Ok(created)
    }

    fn create_run_and_start_inner(
        &self,
        mode: ResolvedExecutionMode,
        owner_id: RunOwnerId,
        start: RunStart,
        start_artifact: Option<(AppendIdentity, SealedArtifactRef, ReplayArtifactFidelity)>,
    ) -> LedgerResult<CreatedRun> {
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let created = self.create_run_and_start_tx(
            &transaction,
            mode,
            owner_id,
            start,
            start_artifact,
        )?;
        transaction.commit()?;
        Ok(created)
    }

    fn create_run_and_start_tx(
        &self,
        transaction: &Transaction<'_>,
        mode: ResolvedExecutionMode,
        owner_id: RunOwnerId,
        start: RunStart,
        start_artifact: Option<(AppendIdentity, SealedArtifactRef, ReplayArtifactFidelity)>,
    ) -> LedgerResult<CreatedRun> {
        validate_execution_certification(mode, &start.provenance, start.execution_certification())?;
        let owner_lease_token = LeaseToken::random();
        let start_event_id = EventId::new();
        let RunStart {
            run_id,
            provenance,
            public_payload,
            ..
        } = start;
        let (start_identity, start_sealed_artifact_ref, start_fidelity) = match start_artifact {
            Some((identity, reference, fidelity)) => (Some(identity), Some(reference), Some(fidelity)),
            None => (None, None, None),
        };
        let start_exact_private =
            start_fidelity == Some(ReplayArtifactFidelity::ExactPrivate)
                && start_sealed_artifact_ref.is_some();
        let provenance_jcs = provenance.canonical_json()?;
        let (prompt_digest, prompt_absence) = provenance.prompt_digest().to_db();
        let (input_digest, input_absence) = provenance.input_digest().to_db();
        let (output_digest, output_absence) = provenance.output_digest().to_db();
        let (tool_digest, tool_absence) = provenance.tool_digest().to_db();
        let (skill_digest, skill_absence) = provenance.skill_digest().to_db();
        let (policy_digest, policy_absence) = provenance.policy_digest().to_db();
        let (sandbox_digest, sandbox_absence) = provenance.sandbox_digest().to_db();
        let started_at_ms = self.now()?;
        let owner_lease_expires_at_ms =
            add_duration(started_at_ms, self.config.owner_lease_duration)?;
        let public_payload =
            bound_run_start_payload(mode, &owner_id, started_at_ms, public_payload)?;

        let exists: Option<i64> = transaction
            .query_row(
                "SELECT 1 FROM session_runs WHERE run_id = ?1",
                [run_id.to_string()],
                |row| row.get(0),
            )
            .optional()?;
        if exists.is_some() {
            return Err(LedgerError::RunAlreadyExists {
                run_id: run_id.to_string(),
            });
        }

        if let Some(parent_run_id) = provenance.parent_run_id() {
            let parent_tenant: Option<String> = transaction
                .query_row(
                    "SELECT tenant_id FROM session_runs WHERE run_id = ?1",
                    [parent_run_id.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            let parent_tenant = parent_tenant.ok_or_else(|| LedgerError::ParentRunNotFound {
                parent_run_id: parent_run_id.to_string(),
            })?;
            if parent_tenant != provenance.tenant_id().as_str() {
                return Err(LedgerError::ParentRunTenantMismatch {
                    parent_run_id: parent_run_id.to_string(),
                    tenant_id: provenance.tenant_id().as_str().to_string(),
                });
            }
        }

        transaction.execute(
            "INSERT INTO session_runs (
                run_id, session_id, parent_run_id, branch_id, tenant_id, action_id, objective_id,
                task_id, model_id, provider_id, config_digest,
                prompt_digest, prompt_digest_absence, input_digest, input_digest_absence,
                output_digest, output_digest_absence, tool_digest, tool_digest_absence,
                skill_digest, skill_digest_absence, policy_digest, policy_digest_absence,
                sandbox_digest, sandbox_digest_absence,
                provenance_canonicalization_algorithm, provenance_canonicalization_version,
                execution_mode, execution_mode_source, owner_id, owner_lease_generation,
                owner_lease_token, owner_lease_expires_at_ms, start_event_id, started_at_ms,
                next_event_sequence, head_hash, required_dead_lettered, pending_required_delivery
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11,
                ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25,
                ?26, ?27, ?28, ?29, ?30, 1, ?31, ?32, ?33, ?34, 0, ?35, 0, 0
            )",
            params![
                run_id.to_string(),
                provenance.session_id().as_str(),
                provenance.parent_run_id().map(|value| value.to_string()),
                provenance.branch_id().as_str(),
                provenance.tenant_id().as_str(),
                provenance.action_id().as_str(),
                provenance.objective_id().as_str(),
                provenance.task_id().as_str(),
                provenance.model_id().as_str(),
                provenance.provider_id().as_str(),
                provenance.config_digest().as_str(),
                prompt_digest,
                prompt_absence,
                input_digest,
                input_absence,
                output_digest,
                output_absence,
                tool_digest,
                tool_absence,
                skill_digest,
                skill_absence,
                policy_digest,
                policy_absence,
                sandbox_digest,
                sandbox_absence,
                provenance.canonicalization_algorithm(),
                i64::from(provenance.canonicalization_version()),
                execution_mode_to_db(mode.mode),
                execution_mode_source_to_db(mode.source),
                owner_id.as_str(),
                owner_lease_token.to_blob(),
                owner_lease_expires_at_ms,
                start_event_id.to_string(),
                started_at_ms,
                genesis_hash(),
            ],
        )?;

        let committed = append_event_tx(
            transaction,
            &self.config,
            EventDraft {
                event_id: start_event_id,
                run_id,
                kind: SessionRunEventKind::RunStart,
                append_key: None,
                effect_intent: EffectIntent::None,
                artifact_degradation_reason: None,
                identity: start_identity,
                sealed_artifact_ref: start_sealed_artifact_ref,
                run_provenance: Some(provenance_jcs),
                public_payload,
                created_at_ms: started_at_ms,
            },
        )?;
        debug_assert_eq!(committed.event_id, start_event_id);
        if start_exact_private {
            transaction.execute(
                "INSERT INTO session_run_exact_replay (
                    run_id, exact_replay, state, state_reason, transition_sequence,
                    latched_at_ms, latched_by_event_id
                 ) VALUES (?1, 1, 'available', NULL, 0, NULL, NULL)",
                [run_id.to_string()],
            )?;
            transaction.execute(
                "INSERT INTO session_run_replay_obligations (
                    run_id, source_event_id, operation_kind, phase, operation_instance_id,
                    replay_fidelity, outcome, resolved_event_id, created_at_ms, resolved_at_ms
                 ) VALUES (?1, ?2, 'run', 'request', ?2, 'exact_private', 'not_applicable', ?2, ?3, ?3)",
                params![run_id.to_string(), start_event_id.to_string(), started_at_ms],
            )?;
        } else {
            transaction.execute(
                "INSERT INTO session_run_exact_replay (
                    run_id, exact_replay, state, state_reason, transition_sequence,
                    latched_at_ms, latched_by_event_id
                 ) VALUES (?1, 0, 'unavailable_sanitized', 'sanitized', 1, ?2, ?3)",
                params![run_id.to_string(), started_at_ms, start_event_id.to_string()],
            )?;
            transaction.execute(
                "INSERT INTO session_run_replay_obligations (
                    run_id, source_event_id, operation_kind, phase, operation_instance_id,
                    replay_fidelity, outcome, resolved_event_id, created_at_ms, resolved_at_ms
                 ) VALUES (?1, ?2, 'run', 'request', ?2, 'metadata_only', 'not_applicable', ?2, ?3, ?3)",
                params![run_id.to_string(), start_event_id.to_string(), started_at_ms],
            )?;
        }

        let header = RunHeader {
            run_id,
            start_event_id,
            execution_mode: mode,
            started_at_ms,
            owner_id: owner_id.clone(),
            owner_lease_generation: 1,
            owner_lease_expires_at_ms,
            provenance,
        };
        Ok(CreatedRun {
            owner_lease: RunOwnerLease {
                run_id,
                owner_id,
                lease_generation: 1,
                lease_expires_at_ms: owner_lease_expires_at_ms,
                token: owner_lease_token,
            },
            header,
        })
    }

    pub fn heartbeat_run_owner(
        &self,
        mode: ResolvedExecutionMode,
        lease: &RunOwnerLease,
    ) -> LedgerResult<RunOwnerLease> {
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = self.now()?;
        let lease_expires_at_ms = add_duration(now, self.config.owner_lease_duration)?;
        ensure_mode_matches(&transaction, lease.run_id, mode)?;
        if !ensure_current_unexpired_run_owner_lease(&transaction, lease, now)? {
            return Err(LedgerError::StaleRunOwnerLease {
                run_id: lease.run_id.to_string(),
            });
        }
        let lease_generation = lease
            .lease_generation
            .checked_add(1)
            .ok_or(LedgerError::NumericOverflow)?;
        let token = LeaseToken::random();
        let changed = transaction.execute(
            "UPDATE session_runs
             SET owner_lease_generation = ?1, owner_lease_token = ?2,
                 owner_lease_expires_at_ms = ?3
             WHERE run_id = ?4 AND exit_event_id IS NULL AND owner_id = ?5
               AND owner_lease_generation = ?6 AND owner_lease_token = ?7
               AND owner_lease_expires_at_ms > ?8",
            params![
                i64::try_from(lease_generation).map_err(|_| LedgerError::NumericOverflow)?,
                token.to_blob(),
                lease_expires_at_ms,
                lease.run_id.to_string(),
                lease.owner_id.as_str(),
                i64::try_from(lease.lease_generation).map_err(|_| LedgerError::NumericOverflow)?,
                lease.token.to_blob(),
                now,
            ],
        )?;
        if changed != 1 {
            return Err(LedgerError::StaleRunOwnerLease {
                run_id: lease.run_id.to_string(),
            });
        }
        transaction.commit()?;
        Ok(RunOwnerLease {
            run_id: lease.run_id,
            owner_id: lease.owner_id.clone(),
            lease_generation,
            lease_expires_at_ms,
            token,
        })
    }

    pub fn recover_stale_run(&self, run_id: RunId) -> LedgerResult<StaleRunRecovery> {
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = self.now()?;
        let status = load_run_status(&transaction, run_id)?;
        if status.exit_terminal_reason.is_some() {
            transaction.commit()?;
            return Ok(StaleRunRecovery::AlreadyFinalized(status));
        }
        let (raw_generation, owner_lease_expires_at_ms): (i64, i64) = transaction.query_row(
            "SELECT owner_lease_generation, owner_lease_expires_at_ms
             FROM session_runs WHERE run_id = ?1",
            [run_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        if owner_lease_expires_at_ms > now {
            transaction.commit()?;
            return Ok(StaleRunRecovery::Live);
        }
        let prior_generation = parse_positive_u64("run owner lease generation", raw_generation)?;
        let recovery_generation = prior_generation
            .checked_add(1)
            .ok_or(LedgerError::NumericOverflow)?;
        let recovery_token = LeaseToken::random();
        let claimed = transaction.execute(
            "UPDATE session_runs
             SET owner_lease_generation = ?1, owner_lease_token = ?2,
                 owner_lease_expires_at_ms = ?3
             WHERE run_id = ?4 AND exit_event_id IS NULL
               AND owner_lease_generation = ?5 AND owner_lease_expires_at_ms <= ?6",
            params![
                i64::try_from(recovery_generation).map_err(|_| LedgerError::NumericOverflow)?,
                recovery_token.to_blob(),
                now,
                run_id.to_string(),
                raw_generation,
                now,
            ],
        )?;
        if claimed != 1 {
            let current = load_run_status(&transaction, run_id)?;
            transaction.commit()?;
            return Ok(if current.exit_terminal_reason.is_some() {
                StaleRunRecovery::AlreadyFinalized(current)
            } else {
                StaleRunRecovery::Live
            });
        }

        let terminalized = terminalize_nonresumable_effects(&transaction, run_id, now)?;
        let terminal_reason = if terminalized.has_unknown {
            TerminalReason::EffectNonResumableUnknown
        } else {
            TerminalReason::ProcessDeath
        };
        let event_id = EventId::new();
        let public_payload = bound_run_exit_payload(
            terminal_reason,
            CanonicalJson::from_value(&json!({
                "recovery": "owner_lease_expired",
                "causal_terminal_reason": TerminalReason::ProcessDeath.as_str(),
            }))?,
        )?;
        append_event_tx(
            &transaction,
            &self.config,
            EventDraft {
                event_id,
                run_id,
                kind: SessionRunEventKind::RunExit,
                append_key: None,
                effect_intent: EffectIntent::None,
                artifact_degradation_reason: None,
                identity: None,
                sealed_artifact_ref: None,
                run_provenance: None,
                public_payload,
                created_at_ms: now,
            },
        )?;
        resolve_pending_replay_obligations_at_exit_tx(
            &transaction,
            run_id,
            terminal_reason,
            event_id,
            now,
        )?;
        let finalized = transaction.execute(
            "UPDATE session_runs
             SET exit_event_id = ?1, exit_terminal_reason = ?2, candidate_outcome = ?3
             WHERE run_id = ?4 AND exit_event_id IS NULL
               AND owner_lease_generation = ?5 AND owner_lease_token = ?6",
            params![
                event_id.to_string(),
                terminal_reason.as_str(),
                terminal_reason.outcome().as_str(),
                run_id.to_string(),
                i64::try_from(recovery_generation).map_err(|_| LedgerError::NumericOverflow)?,
                recovery_token.to_blob(),
            ],
        )?;
        if finalized != 1 {
            return Err(LedgerError::IntegrityViolation(
                "stale-run recovery lost its owner fence after claiming it".to_string(),
            ));
        }
        reconcile_run_finalization(&transaction, run_id)?;
        let status = load_run_status(&transaction, run_id)?;
        transaction.commit()?;
        Ok(StaleRunRecovery::Recovered(status))
    }

    pub fn recover_stale_runs(&self) -> LedgerResult<Vec<RunId>> {
        let connection = self.factory.open()?;
        let now = self.now()?;
        let run_ids = {
            let mut statement = connection.prepare(
                "SELECT run_id FROM session_runs
                 WHERE exit_event_id IS NULL AND owner_lease_expires_at_ms <= ?1
                 ORDER BY owner_lease_expires_at_ms ASC, run_id ASC",
            )?;
            let raw_ids = statement
                .query_map([now], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            raw_ids
                .into_iter()
                .map(parse_run_id)
                .collect::<LedgerResult<Vec<_>>>()?
        };
        let mut recovered = Vec::with_capacity(run_ids.len());
        for run_id in run_ids {
            if matches!(
                self.recover_stale_run(run_id)?,
                StaleRunRecovery::Recovered(_)
            ) {
                recovered.push(run_id);
            }
        }
        Ok(recovered)
    }

    pub(crate) fn reserve_append(
        &self,
        mode: ResolvedExecutionMode,
        owner_lease: &RunOwnerLease,
        request: AppendRequest,
    ) -> LedgerResult<AppendReservation> {
        if owner_lease.run_id != request.run_id {
            return Err(LedgerError::StaleRunOwnerLease {
                run_id: request.run_id.to_string(),
            });
        }
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = self.now()?;
        let lease_expires_at_ms = add_duration(now, self.config.append_lease_duration)?;
        ensure_mode_matches(&transaction, request.run_id, mode)?;
        let existing = load_reservation(&transaction, &request.append_key)?;
        if existing.is_none()
            && load_event_for_append_key(&transaction, &request.append_key)?.is_some()
        {
            return Err(LedgerError::AppendKeyConflict {
                append_key: request.append_key.to_string(),
            });
        }
        if let Some(existing) = existing.as_ref() {
            ensure_matching_reservation(existing, &request)?;
            ensure_matching_replay_append_contract(
                &transaction,
                &existing.append_key,
                request.replay_contract,
                request.replay_fidelity,
            )?;
            if existing.committed_event_id.is_some() {
                let receipt = append_receipt_for_key(&transaction, &request.append_key)?;
                transaction.commit()?;
                return Ok(AppendReservation::Committed(receipt));
            }
            if let Some(audit_event_id) = existing.terminal_audit_event_id {
                let receipt = append_receipt_for_event_id(&transaction, audit_event_id)?;
                transaction.commit()?;
                return Ok(AppendReservation::Committed(receipt));
            }
        }

        if !ensure_current_unexpired_run_owner_lease(&transaction, owner_lease, now)? {
            return Err(LedgerError::StaleRunOwnerLease {
                run_id: request.run_id.to_string(),
            });
        }
        ensure_appendable(&transaction, request.run_id)?;

        if let Some(existing) = existing {
            if existing.lease_expires_at_ms > now {
                transaction.commit()?;
                return Ok(AppendReservation::InFlight {
                    run_id: existing.run_id,
                    event_id: existing.event_id,
                    lease_generation: existing.lease_generation,
                    lease_expires_at_ms: existing.lease_expires_at_ms,
                    published_sealed_artifact_ref: existing.sealed_artifact_ref,
                });
            }

            let next_generation = existing
                .lease_generation
                .checked_add(1)
                .ok_or(LedgerError::NumericOverflow)?;
            let token = LeaseToken::random();
            let changed = transaction.execute(
                "UPDATE session_run_append_reservations
                 SET lease_generation = ?1, lease_token = ?2, lease_expires_at_ms = ?3
                 WHERE append_key = ?4 AND committed_event_id IS NULL
                   AND terminal_audit_event_id IS NULL
                   AND lease_generation = ?5 AND lease_expires_at_ms <= ?6",
                params![
                    i64::try_from(next_generation).map_err(|_| LedgerError::NumericOverflow)?,
                    token.to_blob(),
                    lease_expires_at_ms,
                    request.append_key.as_str(),
                    i64::try_from(existing.lease_generation)
                        .map_err(|_| LedgerError::NumericOverflow)?,
                    now,
                ],
            )?;
            if changed != 1 {
                return Err(LedgerError::StaleAppendLease {
                    append_key: request.append_key.to_string(),
                });
            }
            let lease = AppendLease {
                run_id: existing.run_id,
                append_key: request.append_key,
                event_id: existing.event_id,
                frozen_event_at_ms: existing.frozen_event_at_ms,
                lease_generation: next_generation,
                lease_expires_at_ms,
                published_sealed_artifact_ref: existing.sealed_artifact_ref,
                token,
            };
            transaction.commit()?;
            return Ok(AppendReservation::Reserved(lease));
        }

        let event_id = EventId::new();
        let token = LeaseToken::random();
        transaction.execute(
            "INSERT INTO session_run_append_reservations (
                append_key, run_id, event_id, frozen_event_at_ms, public_identity_jcs,
                private_fingerprint_algorithm, private_fingerprint_algorithm_version,
                private_fingerprint_key_id, private_fingerprint_key_version, private_fingerprint,
                effect_intent, public_payload_jcs, lease_generation, lease_token, lease_expires_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, 1, ?13, ?14)",
            params![
                request.append_key.as_str(),
                request.run_id.to_string(),
                event_id.to_string(),
                now,
                request.identity.public_identity.as_str(),
                request.identity.private_fingerprint.algorithm().as_str(),
                i64::from(request.identity.private_fingerprint.algorithm_version()),
                request.identity.private_fingerprint.key_id().as_str(),
                i64::from(request.identity.private_fingerprint.key_version()),
                request.identity.private_fingerprint.as_hex(),
                request.effect_intent.as_str(),
                request.public_payload.as_str(),
                token.to_blob(),
                lease_expires_at_ms,
            ],
        )?;
        if let Some(contract) = request.replay_contract {
            validate_replay_append_contract(contract)?;
            transaction.execute(
                "INSERT INTO session_run_replay_append_contracts (
                    append_key, run_id, reserved_event_id, operation_kind, phase,
                    resolution_source_event_id, requested_fidelity, created_at_ms
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    request.append_key.as_str(),
                    request.run_id.to_string(),
                    event_id.to_string(),
                    contract.operation.as_str(),
                    contract.phase.as_str(),
                    contract.resolution_source_event_id.map(|value| value.to_string()),
                    request.replay_fidelity.as_str(),
                    now,
                ],
            )?;
        }
        transaction.commit()?;

        Ok(AppendReservation::Reserved(AppendLease {
            run_id: request.run_id,
            append_key: request.append_key,
            event_id,
            frozen_event_at_ms: now,
            lease_generation: 1,
            lease_expires_at_ms,
            published_sealed_artifact_ref: None,
            token,
        }))
    }

    /// Publish an already-sealed, validated artifact reference using the
    /// reservation token and generation as a CAS fence. The first reference is
    /// immutable; a reclaimed reservation reuses it instead of resealing.
    pub(crate) fn publish_sealed_ref(
        &self,
        mode: ResolvedExecutionMode,
        owner_lease: &RunOwnerLease,
        lease: &AppendLease,
        sealed_artifact_ref: SealedArtifactRef,
    ) -> LedgerResult<SealedArtifactRef> {
        if owner_lease.run_id != lease.run_id {
            return Err(LedgerError::StaleRunOwnerLease {
                run_id: lease.run_id.to_string(),
            });
        }
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = self.now()?;
        ensure_mode_matches(&transaction, lease.run_id, mode)?;
        if !ensure_current_unexpired_run_owner_lease(&transaction, owner_lease, now)? {
            return Err(LedgerError::StaleRunOwnerLease {
                run_id: lease.run_id.to_string(),
            });
        }
        let reservation = load_reservation(&transaction, &lease.append_key)?.ok_or_else(|| {
            LedgerError::AppendReservationNotFound {
                append_key: lease.append_key.to_string(),
            }
        })?;
        ensure_active_append_lease(&reservation, lease, now)?;
        if let Some(existing) = reservation.sealed_artifact_ref {
            if existing == sealed_artifact_ref {
                transaction.commit()?;
                return Ok(existing);
            }
            return Err(LedgerError::SealedArtifactRefConflict {
                append_key: lease.append_key.to_string(),
            });
        }

        let changed = transaction.execute(
            "UPDATE session_run_append_reservations
             SET sealed_artifact_envelope_digest = ?1, sealed_artifact_key_id = ?2,
                 sealed_artifact_schema_version = ?3, sealed_artifact_encryption_algorithm = ?4,
                 sealed_artifact_canonicalization_algorithm = ?5,
                 sealed_artifact_canonicalization_version = ?6,
                 sealed_artifact_redactor_version = ?7
             WHERE append_key = ?8 AND run_id = ?9 AND event_id = ?10
               AND lease_generation = ?11 AND lease_token = ?12
               AND lease_expires_at_ms > ?13 AND committed_event_id IS NULL
               AND terminal_audit_event_id IS NULL
               AND sealed_artifact_envelope_digest IS NULL",
            params![
                sealed_artifact_ref.envelope_digest().as_str(),
                sealed_artifact_ref.key_id().as_str(),
                i64::from(sealed_artifact_ref.schema_version()),
                sealed_artifact_ref.encryption_algorithm(),
                sealed_artifact_ref.canonicalization_algorithm(),
                i64::from(sealed_artifact_ref.canonicalization_version()),
                sealed_artifact_ref.redactor_version().as_str(),
                reservation.append_key.as_str(),
                reservation.run_id.to_string(),
                reservation.event_id.to_string(),
                i64::try_from(reservation.lease_generation)
                    .map_err(|_| LedgerError::NumericOverflow)?,
                reservation.token.to_blob(),
                now,
            ],
        )?;
        if changed != 1 {
            return Err(LedgerError::StaleAppendLease {
                append_key: lease.append_key.to_string(),
            });
        }
        transaction.commit()?;
        Ok(sealed_artifact_ref)
    }

    /// Commit only the exact reference previously published by this lease. An
    /// effect-capable append cannot reach an event, manifest, or outbox without
    /// that durable sealed reference.
    pub(crate) fn commit_reserved_append(
        &self,
        mode: ResolvedExecutionMode,
        owner_lease: &RunOwnerLease,
        lease: &AppendLease,
        published_sealed_artifact_ref: Option<&SealedArtifactRef>,
    ) -> LedgerResult<AppendReceipt> {
        if owner_lease.run_id != lease.run_id {
            return Err(LedgerError::StaleRunOwnerLease {
                run_id: lease.run_id.to_string(),
            });
        }
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = self.now()?;
        ensure_mode_matches(&transaction, lease.run_id, mode)?;
        if !ensure_current_unexpired_run_owner_lease(&transaction, owner_lease, now)? {
            return Err(LedgerError::StaleRunOwnerLease {
                run_id: lease.run_id.to_string(),
            });
        }

        let reservation = load_reservation(&transaction, &lease.append_key)?.ok_or_else(|| {
            LedgerError::AppendReservationNotFound {
                append_key: lease.append_key.to_string(),
            }
        })?;
        ensure_active_append_lease(&reservation, lease, now)?;
        let supplied_ref = published_sealed_artifact_ref.cloned();
        if reservation.effect_intent.requires_sealed_artifact()
            && reservation.sealed_artifact_ref.is_none()
        {
            return Err(LedgerError::SealedArtifactRefRequired {
                append_key: lease.append_key.to_string(),
            });
        }
        if reservation.sealed_artifact_ref != supplied_ref {
            return Err(LedgerError::SealedArtifactRefMismatch {
                append_key: lease.append_key.to_string(),
            });
        }
        ensure_appendable(&transaction, lease.run_id)?;

        let committed = append_event_tx(
            &transaction,
            &self.config,
            EventDraft {
                event_id: reservation.event_id,
                run_id: reservation.run_id,
                kind: SessionRunEventKind::Append,
                append_key: Some(reservation.append_key.clone()),
                effect_intent: reservation.effect_intent,
                artifact_degradation_reason: None,
                identity: Some(reservation.identity.clone()),
                sealed_artifact_ref: reservation.sealed_artifact_ref.clone(),
                run_provenance: None,
                public_payload: reservation.public_payload.clone(),
                created_at_ms: reservation.frozen_event_at_ms,
            },
        )?;
        resolve_replay_append_contract_tx(&transaction, &reservation, committed.event_id, now)?;
        let next_exact_state = derive_next_exact_replay_state_tx(&transaction, reservation.run_id)?;
        update_exact_replay_latch_tx(
            &transaction,
            reservation.run_id,
            next_exact_state,
            now,
            committed.event_id,
        )?;
        let changed = transaction.execute(
            "UPDATE session_run_append_reservations
             SET committed_event_id = ?1, committed_at_ms = ?2
             WHERE append_key = ?3 AND run_id = ?4 AND event_id = ?5
               AND lease_generation = ?6 AND lease_token = ?7
               AND lease_expires_at_ms > ?8 AND committed_event_id IS NULL
               AND terminal_audit_event_id IS NULL
               AND ((?9 IS NULL AND sealed_artifact_envelope_digest IS NULL)
                    OR sealed_artifact_envelope_digest = ?9)",
            params![
                reservation.event_id.to_string(),
                now,
                reservation.append_key.as_str(),
                reservation.run_id.to_string(),
                reservation.event_id.to_string(),
                i64::try_from(reservation.lease_generation)
                    .map_err(|_| LedgerError::NumericOverflow)?,
                reservation.token.to_blob(),
                now,
                reservation
                    .sealed_artifact_ref
                    .as_ref()
                    .map(|reference| reference.envelope_digest().as_str()),
            ],
        )?;
        if changed != 1 {
            return Err(LedgerError::StaleAppendLease {
                append_key: lease.append_key.to_string(),
            });
        }
        transaction.commit()?;

        Ok(AppendReceipt {
            run_id: reservation.run_id,
            event_id: committed.event_id,
            event_sequence: committed.sequence,
            event_hash: committed.event_hash,
            event_kind: SessionRunEventKind::Append,
            artifact_degradation_reason: None,
            sealed_artifact_ref: reservation.sealed_artifact_ref,
            outbox_ids: committed.outbox_ids,
        })
    }
    /// Resolve an owned, unpublished reservation as an explicit manual-only
    /// degradation event under a derived audit key. The source reservation is
    /// terminally linked to that event by `terminal_audit_event_id`, so a retry
    /// of the source append key cannot execute the original effect.
    pub(crate) fn degrade_reserved_append(
        &self,
        mode: ResolvedExecutionMode,
        owner_lease: &RunOwnerLease,
        lease: &AppendLease,
        reason: ArtifactDegradationReason,
    ) -> LedgerResult<AppendReceipt> {
        if mode.mode != ExecutionMode::Manual {
            return Err(LedgerError::ManualArtifactDegradationRequiresManual {
                run_id: lease.run_id.to_string(),
            });
        }
        if owner_lease.run_id != lease.run_id {
            return Err(LedgerError::StaleRunOwnerLease {
                run_id: lease.run_id.to_string(),
            });
        }
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = self.now()?;
        ensure_mode_matches(&transaction, lease.run_id, mode)?;
        let reservation = load_reservation(&transaction, &lease.append_key)?.ok_or_else(|| {
            LedgerError::AppendReservationNotFound {
                append_key: lease.append_key.to_string(),
            }
        })?;
        ensure_append_lease_identity(&reservation, lease)?;
        if let Some(audit_event_id) = reservation.terminal_audit_event_id {
            let receipt = append_receipt_for_event_id(&transaction, audit_event_id)?;
            if receipt.artifact_degradation_reason != Some(reason) {
                return Err(LedgerError::AppendKeyConflict {
                    append_key: lease.append_key.to_string(),
                });
            }
            transaction.commit()?;
            return Ok(receipt);
        }
        if !ensure_current_unexpired_run_owner_lease(&transaction, owner_lease, now)? {
            return Err(LedgerError::StaleRunOwnerLease {
                run_id: lease.run_id.to_string(),
            });
        }
        ensure_active_append_lease(&reservation, lease, now)?;
        ensure_appendable(&transaction, lease.run_id)?;
        if reservation.sealed_artifact_ref.is_some() {
            return Err(LedgerError::PublishedArtifactCannotDegrade {
                append_key: lease.append_key.to_string(),
            });
        }
        let degradation = terminal_artifact_degradation(
            reservation.append_key.clone(),
            reason,
            reservation.effect_intent,
            reservation.public_payload.clone(),
        )?;
        if load_reservation(&transaction, &degradation.audit_key)?.is_some()
            || load_event_for_append_key(&transaction, &degradation.audit_key)?.is_some()
        {
            return Err(LedgerError::AppendKeyConflict {
                append_key: degradation.audit_key.to_string(),
            });
        }
        let public_payload = manual_artifact_degradation_payload(&degradation)?;
        let audit_event_id = EventId::new();
        let committed = append_event_tx(
            &transaction,
            &self.config,
            EventDraft {
                event_id: audit_event_id,
                run_id: reservation.run_id,
                kind: SessionRunEventKind::ManualAppendDegradedAudit,
                append_key: Some(degradation.audit_key),
                effect_intent: EffectIntent::None,
                artifact_degradation_reason: Some(reason),
                identity: None,
                sealed_artifact_ref: None,
                run_provenance: None,
                public_payload,
                created_at_ms: now,
            },
        )?;
        resolve_degraded_replay_append_contract_tx(
            &transaction,
            &reservation,
            committed.event_id,
            now,
        )?;
        let changed = transaction.execute(
            "UPDATE session_run_append_reservations
             SET terminal_audit_event_id = ?1, terminal_degraded_at_ms = ?2
             WHERE append_key = ?3 AND run_id = ?4 AND event_id = ?5
               AND lease_generation = ?6 AND lease_token = ?7
               AND lease_expires_at_ms > ?8 AND committed_event_id IS NULL
               AND terminal_audit_event_id IS NULL
               AND sealed_artifact_envelope_digest IS NULL",
            params![
                audit_event_id.to_string(),
                now,
                reservation.append_key.as_str(),
                reservation.run_id.to_string(),
                reservation.event_id.to_string(),
                i64::try_from(reservation.lease_generation)
                    .map_err(|_| LedgerError::NumericOverflow)?,
                reservation.token.to_blob(),
                now,
            ],
        )?;
        if changed != 1 {
            return Err(LedgerError::StaleAppendLease {
                append_key: lease.append_key.to_string(),
            });
        }
        transaction.commit()?;
        Ok(AppendReceipt {
            run_id: reservation.run_id,
            event_id: committed.event_id,
            event_sequence: committed.sequence,
            event_hash: committed.event_hash,
            event_kind: SessionRunEventKind::ManualAppendDegradedAudit,
            artifact_degradation_reason: Some(reason),
            sealed_artifact_ref: None,
            outbox_ids: committed.outbox_ids,
        })
    }

    pub fn claim_next(
        &self,
        mode: ResolvedExecutionMode,
        routes: &OutboxRouteRegistry,
    ) -> LedgerResult<ClaimNext> {
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = self.now()?;
        let expiry = add_duration(now, self.config.outbox_lease_duration)?;

        if let Some(expired) = load_expired_exhausted_outbox(&transaction, now, &mode)? {
            ensure_mode_matches(&transaction, expired.run_id, mode)?;
            let fence = expired.into_existing_fence()?;
            dead_letter_expired_outbox_tx(&transaction, &self.config, mode, &fence, now)?;
            transaction.commit()?;
            return Ok(ClaimNext::Rejected {
                outbox_id: fence.outbox_id,
                reason: DeliveryFailureReason::AttemptsExhausted,
                state: OutboxState::DeadLettered,
            });
        }

        let Some(candidate) = load_claimable_outbox(&transaction, now, &mode)? else {
            transaction.commit()?;
            return Ok(ClaimNext::Empty);
        };
        ensure_mode_matches(&transaction, candidate.run_id, mode)?;
        let next_attempt = candidate
            .attempt_count
            .checked_add(1)
            .ok_or(LedgerError::NumericOverflow)?;
        if next_attempt > candidate.max_attempts {
            return Err(LedgerError::IntegrityViolation(
                "claim selected an outbox row beyond its maximum attempts".to_string(),
            ));
        }
        let next_generation = candidate
            .lease_generation
            .checked_add(1)
            .ok_or(LedgerError::NumericOverflow)?;
        let token = LeaseToken::random();
        let changed = match candidate.state {
            OutboxState::Ready => transaction.execute(
                "UPDATE session_run_outbox
                 SET state = 'leased', attempt_count = ?1, lease_generation = ?2,
                     lease_token = ?3, lease_expires_at_ms = ?4
                 WHERE outbox_id = ?5 AND state = 'ready' AND attempt_count < max_attempts",
                params![
                    i64::from(next_attempt),
                    i64::try_from(next_generation).map_err(|_| LedgerError::NumericOverflow)?,
                    token.to_blob(),
                    expiry,
                    candidate.outbox_id.to_string(),
                ],
            )?,
            OutboxState::Leased => {
                let previous_token = candidate.previous_token.as_ref().ok_or_else(|| {
                    LedgerError::IntegrityViolation(
                        "expired outbox candidate lacks its existing lease token".to_string(),
                    )
                })?;
                transaction.execute(
                    "UPDATE session_run_outbox
                     SET state = 'leased', attempt_count = ?1, lease_generation = ?2,
                         lease_token = ?3, lease_expires_at_ms = ?4
                     WHERE outbox_id = ?5 AND state = 'leased'
                       AND lease_expires_at_ms <= ?6 AND attempt_count < max_attempts
                       AND lease_generation = ?7 AND lease_token = ?8",
                    params![
                        i64::from(next_attempt),
                        i64::try_from(next_generation).map_err(|_| LedgerError::NumericOverflow)?,
                        token.to_blob(),
                        expiry,
                        candidate.outbox_id.to_string(),
                        now,
                        i64::try_from(candidate.lease_generation)
                            .map_err(|_| LedgerError::NumericOverflow)?,
                        previous_token.to_blob(),
                    ],
                )?
            }
            OutboxState::Acknowledged | OutboxState::DeadLettered => {
                return Err(LedgerError::IntegrityViolation(
                    "terminal outbox row was selected for claim".to_string(),
                ));
            }
        };
        if changed != 1 {
            return Err(LedgerError::StaleOutboxLease {
                outbox_id: candidate.outbox_id.to_string(),
            });
        }
        let fence = OutboxFence {
            outbox_id: candidate.outbox_id,
            run_id: candidate.run_id,
            event_id: candidate.event_id,
            required: candidate.required,
            attempt_count: next_attempt,
            max_attempts: candidate.max_attempts,
            lease_generation: next_generation,
            token,
        };

        let rejection = match validate_payload(candidate.destination, &candidate.public_payload) {
            Err(_) => Some(DeliveryFailureReason::InvalidPayload),
            Ok(()) if !routes.contains(candidate.destination) => {
                Some(DeliveryFailureReason::MissingRoute)
            }
            Ok(()) => None,
        };
        if let Some(reason) = rejection {
            let completion = complete_delivery_tx(
                &transaction,
                &self.config,
                mode,
                &fence,
                DeliveryCompletion::Nacked(reason),
                None,
                now,
            )?;
            transaction.commit()?;
            return Ok(ClaimNext::Rejected {
                outbox_id: fence.outbox_id,
                reason,
                state: completion.state,
            });
        }

        transaction.commit()?;
        Ok(ClaimNext::Claimed(OutboxLease {
            outbox_id: fence.outbox_id,
            run_id: fence.run_id,
            event_id: fence.event_id,
            destination: candidate.destination,
            idempotency_key: candidate.idempotency_key,
            public_payload: candidate.public_payload,
            sealed_artifact_ref: candidate.sealed_artifact_ref,
            attempt_count: fence.attempt_count,
            lease_generation: fence.lease_generation,
            lease_expires_at_ms: expiry,
            token: fence.token,
        }))
    }

    /// Complete only a nonterminal retry without sealed evidence. Terminal
    /// ACKs and terminal NACKs must use `complete_delivery_with_evidence` so
    /// the result event is authoritative before the outbox transition commits.
    pub fn complete_delivery(
        &self,
        mode: ResolvedExecutionMode,
        lease: &OutboxLease,
        completion: DeliveryCompletion,
    ) -> LedgerResult<DeliveryCompletionReceipt> {
        self.complete_delivery_inner(mode, lease, completion, None)
    }

    /// Complete a leased delivery while atomically attaching the sealed,
    /// identity-bound result evidence to the immutable delivery-result event.
    /// Callers that publish the evidence must retain their publication guard
    /// until this method returns.
    pub fn complete_delivery_with_evidence(
        &self,
        mode: ResolvedExecutionMode,
        lease: &OutboxLease,
        completion: DeliveryCompletion,
        evidence: DeliveryEvidence,
    ) -> LedgerResult<DeliveryCompletionReceipt> {
        self.complete_delivery_inner(mode, lease, completion, Some(evidence))
    }

    fn complete_delivery_inner(
        &self,
        mode: ResolvedExecutionMode,
        lease: &OutboxLease,
        completion: DeliveryCompletion,
        evidence: Option<DeliveryEvidence>,
    ) -> LedgerResult<DeliveryCompletionReceipt> {
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = self.now()?;
        ensure_mode_matches(&transaction, lease.run_id, mode)?;
        let fence = load_fence_for_lease(&transaction, lease)?;
        let evidence_required = match &completion {
            DeliveryCompletion::Acknowledged(_) => true,
            DeliveryCompletion::Nacked(reason) => {
                matches!(
                    reason,
                    DeliveryFailureReason::RouteDecodeFailed
                        | DeliveryFailureReason::HandlerRejected
                ) || fence.attempt_count >= fence.max_attempts
            }
        };
        if evidence_required && evidence.is_none() {
            return Err(LedgerError::IntegrityViolation(
                "this delivery completion requires sealed result evidence".to_string(),
            ));
        }
        if let Some(evidence) = evidence.as_ref() {
            validate_delivery_evidence_binding(lease, &completion, evidence)?;
        }
        let receipt = complete_delivery_tx(
            &transaction,
            &self.config,
            mode,
            &fence,
            completion,
            evidence.as_ref(),
            now,
        )?;
        transaction.commit()?;
        Ok(receipt)
    }

    pub fn ack(
        &self,
        mode: ResolvedExecutionMode,
        lease: &OutboxLease,
        receipt: DurableDeliveryReceipt,
    ) -> LedgerResult<DeliveryCompletionReceipt> {
        self.complete_delivery(mode, lease, DeliveryCompletion::Acknowledged(receipt))
    }

    pub fn nack(
        &self,
        mode: ResolvedExecutionMode,
        lease: &OutboxLease,
        reason: DeliveryFailureReason,
    ) -> LedgerResult<DeliveryCompletionReceipt> {
        self.complete_delivery(mode, lease, DeliveryCompletion::Nacked(reason))
    }

    pub fn finalize_run_exit(
        &self,
        mode: ResolvedExecutionMode,
        lease: &RunOwnerLease,
        exit: RunExit,
    ) -> LedgerResult<RunStatus> {
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = self.now()?;
        ensure_mode_matches(&transaction, lease.run_id, mode)?;
        let owner_is_open = ensure_current_unexpired_run_owner_lease(&transaction, lease, now)?;
        let current = load_run_status(&transaction, lease.run_id)?;

        if let Some(existing_reason) = current.exit_terminal_reason {
            if existing_reason != exit.terminal_reason
                && existing_reason != TerminalReason::EffectNonResumableUnknown
            {
                return Err(LedgerError::RunExitConflict {
                    run_id: lease.run_id.to_string(),
                });
            }
            transaction.commit()?;
            return Ok(current);
        }
        if !owner_is_open {
            return Err(LedgerError::StaleRunOwnerLease {
                run_id: lease.run_id.to_string(),
            });
        }

        let terminalized = terminalize_nonresumable_effects(&transaction, lease.run_id, now)?;
        if !terminalized.has_unknown
            && exit.terminal_reason == TerminalReason::EffectNonResumableUnknown
        {
            return Err(LedgerError::EffectNonResumableUnknownRequiresEffect {
                run_id: lease.run_id.to_string(),
            });
        }
        if !terminalized.has_unknown
            && terminalized.abandoned_count > 0
            && exit.terminal_reason.outcome() == RunOutcome::Success
        {
            return Err(LedgerError::UnclaimedNonResumableEffects {
                run_id: lease.run_id.to_string(),
                count: terminalized.abandoned_count,
            });
        }
        let terminal_reason = if terminalized.has_unknown {
            TerminalReason::EffectNonResumableUnknown
        } else {
            exit.terminal_reason
        };
        let public_payload = bound_run_exit_payload(terminal_reason, exit.public_payload)?;
        let event_id = EventId::new();
        append_event_tx(
            &transaction,
            &self.config,
            EventDraft {
                event_id,
                run_id: lease.run_id,
                kind: SessionRunEventKind::RunExit,
                append_key: None,
                effect_intent: EffectIntent::None,
                artifact_degradation_reason: None,
                identity: None,
                sealed_artifact_ref: None,
                run_provenance: None,
                public_payload,
                created_at_ms: now,
            },
        )?;
        resolve_pending_replay_obligations_at_exit_tx(
            &transaction,
            lease.run_id,
            terminal_reason,
            event_id,
            now,
        )?;
        let changed = transaction.execute(
            "UPDATE session_runs
             SET exit_event_id = ?1, exit_terminal_reason = ?2, candidate_outcome = ?3
             WHERE run_id = ?4 AND exit_event_id IS NULL AND owner_id = ?5
               AND owner_lease_generation = ?6 AND owner_lease_token = ?7
               AND owner_lease_expires_at_ms > ?8",
            params![
                event_id.to_string(),
                terminal_reason.as_str(),
                terminal_reason.outcome().as_str(),
                lease.run_id.to_string(),
                lease.owner_id.as_str(),
                i64::try_from(lease.lease_generation).map_err(|_| LedgerError::NumericOverflow)?,
                lease.token.to_blob(),
                now,
            ],
        )?;
        if changed != 1 {
            return Err(LedgerError::StaleRunOwnerLease {
                run_id: lease.run_id.to_string(),
            });
        }
        reconcile_run_finalization(&transaction, lease.run_id)?;
        let status = load_run_status(&transaction, lease.run_id)?;
        transaction.commit()?;
        Ok(status)
    }

    pub fn run_header(&self, run_id: RunId) -> LedgerResult<RunHeader> {
        let connection = self.factory.open()?;
        load_run_header(&connection, run_id)
    }

    pub fn run_status(&self, run_id: RunId) -> LedgerResult<RunStatus> {
        let connection = self.factory.open()?;
        load_run_status(&connection, run_id)
    }

    pub fn events_for_run(&self, run_id: RunId) -> LedgerResult<Vec<SessionRunEvent>> {
        let connection = self.factory.open()?;
        load_events_for_run(&connection, run_id)
    }
    pub fn recent_run_ids(&self, limit: usize) -> LedgerResult<Vec<RunId>> {
        let limit =
            i64::try_from(limit.clamp(1, 1_000)).map_err(|_| LedgerError::NumericOverflow)?;
        let connection = self.factory.open()?;
        let mut statement = connection.prepare(
            "SELECT run_id FROM session_runs ORDER BY started_at_ms DESC, run_id DESC LIMIT ?1",
        )?;
        let stored_ids = statement
            .query_map([limit], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        stored_ids.into_iter().map(parse_run_id).collect()
    }
    pub fn run_ids_for_session(
        &self,
        session_id: &SessionId,
        latest_only: bool,
    ) -> LedgerResult<Vec<RunId>> {
        let connection = self.factory.open()?;
        let query = if latest_only {
            "SELECT run_id FROM session_runs
             WHERE session_id = ?1
             ORDER BY started_at_ms DESC, run_id DESC
             LIMIT 1"
        } else {
            "SELECT run_id FROM session_runs
             WHERE session_id = ?1
             ORDER BY started_at_ms ASC, run_id ASC"
        };
        let mut statement = connection.prepare(query)?;
        let stored_ids = statement
            .query_map([session_id.as_str()], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        stored_ids.into_iter().map(parse_run_id).collect()
    }

    pub fn verify_run_chain(&self, run_id: RunId) -> LedgerResult<()> {
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let header = load_run_header(&transaction, run_id)?;
        let status = load_run_status(&transaction, run_id)?;
        let head_hash: String = transaction.query_row(
            "SELECT head_hash FROM session_runs WHERE run_id = ?1",
            [run_id.to_string()],
            |row| row.get(0),
        )?;
        let head_hash = validated_digest_string("run head hash", head_hash)?;
        let events = load_events_for_run(&transaction, run_id)?;
        let manifests = load_manifest_snapshots_for_run(&transaction, run_id)?;
        let outbox = load_outbox_snapshots_for_run(&transaction, run_id)?;
        let nonresumable_effects =
            load_nonresumable_effect_snapshots_for_run(&transaction, run_id)?;
        verify_run_snapshot_contents(
            &header,
            &status,
            &head_hash,
            &events,
            &manifests,
            &outbox,
            &nonresumable_effects,
        )?;
        transaction.commit()?;
        Ok(())
    }

    pub(crate) fn export_snapshot(&self, run_id: RunId) -> LedgerResult<LedgerRunSnapshot> {
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let header = load_run_header(&transaction, run_id)?;
        let status = load_run_status(&transaction, run_id)?;
        let head_hash: String = transaction.query_row(
            "SELECT head_hash FROM session_runs WHERE run_id = ?1",
            [run_id.to_string()],
            |row| row.get(0),
        )?;
        let head_hash = validated_digest_string("run head hash", head_hash)?;
        let events = load_events_for_run(&transaction, run_id)?;
        let manifests = load_manifest_snapshots_for_run(&transaction, run_id)?;
        let outbox = load_outbox_snapshots_for_run(&transaction, run_id)?;
        let nonresumable_effects =
            load_nonresumable_effect_snapshots_for_run(&transaction, run_id)?;
        let replay_obligations = load_replay_obligations_for_run(&transaction, run_id)?;
        verify_run_snapshot_contents(
            &header,
            &status,
            &head_hash,
            &events,
            &manifests,
            &outbox,
            &nonresumable_effects,
        )?;
        let snapshot = LedgerRunSnapshot {
            header,
            status,
            head_hash,
            events,
            manifests,
            outbox,
            nonresumable_effects,
            replay_obligations,
        };
        transaction.commit()?;
        Ok(snapshot)
    }

    /// Claim the exact verified source event owned by the runtime-tool
    /// capability. The effect itself executes outside the owner-lease lock so
    /// the heartbeat may continue; no retry is possible after this claim.
    pub(crate) fn begin_nonresumable_effect(
        &self,
        mode: ResolvedExecutionMode,
        owner_lease: &RunOwnerLease,
        effect: &CertifiedEffectIntent,
    ) -> LedgerResult<NonResumableEffectLease> {
        self.claim_nonresumable_effect(
            mode,
            owner_lease,
            effect.source_event_id(),
            effect.source_event_hash(),
        )
    }

    pub(crate) fn observed_nonresumable_attempt(
        lease: NonResumableEffectLease,
        outcome: &str,
    ) -> LedgerResult<NonResumableEffectAttempt> {
        if !matches!(outcome, "returned" | "handler_error") {
            return Err(LedgerError::IntegrityViolation(
                "invalid non-resumable effect outcome".to_string(),
            ));
        }
        Ok(NonResumableEffectAttempt {
            lease,
            result: CanonicalJson::from_value(&json!({ "outcome": outcome }))?,
        })
    }

    pub(crate) fn persist_nonresumable_result(
        &self,
        mode: ResolvedExecutionMode,
        attempt: &NonResumableEffectAttempt,
    ) -> LedgerResult<CanonicalJson> {
        let result_payload = Self::bind_nonresumable_result_payload(attempt)?;
        let result_payload_digest =
            Sha256Digest::new(sha256_hex(result_payload.as_str().as_bytes()))?;
        self.persist_nonresumable_result_commitment(mode, attempt, &result_payload_digest)?;
        Ok(result_payload)
    }

    /// Bind the certified executor's observed result to the random one-shot
    /// claim. The claim-derived append key prevents a pre-claim event from
    /// masquerading as completion evidence; callers cannot substitute a
    /// different result value.
    pub fn bind_nonresumable_result_payload(
        attempt: &NonResumableEffectAttempt,
    ) -> LedgerResult<CanonicalJson> {
        let lease = &attempt.lease;
        let claim_binding = derive_nonresumable_claim_binding(&lease.token)?;
        let result_append_key =
            derive_nonresumable_result_append_key(lease.event_id, &claim_binding)?;
        if lease.claim_binding != claim_binding || lease.result_append_key != result_append_key {
            return Err(LedgerError::IntegrityViolation(
                "non-resumable effect lease claim binding is inconsistent".to_string(),
            ));
        }
        CanonicalJson::from_value(&json!({
            "schema": NONRESUMABLE_RESULT_SCHEMA,
            "schema_version": RUN_EVENT_BINDING_SCHEMA_VERSION,
            "source_event_id": lease.event_id,
            "claim_binding": claim_binding,
            "payload": attempt.result.as_value()?,
        }))
    }

    pub(crate) fn persist_nonresumable_result_commitment(
        &self,
        mode: ResolvedExecutionMode,
        attempt: &NonResumableEffectAttempt,
        result_payload_digest: &Sha256Digest,
    ) -> LedgerResult<()> {
        let lease = &attempt.lease;
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        ensure_mode_matches(&transaction, lease.run_id, mode)?;
        let row = load_nonresumable_effect(&transaction, lease.event_id)?.ok_or_else(|| {
            LedgerError::NonResumableEffectNotFound {
                event_id: lease.event_id.to_string(),
            }
        })?;
        if row.record.run_id != lease.run_id
            || row.record.state != NonResumableEffectState::Executing
            || row.claim_token.as_ref() != Some(&lease.token)
        {
            return Err(LedgerError::StaleNonResumableEffectLease {
                event_id: lease.event_id.to_string(),
            });
        }
        if let Some(existing) = row.record.result_payload_digest {
            if existing != *result_payload_digest {
                return Err(LedgerError::IntegrityViolation(
                    "non-resumable effect result commitment changed".to_string(),
                ));
            }
            transaction.commit()?;
            return Ok(());
        }
        let changed = transaction.execute(
            "UPDATE session_run_nonresumable_effects
             SET result_payload_digest = ?1
             WHERE event_id = ?2 AND state = 'executing' AND claim_token = ?3
               AND result_payload_digest IS NULL",
            params![
                result_payload_digest.as_str(),
                lease.event_id.to_string(),
                lease.token.to_blob(),
            ],
        )?;
        if changed != 1 {
            return Err(LedgerError::StaleNonResumableEffectLease {
                event_id: lease.event_id.to_string(),
            });
        }
        transaction.commit()?;
        Ok(())
    }

    /// Atomically cross the one-shot boundary. There is deliberately no lease
    /// expiry: after this succeeds, no caller or recovery path may invoke the
    /// effect again.
    fn claim_nonresumable_effect(
        &self,
        mode: ResolvedExecutionMode,
        owner_lease: &RunOwnerLease,
        event_id: EventId,
        expected_source_hash: &Sha256Digest,
    ) -> LedgerResult<NonResumableEffectLease> {
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = self.now()?;
        let row = load_nonresumable_effect(&transaction, event_id)?.ok_or_else(|| {
            LedgerError::NonResumableEffectNotFound {
                event_id: event_id.to_string(),
            }
        })?;
        let events = load_events_for_run(&transaction, row.record.run_id)?;
        verify_event_chain_for_run(&transaction, row.record.run_id, &events)?;
        let source_event = events
            .iter()
            .find(|event| event.event_id == event_id)
            .ok_or_else(|| LedgerError::NonResumableEffectNotFound {
                event_id: event_id.to_string(),
            })?;
        if source_event.event_hash != expected_source_hash.as_str()
            || source_event.effect_intent != EffectIntent::LocalOneShotV1
            || source_event.sealed_artifact_ref.is_none()
        {
            return Err(LedgerError::IntegrityViolation(
                "certified one-shot source event binding is invalid".to_string(),
            ));
        }
        let effects = load_nonresumable_effect_snapshots_for_run(&transaction, row.record.run_id)?;
        verify_event_nonresumable_projection(source_event, &effects, &events)?;
        ensure_mode_matches(&transaction, row.record.run_id, mode)?;
        if owner_lease.run_id != row.record.run_id
            || !ensure_current_unexpired_run_owner_lease(&transaction, owner_lease, now)?
        {
            return Err(LedgerError::StaleRunOwnerLease {
                run_id: row.record.run_id.to_string(),
            });
        }
        if row.record.state != NonResumableEffectState::Armed {
            return Err(LedgerError::NonResumableEffectNotArmed {
                event_id: event_id.to_string(),
                state: row.record.state.as_str(),
            });
        }

        let token = LeaseToken::random();
        let claim_binding = derive_nonresumable_claim_binding(&token)?;
        let result_append_key = derive_nonresumable_result_append_key(event_id, &claim_binding)?;
        let changed = transaction.execute(
            "UPDATE session_run_nonresumable_effects
             SET state = 'executing', claim_token = ?1, claimed_at_ms = ?2
             WHERE event_id = ?3 AND state = 'armed' AND claim_token IS NULL",
            params![token.to_blob(), now, event_id.to_string()],
        )?;
        if changed != 1 {
            return Err(LedgerError::NonResumableEffectNotArmed {
                event_id: event_id.to_string(),
                state: "concurrently_claimed",
            });
        }
        let lease = NonResumableEffectLease {
            run_id: row.record.run_id,
            event_id,
            idempotency_key: row.record.idempotency_key,
            claim_binding,
            result_append_key,
            claimed_at_ms: now,
            token,
        };
        transaction.commit()?;
        Ok(lease)
    }

    /// Link a committed, sealed result append to an executing one-shot. This
    /// never executes the effect and is idempotent for the same lease/result.
    pub(crate) fn complete_nonresumable_effect(
        &self,
        mode: ResolvedExecutionMode,
        attempt: &NonResumableEffectAttempt,
        result_event_id: EventId,
    ) -> LedgerResult<NonResumableEffectRecord> {
        let lease = &attempt.lease;
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let now = self.now()?;
        ensure_mode_matches(&transaction, lease.run_id, mode)?;
        let row = load_nonresumable_effect(&transaction, lease.event_id)?.ok_or_else(|| {
            LedgerError::NonResumableEffectNotFound {
                event_id: lease.event_id.to_string(),
            }
        })?;
        if row.record.run_id != lease.run_id || row.claim_token.as_ref() != Some(&lease.token) {
            return Err(LedgerError::StaleNonResumableEffectLease {
                event_id: lease.event_id.to_string(),
            });
        }
        if row.record.state == NonResumableEffectState::Completed
            && row.record.result_event_id == Some(result_event_id)
        {
            transaction.commit()?;
            return Ok(row.record);
        }
        if row.record.state != NonResumableEffectState::Executing {
            return Err(LedgerError::StaleNonResumableEffectLease {
                event_id: lease.event_id.to_string(),
            });
        }
        let expected_payload = Self::bind_nonresumable_result_payload(attempt)?;
        let expected_digest = Sha256Digest::new(sha256_hex(expected_payload.as_str().as_bytes()))?;
        if row.record.result_payload_digest.as_ref() != Some(&expected_digest) {
            return Err(LedgerError::NonResumableEffectResultMismatch {
                event_id: lease.event_id.to_string(),
                result_event_id: result_event_id.to_string(),
            });
        }
        validate_nonresumable_result_event(&transaction, lease, result_event_id, &expected_digest)?;
        let changed = transaction.execute(
            "UPDATE session_run_nonresumable_effects
             SET state = 'completed', result_event_id = ?1, terminal_at_ms = ?2
             WHERE event_id = ?3 AND state = 'executing' AND claim_token = ?4
               AND result_event_id IS NULL AND terminal_at_ms IS NULL",
            params![
                result_event_id.to_string(),
                now,
                lease.event_id.to_string(),
                lease.token.to_blob(),
            ],
        )?;
        if changed != 1 {
            return Err(LedgerError::StaleNonResumableEffectLease {
                event_id: lease.event_id.to_string(),
            });
        }
        let completed = load_nonresumable_effect(&transaction, lease.event_id)?
            .ok_or_else(|| LedgerError::NonResumableEffectNotFound {
                event_id: lease.event_id.to_string(),
            })?
            .record;
        transaction.commit()?;
        Ok(completed)
    }

    pub fn nonresumable_effect(
        &self,
        event_id: EventId,
    ) -> LedgerResult<Option<NonResumableEffectRecord>> {
        let connection = self.factory.open()?;
        Ok(load_nonresumable_effect(&connection, event_id)?.map(|row| row.record))
    }

    pub fn outbox_for_run(&self, run_id: RunId) -> LedgerResult<Vec<OutboxRecord>> {
        let connection = self.factory.open()?;
        ensure_run_exists(&connection, run_id)?;
        load_outbox_records_for_run(&connection, run_id)
    }

    /// Snapshot only references that still protect physical CAS data. A
    /// publication guard covers a new write before its reservation is
    /// published; a published, uncommitted reservation remains live until it
    /// becomes either a logical reference or a terminal degradation. The
    /// caller holds the CAS store lock through this snapshot and its deletion
    /// decision, preventing publish-to-commit races. Retired and tombstoned
    /// objects are intentionally absent.
    pub fn referenced_sealed_envelope_digests(&self) -> LedgerResult<Vec<Sha256Digest>> {
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Deferred)?;
        let raw_digests = {
            let mut statement = transaction.prepare(
                "SELECT envelope_digest
                 FROM session_run_artifact_refs
                 WHERE state IN ('unverified', 'available', 'retiring')
                 UNION
                 SELECT sealed_artifact_envelope_digest AS envelope_digest
                 FROM session_run_append_reservations
                 WHERE sealed_artifact_envelope_digest IS NOT NULL
                   AND committed_event_id IS NULL
                   AND terminal_audit_event_id IS NULL
                 ORDER BY envelope_digest ASC",
            )?;
            let rows = statement
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            rows
        };
        transaction.commit()?;
        raw_digests.into_iter().map(Sha256Digest::new).collect()
    }

    pub fn artifact_reference_status(
        &self,
        run_id: RunId,
        event_id: EventId,
        artifact: &SealedArtifactRef,
    ) -> LedgerResult<ArtifactReferenceStatus> {
        let connection = self.factory.open()?;
        load_artifact_reference_status(&connection, run_id, event_id, artifact)
    }

    /// Return the ordered, typed replay obligations that make exactness a
    /// run-level invariant. Callers must validate any referenced artifact
    /// through `artifact_reference_status` before unsealing it.
    /// Return the ordered, typed replay obligations that make exactness a
    /// run-level invariant. Export construction uses the same loader under its
    /// one immutable SQLite read transaction.
    pub(crate) fn replay_obligations_for_run(
        &self,
        run_id: RunId,
    ) -> LedgerResult<Vec<ReplayObligationSnapshot>> {
        let connection = self.factory.open()?;
        load_replay_obligations_for_run(&connection, run_id)
    }

    pub fn unverified_artifact_references(&self) -> LedgerResult<Vec<ArtifactReferenceStatus>> {
        let connection = self.factory.open()?;
        let mut statement = connection.prepare(
            "SELECT run_id, event_id, envelope_digest
             FROM session_run_artifact_refs
             WHERE state = 'unverified'
             ORDER BY envelope_digest ASC, run_id ASC, event_id ASC",
        )?;
        let keys = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        keys.into_iter()
            .map(|(run_id, event_id, digest)| {
                let (artifact, _) = load_physical_artifact(&connection, &digest)?;
                load_artifact_reference_status(
                    &connection,
                    parse_run_id(run_id)?,
                    parse_event_id(event_id)?,
                    &artifact,
                )
            })
            .collect()
    }

    /// Objects in this list are durably tombstoned and have no live or
    /// retiring logical references. A caller must still hold the filesystem
    /// store's exclusive guard before unlinking and then call
    /// [`Self::observe_exact_artifact_unlinked`].
    pub fn tombstoned_exact_artifacts(&self) -> LedgerResult<Vec<SealedArtifactRef>> {
        let connection = self.factory.open()?;
        let mut statement = connection.prepare(
            "SELECT envelope_digest
             FROM session_run_artifact_objects
             WHERE state = 'retiring'
             ORDER BY envelope_digest ASC",
        )?;
        let digests = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        digests
            .into_iter()
            .map(|digest| {
                load_physical_artifact(&connection, &digest).map(|(artifact, _)| artifact)
            })
            .collect()
    }

    /// Store guard must be acquired before this transaction. Verification may
    /// promote only provisional historical `unverified` references; it never
    /// reverses a durable unavailable, retiring, or retired state.
    pub fn mark_sealed_artifact_verified(
        &self,
        artifact: &SealedArtifactRef,
    ) -> LedgerResult<Vec<ArtifactStateTransition>> {
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let transitions = mark_sealed_artifact_verified_tx(&transaction, artifact, self.now()?)?;
        transaction.commit()?;
        Ok(transitions)
    }

    /// Store guard must be acquired before this transaction. A verified
    /// physical failure fan-outs to every live logical reference for the
    /// digest, so no sibling run can continue to claim exact replay.
    pub fn mark_sealed_artifact_unavailable(
        &self,
        artifact: &SealedArtifactRef,
        reason: ArtifactStateReason,
    ) -> LedgerResult<Vec<ArtifactStateTransition>> {
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let transitions =
            mark_sealed_artifact_unavailable_tx(&transaction, artifact, reason, self.now()?)?;
        transaction.commit()?;
        Ok(transitions)
    }

    /// Begin retirement of only this logical reference. This transaction does
    /// not make a shared physical object tombstoned while another reference is
    /// still live or retiring.
    pub fn begin_exact_artifact_retirement(
        &self,
        run_id: RunId,
        event_id: EventId,
        artifact: &SealedArtifactRef,
    ) -> LedgerResult<ArtifactStateTransition> {
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let transition = begin_exact_artifact_retirement_tx(
            &transaction,
            run_id,
            event_id,
            artifact,
            self.now()?,
        )?;
        transaction.commit()?;
        Ok(transition)
    }

    /// Finish one logical reference retirement. If it was the final
    /// `unverified`/`available`/`retiring` reference, this durably tombstones
    /// the physical object before any caller may unlink it.
    pub fn finish_exact_artifact_retirement(
        &self,
        run_id: RunId,
        event_id: EventId,
        artifact: &SealedArtifactRef,
    ) -> LedgerResult<ArtifactStateTransition> {
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let transition = finish_exact_artifact_retirement_tx(
            &transaction,
            run_id,
            event_id,
            artifact,
            self.now()?,
        )?;
        transaction.commit()?;
        Ok(transition)
    }

    /// Observe a successful unlink while the exclusive store guard remains
    /// held. This may only move a durable tombstone to `retired`; retrying the
    /// same observation returns successfully without a reversal.
    pub fn observe_exact_artifact_unlinked(
        &self,
        artifact: &SealedArtifactRef,
    ) -> LedgerResult<ArtifactPhysicalState> {
        let mut connection = self.factory.open()?;
        let transaction = connection.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let state = observe_exact_artifact_unlinked_tx(&transaction, artifact, self.now()?)?;
        transaction.commit()?;
        Ok(state)
    }

    pub fn append_identity_for_key(
        &self,
        append_key: &AppendKey,
    ) -> LedgerResult<Option<AppendIdentity>> {
        let connection = self.factory.open()?;
        Ok(load_reservation(&connection, append_key)?.map(|reservation| reservation.identity))
    }

    fn now(&self) -> LedgerResult<i64> {
        self.clock.now_unix_ms()
    }
}

#[derive(Clone)]
struct EventDraft {
    event_id: EventId,
    run_id: RunId,
    kind: SessionRunEventKind,
    append_key: Option<AppendKey>,
    effect_intent: EffectIntent,
    artifact_degradation_reason: Option<ArtifactDegradationReason>,
    identity: Option<AppendIdentity>,
    sealed_artifact_ref: Option<SealedArtifactRef>,
    run_provenance: Option<CanonicalJson>,
    public_payload: CanonicalJson,
    created_at_ms: i64,
}

struct CommittedEvent {
    event_id: EventId,
    sequence: u64,
    event_hash: String,
    event: SessionRunEvent,
    outbox_ids: Vec<OutboxRowId>,
}

fn append_event_tx(
    transaction: &Transaction<'_>,
    config: &RunLedgerConfig,
    draft: EventDraft,
) -> LedgerResult<CommittedEvent> {
    if draft.effect_intent.requires_sealed_artifact() && draft.sealed_artifact_ref.is_none() {
        return Err(LedgerError::IntegrityViolation(
            "effect-capable event draft lacks a sealed artifact reference".to_string(),
        ));
    }
    let manual_degradation = draft.kind == SessionRunEventKind::ManualAppendDegradedAudit;
    if manual_degradation != draft.artifact_degradation_reason.is_some()
        || (manual_degradation
            && (draft.effect_intent != EffectIntent::None
                || draft.identity.is_some()
                || draft.sealed_artifact_ref.is_some()
                || draft.append_key.is_none()))
    {
        return Err(LedgerError::IntegrityViolation(
            "manual artifact degradation event draft has invalid effect or identity state"
                .to_string(),
        ));
    }
    let artifact_state_audit = draft.kind == SessionRunEventKind::ArtifactStateAudit;
    if artifact_state_audit
        && (draft.effect_intent != EffectIntent::None
            || draft.append_key.is_some()
            || draft.identity.is_some()
            || draft.sealed_artifact_ref.is_some()
            || draft.artifact_degradation_reason.is_some()
            || draft.run_provenance.is_some())
    {
        return Err(LedgerError::IntegrityViolation(
            "artifact state audit event draft has invalid effect or identity state".to_string(),
        ));
    }
    if artifact_state_audit {
        validate_artifact_state_audit_payload(&draft.public_payload)?;
    }
    let manifest = DispatchPolicyV1::derive(
        draft.event_id,
        draft.effect_intent,
        &draft.public_payload,
        draft.sealed_artifact_ref.as_ref(),
    )?;
    if manifest.sealed_artifact_ref != draft.sealed_artifact_ref {
        return Err(LedgerError::IntegrityViolation(
            "derived manifest sealed artifact reference does not equal event reference".to_string(),
        ));
    }
    let (current_sequence, previous_hash): (i64, String) = transaction.query_row(
        "SELECT next_event_sequence, head_hash FROM session_runs WHERE run_id = ?1",
        [draft.run_id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let next_sequence = current_sequence
        .checked_add(1)
        .ok_or(LedgerError::NumericOverflow)?;
    let sequence = u64::try_from(next_sequence).map_err(|_| LedgerError::NumericOverflow)?;
    let event = event_from_draft(sequence, previous_hash.clone(), &manifest.hash, &draft)?;

    let fingerprint_algorithm = event
        .private_fingerprint
        .as_ref()
        .map(|fingerprint| fingerprint.algorithm().as_str());
    let fingerprint_algorithm_version = event
        .private_fingerprint
        .as_ref()
        .map(|fingerprint| i64::from(fingerprint.algorithm_version()));
    let fingerprint_key_id = event
        .private_fingerprint
        .as_ref()
        .map(|fingerprint| fingerprint.key_id().as_str());
    let fingerprint_key_version = event
        .private_fingerprint
        .as_ref()
        .map(|fingerprint| i64::from(fingerprint.key_version()));
    let fingerprint = event
        .private_fingerprint
        .as_ref()
        .map(PrivateFingerprint::as_hex);
    let artifact_values = ArtifactRefDbValues::from(event.sealed_artifact_ref.as_ref());
    transaction.execute(
        "INSERT INTO session_run_events (
            event_id, run_id, event_sequence, event_kind, append_key, effect_intent,
            artifact_degradation_reason, public_identity_jcs, private_fingerprint_algorithm,
            private_fingerprint_algorithm_version, private_fingerprint_key_id,
            private_fingerprint_key_version, private_fingerprint,
            sealed_artifact_envelope_digest, sealed_artifact_key_id,
            sealed_artifact_schema_version, sealed_artifact_encryption_algorithm,
            sealed_artifact_canonicalization_algorithm,
            sealed_artifact_canonicalization_version, sealed_artifact_redactor_version,
            run_provenance_jcs, public_payload_jcs, manifest_hash, previous_hash, event_hash,
            canonicalization_algorithm, canonicalization_version, created_at_ms
        ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
            ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28
        )",
        params![
            event.event_id.to_string(),
            event.run_id.to_string(),
            i64::try_from(event.sequence).map_err(|_| LedgerError::NumericOverflow)?,
            event.kind.as_str(),
            event.append_key.as_ref().map(AppendKey::as_str),
            event.effect_intent.as_str(),
            event
                .artifact_degradation_reason
                .map(ArtifactDegradationReason::as_str),
            event.public_identity.as_ref().map(CanonicalJson::as_str),
            fingerprint_algorithm,
            fingerprint_algorithm_version,
            fingerprint_key_id,
            fingerprint_key_version,
            fingerprint,
            artifact_values.envelope_digest,
            artifact_values.key_id,
            artifact_values.schema_version,
            artifact_values.encryption_algorithm,
            artifact_values.canonicalization_algorithm,
            artifact_values.canonicalization_version,
            artifact_values.redactor_version,
            event.run_provenance.as_ref().map(CanonicalJson::as_str),
            event.public_payload.as_str(),
            event.manifest_hash,
            event.previous_hash,
            event.event_hash,
            CANONICALIZATION_ALGORITHM,
            i64::from(CANONICALIZATION_VERSION),
            event.created_at_ms,
        ],
    )?;
    if let Some(reference) = event.sealed_artifact_ref.as_ref() {
        register_sealed_artifact_reference_tx(
            transaction,
            event.run_id,
            event.event_id,
            reference,
            event.created_at_ms,
        )?;
    }
    transaction.execute(
        "INSERT INTO session_run_manifests (
            event_id, manifest_jcs, manifest_hash, canonicalization_algorithm,
            canonicalization_version
         ) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            event.event_id.to_string(),
            manifest.canonical_json.as_str(),
            manifest.hash,
            CANONICALIZATION_ALGORITHM,
            i64::from(CANONICALIZATION_VERSION),
        ],
    )?;

    let mut outbox_ids = Vec::with_capacity(manifest.entries.len());
    for entry in &manifest.entries {
        if event.sealed_artifact_ref.as_ref() != Some(&entry.sealed_artifact_ref) {
            return Err(LedgerError::IntegrityViolation(
                "derived outbox reference does not equal event reference".to_string(),
            ));
        }
        let outbox_id = OutboxRowId::new();
        transaction.execute(
            "INSERT INTO session_run_outbox (
                outbox_id, run_id, event_id, manifest_order, destination, required,
                idempotency_key, public_payload_jcs,
                sealed_artifact_envelope_digest, sealed_artifact_key_id,
                sealed_artifact_schema_version, sealed_artifact_encryption_algorithm,
                sealed_artifact_canonicalization_algorithm,
                sealed_artifact_canonicalization_version, sealed_artifact_redactor_version,
                state, attempt_count, max_attempts, lease_generation
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                'ready', 0, ?16, 0
            )",
            params![
                outbox_id.to_string(),
                event.run_id.to_string(),
                event.event_id.to_string(),
                i64::from(entry.order),
                entry.destination.as_str(),
                i64::from(entry.required),
                entry.idempotency_key.as_str(),
                entry.public_payload.as_str(),
                entry.sealed_artifact_ref.envelope_digest().as_str(),
                entry.sealed_artifact_ref.key_id().as_str(),
                i64::from(entry.sealed_artifact_ref.schema_version()),
                entry.sealed_artifact_ref.encryption_algorithm(),
                entry.sealed_artifact_ref.canonicalization_algorithm(),
                i64::from(entry.sealed_artifact_ref.canonicalization_version()),
                entry.sealed_artifact_ref.redactor_version().as_str(),
                i64::from(config.max_delivery_attempts),
            ],
        )?;
        outbox_ids.push(outbox_id);
    }

    insert_delivery_request_obligations_tx(transaction, &event, &outbox_ids)?;

    if event.effect_intent == EffectIntent::LocalOneShotV1 {
        let idempotency_key = derive_nonresumable_idempotency_key(event.event_id)?;
        transaction.execute(
            "INSERT INTO session_run_nonresumable_effects (
                event_id, run_id, state, idempotency_key
             ) VALUES (?1, ?2, 'armed', ?3)",
            params![
                event.event_id.to_string(),
                event.run_id.to_string(),
                idempotency_key.as_str(),
            ],
        )?;
    }

    let changed = transaction.execute(
        "UPDATE session_runs
         SET next_event_sequence = ?1, head_hash = ?2
         WHERE run_id = ?3 AND next_event_sequence = ?4 AND head_hash = ?5",
        params![
            next_sequence,
            event.event_hash,
            event.run_id.to_string(),
            current_sequence,
            previous_hash,
        ],
    )?;
    if changed != 1 {
        return Err(LedgerError::IntegrityViolation(
            "run chain head changed while appending an event".to_string(),
        ));
    }

    Ok(CommittedEvent {
        event_id: event.event_id,
        sequence: event.sequence,
        event_hash: event.event_hash.clone(),
        event,
        outbox_ids,
    })
}

/// Bind every freshly created outbox row to its own replay-critical delivery
/// request. The request references the source event's already sealed payload;
/// it does not duplicate artifact storage or assert a second payload.
fn insert_delivery_request_obligations_tx(
    transaction: &Transaction<'_>,
    event: &SessionRunEvent,
    outbox_ids: &[OutboxRowId],
) -> LedgerResult<()> {
    if outbox_ids.is_empty() {
        return Ok(());
    }
    let requested_fidelity = event
        .append_key
        .as_ref()
        .map(|append_key| {
            transaction
                .query_row(
                    "SELECT requested_fidelity
                     FROM session_run_replay_append_contracts WHERE append_key = ?1",
                    [append_key.as_str()],
                    |row| row.get::<_, String>(0),
                )
                .optional()
        })
        .transpose()?
        .flatten();
    let fidelity = match (requested_fidelity.as_deref(), event.sealed_artifact_ref.is_some()) {
        (Some("exact_private"), true) => ReplayObligationFidelity::ExactPrivate,
        _ => ReplayObligationFidelity::MetadataOnly,
    };
    for outbox_id in outbox_ids {
        transaction.execute(
            "INSERT INTO session_run_replay_obligations (
                run_id, source_event_id, operation_kind, phase, operation_instance_id,
                replay_fidelity, outcome, resolved_event_id, created_at_ms, resolved_at_ms
             ) VALUES (?1, ?2, 'delivery', 'request', ?3, ?4, 'not_applicable', ?2, ?5, ?5)",
            params![
                event.run_id.to_string(),
                event.event_id.to_string(),
                outbox_id.to_string(),
                fidelity.as_str(),
                event.created_at_ms,
            ],
        )?;
    }
    Ok(())
}
#[derive(Clone)]
struct PhysicalArtifactRecord {
    artifact: SealedArtifactRef,
    state: ArtifactPhysicalState,
    reason: Option<ArtifactStateReason>,
    transition_sequence: u64,
    tombstoned_at_ms: Option<i64>,
    retired_at_ms: Option<i64>,
}

fn load_physical_artifact(
    connection: &Connection,
    digest: &str,
) -> LedgerResult<(SealedArtifactRef, PhysicalArtifactRecord)> {
    let raw = connection
        .query_row(
            "SELECT envelope_digest, key_id, schema_version, encryption_algorithm,
                    canonicalization_algorithm, canonicalization_version, redactor_version,
                    state, state_reason, transition_sequence, tombstoned_at_ms, retired_at_ms
             FROM session_run_artifact_objects
             WHERE envelope_digest = ?1",
            [digest],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, Option<i64>>(10)?,
                    row.get::<_, Option<i64>>(11)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| {
            LedgerError::SealedArtifactReference(format!(
                "sealed artifact object {digest} was not registered"
            ))
        })?;
    let artifact = SealedArtifactRef::from_db(raw.0, raw.1, raw.2, raw.3, raw.4, raw.5, raw.6)?;
    let record = PhysicalArtifactRecord {
        artifact: artifact.clone(),
        state: ArtifactPhysicalState::from_db(&raw.7)?,
        reason: raw
            .8
            .as_deref()
            .map(ArtifactStateReason::from_db)
            .transpose()?,
        transition_sequence: parse_nonnegative_u64("artifact physical transition sequence", raw.9)?,
        tombstoned_at_ms: raw.10,
        retired_at_ms: raw.11,
    };
    validate_physical_state_record(&record)?;
    Ok((artifact, record))
}

fn load_artifact_reference_status(
    connection: &Connection,
    run_id: RunId,
    event_id: EventId,
    artifact: &SealedArtifactRef,
) -> LedgerResult<ArtifactReferenceStatus> {
    let raw = connection
        .query_row(
            "SELECT a.run_id, a.event_id,
                    o.envelope_digest, o.key_id, o.schema_version, o.encryption_algorithm,
                    o.canonicalization_algorithm, o.canonicalization_version, o.redactor_version,
                    o.state, o.state_reason,
                    a.state, a.state_reason,
                    x.exact_replay, x.state, x.state_reason, x.transition_sequence,
                    x.latched_at_ms, x.latched_by_event_id
             FROM session_run_artifact_refs AS a
             JOIN session_run_artifact_objects AS o
               ON o.envelope_digest = a.envelope_digest
             JOIN session_run_exact_replay AS x
               ON x.run_id = a.run_id
             WHERE a.run_id = ?1 AND a.event_id = ?2 AND a.envelope_digest = ?3",
            params![
                run_id.to_string(),
                event_id.to_string(),
                artifact.envelope_digest().as_str(),
            ],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                    row.get::<_, Option<String>>(10)?,
                    row.get::<_, String>(11)?,
                    row.get::<_, Option<String>>(12)?,
                    row.get::<_, i64>(13)?,
                    row.get::<_, String>(14)?,
                    row.get::<_, Option<String>>(15)?,
                    row.get::<_, i64>(16)?,
                    row.get::<_, Option<i64>>(17)?,
                    row.get::<_, Option<String>>(18)?,
                ))
            },
        )
        .optional()?
        .ok_or_else(|| LedgerError::ArtifactReferenceNotFound {
            run_id: run_id.to_string(),
            event_id: event_id.to_string(),
            digest: artifact.envelope_digest().as_str().to_string(),
        })?;
    let stored_artifact =
        SealedArtifactRef::from_db(raw.2, raw.3, raw.4, raw.5, raw.6, raw.7, raw.8)?;
    if &stored_artifact != artifact {
        return Err(LedgerError::ArtifactReferenceAliasConflict {
            digest: artifact.envelope_digest().as_str().to_string(),
        });
    }
    let exact_state = ExactReplayState::from_db(&raw.14)?;
    let exact_replay = parse_sql_bool(raw.13, "exact replay")?;
    if exact_replay != exact_state.exact_replay() {
        return Err(LedgerError::InvalidStoredState {
            kind: "exact replay latch",
            value: raw.14,
        });
    }
    let transition_sequence =
        parse_nonnegative_u64("exact replay transition sequence", raw.16)?;
    let latched_by_event_id = raw.18.map(parse_event_id).transpose()?;
    match (transition_sequence, raw.17, latched_by_event_id) {
        (0, None, None) => {}
        (0, _, _) => {
            return Err(LedgerError::InvalidStoredState {
                kind: "exact replay latch",
                value: "zero transition sequence has latch fields".to_string(),
            });
        }
        (_, Some(_), Some(latched_event_id)) => {
            let latched_run_id: Option<String> = connection
                .query_row(
                    "SELECT run_id FROM session_run_events WHERE event_id = ?1",
                    [latched_event_id.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            let expected_run_id = run_id.to_string();
            if latched_run_id.as_deref() != Some(expected_run_id.as_str()) {
                return Err(LedgerError::InvalidStoredState {
                    kind: "exact replay latch",
                    value: "latched event belongs to another run or is absent".to_string(),
                });
            }
        }
        (_, _, _) => {
            return Err(LedgerError::InvalidStoredState {
                kind: "exact replay latch",
                value: "nonzero transition sequence has partial latch fields".to_string(),
            });
        }
    }
    let status = ArtifactReferenceStatus {
        run_id: parse_run_id(raw.0)?,
        event_id: parse_event_id(raw.1)?,
        artifact: stored_artifact,
        physical_state: ArtifactPhysicalState::from_db(&raw.9)?,
        physical_reason: raw
            .10
            .as_deref()
            .map(ArtifactStateReason::from_db)
            .transpose()?,
        reference_state: ArtifactReferenceState::from_db(&raw.11)?,
        reference_reason: raw
            .12
            .as_deref()
            .map(ArtifactStateReason::from_db)
            .transpose()?,
        exact_replay: ExactReplayStatus {
            state: exact_state,
            reason: raw
                .15
                .as_deref()
                .map(ArtifactStateReason::from_db)
                .transpose()?,
            exact_replay,
            transition_sequence,
            latched_at_ms: raw.17,
            latched_by_event_id,
        },
    };
    validate_artifact_status(&status)?;
    Ok(status)
}

fn register_sealed_artifact_reference_tx(
    transaction: &Transaction<'_>,
    run_id: RunId,
    event_id: EventId,
    artifact: &SealedArtifactRef,
    now: i64,
) -> LedgerResult<()> {
    let digest = artifact.envelope_digest().as_str();
    let existing = transaction
        .query_row(
            "SELECT envelope_digest FROM session_run_artifact_objects WHERE envelope_digest = ?1",
            [digest],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    if existing.is_none() {
        transaction.execute(
            "INSERT INTO session_run_artifact_objects (
                envelope_digest, key_id, schema_version, encryption_algorithm,
                canonicalization_algorithm, canonicalization_version, redactor_version,
                state, state_reason, transition_sequence, tombstoned_at_ms, retired_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'available', NULL, 0, NULL, NULL)",
            params![
                digest,
                artifact.key_id().as_str(),
                i64::from(artifact.schema_version()),
                artifact.encryption_algorithm(),
                artifact.canonicalization_algorithm(),
                i64::from(artifact.canonicalization_version()),
                artifact.redactor_version().as_str(),
            ],
        )?;
    } else {
        let (_, physical) = load_physical_artifact(transaction, digest)?;
        if physical.artifact != *artifact {
            return Err(LedgerError::ArtifactReferenceAliasConflict {
                digest: digest.to_string(),
            });
        }
        if !matches!(
            physical.state,
            ArtifactPhysicalState::Unverified | ArtifactPhysicalState::Available
        ) {
            return Err(LedgerError::ArtifactStateTransitionInvalid {
                event_id: "<physical>".to_string(),
                digest: artifact.envelope_digest().as_str().to_string(),
                from: physical.state.as_str().to_string(),
                to: "available".to_string(),
            });
        }
    }
    let existing_ref = transaction
        .query_row(
            "SELECT run_id, state, state_reason
             FROM session_run_artifact_refs
             WHERE event_id = ?1 AND envelope_digest = ?2",
            params![event_id.to_string(), digest],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            },
        )
        .optional()?;
    if let Some((stored_run_id, _, _)) = existing_ref {
        if stored_run_id != run_id.to_string() {
            return Err(LedgerError::IntegrityViolation(
                "artifact event reference is bound to a different run".to_string(),
            ));
        }
        return Ok(());
    }
    transaction.execute(
        "INSERT INTO session_run_artifact_refs (
            event_id, run_id, envelope_digest, state, state_reason,
            transition_sequence, state_changed_at_ms
         ) VALUES (?1, ?2, ?3, 'available', NULL, 0, ?4)",
        params![event_id.to_string(), run_id.to_string(), digest, now],
    )?;
    Ok(())
}

fn mark_sealed_artifact_verified_tx(
    transaction: &Transaction<'_>,
    artifact: &SealedArtifactRef,
    now: i64,
) -> LedgerResult<Vec<ArtifactStateTransition>> {
    let (_, physical) = load_physical_artifact(transaction, artifact.envelope_digest().as_str())?;
    if physical.artifact != *artifact {
        return Err(LedgerError::ArtifactReferenceAliasConflict {
            digest: artifact.envelope_digest().as_str().to_string(),
        });
    }
    match physical.state {
        ArtifactPhysicalState::Unverified => {
            set_physical_artifact_state_tx(
                transaction,
                artifact,
                ArtifactPhysicalState::Available,
                None,
                now,
            )?;
        }
        ArtifactPhysicalState::Available => {}
        state => {
            return Err(LedgerError::ArtifactStateTransitionInvalid {
                event_id: "<physical>".to_string(),
                digest: artifact.envelope_digest().as_str().to_string(),
                from: state.as_str().to_string(),
                to: ArtifactPhysicalState::Available.as_str().to_string(),
            });
        }
    }
    let references = artifact_reference_keys_with_state_tx(
        transaction,
        artifact.envelope_digest().as_str(),
        ArtifactReferenceState::Unverified,
    )?;
    references
        .into_iter()
        .map(|(run_id, event_id)| {
            transition_artifact_reference_tx(
                transaction,
                run_id,
                event_id,
                artifact,
                ArtifactReferenceState::Available,
                None,
                now,
            )
        })
        .collect()
}

fn mark_sealed_artifact_unavailable_tx(
    transaction: &Transaction<'_>,
    artifact: &SealedArtifactRef,
    reason: ArtifactStateReason,
    now: i64,
) -> LedgerResult<Vec<ArtifactStateTransition>> {
    let to_physical = physical_state_for_reason(reason)?;
    let (_, physical) = load_physical_artifact(transaction, artifact.envelope_digest().as_str())?;
    if physical.artifact != *artifact {
        return Err(LedgerError::ArtifactReferenceAliasConflict {
            digest: artifact.envelope_digest().as_str().to_string(),
        });
    }
    if physical.state != to_physical {
        set_physical_artifact_state_tx(transaction, artifact, to_physical, Some(reason), now)?;
    } else if physical.reason != Some(reason) {
        return Err(LedgerError::ArtifactStateTransitionInvalid {
            event_id: "<physical>".to_string(),
            digest: artifact.envelope_digest().as_str().to_string(),
            from: physical.state.as_str().to_string(),
            to: to_physical.as_str().to_string(),
        });
    }
    let target = reference_state_for_reason(reason)?;
    let references =
        artifact_live_reference_keys_tx(transaction, artifact.envelope_digest().as_str())?;
    references
        .into_iter()
        .map(|(run_id, event_id)| {
            transition_artifact_reference_tx(
                transaction,
                run_id,
                event_id,
                artifact,
                target,
                Some(reason),
                now,
            )
        })
        .collect()
}

fn begin_exact_artifact_retirement_tx(
    transaction: &Transaction<'_>,
    run_id: RunId,
    event_id: EventId,
    artifact: &SealedArtifactRef,
    now: i64,
) -> LedgerResult<ArtifactStateTransition> {
    transition_artifact_reference_tx(
        transaction,
        run_id,
        event_id,
        artifact,
        ArtifactReferenceState::Retiring,
        Some(ArtifactStateReason::RetentionDeleted),
        now,
    )
}

fn finish_exact_artifact_retirement_tx(
    transaction: &Transaction<'_>,
    run_id: RunId,
    event_id: EventId,
    artifact: &SealedArtifactRef,
    now: i64,
) -> LedgerResult<ArtifactStateTransition> {
    let transition = transition_artifact_reference_tx(
        transaction,
        run_id,
        event_id,
        artifact,
        ArtifactReferenceState::Retired,
        Some(ArtifactStateReason::RetentionDeleted),
        now,
    )?;
    if protected_reference_count_tx(transaction, artifact.envelope_digest().as_str())? == 0 {
        let (_, physical) =
            load_physical_artifact(transaction, artifact.envelope_digest().as_str())?;
        if !matches!(
            physical.state,
            ArtifactPhysicalState::Retiring | ArtifactPhysicalState::Retired
        ) {
            set_physical_artifact_state_tx(
                transaction,
                artifact,
                ArtifactPhysicalState::Retiring,
                Some(ArtifactStateReason::RetentionDeleted),
                now,
            )?;
        }
    }
    Ok(transition)
}

fn observe_exact_artifact_unlinked_tx(
    transaction: &Transaction<'_>,
    artifact: &SealedArtifactRef,
    now: i64,
) -> LedgerResult<ArtifactPhysicalState> {
    if protected_reference_count_tx(transaction, artifact.envelope_digest().as_str())? != 0 {
        return Err(LedgerError::IntegrityViolation(
            "cannot retire a physical artifact while a logical reference remains protected"
                .to_string(),
        ));
    }
    let (_, physical) = load_physical_artifact(transaction, artifact.envelope_digest().as_str())?;
    if physical.artifact != *artifact {
        return Err(LedgerError::ArtifactReferenceAliasConflict {
            digest: artifact.envelope_digest().as_str().to_string(),
        });
    }
    match physical.state {
        ArtifactPhysicalState::Retiring => {
            set_physical_artifact_state_tx(
                transaction,
                artifact,
                ArtifactPhysicalState::Retired,
                Some(ArtifactStateReason::RetentionDeleted),
                now,
            )?;
            Ok(ArtifactPhysicalState::Retired)
        }
        ArtifactPhysicalState::Retired => Ok(ArtifactPhysicalState::Retired),
        state => Err(LedgerError::ArtifactStateTransitionInvalid {
            event_id: "<physical>".to_string(),
            digest: artifact.envelope_digest().as_str().to_string(),
            from: state.as_str().to_string(),
            to: ArtifactPhysicalState::Retired.as_str().to_string(),
        }),
    }
}

fn transition_artifact_reference_tx(
    transaction: &Transaction<'_>,
    run_id: RunId,
    event_id: EventId,
    artifact: &SealedArtifactRef,
    to_state: ArtifactReferenceState,
    reason: Option<ArtifactStateReason>,
    now: i64,
) -> LedgerResult<ArtifactStateTransition> {
    validate_state_reason(to_state, reason)?;
    let status = load_artifact_reference_status(transaction, run_id, event_id, artifact)?;
    if status.reference_state == to_state {
        if status.reference_reason != reason {
            return Err(LedgerError::ArtifactStateTransitionInvalid {
                event_id: event_id.to_string(),
                digest: artifact.envelope_digest().as_str().to_string(),
                from: status.reference_state.as_str().to_string(),
                to: to_state.as_str().to_string(),
            });
        }
        return load_existing_artifact_transition(
            transaction,
            run_id,
            event_id,
            artifact,
            to_state,
            reason,
        )?
        .ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "artifact state transition retry lacks its immutable audit record".to_string(),
            )
        });
    }
    if !artifact_reference_transition_allowed(status.reference_state, to_state) {
        return Err(LedgerError::ArtifactStateTransitionInvalid {
            event_id: event_id.to_string(),
            digest: artifact.envelope_digest().as_str().to_string(),
            from: status.reference_state.as_str().to_string(),
            to: to_state.as_str().to_string(),
        });
    }
    let current_sequence = transaction
        .query_row(
            "SELECT transition_sequence FROM session_run_artifact_refs
             WHERE run_id = ?1 AND event_id = ?2 AND envelope_digest = ?3",
            params![
                run_id.to_string(),
                event_id.to_string(),
                artifact.envelope_digest().as_str(),
            ],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .ok_or_else(|| LedgerError::ArtifactReferenceNotFound {
            run_id: run_id.to_string(),
            event_id: event_id.to_string(),
            digest: artifact.envelope_digest().as_str().to_string(),
        })?;
    let current_sequence =
        parse_nonnegative_u64("artifact reference transition sequence", current_sequence)?;
    let next_sequence = current_sequence
        .checked_add(1)
        .ok_or(LedgerError::NumericOverflow)?;
    let changed = transaction.execute(
        "UPDATE session_run_artifact_refs
         SET state = ?1, state_reason = ?2, transition_sequence = ?3, state_changed_at_ms = ?4
         WHERE run_id = ?5 AND event_id = ?6 AND envelope_digest = ?7
           AND state = ?8 AND state_reason IS ?9 AND transition_sequence = ?10",
        params![
            to_state.as_str(),
            reason.map(ArtifactStateReason::as_str),
            i64::try_from(next_sequence).map_err(|_| LedgerError::NumericOverflow)?,
            now,
            run_id.to_string(),
            event_id.to_string(),
            artifact.envelope_digest().as_str(),
            status.reference_state.as_str(),
            status.reference_reason.map(ArtifactStateReason::as_str),
            i64::try_from(current_sequence).map_err(|_| LedgerError::NumericOverflow)?,
        ],
    )?;
    if changed != 1 {
        return Err(LedgerError::ArtifactStateTransitionInvalid {
            event_id: event_id.to_string(),
            digest: artifact.envelope_digest().as_str().to_string(),
            from: status.reference_state.as_str().to_string(),
            to: to_state.as_str().to_string(),
        });
    }
    let next_exact = derive_next_exact_replay_state_tx(transaction, run_id)?;
    let audit_event_id = EventId::new();
    let payload = artifact_state_audit_payload(
        audit_event_id,
        event_id,
        artifact,
        status.reference_state,
        to_state,
        reason,
        next_sequence,
        next_exact,
    )?;
    append_event_tx(
        transaction,
        &RunLedgerConfig::default(),
        EventDraft {
            event_id: audit_event_id,
            run_id,
            kind: SessionRunEventKind::ArtifactStateAudit,
            append_key: None,
            effect_intent: EffectIntent::None,
            artifact_degradation_reason: None,
            identity: None,
            sealed_artifact_ref: None,
            run_provenance: None,
            public_payload: payload,
            created_at_ms: now,
        },
    )?;
    update_exact_replay_latch_tx(transaction, run_id, next_exact, now, audit_event_id)?;
    transaction.execute(
        "INSERT INTO session_run_artifact_transitions (
            event_id, envelope_digest, run_id, transition_sequence, from_state, to_state,
            state_reason, audit_event_id, created_at_ms
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            event_id.to_string(),
            artifact.envelope_digest().as_str(),
            run_id.to_string(),
            i64::try_from(next_sequence).map_err(|_| LedgerError::NumericOverflow)?,
            status.reference_state.as_str(),
            to_state.as_str(),
            reason.map(ArtifactStateReason::as_str),
            audit_event_id.to_string(),
            now,
        ],
    )?;
    Ok(ArtifactStateTransition {
        run_id,
        event_id,
        artifact: artifact.clone(),
        transition_sequence: next_sequence,
        from_state: status.reference_state,
        to_state,
        reason,
        audit_event_id,
        created_at_ms: now,
    })
}
fn artifact_reference_keys_with_state_tx(
    transaction: &Transaction<'_>,
    digest: &str,
    state: ArtifactReferenceState,
) -> LedgerResult<Vec<(RunId, EventId)>> {
    let mut statement = transaction.prepare(
        "SELECT run_id, event_id
         FROM session_run_artifact_refs
         WHERE envelope_digest = ?1 AND state = ?2
         ORDER BY run_id ASC, event_id ASC",
    )?;
    let raw = statement
        .query_map(params![digest, state.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    raw.into_iter()
        .map(|(run_id, event_id)| Ok((parse_run_id(run_id)?, parse_event_id(event_id)?)))
        .collect()
}

fn artifact_live_reference_keys_tx(
    transaction: &Transaction<'_>,
    digest: &str,
) -> LedgerResult<Vec<(RunId, EventId)>> {
    let mut statement = transaction.prepare(
        "SELECT run_id, event_id
         FROM session_run_artifact_refs
         WHERE envelope_digest = ?1 AND state IN ('unverified', 'available')
         ORDER BY run_id ASC, event_id ASC",
    )?;
    let raw = statement
        .query_map([digest], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    raw.into_iter()
        .map(|(run_id, event_id)| Ok((parse_run_id(run_id)?, parse_event_id(event_id)?)))
        .collect()
}

fn protected_reference_count_tx(transaction: &Transaction<'_>, digest: &str) -> LedgerResult<u64> {
    let count: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM session_run_artifact_refs
         WHERE envelope_digest = ?1 AND state IN ('unverified', 'available', 'retiring')",
        [digest],
        |row| row.get(0),
    )?;
    parse_nonnegative_u64("protected artifact reference count", count)
}

fn load_existing_artifact_transition(
    transaction: &Transaction<'_>,
    run_id: RunId,
    event_id: EventId,
    artifact: &SealedArtifactRef,
    to_state: ArtifactReferenceState,
    reason: Option<ArtifactStateReason>,
) -> LedgerResult<Option<ArtifactStateTransition>> {
    let raw = transaction
        .query_row(
            "SELECT transition_sequence, from_state, to_state, state_reason, audit_event_id,
                    created_at_ms
             FROM session_run_artifact_transitions
             WHERE run_id = ?1 AND event_id = ?2 AND envelope_digest = ?3
               AND to_state = ?4 AND state_reason IS ?5
             ORDER BY transition_sequence DESC
             LIMIT 1",
            params![
                run_id.to_string(),
                event_id.to_string(),
                artifact.envelope_digest().as_str(),
                to_state.as_str(),
                reason.map(ArtifactStateReason::as_str),
            ],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()?;
    raw.map(|raw| {
        let stored_to = ArtifactReferenceState::from_db(&raw.2)?;
        let stored_reason = raw
            .3
            .as_deref()
            .map(ArtifactStateReason::from_db)
            .transpose()?;
        if stored_to != to_state || stored_reason != reason {
            return Err(LedgerError::IntegrityViolation(
                "artifact transition idempotency lookup returned incompatible state".to_string(),
            ));
        }
        Ok(ArtifactStateTransition {
            run_id,
            event_id,
            artifact: artifact.clone(),
            transition_sequence: parse_nonnegative_u64(
                "artifact reference transition sequence",
                raw.0,
            )?,
            from_state: ArtifactReferenceState::from_db(&raw.1)?,
            to_state: stored_to,
            reason: stored_reason,
            audit_event_id: parse_event_id(raw.4)?,
            created_at_ms: raw.5,
        })
    })
    .transpose()
}

fn set_physical_artifact_state_tx(
    transaction: &Transaction<'_>,
    artifact: &SealedArtifactRef,
    to_state: ArtifactPhysicalState,
    reason: Option<ArtifactStateReason>,
    now: i64,
) -> LedgerResult<ArtifactPhysicalState> {
    validate_physical_state_reason(to_state, reason)?;
    let (_, current) = load_physical_artifact(transaction, artifact.envelope_digest().as_str())?;
    if current.artifact != *artifact {
        return Err(LedgerError::ArtifactReferenceAliasConflict {
            digest: artifact.envelope_digest().as_str().to_string(),
        });
    }
    if current.state == to_state {
        if current.reason != reason {
            return Err(LedgerError::ArtifactStateTransitionInvalid {
                event_id: "<physical>".to_string(),
                digest: artifact.envelope_digest().as_str().to_string(),
                from: current.state.as_str().to_string(),
                to: to_state.as_str().to_string(),
            });
        }
        return Ok(to_state);
    }
    if !artifact_physical_transition_allowed(current.state, to_state) {
        return Err(LedgerError::ArtifactStateTransitionInvalid {
            event_id: "<physical>".to_string(),
            digest: artifact.envelope_digest().as_str().to_string(),
            from: current.state.as_str().to_string(),
            to: to_state.as_str().to_string(),
        });
    }
    let next_sequence = current
        .transition_sequence
        .checked_add(1)
        .ok_or(LedgerError::NumericOverflow)?;
    let tombstoned_at_ms = match to_state {
        ArtifactPhysicalState::Retiring => Some(now),
        ArtifactPhysicalState::Retired => current.tombstoned_at_ms,
        _ => None,
    };
    let retired_at_ms = match to_state {
        ArtifactPhysicalState::Retired => Some(now),
        _ => None,
    };
    let changed = transaction.execute(
        "UPDATE session_run_artifact_objects
         SET state = ?1, state_reason = ?2, transition_sequence = ?3,
             tombstoned_at_ms = ?4, retired_at_ms = ?5
         WHERE envelope_digest = ?6 AND state = ?7 AND state_reason IS ?8
           AND transition_sequence = ?9",
        params![
            to_state.as_str(),
            reason.map(ArtifactStateReason::as_str),
            i64::try_from(next_sequence).map_err(|_| LedgerError::NumericOverflow)?,
            tombstoned_at_ms,
            retired_at_ms,
            artifact.envelope_digest().as_str(),
            current.state.as_str(),
            current.reason.map(ArtifactStateReason::as_str),
            i64::try_from(current.transition_sequence).map_err(|_| LedgerError::NumericOverflow)?,
        ],
    )?;
    if changed != 1 {
        return Err(LedgerError::ArtifactStateTransitionInvalid {
            event_id: "<physical>".to_string(),
            digest: artifact.envelope_digest().as_str().to_string(),
            from: current.state.as_str().to_string(),
            to: to_state.as_str().to_string(),
        });
    }
    Ok(to_state)
}

fn load_exact_replay_status_tx(
    transaction: &Transaction<'_>,
    run_id: RunId,
) -> LedgerResult<ExactReplayStatus> {
    let raw = transaction
        .query_row(
            "SELECT exact_replay, state, state_reason, transition_sequence, latched_at_ms,
                    latched_by_event_id
             FROM session_run_exact_replay WHERE run_id = ?1",
            [run_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                ))
            },
        )
        .optional()?
        .ok_or(LedgerError::RunNotFound {
            run_id: run_id.to_string(),
        })?;
    let state = ExactReplayState::from_db(&raw.1)?;
    let exact_replay = parse_sql_bool(raw.0, "exact replay")?;
    let reason = raw
        .2
        .as_deref()
        .map(ArtifactStateReason::from_db)
        .transpose()?;
    validate_exact_replay_state_reason(state, reason)?;
    if exact_replay != state.exact_replay() {
        return Err(LedgerError::InvalidStoredState {
            kind: "exact replay latch",
            value: raw.1,
        });
    }
    let transition_sequence =
        parse_nonnegative_u64("exact replay transition sequence", raw.3)?;
    let latched_by_event_id = raw.5.map(parse_event_id).transpose()?;
    match (transition_sequence, raw.4, latched_by_event_id) {
        (0, None, None) => {}
        (0, _, _) => {
            return Err(LedgerError::InvalidStoredState {
                kind: "exact replay latch",
                value: "zero transition sequence has latch fields".to_string(),
            });
        }
        (_, Some(_), Some(event_id)) => {
            let event_run_id: Option<String> = transaction
                .query_row(
                    "SELECT run_id FROM session_run_events WHERE event_id = ?1",
                    [event_id.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            let expected_run_id = run_id.to_string();
            if event_run_id.as_deref() != Some(expected_run_id.as_str()) {
                return Err(LedgerError::InvalidStoredState {
                    kind: "exact replay latch",
                    value: "latched event belongs to another run or is absent".to_string(),
                });
            }
        }
        (_, _, _) => {
            return Err(LedgerError::InvalidStoredState {
                kind: "exact replay latch",
                value: "nonzero transition sequence has partial latch fields".to_string(),
            });
        }
    }
    Ok(ExactReplayStatus {
        state,
        reason,
        exact_replay,
        transition_sequence,
        latched_at_ms: raw.4,
        latched_by_event_id,
    })
}

fn derive_next_exact_replay_state_tx(
    transaction: &Transaction<'_>,
    run_id: RunId,
) -> LedgerResult<ExactReplayState> {
    let current = load_exact_replay_status_tx(transaction, run_id)?;
    if !matches!(
        current.state,
        ExactReplayState::Unverified | ExactReplayState::Available
    ) {
        return Ok(current.state);
    }
    let degraded_fidelity: Option<String> = transaction
        .query_row(
            "SELECT replay_fidelity FROM session_run_replay_obligations
             WHERE run_id = ?1 AND replay_fidelity IN ('metadata_only', 'unavailable')
             ORDER BY source_event_id ASC, operation_kind ASC, phase ASC LIMIT 1",
            [run_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if degraded_fidelity.is_some() {
        return Ok(ExactReplayState::UnavailableSanitized);
    }
    let pending: Option<i64> = transaction
        .query_row(
            "SELECT 1 FROM session_run_replay_obligations
             WHERE run_id = ?1 AND replay_fidelity = 'pending' LIMIT 1",
            [run_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if pending.is_some() {
        return Ok(ExactReplayState::Unverified);
    }
    let state = transaction
        .query_row(
            "SELECT state FROM session_run_artifact_refs
             WHERE run_id = ?1 AND state <> 'available'
             ORDER BY CASE state
                WHEN 'retired' THEN 0
                WHEN 'retiring' THEN 1
                WHEN 'unavailable_missing' THEN 2
                WHEN 'unavailable_corrupt' THEN 3
                WHEN 'unavailable_key_revoked' THEN 4
                WHEN 'unverified' THEN 5
                ELSE 6
             END
             LIMIT 1",
            [run_id.to_string()],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    match state {
        Some(state) => ExactReplayState::from_db(&state),
        None => Ok(ExactReplayState::Available),
    }
}

fn update_exact_replay_latch_tx(
    transaction: &Transaction<'_>,
    run_id: RunId,
    to_state: ExactReplayState,
    now: i64,
    audit_event_id: EventId,
) -> LedgerResult<ExactReplayStatus> {
    let current = load_exact_replay_status_tx(transaction, run_id)?;
    if current.state == to_state {
        return Ok(current);
    }
    if !exact_replay_transition_allowed(current.state, to_state) {
        return Err(LedgerError::ArtifactStateTransitionInvalid {
            event_id: format!("run:{run_id}"),
            digest: "<exact-replay>".to_string(),
            from: current.state.as_str().to_string(),
            to: to_state.as_str().to_string(),
        });
    }
    let next_sequence = current
        .transition_sequence
        .checked_add(1)
        .ok_or(LedgerError::NumericOverflow)?;
    let reason = exact_replay_reason(to_state);
    let changed = transaction.execute(
        "UPDATE session_run_exact_replay
         SET exact_replay = ?1, state = ?2, state_reason = ?3, transition_sequence = ?4,
             latched_at_ms = ?5, latched_by_event_id = ?6
         WHERE run_id = ?7 AND state = ?8 AND transition_sequence = ?9",
        params![
            i64::from(to_state.exact_replay()),
            to_state.as_str(),
            reason.map(ArtifactStateReason::as_str),
            i64::try_from(next_sequence).map_err(|_| LedgerError::NumericOverflow)?,
            now,
            audit_event_id.to_string(),
            run_id.to_string(),
            current.state.as_str(),
            i64::try_from(current.transition_sequence).map_err(|_| LedgerError::NumericOverflow)?,
        ],
    )?;
    if changed != 1 {
        return Err(LedgerError::ArtifactStateTransitionInvalid {
            event_id: format!("run:{run_id}"),
            digest: "<exact-replay>".to_string(),
            from: current.state.as_str().to_string(),
            to: to_state.as_str().to_string(),
        });
    }
    Ok(ExactReplayStatus {
        state: to_state,
        reason,
        exact_replay: to_state.exact_replay(),
        transition_sequence: next_sequence,
        latched_at_ms: Some(now),
        latched_by_event_id: Some(audit_event_id),
    })
}
fn physical_state_for_reason(reason: ArtifactStateReason) -> LedgerResult<ArtifactPhysicalState> {
    match reason {
        ArtifactStateReason::Missing => Ok(ArtifactPhysicalState::UnavailableMissing),
        ArtifactStateReason::Corrupt => Ok(ArtifactPhysicalState::UnavailableCorrupt),
        ArtifactStateReason::KeyRevoked => Ok(ArtifactPhysicalState::UnavailableKeyRevoked),
        ArtifactStateReason::RetentionDeleted => Ok(ArtifactPhysicalState::Retiring),
        ArtifactStateReason::Sanitized => Err(LedgerError::IntegrityViolation(
            "sanitized replay fidelity is a run-level latch reason, not an artifact state"
                .to_string(),
        )),
    }
}

fn reference_state_for_reason(
    reason: ArtifactStateReason,
) -> LedgerResult<ArtifactReferenceState> {
    match reason {
        ArtifactStateReason::Missing => Ok(ArtifactReferenceState::UnavailableMissing),
        ArtifactStateReason::Corrupt => Ok(ArtifactReferenceState::UnavailableCorrupt),
        ArtifactStateReason::KeyRevoked => Ok(ArtifactReferenceState::UnavailableKeyRevoked),
        ArtifactStateReason::RetentionDeleted => Ok(ArtifactReferenceState::Retiring),
        ArtifactStateReason::Sanitized => Err(LedgerError::IntegrityViolation(
            "sanitized replay fidelity is a run-level latch reason, not an artifact state"
                .to_string(),
        )),
    }
}

fn exact_replay_reason(state: ExactReplayState) -> Option<ArtifactStateReason> {
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

fn expected_reference_reason(state: ArtifactReferenceState) -> Option<ArtifactStateReason> {
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

fn expected_physical_reason(state: ArtifactPhysicalState) -> Option<ArtifactStateReason> {
    match state {
        ArtifactPhysicalState::UnavailableMissing => Some(ArtifactStateReason::Missing),
        ArtifactPhysicalState::UnavailableCorrupt => Some(ArtifactStateReason::Corrupt),
        ArtifactPhysicalState::UnavailableKeyRevoked => Some(ArtifactStateReason::KeyRevoked),
        ArtifactPhysicalState::Retiring | ArtifactPhysicalState::Retired => {
            Some(ArtifactStateReason::RetentionDeleted)
        }
        ArtifactPhysicalState::Unverified | ArtifactPhysicalState::Available => None,
    }
}

fn validate_state_reason(
    state: ArtifactReferenceState,
    reason: Option<ArtifactStateReason>,
) -> LedgerResult<()> {
    if expected_reference_reason(state) != reason {
        return Err(LedgerError::IntegrityViolation(
            "artifact reference state and reason are not a closed pair".to_string(),
        ));
    }
    Ok(())
}

fn validate_physical_state_reason(
    state: ArtifactPhysicalState,
    reason: Option<ArtifactStateReason>,
) -> LedgerResult<()> {
    if expected_physical_reason(state) != reason {
        return Err(LedgerError::IntegrityViolation(
            "artifact physical state and reason are not a closed pair".to_string(),
        ));
    }
    Ok(())
}

fn validate_exact_replay_state_reason(
    state: ExactReplayState,
    reason: Option<ArtifactStateReason>,
) -> LedgerResult<()> {
    if exact_replay_reason(state) != reason {
        return Err(LedgerError::InvalidStoredState {
            kind: "exact replay state reason",
            value: format!("{} / {:?}", state.as_str(), reason),
        });
    }
    Ok(())
}

fn validate_physical_state_record(record: &PhysicalArtifactRecord) -> LedgerResult<()> {
    validate_physical_state_reason(record.state, record.reason)?;
    let valid_times = match record.state {
        ArtifactPhysicalState::Unverified
        | ArtifactPhysicalState::Available
        | ArtifactPhysicalState::UnavailableMissing
        | ArtifactPhysicalState::UnavailableCorrupt
        | ArtifactPhysicalState::UnavailableKeyRevoked => {
            record.tombstoned_at_ms.is_none() && record.retired_at_ms.is_none()
        }
        ArtifactPhysicalState::Retiring => {
            record.tombstoned_at_ms.is_some() && record.retired_at_ms.is_none()
        }
        ArtifactPhysicalState::Retired => {
            record.tombstoned_at_ms.is_some() && record.retired_at_ms.is_some()
        }
    };
    if !valid_times {
        return Err(LedgerError::InvalidStoredState {
            kind: "artifact physical lifecycle timestamps",
            value: record.state.as_str().to_string(),
        });
    }
    Ok(())
}

fn validate_artifact_status(status: &ArtifactReferenceStatus) -> LedgerResult<()> {
    validate_physical_state_reason(status.physical_state, status.physical_reason)?;
    validate_state_reason(status.reference_state, status.reference_reason)?;
    validate_exact_replay_state_reason(status.exact_replay.state, status.exact_replay.reason)?;
    if status.exact_replay.exact_replay != status.exact_replay.state.exact_replay() {
        return Err(LedgerError::InvalidStoredState {
            kind: "exact replay latch",
            value: status.exact_replay.state.as_str().to_string(),
        });
    }
    Ok(())
}

fn artifact_reference_transition_allowed(
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
            matches!(to, ArtifactReferenceState::Retiring)
        }
        ArtifactReferenceState::Retiring => matches!(to, ArtifactReferenceState::Retired),
        ArtifactReferenceState::Retired => false,
    }
}

fn artifact_physical_transition_allowed(
    from: ArtifactPhysicalState,
    to: ArtifactPhysicalState,
) -> bool {
    match from {
        ArtifactPhysicalState::Unverified => matches!(
            to,
            ArtifactPhysicalState::Available
                | ArtifactPhysicalState::UnavailableMissing
                | ArtifactPhysicalState::UnavailableCorrupt
                | ArtifactPhysicalState::UnavailableKeyRevoked
                | ArtifactPhysicalState::Retiring
        ),
        ArtifactPhysicalState::Available => matches!(
            to,
            ArtifactPhysicalState::UnavailableMissing
                | ArtifactPhysicalState::UnavailableCorrupt
                | ArtifactPhysicalState::UnavailableKeyRevoked
                | ArtifactPhysicalState::Retiring
        ),
        ArtifactPhysicalState::UnavailableMissing
        | ArtifactPhysicalState::UnavailableCorrupt
        | ArtifactPhysicalState::UnavailableKeyRevoked => {
            matches!(to, ArtifactPhysicalState::Retiring)
        }
        ArtifactPhysicalState::Retiring => matches!(to, ArtifactPhysicalState::Retired),
        ArtifactPhysicalState::Retired => false,
    }
}

fn exact_replay_transition_allowed(from: ExactReplayState, to: ExactReplayState) -> bool {
    match from {
        ExactReplayState::Unverified => !matches!(to, ExactReplayState::Unverified),
        ExactReplayState::Available => !matches!(to, ExactReplayState::Available),
        ExactReplayState::Retiring => matches!(to, ExactReplayState::Retired),
        ExactReplayState::UnavailableMissing
        | ExactReplayState::UnavailableCorrupt
        | ExactReplayState::UnavailableKeyRevoked
        | ExactReplayState::UnavailableSanitized
        | ExactReplayState::Retired => false,
    }
}
fn artifact_state_audit_payload(
    audit_event_id: EventId,
    affected_event_id: EventId,
    artifact: &SealedArtifactRef,
    from_state: ArtifactReferenceState,
    to_state: ArtifactReferenceState,
    reason: Option<ArtifactStateReason>,
    transition_sequence: u64,
    exact_replay_state: ExactReplayState,
) -> LedgerResult<CanonicalJson> {
    CanonicalJson::from_value(&serde_json::json!({
        "schema": "hermes.artifact-state-audit/v1",
        "schema_version": 1,
        "audit_event_id": audit_event_id.to_string(),
        "affected_event_id": affected_event_id.to_string(),
        "artifact": {
            "envelope_digest": artifact.envelope_digest().as_str(),
            "key_id": artifact.key_id().as_str(),
            "schema_version": artifact.schema_version(),
            "encryption_algorithm": artifact.encryption_algorithm(),
            "canonicalization_algorithm": artifact.canonicalization_algorithm(),
            "canonicalization_version": artifact.canonicalization_version(),
            "redactor_version": artifact.redactor_version().as_str(),
        },
        "from_state": from_state.as_str(),
        "to_state": to_state.as_str(),
        "reason": reason.map(ArtifactStateReason::as_str),
        "transition_sequence": transition_sequence,
        "exact_replay_state": exact_replay_state.as_str(),
        "exact_replay": exact_replay_state.exact_replay(),
    }))
}

fn validate_artifact_state_audit_payload(payload: &CanonicalJson) -> LedgerResult<()> {
    let value = payload.as_value()?;
    let object = value.as_object().ok_or_else(|| {
        LedgerError::IntegrityViolation(
            "artifact state audit payload must be an object".to_string(),
        )
    })?;
    if object.get("schema").and_then(Value::as_str) != Some("hermes.artifact-state-audit/v1") {
        return Err(LedgerError::IntegrityViolation(
            "artifact state audit payload has an unknown schema".to_string(),
        ));
    }
    if object.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(LedgerError::IntegrityViolation(
            "artifact state audit payload has an unsupported schema version".to_string(),
        ));
    }
    let audit_event_id = object
        .get("audit_event_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "artifact state audit has no audit event identity".to_string(),
            )
        })?;
    parse_event_id(audit_event_id.to_string())?;
    let affected_event_id = object
        .get("affected_event_id")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "artifact state audit has no affected event identity".to_string(),
            )
        })?;
    parse_event_id(affected_event_id.to_string())?;
    let from_state = object
        .get("from_state")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            LedgerError::IntegrityViolation("artifact state audit has no source state".to_string())
        })?;
    ArtifactReferenceState::from_db(from_state)?;
    let to_state = object
        .get("to_state")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            LedgerError::IntegrityViolation("artifact state audit has no target state".to_string())
        })?;
    let to_state = ArtifactReferenceState::from_db(to_state)?;
    let reason = match object.get("reason") {
        Some(Value::String(reason)) => Some(ArtifactStateReason::from_db(reason)?),
        Some(Value::Null) | None => None,
        _ => {
            return Err(LedgerError::IntegrityViolation(
                "artifact state audit reason is malformed".to_string(),
            ));
        }
    };
    validate_state_reason(to_state, reason)?;
    let exact_state = object
        .get("exact_replay_state")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "artifact state audit has no exact replay state".to_string(),
            )
        })?;
    let exact_state = ExactReplayState::from_db(exact_state)?;
    if object.get("exact_replay").and_then(Value::as_bool) != Some(exact_state.exact_replay()) {
        return Err(LedgerError::IntegrityViolation(
            "artifact state audit exact replay flag conflicts with state".to_string(),
        ));
    }
    Ok(())
}

fn parse_sql_bool(value: i64, kind: &'static str) -> LedgerResult<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(LedgerError::InvalidStoredState {
            kind,
            value: value.to_string(),
        }),
    }
}

fn event_from_draft(
    sequence: u64,
    previous_hash: String,
    manifest_hash: &str,
    draft: &EventDraft,
) -> LedgerResult<SessionRunEvent> {
    let mut event = SessionRunEvent {
        event_id: draft.event_id,
        run_id: draft.run_id,
        sequence,
        kind: draft.kind,
        append_key: draft.append_key.clone(),
        effect_intent: draft.effect_intent,
        artifact_degradation_reason: draft.artifact_degradation_reason,
        public_identity: draft
            .identity
            .as_ref()
            .map(|identity| identity.public_identity.clone()),
        private_fingerprint: draft
            .identity
            .as_ref()
            .map(|identity| identity.private_fingerprint.clone()),
        sealed_artifact_ref: draft.sealed_artifact_ref.clone(),
        run_provenance: draft.run_provenance.clone(),
        public_payload: draft.public_payload.clone(),
        manifest_hash: manifest_hash.to_string(),
        previous_hash,
        event_hash: String::new(),
        created_at_ms: draft.created_at_ms,
    };
    event.event_hash = recompute_event_hash(&event)?;
    Ok(event)
}

pub(crate) fn recompute_event_hash(event: &SessionRunEvent) -> LedgerResult<String> {
    #[derive(Serialize)]
    struct EventHashBody<'a> {
        canonicalization_algorithm: &'static str,
        canonicalization_version: u8,
        event_id: String,
        run_id: String,
        event_sequence: u64,
        event_kind: &'static str,
        append_key: Option<&'a str>,
        effect_intent: &'static str,
        artifact_degradation_reason: Option<&'static str>,
        public_identity_jcs: Option<&'a str>,
        private_fingerprint_algorithm: Option<&'static str>,
        private_fingerprint_algorithm_version: Option<u8>,
        private_fingerprint_key_id: Option<&'a str>,
        private_fingerprint_key_version: Option<u32>,
        private_fingerprint: Option<String>,
        sealed_artifact_ref: Option<&'a SealedArtifactRef>,
        run_provenance_jcs: Option<&'a str>,
        public_payload_jcs: &'a str,
        manifest_hash: &'a str,
        previous_hash: &'a str,
        created_at_ms: i64,
    }

    let private_fingerprint_algorithm = event
        .private_fingerprint
        .as_ref()
        .map(|fingerprint| fingerprint.algorithm().as_str());
    let private_fingerprint_algorithm_version = event
        .private_fingerprint
        .as_ref()
        .map(PrivateFingerprint::algorithm_version);
    let private_fingerprint_key_id = event
        .private_fingerprint
        .as_ref()
        .map(|fingerprint| fingerprint.key_id().as_str());
    let private_fingerprint_key_version = event
        .private_fingerprint
        .as_ref()
        .map(PrivateFingerprint::key_version);
    let private_fingerprint = event
        .private_fingerprint
        .as_ref()
        .map(PrivateFingerprint::as_hex);
    let body = EventHashBody {
        canonicalization_algorithm: CANONICALIZATION_ALGORITHM,
        canonicalization_version: CANONICALIZATION_VERSION,
        event_id: event.event_id.to_string(),
        run_id: event.run_id.to_string(),
        event_sequence: event.sequence,
        event_kind: event.kind.as_str(),
        append_key: event.append_key.as_ref().map(AppendKey::as_str),
        effect_intent: event.effect_intent.as_str(),
        artifact_degradation_reason: event
            .artifact_degradation_reason
            .map(ArtifactDegradationReason::as_str),
        public_identity_jcs: event.public_identity.as_ref().map(CanonicalJson::as_str),
        private_fingerprint_algorithm,
        private_fingerprint_algorithm_version,
        private_fingerprint_key_id,
        private_fingerprint_key_version,
        private_fingerprint,
        sealed_artifact_ref: event.sealed_artifact_ref.as_ref(),
        run_provenance_jcs: event.run_provenance.as_ref().map(CanonicalJson::as_str),
        public_payload_jcs: event.public_payload.as_str(),
        manifest_hash: &event.manifest_hash,
        previous_hash: &event.previous_hash,
        created_at_ms: event.created_at_ms,
    };
    let canonical = CanonicalJson::from_value(&body)?;
    Ok(sha256_hex(canonical.as_str().as_bytes()))
}

pub(crate) fn genesis_hash() -> String {
    sha256_hex(CHAIN_GENESIS_DOMAIN)
}

pub(crate) fn derive_nonresumable_idempotency_key(
    event_id: EventId,
) -> LedgerResult<NonResumableIdempotencyKey> {
    let mut hasher = Sha256::new();
    hasher.update(NONRESUMABLE_EFFECT_IDEMPOTENCY_DOMAIN);
    hasher.update(event_id.as_uuid().as_bytes());
    NonResumableIdempotencyKey::from_sha256_hex(hex::encode(hasher.finalize()))
}

pub(crate) fn derive_nonresumable_claim_binding(token: &LeaseToken) -> LedgerResult<Sha256Digest> {
    let mut hasher = Sha256::new();
    hasher.update(NONRESUMABLE_CLAIM_BINDING_DOMAIN);
    hasher.update(token.as_bytes());
    Sha256Digest::new(hex::encode(hasher.finalize()))
}

pub(crate) fn derive_nonresumable_result_append_key(
    event_id: EventId,
    claim_binding: &Sha256Digest,
) -> LedgerResult<AppendKey> {
    let mut claim_binding_bytes = [0_u8; 32];
    hex::decode_to_slice(claim_binding.as_str(), &mut claim_binding_bytes).map_err(|_| {
        LedgerError::IntegrityViolation(
            "non-resumable claim binding is not a SHA-256 digest".to_string(),
        )
    })?;
    let mut hasher = Sha256::new();
    hasher.update(NONRESUMABLE_RESULT_APPEND_KEY_DOMAIN);
    hasher.update(event_id.as_uuid().as_bytes());
    hasher.update(claim_binding_bytes);
    AppendKey::new(hex::encode(hasher.finalize()))
}

fn bound_run_start_payload(
    mode: ResolvedExecutionMode,
    owner_id: &RunOwnerId,
    started_at_ms: i64,
    payload: CanonicalJson,
) -> LedgerResult<CanonicalJson> {
    CanonicalJson::from_value(&json!({
        "schema": RUN_START_BINDING_SCHEMA,
        "schema_version": RUN_EVENT_BINDING_SCHEMA_VERSION,
        "execution_mode": mode,
        "owner_id": owner_id.as_str(),
        "started_at_ms": started_at_ms,
        "payload": payload.as_value()?,
    }))
}

fn bound_run_exit_payload(
    terminal_reason: TerminalReason,
    payload: CanonicalJson,
) -> LedgerResult<CanonicalJson> {
    CanonicalJson::from_value(&json!({
        "schema": RUN_EXIT_BINDING_SCHEMA,
        "schema_version": RUN_EVENT_BINDING_SCHEMA_VERSION,
        "terminal_reason": terminal_reason.as_str(),
        "payload": payload.as_value()?,
    }))
}

fn manual_artifact_degradation_payload(
    degradation: &TerminalArtifactDegradation,
) -> LedgerResult<CanonicalJson> {
    CanonicalJson::from_value(&json!({
        "artifact_degradation_reason": degradation.reason.as_str(),
        "public_context": degradation.public_context.as_value()?,
    }))
}

fn terminal_artifact_degradation(
    source_append_key: AppendKey,
    reason: ArtifactDegradationReason,
    effect_intent: EffectIntent,
    source_public_payload: CanonicalJson,
) -> LedgerResult<TerminalArtifactDegradation> {
    let audit_key = derived_manual_degradation_audit_key(&source_append_key)?;
    let public_context = CanonicalJson::from_value(&json!({
        "schema": TERMINAL_ARTIFACT_DEGRADATION_SCHEMA,
        "schema_version": RUN_EVENT_BINDING_SCHEMA_VERSION,
        "audit_append_key": audit_key.as_str(),
        "source_append_key": source_append_key.as_str(),
        "source_effect_intent": effect_intent.as_str(),
        "source_public_payload": source_public_payload.as_value()?,
    }))?;
    Ok(TerminalArtifactDegradation {
        audit_key,
        reason,
        public_context,
    })
}

fn derived_manual_degradation_audit_key(source: &AppendKey) -> LedgerResult<AppendKey> {
    let mut hasher = Sha256::new();
    hasher.update(b"hermes.session-run.manual-degradation-key/v1");
    hasher.update([0]);
    hasher.update(source.as_str().as_bytes());
    AppendKey::new(format!(
        "manual-degradation-{}",
        hex::encode(hasher.finalize())
    ))
}

fn validate_execution_certification(
    mode: ResolvedExecutionMode,
    provenance: &RunProvenance,
    certification: &RunExecutionCertification,
) -> LedgerResult<()> {
    match (mode.mode, certification) {
        (ExecutionMode::Manual, RunExecutionCertification::ManualHost) => {
            if matches!(
                provenance.sandbox_digest(),
                ProvenanceDigest::Absent(DigestAbsence::NotApplicable)
            ) {
                Ok(())
            } else {
                Err(LedgerError::InvalidExecutionCertification {
                    reason: "manual mode must record sandbox digest as not_applicable",
                })
            }
        }
        (
            ExecutionMode::Manual,
            RunExecutionCertification::CertifiedBackendConfigured {
                mode: certified_mode,
                ..
            },
        ) => Err(LedgerError::ExecutionCertificationModeMismatch {
            expected: ExecutionMode::Manual.as_str(),
            received: certified_mode.mode.as_str(),
        }),
        (closed_mode, RunExecutionCertification::ManualHost) => {
            Err(LedgerError::MissingExecutionCertification {
                mode: closed_mode.as_str(),
            })
        }
        (
            closed_mode,
            RunExecutionCertification::CertifiedBackendConfigured {
                mode: certified_mode,
                backend_kind,
                scope,
                policy_version,
                configuration_digest,
            },
        ) => {
            if *certified_mode != mode {
                return Err(LedgerError::ExecutionCertificationModeMismatch {
                    expected: mode.mode.as_str(),
                    received: certified_mode.mode.as_str(),
                });
            }
            if *backend_kind != VerifiedIsolatedBackendKind::Docker
                || *scope != VerifiedIsolatedBackendScope::ClosedExecution
                || *policy_version != CLOSED_EXECUTION_POLICY_VERSION
            {
                return Err(LedgerError::InvalidExecutionCertification {
                    reason: "closed mode requires the current Docker closed-execution policy",
                });
            }
            match provenance.sandbox_digest() {
                ProvenanceDigest::Present(digest) if digest == configuration_digest => Ok(()),
                _ => Err(LedgerError::InvalidExecutionCertification {
                    reason: "closed mode sandbox digest must match certified backend configuration",
                }),
            }
        }
    }
}

fn ensure_mode_matches(
    connection: &rusqlite::Connection,
    run_id: RunId,
    supplied: ResolvedExecutionMode,
) -> LedgerResult<()> {
    let (mode, source): (String, String) = connection
        .query_row(
            "SELECT execution_mode, execution_mode_source FROM session_runs WHERE run_id = ?1",
            [run_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => LedgerError::RunNotFound {
                run_id: run_id.to_string(),
            },
            other => LedgerError::Sqlite(other),
        })?;
    if execution_mode_from_db(&mode, &source)? != supplied {
        return Err(LedgerError::ExecutionModeMismatch {
            run_id: run_id.to_string(),
        });
    }
    Ok(())
}

fn ensure_current_unexpired_run_owner_lease(
    connection: &rusqlite::Connection,
    lease: &RunOwnerLease,
    now: i64,
) -> LedgerResult<bool> {
    let raw: Option<(String, i64, Vec<u8>, i64, Option<String>)> = connection
        .query_row(
            "SELECT owner_id, owner_lease_generation, owner_lease_token,
                    owner_lease_expires_at_ms, exit_event_id
             FROM session_runs WHERE run_id = ?1",
            [lease.run_id.to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )
        .optional()?;
    let (owner_id, lease_generation, token, lease_expires_at_ms, exit_event_id) =
        raw.ok_or_else(|| LedgerError::RunNotFound {
            run_id: lease.run_id.to_string(),
        })?;
    if RunOwnerId::new(owner_id)? != lease.owner_id
        || parse_positive_u64("run owner lease generation", lease_generation)?
            != lease.lease_generation
        || LeaseToken::from_blob(token)? != lease.token
        || lease_expires_at_ms <= now
    {
        return Err(LedgerError::StaleRunOwnerLease {
            run_id: lease.run_id.to_string(),
        });
    }
    Ok(exit_event_id.is_none())
}

fn branch_replay_child_lineage_error(
    source: &BranchReplaySourceSnapshot,
    reason: &'static str,
) -> LedgerError {
    LedgerError::BranchReplayChildLineage {
        source_run_id: source.source_run_id().to_string(),
        reason,
    }
}

fn branch_replay_source_mismatch(
    source: &BranchReplaySourceSnapshot,
    reason: &'static str,
) -> LedgerError {
    LedgerError::BranchReplaySourceMismatch {
        source_run_id: source.source_run_id().to_string(),
        reason,
    }
}

fn validate_branch_replay_child_lineage(
    source: &BranchReplaySourceSnapshot,
    start: &RunStart,
) -> LedgerResult<()> {
    let provenance = &start.provenance;
    if start.run_id == source.source_run_id() {
        return Err(branch_replay_child_lineage_error(
            source,
            "child run id must differ from the source run id",
        ));
    }
    if provenance.parent_run_id() != Some(source.source_run_id()) {
        return Err(branch_replay_child_lineage_error(
            source,
            "child parent run id does not bind the verified source run",
        ));
    }
    if provenance.session_id() != source.source_session_id() {
        return Err(branch_replay_child_lineage_error(
            source,
            "child session id does not preserve source lineage",
        ));
    }
    if provenance.tenant_id() != source.source_tenant_id() {
        return Err(branch_replay_child_lineage_error(
            source,
            "child tenant differs from the verified source tenant",
        ));
    }
    if provenance.branch_id().as_str() == "main" {
        return Err(branch_replay_child_lineage_error(
            source,
            "branch replay child must use a non-main branch id",
        ));
    }
    if provenance.branch_id() == source.source_branch_id() {
        return Err(branch_replay_child_lineage_error(
            source,
            "branch replay child must use a fresh branch id",
        ));
    }
    Ok(())
}

fn ensure_branch_replay_source_snapshot_tx(
    transaction: &Transaction<'_>,
    source: &BranchReplaySourceSnapshot,
) -> LedgerResult<()> {
    let raw: Option<(
        String,
        String,
        String,
        Option<String>,
        i64,
        String,
        String,
        i64,
    )> = transaction
        .query_row(
            "SELECT s.session_id, s.tenant_id, s.branch_id, s.exit_event_id,
                    s.next_event_sequence, s.head_hash,
                    x.state, x.transition_sequence
             FROM session_runs AS s
             JOIN session_run_exact_replay AS x ON x.run_id = s.run_id
             WHERE s.run_id = ?1",
            [source.source_run_id().to_string()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                ))
            },
        )
        .optional()?;
    let Some((
        session_id,
        tenant_id,
        branch_id,
        exit_event_id,
        next_event_sequence,
        head_hash,
        exact_replay_state,
        exact_replay_transition_sequence,
    )) = raw
    else {
        return Err(branch_replay_source_mismatch(source, "source run no longer exists"));
    };
    if exit_event_id.is_none() {
        return Err(branch_replay_source_mismatch(
            source,
            "source run is not terminal",
        ));
    }
    if session_id != source.source_session_id().as_str()
        || tenant_id != source.source_tenant_id().as_str()
        || branch_id != source.source_branch_id().as_str()
    {
        return Err(branch_replay_source_mismatch(
            source,
            "source lineage no longer matches the verified export",
        ));
    }
    if parse_nonnegative_u64("branch replay source next-event sequence", next_event_sequence)?
        != source.source_next_event_sequence()
        || head_hash != source.source_head_hash().as_str()
        || exact_replay_state != source.source_exact_replay_state().as_str()
        || parse_nonnegative_u64(
            "branch replay source exact-replay transition sequence",
            exact_replay_transition_sequence,
        )? != source.source_exact_replay_transition_sequence()
    {
        return Err(branch_replay_source_mismatch(
            source,
            "source head or exact-replay state changed after verification",
        ));
    }

    let events = load_events_for_run(transaction, source.source_run_id())?;
    verify_event_chain_for_run(transaction, source.source_run_id(), &events)?;
    let head = events.last().ok_or_else(|| {
        branch_replay_source_mismatch(source, "source run has no committed events")
    })?;
    if head.event_id != source.source_head_event_id()
        || head.sequence != source.source_head_event_sequence()
        || head.event_hash != source.source_head_hash().as_str()
    {
        return Err(branch_replay_source_mismatch(
            source,
            "source head event no longer matches the verified export",
        ));
    }
    let boundary = events
        .iter()
        .find(|event| event.event_id == source.boundary_event_id())
        .ok_or_else(|| {
            branch_replay_source_mismatch(source, "source boundary event no longer exists")
        })?;
    if boundary.sequence != source.boundary_event_sequence()
        || boundary.event_hash != source.boundary_event_hash().as_str()
    {
        return Err(branch_replay_source_mismatch(
            source,
            "source boundary event changed after verification",
        ));
    }

    for event in events
        .iter()
        .filter(|event| event.sequence <= source.boundary_event_sequence())
    {
        let Some(artifact) = event.sealed_artifact_ref.as_ref() else {
            continue;
        };
        if !source.artifacts().iter().any(|binding| {
            binding.source_event_id() == event.event_id && binding.artifact() == artifact
        }) {
            return Err(branch_replay_source_mismatch(
                source,
                "verified source artifact set is incomplete at the replay boundary",
            ));
        }
    }
    for binding in source.artifacts() {
        let event = events
            .iter()
            .find(|event| event.event_id == binding.source_event_id())
            .ok_or_else(|| {
                branch_replay_source_mismatch(
                    source,
                    "verified source artifact event no longer exists",
                )
            })?;
        if event.sequence > source.boundary_event_sequence()
            || event.sealed_artifact_ref.as_ref() != Some(binding.artifact())
        {
            return Err(branch_replay_source_mismatch(
                source,
                "verified source artifact reference changed after verification",
            ));
        }
        let status = match load_artifact_reference_status(
            transaction,
            source.source_run_id(),
            binding.source_event_id(),
            binding.artifact(),
        ) {
            Ok(status) => status,
            Err(LedgerError::ArtifactReferenceNotFound { .. })
            | Err(LedgerError::SealedArtifactReference(_)) => {
                return Err(branch_replay_source_mismatch(
                    source,
                    "verified source artifact reference is no longer available",
                ));
            }
            Err(error) => return Err(error),
        };
        if status.physical_state != binding.physical_state()
            || status.reference_state != binding.reference_state()
        {
            return Err(branch_replay_source_mismatch(
                source,
                "source artifact transition changed after verification",
            ));
        }
    }

    let incomplete_obligation: Option<i64> = transaction
        .query_row(
            "SELECT 1
             FROM session_run_replay_obligations AS obligation
             JOIN session_run_events AS source_event
               ON source_event.event_id = obligation.source_event_id
             LEFT JOIN session_run_events AS resolved_event
               ON resolved_event.event_id = obligation.resolved_event_id
             WHERE obligation.run_id = ?1
               AND source_event.event_sequence <= ?2
               AND (
                    obligation.replay_fidelity = 'pending'
                    OR obligation.resolved_event_id IS NULL
                    OR resolved_event.event_sequence > ?2
               )
             LIMIT 1",
            params![
                source.source_run_id().to_string(),
                i64::try_from(source.boundary_event_sequence())
                    .map_err(|_| LedgerError::NumericOverflow)?,
            ],
            |row| row.get(0),
        )
        .optional()?;
    if incomplete_obligation.is_some() {
        return Err(branch_replay_source_mismatch(
            source,
            "source replay obligation crosses or remains pending at the boundary",
        ));
    }

    let pending_outbox: Option<i64> = transaction
        .query_row(
            "SELECT 1
             FROM session_run_outbox AS outbox
             JOIN session_run_events AS source_event
               ON source_event.event_id = outbox.event_id
             WHERE outbox.run_id = ?1
               AND source_event.event_sequence <= ?2
               AND outbox.state NOT IN ('acknowledged', 'dead_lettered')
             LIMIT 1",
            params![
                source.source_run_id().to_string(),
                i64::try_from(source.boundary_event_sequence())
                    .map_err(|_| LedgerError::NumericOverflow)?,
            ],
            |row| row.get(0),
        )
        .optional()?;
    if pending_outbox.is_some() {
        return Err(branch_replay_source_mismatch(
            source,
            "source outbox remains pending at the boundary",
        ));
    }
    Ok(())
}

fn ensure_run_exists(connection: &rusqlite::Connection, run_id: RunId) -> LedgerResult<()> {
    let exists: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM session_runs WHERE run_id = ?1",
            [run_id.to_string()],
            |row| row.get(0),
        )
        .optional()?;
    if exists.is_none() {
        return Err(LedgerError::RunNotFound {
            run_id: run_id.to_string(),
        });
    }
    Ok(())
}

fn ensure_appendable(connection: &rusqlite::Connection, run_id: RunId) -> LedgerResult<()> {
    let state: Option<(Option<String>, Option<String>)> = connection
        .query_row(
            "SELECT exit_event_id, final_outcome FROM session_runs WHERE run_id = ?1",
            [run_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    match state {
        None => Err(LedgerError::RunNotFound {
            run_id: run_id.to_string(),
        }),
        Some((Some(_), _)) | Some((None, Some(_))) => Err(LedgerError::RunAlreadyTerminal {
            run_id: run_id.to_string(),
        }),
        Some((None, None)) => Ok(()),
    }
}

fn ensure_active_append_lease(
    reservation: &ReservationRow,
    lease: &AppendLease,
    now: i64,
) -> LedgerResult<()> {
    ensure_append_lease_identity(reservation, lease)?;
    if reservation.committed_event_id.is_some()
        || reservation.terminal_audit_event_id.is_some()
        || reservation.lease_expires_at_ms <= now
    {
        return Err(LedgerError::StaleAppendLease {
            append_key: lease.append_key.to_string(),
        });
    }
    Ok(())
}

fn ensure_append_lease_identity(
    reservation: &ReservationRow,
    lease: &AppendLease,
) -> LedgerResult<()> {
    if reservation.append_key != lease.append_key
        || reservation.run_id != lease.run_id
        || reservation.event_id != lease.event_id
        || reservation.lease_generation != lease.lease_generation
        || reservation.token != lease.token
    {
        return Err(LedgerError::StaleAppendLease {
            append_key: lease.append_key.to_string(),
        });
    }
    Ok(())
}

fn load_run_status(connection: &rusqlite::Connection, run_id: RunId) -> LedgerResult<RunStatus> {
    let raw: Option<RawRunStatus> = connection
        .query_row(
            "SELECT s.exit_event_id, s.exit_terminal_reason, s.candidate_outcome, s.final_outcome,
                    s.required_dead_lettered, s.pending_required_delivery,
                    x.exact_replay, x.state, x.state_reason, x.transition_sequence,
                    x.latched_at_ms, x.latched_by_event_id
             FROM session_runs AS s
             JOIN session_run_exact_replay AS x ON x.run_id = s.run_id
             WHERE s.run_id = ?1",
            [run_id.to_string()],
            raw_run_status,
        )
        .optional()?;
    let raw = raw.ok_or_else(|| LedgerError::RunNotFound {
        run_id: run_id.to_string(),
    })?;
    RunStatus::try_from((run_id, raw))
}

fn load_run_header(connection: &rusqlite::Connection, run_id: RunId) -> LedgerResult<RunHeader> {
    let raw: Option<RawRunHeader> = connection
        .query_row(
            "SELECT start_event_id, execution_mode, execution_mode_source, started_at_ms,
                    owner_id, owner_lease_generation, owner_lease_expires_at_ms,
                    provenance_canonicalization_algorithm, provenance_canonicalization_version,
                    session_id, parent_run_id, branch_id, tenant_id, action_id, objective_id,
                    task_id, model_id, provider_id, config_digest,
                    prompt_digest, prompt_digest_absence, input_digest, input_digest_absence,
                    output_digest, output_digest_absence, tool_digest, tool_digest_absence,
                    skill_digest, skill_digest_absence, policy_digest, policy_digest_absence,
                    sandbox_digest, sandbox_digest_absence
             FROM session_runs WHERE run_id = ?1",
            [run_id.to_string()],
            raw_run_header,
        )
        .optional()?;
    let raw = raw.ok_or_else(|| LedgerError::RunNotFound {
        run_id: run_id.to_string(),
    })?;
    RunHeader::try_from((run_id, raw))
}

fn load_events_for_run(
    connection: &rusqlite::Connection,
    run_id: RunId,
) -> LedgerResult<Vec<SessionRunEvent>> {
    ensure_run_exists(connection, run_id)?;
    let mut statement = connection.prepare(
        "SELECT event_id, run_id, event_sequence, event_kind, append_key, effect_intent,
                artifact_degradation_reason, public_identity_jcs,
                private_fingerprint_algorithm,
                private_fingerprint_algorithm_version, private_fingerprint_key_id,
                private_fingerprint_key_version, private_fingerprint,
                sealed_artifact_envelope_digest, sealed_artifact_key_id,
                sealed_artifact_schema_version, sealed_artifact_encryption_algorithm,
                sealed_artifact_canonicalization_algorithm,
                sealed_artifact_canonicalization_version, sealed_artifact_redactor_version,
                run_provenance_jcs, public_payload_jcs, manifest_hash, previous_hash,
                event_hash, created_at_ms
         FROM session_run_events WHERE run_id = ?1 ORDER BY event_sequence ASC",
    )?;
    let rows = statement
        .query_map([run_id.to_string()], raw_event)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().map(SessionRunEvent::try_from).collect()
}

fn verify_event_chain_for_run(
    connection: &rusqlite::Connection,
    run_id: RunId,
    events: &[SessionRunEvent],
) -> LedgerResult<()> {
    let head_hash: String = connection.query_row(
        "SELECT head_hash FROM session_runs WHERE run_id = ?1",
        [run_id.to_string()],
        |row| row.get(0),
    )?;
    let mut previous_hash = genesis_hash();
    for (index, event) in events.iter().enumerate() {
        let expected_sequence = u64::try_from(index)
            .map_err(|_| LedgerError::NumericOverflow)?
            .checked_add(1)
            .ok_or(LedgerError::NumericOverflow)?;
        if event.run_id != run_id
            || event.sequence != expected_sequence
            || event.previous_hash != previous_hash
            || recompute_event_hash(event)? != event.event_hash
        {
            return Err(LedgerError::IntegrityViolation(
                "run event chain does not match its authoritative head".to_string(),
            ));
        }
        previous_hash.clone_from(&event.event_hash);
    }
    if previous_hash != head_hash {
        return Err(LedgerError::IntegrityViolation(
            "run event chain head does not match session_runs".to_string(),
        ));
    }
    Ok(())
}

fn load_manifest_snapshots_for_run(
    connection: &rusqlite::Connection,
    run_id: RunId,
) -> LedgerResult<Vec<LedgerManifestSnapshot>> {
    let mut statement = connection.prepare(
        "SELECT m.event_id, m.manifest_jcs, m.manifest_hash
         FROM session_run_manifests m
         JOIN session_run_events e ON e.event_id = m.event_id
         WHERE e.run_id = ?1
         ORDER BY e.event_sequence ASC",
    )?;
    let rows = statement
        .query_map([run_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(event_id, canonical_json, hash)| {
            Ok(LedgerManifestSnapshot {
                event_id: parse_event_id(event_id)?,
                canonical_json: CanonicalJson::from_jcs_text(canonical_json)?,
                hash: validated_digest_string("manifest hash", hash)?,
            })
        })
        .collect()
}

fn load_outbox_snapshots_for_run(
    connection: &rusqlite::Connection,
    run_id: RunId,
) -> LedgerResult<Vec<LedgerOutboxSnapshot>> {
    let mut statement = connection.prepare(
        "SELECT o.outbox_id, o.run_id, o.event_id, o.destination, o.required,
                o.idempotency_key, o.public_payload_jcs,
                o.sealed_artifact_envelope_digest, o.sealed_artifact_key_id,
                o.sealed_artifact_schema_version, o.sealed_artifact_encryption_algorithm,
                o.sealed_artifact_canonicalization_algorithm,
                o.sealed_artifact_canonicalization_version, o.sealed_artifact_redactor_version,
                o.state, o.attempt_count, o.max_attempts, o.lease_generation,
                o.delivery_receipt, o.delivery_error, o.manifest_order
         FROM session_run_outbox o
         JOIN session_run_events e ON e.event_id = o.event_id
         WHERE o.run_id = ?1
         ORDER BY e.event_sequence ASC, o.manifest_order ASC",
    )?;
    let rows = statement
        .query_map([run_id.to_string()], |row| {
            Ok((raw_outbox_record(row)?, row.get::<_, i64>(20)?))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(raw, manifest_order)| {
            Ok(LedgerOutboxSnapshot {
                record: OutboxRecord::try_from(raw)?,
                manifest_order: parse_nonnegative_u32("outbox manifest order", manifest_order)?,
            })
        })
        .collect()
}

fn load_nonresumable_effect_snapshots_for_run(
    connection: &rusqlite::Connection,
    run_id: RunId,
) -> LedgerResult<Vec<NonResumableEffectSnapshot>> {
    let event_ids = {
        let mut statement = connection.prepare(
            "SELECT n.event_id
             FROM session_run_nonresumable_effects n
             JOIN session_run_events e ON e.event_id = n.event_id
             WHERE n.run_id = ?1
             ORDER BY e.event_sequence ASC",
        )?;
        let rows = statement
            .query_map([run_id.to_string()], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    event_ids
        .into_iter()
        .map(|raw_event_id| {
            let event_id = parse_event_id(raw_event_id)?;
            let row = load_nonresumable_effect(connection, event_id)?.ok_or_else(|| {
                LedgerError::NonResumableEffectNotFound {
                    event_id: event_id.to_string(),
                }
            })?;
            let claim_binding = row
                .claim_token
                .as_ref()
                .map(derive_nonresumable_claim_binding)
                .transpose()?;
            Ok(NonResumableEffectSnapshot {
                record: row.record,
                claim_binding,
            })
        })
        .collect()
}
fn load_replay_obligations_for_run(
    connection: &Connection,
    run_id: RunId,
) -> LedgerResult<Vec<ReplayObligationSnapshot>> {
    ensure_run_exists(connection, run_id)?;
    let mut statement = connection.prepare(
        "SELECT o.source_event_id, source.event_sequence, o.operation_kind, o.phase,
                o.operation_instance_id, o.replay_fidelity, o.outcome, o.resolved_event_id,
                resolved.sealed_artifact_envelope_digest,
                resolved.sealed_artifact_key_id,
                resolved.sealed_artifact_schema_version,
                resolved.sealed_artifact_encryption_algorithm,
                resolved.sealed_artifact_canonicalization_algorithm,
                resolved.sealed_artifact_canonicalization_version,
                resolved.sealed_artifact_redactor_version
         FROM session_run_replay_obligations o
         JOIN session_run_events source ON source.event_id = o.source_event_id
         LEFT JOIN session_run_events resolved ON resolved.event_id = o.resolved_event_id
         WHERE o.run_id = ?1
         ORDER BY source.event_sequence ASC, o.operation_kind ASC, o.phase ASC,
                  o.operation_instance_id ASC",
    )?;
    let rows = statement
        .query_map([run_id.to_string()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, String>(6)?,
                row.get::<_, Option<String>>(7)?,
                row.get::<_, Option<String>>(8)?,
                row.get::<_, Option<String>>(9)?,
                row.get::<_, Option<i64>>(10)?,
                row.get::<_, Option<String>>(11)?,
                row.get::<_, Option<String>>(12)?,
                row.get::<_, Option<i64>>(13)?,
                row.get::<_, Option<String>>(14)?,
            ))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(
            |(
                source_event_id,
                source_event_sequence,
                operation,
                phase,
                operation_instance_id,
                fidelity,
                outcome,
                resolved_event_id,
                envelope_digest,
                key_id,
                schema_version,
                encryption_algorithm,
                canonicalization_algorithm,
                canonicalization_version,
                redactor_version,
            )| {
                let fidelity = ReplayObligationFidelity::from_db(&fidelity)?;
                Ok(ReplayObligationSnapshot {
                    source_event_id: parse_event_id(source_event_id)?,
                    source_event_sequence: parse_nonnegative_u64(
                        "replay obligation source event sequence",
                        source_event_sequence,
                    )?,
                    operation: ReplayOperationKind::from_db(&operation)?,
                    phase: ReplayOperationPhase::from_db(&phase)?,
                    operation_instance_id: parse_replay_operation_instance_id(operation_instance_id)?,
                    required: true,
                    fidelity,
                    outcome: ReplayOperationOutcome::from_db(&outcome)?,
                    resolved_event_id: resolved_event_id.map(parse_event_id).transpose()?,
                    artifact: parse_optional_artifact_ref(
                        envelope_digest,
                        key_id,
                        schema_version,
                        encryption_algorithm,
                        canonicalization_algorithm,
                        canonicalization_version,
                        redactor_version,
                    )?,
                    reason: match fidelity {
                        ReplayObligationFidelity::MetadataOnly
                        | ReplayObligationFidelity::Unavailable => {
                            Some(ArtifactStateReason::Sanitized)
                        }
                        ReplayObligationFidelity::Pending
                        | ReplayObligationFidelity::ExactPrivate => None,
                    },
                })
            },
        )
        .collect()
}

fn load_event_for_append_key(
    connection: &rusqlite::Connection,
    append_key: &AppendKey,
) -> LedgerResult<Option<SessionRunEvent>> {
    let raw_run_id: Option<String> = connection
        .query_row(
            "SELECT run_id FROM session_run_events WHERE append_key = ?1",
            [append_key.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    let Some(raw_run_id) = raw_run_id else {
        return Ok(None);
    };
    let run_id = parse_run_id(raw_run_id)?;
    load_events_for_run(connection, run_id)?
        .into_iter()
        .find(|event| event.append_key.as_ref() == Some(append_key))
        .map(Some)
        .ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "event append key lookup did not resolve to a run event".to_string(),
            )
        })
}

struct TerminalizedNonResumableEffects {
    has_unknown: bool,
    abandoned_count: usize,
}

fn terminalize_nonresumable_effects(
    transaction: &Transaction<'_>,
    run_id: RunId,
    now: i64,
) -> LedgerResult<TerminalizedNonResumableEffects> {
    let pending = {
        let mut statement = transaction.prepare(
            "SELECT event_id, state
             FROM session_run_nonresumable_effects
             WHERE run_id = ?1 AND state IN ('armed', 'executing')
             ORDER BY event_id ASC",
        )?;
        let rows = statement
            .query_map([run_id.to_string()], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    let mut has_unknown = false;
    let mut abandoned_count = 0_usize;
    for (raw_event_id, raw_state) in pending {
        let event_id = parse_event_id(raw_event_id)?;
        let state = NonResumableEffectState::from_db(&raw_state)?;
        if state == NonResumableEffectState::Armed {
            let changed = transaction.execute(
                "UPDATE session_run_nonresumable_effects
                 SET state = 'abandoned', terminal_at_ms = ?1
                 WHERE event_id = ?2 AND run_id = ?3 AND state = 'armed'",
                params![now, event_id.to_string(), run_id.to_string()],
            )?;
            if changed != 1 {
                return Err(LedgerError::IntegrityViolation(
                    "non-resumable armed effect changed during terminalization".to_string(),
                ));
            }
            abandoned_count = abandoned_count
                .checked_add(1)
                .ok_or(LedgerError::NumericOverflow)?;
            continue;
        }

        let row = load_nonresumable_effect(transaction, event_id)?.ok_or_else(|| {
            LedgerError::NonResumableEffectNotFound {
                event_id: event_id.to_string(),
            }
        })?;
        let token = row.claim_token.ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "executing non-resumable effect lacks a claim token".to_string(),
            )
        })?;
        let claimed_at_ms = row.record.claimed_at_ms.ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "executing non-resumable effect lacks a claim timestamp".to_string(),
            )
        })?;
        let result_payload_digest = row.record.result_payload_digest.clone();
        let claim_binding = derive_nonresumable_claim_binding(&token)?;
        let result_append_key = derive_nonresumable_result_append_key(event_id, &claim_binding)?;
        let lease = NonResumableEffectLease {
            run_id,
            event_id,
            idempotency_key: row.record.idempotency_key,
            claim_binding,
            result_append_key,
            claimed_at_ms,
            token,
        };
        let result_event_id = transaction
            .query_row(
                "SELECT event_id FROM session_run_events
                 WHERE run_id = ?1 AND append_key = ?2",
                params![run_id.to_string(), lease.result_append_key.as_str()],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(parse_event_id)
            .transpose()?;
        let (state, result_event_id) = match (result_event_id, result_payload_digest.as_ref()) {
            (Some(result_event_id), Some(result_payload_digest)) => {
                match validate_nonresumable_result_event(
                    transaction,
                    &lease,
                    result_event_id,
                    result_payload_digest,
                ) {
                    Ok(()) => ("completed", Some(result_event_id)),
                    Err(LedgerError::NonResumableEffectResultMismatch { .. }) => {
                        has_unknown = true;
                        ("unknown_non_resumable", None)
                    }
                    Err(error) => return Err(error),
                }
            }
            _ => {
                has_unknown = true;
                ("unknown_non_resumable", None)
            }
        };
        let changed = transaction.execute(
            "UPDATE session_run_nonresumable_effects
             SET state = ?1, result_event_id = ?2, terminal_at_ms = ?3
             WHERE event_id = ?4 AND run_id = ?5 AND state = 'executing'
               AND claim_token = ?6",
            params![
                state,
                result_event_id.map(|value| value.to_string()),
                now,
                event_id.to_string(),
                run_id.to_string(),
                lease.token.to_blob(),
            ],
        )?;
        if changed != 1 {
            return Err(LedgerError::IntegrityViolation(
                "executing non-resumable effect changed during terminalization".to_string(),
            ));
        }
    }
    Ok(TerminalizedNonResumableEffects {
        has_unknown,
        abandoned_count,
    })
}

fn reconcile_run_finalization(transaction: &Transaction<'_>, run_id: RunId) -> LedgerResult<()> {
    let status = load_run_status(transaction, run_id)?;
    if status.required_dead_lettered {
        transaction.execute(
            "UPDATE session_runs
             SET final_outcome = 'dead_lettered', pending_required_delivery = 0
             WHERE run_id = ?1",
            [run_id.to_string()],
        )?;
        return Ok(());
    }

    let Some(candidate_outcome) = status.candidate_outcome else {
        return Ok(());
    };
    let unresolved_required: i64 = transaction.query_row(
        "SELECT COUNT(*) FROM session_run_outbox
         WHERE run_id = ?1 AND required = 1 AND state != 'acknowledged'",
        [run_id.to_string()],
        |row| row.get(0),
    )?;
    if unresolved_required == 0 {
        transaction.execute(
            "UPDATE session_runs
             SET final_outcome = ?1, pending_required_delivery = 0
             WHERE run_id = ?2 AND required_dead_lettered = 0",
            params![candidate_outcome.as_str(), run_id.to_string()],
        )?;
    } else {
        transaction.execute(
            "UPDATE session_runs
             SET pending_required_delivery = 1
             WHERE run_id = ?1 AND final_outcome IS NULL",
            [run_id.to_string()],
        )?;
    }
    Ok(())
}

fn complete_delivery_tx(
    transaction: &Transaction<'_>,
    config: &RunLedgerConfig,
    mode: ResolvedExecutionMode,
    fence: &OutboxFence,
    completion: DeliveryCompletion,
    evidence: Option<&DeliveryEvidence>,
    now: i64,
) -> LedgerResult<DeliveryCompletionReceipt> {
    ensure_mode_matches(transaction, fence.run_id, mode)?;
    let state = match completion {
        DeliveryCompletion::Acknowledged(receipt) => {
            let changed = transaction.execute(
                "UPDATE session_run_outbox
                 SET state = 'acknowledged', lease_token = NULL, lease_expires_at_ms = NULL,
                     delivery_receipt = ?1, delivery_error = NULL, terminal_at_ms = ?2
                 WHERE outbox_id = ?3 AND state = 'leased' AND lease_generation = ?4
                   AND lease_token = ?5",
                params![
                    receipt.as_str(),
                    now,
                    fence.outbox_id.to_string(),
                    i64::try_from(fence.lease_generation)
                        .map_err(|_| LedgerError::NumericOverflow)?,
                    fence.token.to_blob(),
                ],
            )?;
            if changed != 1 {
                return Err(LedgerError::StaleOutboxLease {
                    outbox_id: fence.outbox_id.to_string(),
                });
            }
            append_delivery_result_event(
                transaction,
                config,
                fence,
                OutboxState::Acknowledged,
                None,
                evidence,
                now,
            )?;
            OutboxState::Acknowledged
        }
        DeliveryCompletion::Nacked(reason) => {
            let dead_letter = fence.attempt_count >= fence.max_attempts;
            let next_state = if dead_letter {
                OutboxState::DeadLettered
            } else {
                OutboxState::Ready
            };
            let changed = transaction.execute(
                "UPDATE session_run_outbox
                 SET state = ?1, lease_token = NULL, lease_expires_at_ms = NULL,
                     delivery_receipt = NULL, delivery_error = ?2, terminal_at_ms = ?3
                 WHERE outbox_id = ?4 AND state = 'leased' AND lease_generation = ?5
                   AND lease_token = ?6",
                params![
                    next_state.as_str(),
                    if dead_letter {
                        Some(reason.as_str())
                    } else {
                        None
                    },
                    if dead_letter { Some(now) } else { None },
                    fence.outbox_id.to_string(),
                    i64::try_from(fence.lease_generation)
                        .map_err(|_| LedgerError::NumericOverflow)?,
                    fence.token.to_blob(),
                ],
            )?;
            if changed != 1 {
                return Err(LedgerError::StaleOutboxLease {
                    outbox_id: fence.outbox_id.to_string(),
                });
            }
            append_delivery_result_event(
                transaction,
                config,
                fence,
                next_state,
                Some(reason),
                evidence,
                now,
            )?;
            if dead_letter && fence.required {
                transaction.execute(
                    "UPDATE session_runs SET required_dead_lettered = 1 WHERE run_id = ?1",
                    [fence.run_id.to_string()],
                )?;
            }
            next_state
        }
    };
    reconcile_run_finalization(transaction, fence.run_id)?;
    let run_status = load_run_status(transaction, fence.run_id)?;
    Ok(DeliveryCompletionReceipt {
        outbox_id: fence.outbox_id,
        state,
        run_status,
    })
}

fn dead_letter_expired_outbox_tx(
    transaction: &Transaction<'_>,
    config: &RunLedgerConfig,
    mode: ResolvedExecutionMode,
    fence: &OutboxFence,
    now: i64,
) -> LedgerResult<()> {
    ensure_mode_matches(transaction, fence.run_id, mode)?;
    if fence.attempt_count != fence.max_attempts {
        return Err(LedgerError::IntegrityViolation(
            "only a lease at its exact maximum attempts may terminalize on expiry".to_string(),
        ));
    }
    let changed = transaction.execute(
        "UPDATE session_run_outbox
         SET state = 'dead_lettered', lease_token = NULL, lease_expires_at_ms = NULL,
             delivery_receipt = NULL, delivery_error = 'attempts_exhausted', terminal_at_ms = ?1
         WHERE outbox_id = ?2 AND state = 'leased' AND lease_generation = ?3
           AND lease_token = ?4 AND lease_expires_at_ms <= ?5
           AND attempt_count = max_attempts",
        params![
            now,
            fence.outbox_id.to_string(),
            i64::try_from(fence.lease_generation).map_err(|_| LedgerError::NumericOverflow)?,
            fence.token.to_blob(),
            now,
        ],
    )?;
    if changed != 1 {
        return Err(LedgerError::StaleOutboxLease {
            outbox_id: fence.outbox_id.to_string(),
        });
    }
    append_delivery_result_event(
        transaction,
        config,
        fence,
        OutboxState::DeadLettered,
        Some(DeliveryFailureReason::AttemptsExhausted),
        None,
        now,
    )?;
    if fence.required {
        transaction.execute(
            "UPDATE session_runs SET required_dead_lettered = 1 WHERE run_id = ?1",
            [fence.run_id.to_string()],
        )?;
    }
    reconcile_run_finalization(transaction, fence.run_id)
}

fn append_delivery_result_event(
    transaction: &Transaction<'_>,
    config: &RunLedgerConfig,
    fence: &OutboxFence,
    state: OutboxState,
    error: Option<DeliveryFailureReason>,
    evidence: Option<&DeliveryEvidence>,
    now: i64,
) -> LedgerResult<()> {
    let payload = CanonicalJson::from_value(&json!({
        "attempt_count": fence.attempt_count,
        "delivery_error": error.map(DeliveryFailureReason::as_str),
        "max_attempts": fence.max_attempts,
        "outbox_id": fence.outbox_id.to_string(),
        "schema": DELIVERY_RESULT_BINDING_SCHEMA,
        "state": state.as_str(),
    }))?;
    let committed = append_event_tx(
        transaction,
        config,
        EventDraft {
            event_id: EventId::new(),
            run_id: fence.run_id,
            kind: SessionRunEventKind::DeliveryResult,
            append_key: None,
            effect_intent: EffectIntent::None,
            artifact_degradation_reason: None,
            identity: evidence.map(|value| value.identity.clone()),
            sealed_artifact_ref: evidence.map(|value| value.sealed_artifact_ref.clone()),
            run_provenance: None,
            public_payload: payload,
            created_at_ms: now,
        },
    )?;
    let fidelity = match evidence.map(|value| value.replay_fidelity) {
        Some(ReplayArtifactFidelity::ExactPrivate) => ReplayObligationFidelity::ExactPrivate,
        Some(ReplayArtifactFidelity::MetadataOnly) | None => ReplayObligationFidelity::Unavailable,
    };
    let outcome = if state == OutboxState::Acknowledged {
        ReplayOperationOutcome::Returned
    } else {
        ReplayOperationOutcome::HandlerError
    };
    transaction.execute(
        "INSERT INTO session_run_replay_obligations (
            run_id, source_event_id, operation_kind, phase, operation_instance_id,
            replay_fidelity, outcome, resolved_event_id, created_at_ms, resolved_at_ms
         ) VALUES (?1, ?2, 'delivery', 'result', ?3, ?4, ?5, ?2, ?6, ?6)",
        params![
            fence.run_id.to_string(),
            committed.event_id.to_string(),
            fence.outbox_id.to_string(),
            fidelity.as_str(),
            outcome.as_str(),
            now,
        ],
    )?;
    let next_state = derive_next_exact_replay_state_tx(transaction, fence.run_id)?;
    update_exact_replay_latch_tx(transaction, fence.run_id, next_state, now, committed.event_id)?;
    Ok(())
}

fn append_receipt_for_key(
    connection: &rusqlite::Connection,
    append_key: &AppendKey,
) -> LedgerResult<AppendReceipt> {
    try_append_receipt_for_key(connection, append_key)?.ok_or_else(|| {
        LedgerError::AppendReservationNotFound {
            append_key: append_key.to_string(),
        }
    })
}

fn try_append_receipt_for_key(
    connection: &rusqlite::Connection,
    append_key: &AppendKey,
) -> LedgerResult<Option<AppendReceipt>> {
    let raw = connection
        .query_row(
            "SELECT event_id, run_id, event_sequence, event_hash,
                    sealed_artifact_envelope_digest, sealed_artifact_key_id,
                    sealed_artifact_schema_version, sealed_artifact_encryption_algorithm,
                    sealed_artifact_canonicalization_algorithm,
                    sealed_artifact_canonicalization_version, sealed_artifact_redactor_version,
                    event_kind, artifact_degradation_reason
             FROM session_run_events WHERE append_key = ?1",
            [append_key.as_str()],
            raw_append_receipt,
        )
        .optional()?;
    raw.map(|raw| append_receipt_from_raw(connection, raw))
        .transpose()
}

fn append_receipt_from_raw(
    connection: &rusqlite::Connection,
    raw: RawAppendReceipt,
) -> LedgerResult<AppendReceipt> {
    let mut statement = connection.prepare(
        "SELECT outbox_id FROM session_run_outbox WHERE event_id = ?1 ORDER BY manifest_order",
    )?;

    let outbox_ids = statement
        .query_map([raw.event_id.clone()], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(parse_outbox_id)
        .collect::<LedgerResult<Vec<_>>>()?;
    Ok(AppendReceipt {
        run_id: parse_run_id(raw.run_id)?,
        event_id: parse_event_id(raw.event_id)?,
        event_sequence: parse_nonnegative_u64("event sequence", raw.event_sequence)?,
        event_hash: validated_digest_string("event hash", raw.event_hash)?,
        event_kind: SessionRunEventKind::from_db(&raw.event_kind)?,
        artifact_degradation_reason: raw
            .artifact_degradation_reason
            .map(|reason| ArtifactDegradationReason::from_db(&reason))
            .transpose()?,
        sealed_artifact_ref: parse_optional_artifact_ref(
            raw.sealed_artifact_envelope_digest,
            raw.sealed_artifact_key_id,
            raw.sealed_artifact_schema_version,
            raw.sealed_artifact_encryption_algorithm,
            raw.sealed_artifact_canonicalization_algorithm,
            raw.sealed_artifact_canonicalization_version,
            raw.sealed_artifact_redactor_version,
        )?,
        outbox_ids,
    })
}

fn append_receipt_for_event_id(
    connection: &rusqlite::Connection,
    event_id: EventId,
) -> LedgerResult<AppendReceipt> {
    let raw = connection
        .query_row(
            "SELECT event_id, run_id, event_sequence, event_hash,
                    sealed_artifact_envelope_digest, sealed_artifact_key_id,
                    sealed_artifact_schema_version, sealed_artifact_encryption_algorithm,
                    sealed_artifact_canonicalization_algorithm,
                    sealed_artifact_canonicalization_version, sealed_artifact_redactor_version,
                    event_kind, artifact_degradation_reason
             FROM session_run_events WHERE event_id = ?1",
            [event_id.to_string()],
            raw_append_receipt,
        )
        .map_err(|error| match error {
            rusqlite::Error::QueryReturnedNoRows => LedgerError::IntegrityViolation(format!(
                "terminal audit event {event_id} is missing"
            )),
            other => LedgerError::Sqlite(other),
        })?;
    append_receipt_from_raw(connection, raw)
}

fn ensure_matching_replay_append_contract(
    connection: &rusqlite::Connection,
    append_key: &AppendKey,
    requested: Option<ReplayAppendContract>,
    requested_fidelity: ReplayArtifactFidelity,
) -> LedgerResult<()> {
    let stored: Option<(String, String, Option<String>, String)> = connection
        .query_row(
            "SELECT operation_kind, phase, resolution_source_event_id, requested_fidelity
             FROM session_run_replay_append_contracts WHERE append_key = ?1",
            [append_key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()?;
    match (stored, requested) {
        (None, None) => Ok(()),
        (Some((operation, phase, source_event_id, fidelity)), Some(contract))
            if ReplayOperationKind::from_db(&operation)? == contract.operation
                && ReplayOperationPhase::from_db(&phase)? == contract.phase
                && source_event_id.clone().map(parse_event_id).transpose()?
                    == contract.resolution_source_event_id
                && ReplayArtifactFidelity::from_db(&fidelity)? == requested_fidelity =>
        {
            Ok(())
        }
        _ => Err(LedgerError::AppendKeyConflict {
            append_key: append_key.to_string(),
        }),
    }
}

fn validate_replay_append_contract(contract: ReplayAppendContract) -> LedgerResult<()> {
    let valid = matches!(
        (contract.operation, contract.phase, contract.resolution_source_event_id.is_some()),
        (ReplayOperationKind::Model, ReplayOperationPhase::Request, false)
            | (ReplayOperationKind::Model, ReplayOperationPhase::Result, true)
            | (ReplayOperationKind::Tool, ReplayOperationPhase::Request, false)
            | (ReplayOperationKind::Tool, ReplayOperationPhase::Result, true)
            | (ReplayOperationKind::Delegation, ReplayOperationPhase::Request, false)
            | (ReplayOperationKind::Delegation, ReplayOperationPhase::Result, true)
            | (ReplayOperationKind::Oauth, ReplayOperationPhase::Request, false)
            | (ReplayOperationKind::Oauth, ReplayOperationPhase::Result, true)
            | (ReplayOperationKind::AgentOutput, ReplayOperationPhase::Result, false)
    );
    if valid {
        Ok(())
    } else {
        Err(LedgerError::IntegrityViolation(
            "invalid replay append operation/phase/source contract".to_string(),
        ))
    }
}

fn resolve_replay_append_contract_tx(
    transaction: &Transaction<'_>,
    reservation: &ReservationRow,
    committed_event_id: EventId,
    now: i64,
) -> LedgerResult<()> {
    let contract: Option<(String, String, Option<String>, String, String, String)> = transaction
        .query_row(
            "SELECT run_id, reserved_event_id, resolution_source_event_id, operation_kind, phase,
                    requested_fidelity
             FROM session_run_replay_append_contracts WHERE append_key = ?1",
            [reservation.append_key.as_str()],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()?;
    let Some((run_id, reserved_event_id, source_event_id, operation, phase, fidelity)) = contract
    else {
        return Ok(());
    };
    if run_id != reservation.run_id.to_string()
        || reserved_event_id != committed_event_id.to_string()
    {
        return Err(LedgerError::IntegrityViolation(
            "replay append contract does not bind its reserved run event".to_string(),
        ));
    }
    let operation = ReplayOperationKind::from_db(&operation)?;
    let phase = ReplayOperationPhase::from_db(&phase)?;
    let fidelity = ReplayArtifactFidelity::from_db(&fidelity)?;
    let source_event_id = source_event_id.map(parse_event_id).transpose()?;
    let contract = ReplayAppendContract {
        operation,
        phase,
        resolution_source_event_id: source_event_id,
    };
    validate_replay_append_contract(contract)?;
    let identity: Value = serde_json::from_str(reservation.identity.public_identity.as_str())
        .map_err(|error| LedgerError::IntegrityViolation(format!(
            "replay append identity is not valid canonical JSON: {error}"
        )))?;
    let outcome = identity
        .get("outcome")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "replay append identity lacks its closed outcome classification".to_string(),
            )
        })
        .and_then(ReplayOperationOutcome::from_db)?;
    if (phase == ReplayOperationPhase::Request
        && outcome != ReplayOperationOutcome::NotApplicable)
        || (phase == ReplayOperationPhase::Result
            && outcome == ReplayOperationOutcome::NotApplicable)
    {
        return Err(LedgerError::IntegrityViolation(
            "replay append identity outcome does not match its operation phase".to_string(),
        ));
    }
    let expected_identity = match (operation, phase) {
        (ReplayOperationKind::Model, ReplayOperationPhase::Request) => ("request", "llm-request"),
        (ReplayOperationKind::Model, ReplayOperationPhase::Result) => ("result", "llm-response"),
        (ReplayOperationKind::Tool, ReplayOperationPhase::Request) => {
            ("request", "tool-invocation")
        }
        (ReplayOperationKind::Tool, ReplayOperationPhase::Result) => ("result", "tool-invocation"),
        (ReplayOperationKind::Delegation, ReplayOperationPhase::Request) => {
            ("request", "delegation")
        }
        (ReplayOperationKind::Delegation, ReplayOperationPhase::Result) => {
            ("result", "delegation")
        }
        (ReplayOperationKind::Oauth, ReplayOperationPhase::Request) => {
            ("request", "oauth-refresh")
        }
        (ReplayOperationKind::Oauth, ReplayOperationPhase::Result) => {
            ("result", "oauth-refresh")
        }
        (ReplayOperationKind::AgentOutput, ReplayOperationPhase::Result) => {
            ("result", "agent-output")
        }
        _ => unreachable!("validated replay append contract"),
    };
    if identity
        .get("artifact_kind")
        .and_then(Value::as_str)
        != Some(expected_identity.0)
        || identity
            .get("evidence_subject")
            .and_then(Value::as_str)
            != Some(expected_identity.1)
    {
        return Err(LedgerError::IntegrityViolation(
            "replay append identity does not match its typed contract".to_string(),
        ));
    }

    match (phase, source_event_id) {
        (ReplayOperationPhase::Request, None) => {
            transaction.execute(
                "INSERT INTO session_run_replay_obligations (
                    run_id, source_event_id, operation_kind, phase, operation_instance_id,
                    replay_fidelity, outcome, resolved_event_id, created_at_ms, resolved_at_ms
                 ) VALUES (?1, ?2, ?3, 'request', ?2, ?4, 'not_applicable', ?2, ?5, ?5)",
                params![
                    reservation.run_id.to_string(),
                    committed_event_id.to_string(),
                    operation.as_str(),
                    fidelity.as_str(),
                    now,
                ],
            )?;
            transaction.execute(
                "INSERT INTO session_run_replay_obligations (
                    run_id, source_event_id, operation_kind, phase, operation_instance_id,
                    replay_fidelity, outcome, resolved_event_id, created_at_ms, resolved_at_ms
                 ) VALUES (?1, ?2, ?3, 'result', ?2, 'pending', 'unavailable', NULL, ?4, NULL)",
                params![
                    reservation.run_id.to_string(),
                    committed_event_id.to_string(),
                    operation.as_str(),
                    now,
                ],
            )?;
        }
        (ReplayOperationPhase::Result, Some(source_event_id)) => {
            let source_run: Option<String> = transaction
                .query_row(
                    "SELECT run_id FROM session_run_events WHERE event_id = ?1",
                    [source_event_id.to_string()],
                    |row| row.get(0),
                )
                .optional()?;
            let expected_run_id = reservation.run_id.to_string();
            if source_run.as_deref() != Some(expected_run_id.as_str()) {
                return Err(LedgerError::IntegrityViolation(
                    "replay result contract resolves an event from another run".to_string(),
                ));
            }
            let changed = transaction.execute(
                "UPDATE session_run_replay_obligations
                 SET replay_fidelity = ?1, outcome = ?2, resolved_event_id = ?3, resolved_at_ms = ?4
                 WHERE run_id = ?5 AND source_event_id = ?6 AND operation_kind = ?7
                   AND operation_instance_id = ?6
                   AND phase = 'result' AND replay_fidelity = 'pending'
                   AND outcome = 'unavailable' AND resolved_event_id IS NULL AND resolved_at_ms IS NULL",
                params![
                    fidelity.as_str(),
                    outcome.as_str(),
                    committed_event_id.to_string(),
                    now,
                    reservation.run_id.to_string(),
                    source_event_id.to_string(),
                    operation.as_str(),
                ],
            )?;
            if changed != 1 {
                return Err(LedgerError::IntegrityViolation(
                    "replay result contract has no matching pending request obligation".to_string(),
                ));
            }
        }
        (ReplayOperationPhase::Result, None) => {
            transaction.execute(
                "INSERT INTO session_run_replay_obligations (
                    run_id, source_event_id, operation_kind, phase, operation_instance_id,
                    replay_fidelity, outcome, resolved_event_id, created_at_ms, resolved_at_ms
                 ) VALUES (?1, ?2, ?3, 'result', ?2, ?4, ?5, ?2, ?6, ?6)",
                params![
                    reservation.run_id.to_string(),
                    committed_event_id.to_string(),
                    operation.as_str(),
                    fidelity.as_str(),
                    outcome.as_str(),
                    now,
                ],
            )?;
        }
        (ReplayOperationPhase::Request, Some(_)) => {
            return Err(LedgerError::IntegrityViolation(
                "replay request contract cannot resolve a prior event".to_string(),
            ));
        }
    }
    let next_state = derive_next_exact_replay_state_tx(transaction, reservation.run_id)?;
    update_exact_replay_latch_tx(transaction, reservation.run_id, next_state, now, committed_event_id)?;
    Ok(())
}

/// Close every unresolved result obligation in the same transaction that
/// appends `RunExit`. A crash or cancellation cannot leave a replay-critical
/// request without a typed terminal result.
fn resolve_pending_replay_obligations_at_exit_tx(
    transaction: &Transaction<'_>,
    run_id: RunId,
    terminal_reason: TerminalReason,
    exit_event_id: EventId,
    now: i64,
) -> LedgerResult<()> {
    let outcome = match terminal_reason {
        TerminalReason::Cancellation => ReplayOperationOutcome::Cancelled,
        TerminalReason::Timeout => ReplayOperationOutcome::TimedOut,
        TerminalReason::EffectNonResumableUnknown => {
            ReplayOperationOutcome::EffectNonResumableUnknown
        }
        TerminalReason::Completed
        | TerminalReason::ExecutionFailure
        | TerminalReason::PolicyBlocked
        | TerminalReason::RequiredDeliveryDeadLettered
        | TerminalReason::ProcessDeath => ReplayOperationOutcome::Unavailable,
    };
    let changed = transaction.execute(
        "UPDATE session_run_replay_obligations
         SET replay_fidelity = 'unavailable', outcome = ?1, resolved_event_id = ?2,
             resolved_at_ms = ?3
         WHERE run_id = ?4 AND phase = 'result' AND replay_fidelity = 'pending'
           AND outcome = 'unavailable' AND resolved_event_id IS NULL AND resolved_at_ms IS NULL",
        params![
            outcome.as_str(),
            exit_event_id.to_string(),
            now,
            run_id.to_string(),
        ],
    )?;
    if changed == 0 {
        return Ok(());
    }
    let next_state = derive_next_exact_replay_state_tx(transaction, run_id)?;
    update_exact_replay_latch_tx(transaction, run_id, next_state, now, exit_event_id)?;
    Ok(())
}

fn resolve_degraded_replay_append_contract_tx(
    transaction: &Transaction<'_>,
    reservation: &ReservationRow,
    audit_event_id: EventId,
    now: i64,
) -> LedgerResult<()> {
    let contract: Option<(Option<String>, String, String)> = transaction
        .query_row(
            "SELECT resolution_source_event_id, operation_kind, phase
             FROM session_run_replay_append_contracts WHERE append_key = ?1",
            [reservation.append_key.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()?;
    let Some((source_event_id, operation, phase)) = contract else {
        return Ok(());
    };
    let operation = ReplayOperationKind::from_db(&operation)?;
    let phase = ReplayOperationPhase::from_db(&phase)?;
    let source_event_id = source_event_id.map(parse_event_id).transpose()?;
    match (phase, source_event_id) {
        (ReplayOperationPhase::Request, None) => {
            transaction.execute(
                "INSERT INTO session_run_replay_obligations (
                    run_id, source_event_id, operation_kind, phase, operation_instance_id,
                    replay_fidelity, outcome, resolved_event_id, created_at_ms, resolved_at_ms
                 ) VALUES (?1, ?2, ?3, 'request', ?2, 'unavailable', 'not_applicable', ?2, ?4, ?4)",
                params![
                    reservation.run_id.to_string(),
                    audit_event_id.to_string(),
                    operation.as_str(),
                    now,
                ],
            )?;
            transaction.execute(
                "INSERT INTO session_run_replay_obligations (
                    run_id, source_event_id, operation_kind, phase, operation_instance_id,
                    replay_fidelity, outcome, resolved_event_id, created_at_ms, resolved_at_ms
                 ) VALUES (?1, ?2, ?3, 'result', ?2, 'unavailable', 'unavailable', ?2, ?4, ?4)",
                params![
                    reservation.run_id.to_string(),
                    audit_event_id.to_string(),
                    operation.as_str(),
                    now,
                ],
            )?;
        }
        (ReplayOperationPhase::Result, Some(source_event_id)) => {
            let changed = transaction.execute(
                "UPDATE session_run_replay_obligations
                 SET replay_fidelity = 'unavailable', outcome = 'unavailable',
                     resolved_event_id = ?1, resolved_at_ms = ?2
                 WHERE run_id = ?3 AND source_event_id = ?4 AND operation_kind = ?5
                   AND operation_instance_id = ?4
                   AND phase = 'result' AND replay_fidelity = 'pending'
                   AND outcome = 'unavailable' AND resolved_event_id IS NULL AND resolved_at_ms IS NULL",
                params![
                    audit_event_id.to_string(),
                    now,
                    reservation.run_id.to_string(),
                    source_event_id.to_string(),
                    operation.as_str(),
                ],
            )?;
            if changed != 1 {
                return Err(LedgerError::IntegrityViolation(
                    "degraded replay result has no matching pending request obligation".to_string(),
                ));
            }
        }
        (ReplayOperationPhase::Result, None) => {
            transaction.execute(
                "INSERT INTO session_run_replay_obligations (
                    run_id, source_event_id, operation_kind, phase, operation_instance_id,
                    replay_fidelity, outcome, resolved_event_id, created_at_ms, resolved_at_ms
                 ) VALUES (?1, ?2, ?3, 'result', ?2, 'unavailable', 'unavailable', ?2, ?4, ?4)",
                params![
                    reservation.run_id.to_string(),
                    audit_event_id.to_string(),
                    operation.as_str(),
                    now,
                ],
            )?;
        }
        (ReplayOperationPhase::Request, Some(_)) => {
            return Err(LedgerError::IntegrityViolation(
                "degraded replay request resolves a prior event".to_string(),
            ));
        }
    }
    update_exact_replay_latch_tx(
        transaction,
        reservation.run_id,
        ExactReplayState::UnavailableSanitized,
        now,
        audit_event_id,
    )?;
    Ok(())
}

fn ensure_matching_reservation(
    reservation: &ReservationRow,
    request: &AppendRequest,
) -> LedgerResult<()> {
    if reservation.identity.public_identity != request.identity.public_identity {
        return Err(LedgerError::AppendKeyConflict {
            append_key: request.append_key.to_string(),
        });
    }
    if reservation.identity.private_fingerprint != request.identity.private_fingerprint {
        return Err(LedgerError::PrivateFingerprintMismatch {
            append_key: request.append_key.to_string(),
        });
    }
    if reservation.run_id != request.run_id
        || reservation.effect_intent != request.effect_intent
        || reservation.public_payload != request.public_payload
    {
        return Err(LedgerError::AppendKeyConflict {
            append_key: request.append_key.to_string(),
        });
    }
    Ok(())
}

fn load_reservation(
    connection: &rusqlite::Connection,
    append_key: &AppendKey,
) -> LedgerResult<Option<ReservationRow>> {
    let raw: Option<RawReservation> = connection
        .query_row(
            "SELECT append_key, run_id, event_id, frozen_event_at_ms, public_identity_jcs,
                    private_fingerprint_algorithm, private_fingerprint_algorithm_version,
                    private_fingerprint_key_id, private_fingerprint_key_version,
                    private_fingerprint, effect_intent, public_payload_jcs,
                    sealed_artifact_envelope_digest, sealed_artifact_key_id,
                    sealed_artifact_schema_version, sealed_artifact_encryption_algorithm,
                    sealed_artifact_canonicalization_algorithm,
                    sealed_artifact_canonicalization_version, sealed_artifact_redactor_version,
                    lease_generation, lease_token, lease_expires_at_ms, committed_event_id,
                    terminal_audit_event_id
             FROM session_run_append_reservations WHERE append_key = ?1",
            [append_key.as_str()],
            raw_reservation,
        )
        .optional()?;
    raw.map(ReservationRow::try_from).transpose()
}

fn load_expired_exhausted_outbox(
    transaction: &Transaction<'_>,
    now: i64,
    mode: &ResolvedExecutionMode,
) -> LedgerResult<Option<OutboxCandidate>> {
    load_outbox_candidate(
        transaction,
        "o.state = 'leased' AND o.lease_expires_at_ms <= ?1 AND o.attempt_count = o.max_attempts",
        now,
        mode,
    )
}

fn load_claimable_outbox(
    transaction: &Transaction<'_>,
    now: i64,
    mode: &ResolvedExecutionMode,
) -> LedgerResult<Option<OutboxCandidate>> {
    load_outbox_candidate(
        transaction,
        "(o.state = 'ready' OR (o.state = 'leased' AND o.lease_expires_at_ms <= ?1)) AND o.attempt_count < o.max_attempts",
        now,
        mode,
    )
}

fn load_outbox_candidate(
    transaction: &Transaction<'_>,
    predicate: &str,
    now: i64,
    mode: &ResolvedExecutionMode,
) -> LedgerResult<Option<OutboxCandidate>> {
    let query = format!(
        "SELECT o.outbox_id, o.run_id, o.event_id, o.destination, o.required, o.idempotency_key,
                o.public_payload_jcs,
                o.sealed_artifact_envelope_digest, o.sealed_artifact_key_id,
                o.sealed_artifact_schema_version, o.sealed_artifact_encryption_algorithm,
                o.sealed_artifact_canonicalization_algorithm,
                o.sealed_artifact_canonicalization_version, o.sealed_artifact_redactor_version,
                e.sealed_artifact_envelope_digest, e.sealed_artifact_key_id,
                e.sealed_artifact_schema_version, e.sealed_artifact_encryption_algorithm,
                e.sealed_artifact_canonicalization_algorithm,
                e.sealed_artifact_canonicalization_version, e.sealed_artifact_redactor_version,
                o.attempt_count, o.max_attempts, o.lease_generation, o.state,
                o.lease_token, o.lease_expires_at_ms
         FROM session_run_outbox o
         JOIN session_run_events e ON e.event_id = o.event_id
         JOIN session_runs r ON r.run_id = o.run_id
         WHERE {predicate}
           AND r.execution_mode = ?2 AND r.execution_mode_source = ?3
         ORDER BY CASE o.state WHEN 'ready' THEN 0 ELSE 1 END, o.outbox_id ASC
         LIMIT 1"
    );
    let raw: Option<RawOutboxCandidate> = transaction
        .query_row(
            &query,
            params![
                now,
                execution_mode_to_db(mode.mode),
                execution_mode_source_to_db(mode.source),
            ],
            raw_outbox_candidate,
        )
        .optional()?;
    raw.map(OutboxCandidate::try_from).transpose()
}

fn load_fence_for_lease(
    transaction: &Transaction<'_>,
    lease: &OutboxLease,
) -> LedgerResult<OutboxFence> {
    let raw: Option<RawOutboxFence> = transaction
        .query_row(
            "SELECT outbox_id, run_id, event_id, required, attempt_count, max_attempts,
                    lease_generation, lease_token, state
             FROM session_run_outbox WHERE outbox_id = ?1",
            [lease.outbox_id.to_string()],
            raw_outbox_fence,
        )
        .optional()?;
    let raw = raw.ok_or_else(|| LedgerError::StaleOutboxLease {
        outbox_id: lease.outbox_id.to_string(),
    })?;
    let fence = OutboxFence::try_from(raw)?;
    if fence.run_id != lease.run_id
        || fence.event_id != lease.event_id
        || fence.lease_generation != lease.lease_generation
        || fence.token != lease.token
    {
        return Err(LedgerError::StaleOutboxLease {
            outbox_id: lease.outbox_id.to_string(),
        });
    }
    Ok(fence)
}

struct NonResumableEffectRow {
    record: NonResumableEffectRecord,
    claim_token: Option<LeaseToken>,
}

fn load_nonresumable_effect(
    connection: &rusqlite::Connection,
    event_id: EventId,
) -> LedgerResult<Option<NonResumableEffectRow>> {
    let raw = connection
        .query_row(
            "SELECT run_id, state, idempotency_key, claim_token, claimed_at_ms,
                    result_payload_digest, result_event_id, terminal_at_ms
             FROM session_run_nonresumable_effects WHERE event_id = ?1",
            [event_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, Option<Vec<u8>>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, Option<String>>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<i64>>(7)?,
                ))
            },
        )
        .optional()?;
    let Some((
        run_id,
        state,
        idempotency_key,
        claim_token,
        claimed_at_ms,
        result_payload_digest,
        result_event_id,
        terminal_at_ms,
    )) = raw
    else {
        return Ok(None);
    };
    Ok(Some(NonResumableEffectRow {
        record: NonResumableEffectRecord {
            run_id: parse_run_id(run_id)?,
            event_id,
            state: NonResumableEffectState::from_db(&state)?,
            idempotency_key: NonResumableIdempotencyKey::from_sha256_hex(idempotency_key)?,
            claimed_at_ms,
            result_payload_digest: result_payload_digest.map(Sha256Digest::new).transpose()?,
            result_event_id: result_event_id.map(parse_event_id).transpose()?,
            terminal_at_ms,
        },
        claim_token: claim_token.map(LeaseToken::from_blob).transpose()?,
    }))
}

fn validate_nonresumable_result_event(
    connection: &rusqlite::Connection,
    lease: &NonResumableEffectLease,
    result_event_id: EventId,
    expected_result_digest: &Sha256Digest,
) -> LedgerResult<()> {
    let raw = connection
        .query_row(
            "SELECT result.run_id, result.event_kind, result.append_key,
                    result.effect_intent, result.sealed_artifact_envelope_digest,
                    result.event_sequence, result.public_payload_jcs,
                    source.event_sequence, source.run_id, source.effect_intent
             FROM session_run_events AS result
             JOIN session_run_events AS source ON source.event_id = ?2
             WHERE result.event_id = ?1",
            params![result_event_id.to_string(), lease.event_id.to_string()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                    row.get::<_, i64>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, i64>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, String>(9)?,
                ))
            },
        )
        .optional()?;
    let mismatch = || LedgerError::NonResumableEffectResultMismatch {
        event_id: lease.event_id.to_string(),
        result_event_id: result_event_id.to_string(),
    };
    let Some((
        run_id,
        kind,
        append_key,
        effect_intent,
        envelope_digest,
        result_sequence,
        public_payload_jcs,
        source_sequence,
        source_run_id,
        source_effect_intent,
    )) = raw
    else {
        return Err(mismatch());
    };
    let claim_binding = derive_nonresumable_claim_binding(&lease.token)?;
    let expected_append_key =
        derive_nonresumable_result_append_key(lease.event_id, &claim_binding)?;
    let public_payload = CanonicalJson::from_jcs_text(public_payload_jcs)?;
    let payload_value = public_payload.as_value()?;
    let observed_payload = payload_value
        .as_object()
        .and_then(|object| object.get("payload"))
        .ok_or_else(mismatch)?;
    let expected_payload = CanonicalJson::from_value(&json!({
        "schema": NONRESUMABLE_RESULT_SCHEMA,
        "schema_version": RUN_EVENT_BINDING_SCHEMA_VERSION,
        "source_event_id": lease.event_id,
        "claim_binding": claim_binding,
        "payload": observed_payload,
    }))?;
    let actual_result_digest = Sha256Digest::new(sha256_hex(public_payload.as_str().as_bytes()))?;
    let matches = run_id == lease.run_id.to_string()
        && source_run_id == run_id
        && source_effect_intent == EffectIntent::LocalOneShotV1.as_str()
        && kind == SessionRunEventKind::Append.as_str()
        && append_key.as_deref() == Some(expected_append_key.as_str())
        && effect_intent == EffectIntent::None.as_str()
        && envelope_digest.is_some()
        && result_sequence > source_sequence
        && public_payload == expected_payload
        && actual_result_digest == *expected_result_digest;
    if !matches {
        return Err(mismatch());
    }
    Ok(())
}

fn add_duration(now: i64, duration: Duration) -> LedgerResult<i64> {
    let duration = i64::try_from(duration.as_millis()).map_err(|_| LedgerError::NumericOverflow)?;
    now.checked_add(duration)
        .ok_or(LedgerError::NumericOverflow)
}

struct ArtifactRefDbValues<'a> {
    envelope_digest: Option<&'a str>,
    key_id: Option<&'a str>,
    schema_version: Option<i64>,
    encryption_algorithm: Option<&'a str>,
    canonicalization_algorithm: Option<&'a str>,
    canonicalization_version: Option<i64>,
    redactor_version: Option<&'a str>,
}

impl<'a> From<Option<&'a SealedArtifactRef>> for ArtifactRefDbValues<'a> {
    fn from(reference: Option<&'a SealedArtifactRef>) -> Self {
        Self {
            envelope_digest: reference.map(|value| value.envelope_digest().as_str()),
            key_id: reference.map(|value| value.key_id().as_str()),
            schema_version: reference.map(|value| i64::from(value.schema_version())),
            encryption_algorithm: reference.map(SealedArtifactRef::encryption_algorithm),
            canonicalization_algorithm: reference
                .map(SealedArtifactRef::canonicalization_algorithm),
            canonicalization_version: reference
                .map(|value| i64::from(value.canonicalization_version())),
            redactor_version: reference.map(|value| value.redactor_version().as_str()),
        }
    }
}

struct ReservationRow {
    append_key: AppendKey,
    run_id: RunId,
    event_id: EventId,
    frozen_event_at_ms: i64,
    identity: AppendIdentity,
    effect_intent: EffectIntent,
    public_payload: CanonicalJson,
    sealed_artifact_ref: Option<SealedArtifactRef>,
    lease_generation: u64,
    token: LeaseToken,
    lease_expires_at_ms: i64,
    committed_event_id: Option<EventId>,
    terminal_audit_event_id: Option<EventId>,
}

struct RawReservation {
    append_key: String,
    run_id: String,
    event_id: String,
    frozen_event_at_ms: i64,
    public_identity_jcs: String,
    private_fingerprint_algorithm: String,
    private_fingerprint_algorithm_version: i64,
    private_fingerprint_key_id: String,
    private_fingerprint_key_version: i64,
    private_fingerprint: String,
    effect_intent: String,
    public_payload_jcs: String,
    sealed_artifact_envelope_digest: Option<String>,
    sealed_artifact_key_id: Option<String>,
    sealed_artifact_schema_version: Option<i64>,
    sealed_artifact_encryption_algorithm: Option<String>,
    sealed_artifact_canonicalization_algorithm: Option<String>,
    sealed_artifact_canonicalization_version: Option<i64>,
    sealed_artifact_redactor_version: Option<String>,
    lease_generation: i64,
    lease_token: Vec<u8>,
    lease_expires_at_ms: i64,
    committed_event_id: Option<String>,
    terminal_audit_event_id: Option<String>,
}

impl TryFrom<RawReservation> for ReservationRow {
    type Error = LedgerError;

    fn try_from(raw: RawReservation) -> Result<Self, Self::Error> {
        Ok(Self {
            append_key: AppendKey::new(raw.append_key)?,
            run_id: parse_run_id(raw.run_id)?,
            event_id: parse_event_id(raw.event_id)?,
            frozen_event_at_ms: raw.frozen_event_at_ms,
            identity: AppendIdentity {
                public_identity: CanonicalJson::from_jcs_text(raw.public_identity_jcs)?,
                private_fingerprint: PrivateFingerprint::from_db(
                    &raw.private_fingerprint_algorithm,
                    raw.private_fingerprint_algorithm_version,
                    raw.private_fingerprint_key_id,
                    raw.private_fingerprint_key_version,
                    raw.private_fingerprint,
                )?,
            },
            effect_intent: EffectIntent::from_db(&raw.effect_intent)?,
            public_payload: CanonicalJson::from_jcs_text(raw.public_payload_jcs)?,
            sealed_artifact_ref: parse_optional_artifact_ref(
                raw.sealed_artifact_envelope_digest,
                raw.sealed_artifact_key_id,
                raw.sealed_artifact_schema_version,
                raw.sealed_artifact_encryption_algorithm,
                raw.sealed_artifact_canonicalization_algorithm,
                raw.sealed_artifact_canonicalization_version,
                raw.sealed_artifact_redactor_version,
            )?,
            lease_generation: parse_positive_u64("append lease generation", raw.lease_generation)?,
            token: LeaseToken::from_blob(raw.lease_token)?,
            lease_expires_at_ms: raw.lease_expires_at_ms,
            committed_event_id: raw.committed_event_id.map(parse_event_id).transpose()?,
            terminal_audit_event_id: raw
                .terminal_audit_event_id
                .map(parse_event_id)
                .transpose()?,
        })
    }
}

struct OutboxCandidate {
    outbox_id: OutboxRowId,
    run_id: RunId,
    event_id: EventId,
    destination: Destination,
    required: bool,
    idempotency_key: DownstreamIdempotencyKey,
    public_payload: CanonicalJson,
    sealed_artifact_ref: SealedArtifactRef,
    attempt_count: u32,
    max_attempts: u32,
    lease_generation: u64,
    state: OutboxState,
    previous_token: Option<LeaseToken>,
    lease_expires_at_ms: Option<i64>,
}

impl OutboxCandidate {
    fn into_existing_fence(self) -> LedgerResult<OutboxFence> {
        if self.state != OutboxState::Leased || self.lease_expires_at_ms.is_none() {
            return Err(LedgerError::IntegrityViolation(
                "expired outbox terminalization candidate is not an active lease".to_string(),
            ));
        }
        Ok(OutboxFence {
            outbox_id: self.outbox_id,
            run_id: self.run_id,
            event_id: self.event_id,
            required: self.required,
            attempt_count: self.attempt_count,
            max_attempts: self.max_attempts,
            lease_generation: self.lease_generation,
            token: self.previous_token.ok_or_else(|| {
                LedgerError::IntegrityViolation(
                    "expired outbox terminalization candidate has no lease token".to_string(),
                )
            })?,
        })
    }
}

struct RawOutboxCandidate {
    outbox_id: String,
    run_id: String,
    event_id: String,
    destination: String,
    required: i64,
    idempotency_key: String,
    public_payload: String,
    sealed_artifact_envelope_digest: String,
    sealed_artifact_key_id: String,
    sealed_artifact_schema_version: i64,
    sealed_artifact_encryption_algorithm: String,
    sealed_artifact_canonicalization_algorithm: String,
    sealed_artifact_canonicalization_version: i64,
    sealed_artifact_redactor_version: String,
    event_sealed_artifact_envelope_digest: Option<String>,
    event_sealed_artifact_key_id: Option<String>,
    event_sealed_artifact_schema_version: Option<i64>,
    event_sealed_artifact_encryption_algorithm: Option<String>,
    event_sealed_artifact_canonicalization_algorithm: Option<String>,
    event_sealed_artifact_canonicalization_version: Option<i64>,
    event_sealed_artifact_redactor_version: Option<String>,
    attempt_count: i64,
    max_attempts: i64,
    lease_generation: i64,
    state: String,
    lease_token: Option<Vec<u8>>,
    lease_expires_at_ms: Option<i64>,
}

impl TryFrom<RawOutboxCandidate> for OutboxCandidate {
    type Error = LedgerError;

    fn try_from(raw: RawOutboxCandidate) -> Result<Self, Self::Error> {
        let sealed_artifact_ref = SealedArtifactRef::from_db(
            raw.sealed_artifact_envelope_digest,
            raw.sealed_artifact_key_id,
            raw.sealed_artifact_schema_version,
            raw.sealed_artifact_encryption_algorithm,
            raw.sealed_artifact_canonicalization_algorithm,
            raw.sealed_artifact_canonicalization_version,
            raw.sealed_artifact_redactor_version,
        )?;
        let event_ref = parse_optional_artifact_ref(
            raw.event_sealed_artifact_envelope_digest,
            raw.event_sealed_artifact_key_id,
            raw.event_sealed_artifact_schema_version,
            raw.event_sealed_artifact_encryption_algorithm,
            raw.event_sealed_artifact_canonicalization_algorithm,
            raw.event_sealed_artifact_canonicalization_version,
            raw.event_sealed_artifact_redactor_version,
        )?;
        if event_ref.as_ref() != Some(&sealed_artifact_ref) {
            return Err(LedgerError::IntegrityViolation(
                "outbox sealed artifact reference differs from originating event".to_string(),
            ));
        }
        let state = OutboxState::from_db(&raw.state)?;
        let token = raw.lease_token.map(LeaseToken::from_blob).transpose()?;
        if (state == OutboxState::Leased) != (token.is_some() && raw.lease_expires_at_ms.is_some())
        {
            return Err(LedgerError::InvalidStoredState {
                kind: "outbox lease fields",
                value: "state and lease fields disagree".to_string(),
            });
        }
        Ok(Self {
            outbox_id: parse_outbox_id(raw.outbox_id)?,
            run_id: parse_run_id(raw.run_id)?,
            event_id: parse_event_id(raw.event_id)?,
            destination: Destination::from_db(&raw.destination)?,
            required: parse_bool("outbox required", raw.required)?,
            idempotency_key: DownstreamIdempotencyKey::from_sha256_hex(raw.idempotency_key)?,
            public_payload: CanonicalJson::from_jcs_text(raw.public_payload)?,
            sealed_artifact_ref,
            attempt_count: parse_nonnegative_u32("outbox attempt count", raw.attempt_count)?,
            max_attempts: parse_positive_u32("outbox maximum attempts", raw.max_attempts)?,
            lease_generation: parse_nonnegative_u64(
                "outbox lease generation",
                raw.lease_generation,
            )?,
            state,
            previous_token: token,
            lease_expires_at_ms: raw.lease_expires_at_ms,
        })
    }
}

struct OutboxFence {
    outbox_id: OutboxRowId,
    run_id: RunId,
    event_id: EventId,
    required: bool,
    attempt_count: u32,
    max_attempts: u32,
    lease_generation: u64,
    token: LeaseToken,
}

struct RawOutboxFence {
    outbox_id: String,
    run_id: String,
    event_id: String,
    required: i64,
    attempt_count: i64,
    max_attempts: i64,
    lease_generation: i64,
    lease_token: Option<Vec<u8>>,
    state: String,
}

impl TryFrom<RawOutboxFence> for OutboxFence {
    type Error = LedgerError;

    fn try_from(raw: RawOutboxFence) -> Result<Self, Self::Error> {
        if raw.state != OutboxState::Leased.as_str() {
            return Err(LedgerError::StaleOutboxLease {
                outbox_id: raw.outbox_id,
            });
        }
        let outbox_id = parse_outbox_id(raw.outbox_id.clone())?;
        let token = raw
            .lease_token
            .ok_or_else(|| LedgerError::StaleOutboxLease {
                outbox_id: raw.outbox_id,
            })?;
        Ok(Self {
            outbox_id,
            run_id: parse_run_id(raw.run_id)?,
            event_id: parse_event_id(raw.event_id)?,
            required: parse_bool("outbox required", raw.required)?,
            attempt_count: parse_nonnegative_u32("outbox attempt count", raw.attempt_count)?,
            max_attempts: parse_positive_u32("outbox maximum attempts", raw.max_attempts)?,
            lease_generation: parse_nonnegative_u64(
                "outbox lease generation",
                raw.lease_generation,
            )?,
            token: LeaseToken::from_blob(token)?,
        })
    }
}

struct RawRunStatus {
    exit_event_id: Option<String>,
    exit_terminal_reason: Option<String>,
    candidate_outcome: Option<String>,
    final_outcome: Option<String>,
    required_dead_lettered: i64,
    pending_required_delivery: i64,
    exact_replay: i64,
    exact_replay_state: String,
    exact_replay_reason: Option<String>,
    exact_replay_transition_sequence: i64,
    exact_replay_latched_at_ms: Option<i64>,
    exact_replay_latched_by_event_id: Option<String>,
}

impl TryFrom<(RunId, RawRunStatus)> for RunStatus {
    type Error = LedgerError;

    fn try_from((run_id, raw): (RunId, RawRunStatus)) -> Result<Self, Self::Error> {
        Ok(Self {
            run_id,
            exit_event_id: raw.exit_event_id.map(parse_event_id).transpose()?,
            exit_terminal_reason: raw
                .exit_terminal_reason
                .map(|value| TerminalReason::from_db(&value))
                .transpose()?,
            candidate_outcome: raw
                .candidate_outcome
                .map(|value| RunOutcome::from_db(&value))
                .transpose()?,
            final_outcome: raw
                .final_outcome
                .map(|value| RunOutcome::from_db(&value))
                .transpose()?,
            required_dead_lettered: parse_bool(
                "required dead letter latch",
                raw.required_dead_lettered,
            )?,
            pending_required_delivery: parse_bool(
                "pending required delivery",
                raw.pending_required_delivery,
            )?,
            exact_replay: {
                let state = ExactReplayState::from_db(&raw.exact_replay_state)?;
                let reason = raw
                    .exact_replay_reason
                    .as_deref()
                    .map(ArtifactStateReason::from_db)
                    .transpose()?;
                validate_exact_replay_state_reason(state, reason)?;
                let exact_replay = parse_sql_bool(raw.exact_replay, "exact replay")?;
                if exact_replay != state.exact_replay() {
                    return Err(LedgerError::InvalidStoredState {
                        kind: "exact replay latch",
                        value: raw.exact_replay_state,
                    });
                }
                ExactReplayStatus {
                    state,
                    reason,
                    exact_replay,
                    transition_sequence: parse_nonnegative_u64(
                        "exact replay transition sequence",
                        raw.exact_replay_transition_sequence,
                    )?,
                    latched_at_ms: raw.exact_replay_latched_at_ms,
                    latched_by_event_id: raw
                        .exact_replay_latched_by_event_id
                        .map(parse_event_id)
                        .transpose()?,
                }
            },
        })
    }
}

struct RawRunHeader {
    start_event_id: String,
    execution_mode: String,
    execution_mode_source: String,
    started_at_ms: i64,
    owner_id: String,
    owner_lease_generation: i64,
    owner_lease_expires_at_ms: i64,
    provenance_canonicalization_algorithm: String,
    provenance_canonicalization_version: i64,
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
    prompt_digest: Option<String>,
    prompt_digest_absence: Option<String>,
    input_digest: Option<String>,
    input_digest_absence: Option<String>,
    output_digest: Option<String>,
    output_digest_absence: Option<String>,
    tool_digest: Option<String>,
    tool_digest_absence: Option<String>,
    skill_digest: Option<String>,
    skill_digest_absence: Option<String>,
    policy_digest: Option<String>,
    policy_digest_absence: Option<String>,
    sandbox_digest: Option<String>,
    sandbox_digest_absence: Option<String>,
}

impl TryFrom<(RunId, RawRunHeader)> for RunHeader {
    type Error = LedgerError;

    fn try_from((run_id, raw): (RunId, RawRunHeader)) -> Result<Self, Self::Error> {
        Ok(Self {
            run_id,
            start_event_id: parse_event_id(raw.start_event_id)?,
            execution_mode: execution_mode_from_db(
                &raw.execution_mode,
                &raw.execution_mode_source,
            )?,
            started_at_ms: raw.started_at_ms,
            owner_id: RunOwnerId::new(raw.owner_id)?,
            owner_lease_generation: parse_positive_u64(
                "run owner lease generation",
                raw.owner_lease_generation,
            )?,
            owner_lease_expires_at_ms: raw.owner_lease_expires_at_ms,
            provenance: RunProvenance::from_db(
                raw.provenance_canonicalization_algorithm,
                raw.provenance_canonicalization_version,
                raw.session_id,
                raw.parent_run_id,
                raw.branch_id,
                raw.tenant_id,
                raw.action_id,
                raw.objective_id,
                raw.task_id,
                raw.model_id,
                raw.provider_id,
                raw.config_digest,
                raw.prompt_digest,
                raw.prompt_digest_absence,
                raw.input_digest,
                raw.input_digest_absence,
                raw.output_digest,
                raw.output_digest_absence,
                raw.tool_digest,
                raw.tool_digest_absence,
                raw.skill_digest,
                raw.skill_digest_absence,
                raw.policy_digest,
                raw.policy_digest_absence,
                raw.sandbox_digest,
                raw.sandbox_digest_absence,
            )?,
        })
    }
}

struct RawEvent {
    event_id: String,
    run_id: String,
    sequence: i64,
    kind: String,
    append_key: Option<String>,
    effect_intent: String,
    artifact_degradation_reason: Option<String>,
    public_identity: Option<String>,
    fingerprint_algorithm: Option<String>,
    fingerprint_algorithm_version: Option<i64>,
    fingerprint_key_id: Option<String>,
    fingerprint_key_version: Option<i64>,
    fingerprint: Option<String>,
    sealed_artifact_envelope_digest: Option<String>,
    sealed_artifact_key_id: Option<String>,
    sealed_artifact_schema_version: Option<i64>,
    sealed_artifact_encryption_algorithm: Option<String>,
    sealed_artifact_canonicalization_algorithm: Option<String>,
    sealed_artifact_canonicalization_version: Option<i64>,
    sealed_artifact_redactor_version: Option<String>,
    run_provenance: Option<String>,
    public_payload: String,
    manifest_hash: String,
    previous_hash: String,
    event_hash: String,
    created_at_ms: i64,
}

impl TryFrom<RawEvent> for SessionRunEvent {
    type Error = LedgerError;

    fn try_from(raw: RawEvent) -> Result<Self, Self::Error> {
        let private_fingerprint = match (
            raw.fingerprint_algorithm,
            raw.fingerprint_algorithm_version,
            raw.fingerprint_key_id,
            raw.fingerprint_key_version,
            raw.fingerprint,
        ) {
            (None, None, None, None, None) => None,
            (
                Some(algorithm),
                Some(algorithm_version),
                Some(key_id),
                Some(key_version),
                Some(fingerprint),
            ) => Some(PrivateFingerprint::from_db(
                &algorithm,
                algorithm_version,
                key_id,
                key_version,
                fingerprint,
            )?),
            _ => {
                return Err(LedgerError::InvalidStoredState {
                    kind: "event private fingerprint metadata",
                    value: "partially populated".to_string(),
                });
            }
        };
        let public_identity = raw
            .public_identity
            .map(CanonicalJson::from_jcs_text)
            .transpose()?;
        if public_identity.is_some() != private_fingerprint.is_some() {
            return Err(LedgerError::InvalidStoredState {
                kind: "event append identity",
                value: "public identity and fingerprint must be present together".to_string(),
            });
        }
        let append_key = raw.append_key.map(AppendKey::new).transpose()?;
        let effect_intent = EffectIntent::from_db(&raw.effect_intent)?;
        let artifact_degradation_reason = raw
            .artifact_degradation_reason
            .map(|value| ArtifactDegradationReason::from_db(&value))
            .transpose()?;
        let sealed_artifact_ref = parse_optional_artifact_ref(
            raw.sealed_artifact_envelope_digest,
            raw.sealed_artifact_key_id,
            raw.sealed_artifact_schema_version,
            raw.sealed_artifact_encryption_algorithm,
            raw.sealed_artifact_canonicalization_algorithm,
            raw.sealed_artifact_canonicalization_version,
            raw.sealed_artifact_redactor_version,
        )?;
        let public_payload = CanonicalJson::from_jcs_text(raw.public_payload)?;
        let kind = SessionRunEventKind::from_db(&raw.kind)?;
        let valid_kind_state = match kind {
            SessionRunEventKind::Append => {
                append_key.is_some()
                    && public_identity.is_some()
                    && artifact_degradation_reason.is_none()
            }
            SessionRunEventKind::ManualAppendDegradedAudit => {
                append_key.is_some()
                    && public_identity.is_none()
                    && private_fingerprint.is_none()
                    && effect_intent == EffectIntent::None
                    && artifact_degradation_reason.is_some()
                    && sealed_artifact_ref.is_none()
            }
            SessionRunEventKind::DeliveryResult => {
                append_key.is_none()
                    && effect_intent == EffectIntent::None
                    && artifact_degradation_reason.is_none()
                    && ((public_identity.is_none()
                        && private_fingerprint.is_none()
                        && sealed_artifact_ref.is_none())
                        || (public_identity.is_some()
                            && private_fingerprint.is_some()
                            && sealed_artifact_ref.is_some()))
            }
            SessionRunEventKind::RunStart => {
                append_key.is_none()
                    && artifact_degradation_reason.is_none()
                    && ((public_identity.is_none()
                        && private_fingerprint.is_none()
                        && sealed_artifact_ref.is_none())
                        || (public_identity.is_some()
                            && private_fingerprint.is_some()
                            && sealed_artifact_ref.is_some()))
            }
            SessionRunEventKind::RunExit => {
                append_key.is_none()
                    && public_identity.is_none()
                    && private_fingerprint.is_none()
                    && artifact_degradation_reason.is_none()
                    && sealed_artifact_ref.is_none()
            }
            SessionRunEventKind::ArtifactStateAudit => {
                append_key.is_none()
                    && public_identity.is_none()
                    && private_fingerprint.is_none()
                    && effect_intent == EffectIntent::None
                    && artifact_degradation_reason.is_none()
                    && sealed_artifact_ref.is_none()
                    && validate_artifact_state_audit_payload(&public_payload).is_ok()
            }
        };
        if !valid_kind_state {
            return Err(LedgerError::InvalidStoredState {
                kind: "event kind fields",
                value: kind.as_str().to_string(),
            });
        }
        if kind == SessionRunEventKind::DeliveryResult {
            validate_delivery_result_storage_shape(&public_payload, public_identity.is_some())?;
        }
        let run_provenance = raw
            .run_provenance
            .map(CanonicalJson::from_jcs_text)
            .transpose()?;
        if (kind == SessionRunEventKind::RunStart) != run_provenance.is_some() {
            return Err(LedgerError::InvalidStoredState {
                kind: "event run provenance",
                value: "only run_start may contain provenance".to_string(),
            });
        }
        Ok(Self {
            event_id: parse_event_id(raw.event_id)?,
            run_id: parse_run_id(raw.run_id)?,
            sequence: parse_positive_u64("event sequence", raw.sequence)?,
            kind,
            append_key,
            effect_intent,
            artifact_degradation_reason,
            public_identity,
            private_fingerprint,
            sealed_artifact_ref,
            run_provenance,
            public_payload,
            manifest_hash: validated_digest_string("manifest hash", raw.manifest_hash)?,
            previous_hash: validated_digest_string("previous hash", raw.previous_hash)?,
            event_hash: validated_digest_string("event hash", raw.event_hash)?,
            created_at_ms: raw.created_at_ms,
        })
    }
}

struct RawOutboxRecord {
    outbox_id: String,
    run_id: String,
    event_id: String,
    destination: String,
    required: i64,
    idempotency_key: String,
    public_payload: String,
    sealed_artifact_envelope_digest: String,
    sealed_artifact_key_id: String,
    sealed_artifact_schema_version: i64,
    sealed_artifact_encryption_algorithm: String,
    sealed_artifact_canonicalization_algorithm: String,
    sealed_artifact_canonicalization_version: i64,
    sealed_artifact_redactor_version: String,
    state: String,
    attempt_count: i64,
    max_attempts: i64,
    lease_generation: i64,
    delivery_receipt: Option<String>,
    delivery_error: Option<String>,
}

impl TryFrom<RawOutboxRecord> for OutboxRecord {
    type Error = LedgerError;

    fn try_from(raw: RawOutboxRecord) -> Result<Self, Self::Error> {
        Ok(Self {
            outbox_id: parse_outbox_id(raw.outbox_id)?,
            run_id: parse_run_id(raw.run_id)?,
            event_id: parse_event_id(raw.event_id)?,
            destination: Destination::from_db(&raw.destination)?,
            idempotency_key: DownstreamIdempotencyKey::from_sha256_hex(raw.idempotency_key)?,
            required: parse_bool("outbox required", raw.required)?,
            public_payload: CanonicalJson::from_jcs_text(raw.public_payload)?,
            sealed_artifact_ref: SealedArtifactRef::from_db(
                raw.sealed_artifact_envelope_digest,
                raw.sealed_artifact_key_id,
                raw.sealed_artifact_schema_version,
                raw.sealed_artifact_encryption_algorithm,
                raw.sealed_artifact_canonicalization_algorithm,
                raw.sealed_artifact_canonicalization_version,
                raw.sealed_artifact_redactor_version,
            )?,
            state: OutboxState::from_db(&raw.state)?,
            attempt_count: parse_nonnegative_u32("outbox attempt count", raw.attempt_count)?,
            max_attempts: parse_positive_u32("outbox maximum attempts", raw.max_attempts)?,
            lease_generation: parse_nonnegative_u64(
                "outbox lease generation",
                raw.lease_generation,
            )?,
            delivery_receipt: raw
                .delivery_receipt
                .map(DurableDeliveryReceipt::new)
                .transpose()?,
            delivery_error: raw
                .delivery_error
                .map(|value| DeliveryFailureReason::from_db(&value))
                .transpose()?,
        })
    }
}

struct RawAppendReceipt {
    event_id: String,
    run_id: String,
    event_sequence: i64,
    event_hash: String,
    sealed_artifact_envelope_digest: Option<String>,
    sealed_artifact_key_id: Option<String>,
    sealed_artifact_schema_version: Option<i64>,
    sealed_artifact_encryption_algorithm: Option<String>,
    sealed_artifact_canonicalization_algorithm: Option<String>,
    sealed_artifact_canonicalization_version: Option<i64>,
    sealed_artifact_redactor_version: Option<String>,
    event_kind: String,
    artifact_degradation_reason: Option<String>,
}

fn raw_reservation(row: &Row<'_>) -> rusqlite::Result<RawReservation> {
    Ok(RawReservation {
        append_key: row.get(0)?,
        run_id: row.get(1)?,
        event_id: row.get(2)?,
        frozen_event_at_ms: row.get(3)?,
        public_identity_jcs: row.get(4)?,
        private_fingerprint_algorithm: row.get(5)?,
        private_fingerprint_algorithm_version: row.get(6)?,
        private_fingerprint_key_id: row.get(7)?,
        private_fingerprint_key_version: row.get(8)?,
        private_fingerprint: row.get(9)?,
        effect_intent: row.get(10)?,
        public_payload_jcs: row.get(11)?,
        sealed_artifact_envelope_digest: row.get(12)?,
        sealed_artifact_key_id: row.get(13)?,
        sealed_artifact_schema_version: row.get(14)?,
        sealed_artifact_encryption_algorithm: row.get(15)?,
        sealed_artifact_canonicalization_algorithm: row.get(16)?,
        sealed_artifact_canonicalization_version: row.get(17)?,
        sealed_artifact_redactor_version: row.get(18)?,
        lease_generation: row.get(19)?,
        lease_token: row.get(20)?,
        lease_expires_at_ms: row.get(21)?,
        committed_event_id: row.get(22)?,
        terminal_audit_event_id: row.get(23)?,
    })
}

fn raw_outbox_candidate(row: &Row<'_>) -> rusqlite::Result<RawOutboxCandidate> {
    Ok(RawOutboxCandidate {
        outbox_id: row.get(0)?,
        run_id: row.get(1)?,
        event_id: row.get(2)?,
        destination: row.get(3)?,
        required: row.get(4)?,
        idempotency_key: row.get(5)?,
        public_payload: row.get(6)?,
        sealed_artifact_envelope_digest: row.get(7)?,
        sealed_artifact_key_id: row.get(8)?,
        sealed_artifact_schema_version: row.get(9)?,
        sealed_artifact_encryption_algorithm: row.get(10)?,
        sealed_artifact_canonicalization_algorithm: row.get(11)?,
        sealed_artifact_canonicalization_version: row.get(12)?,
        sealed_artifact_redactor_version: row.get(13)?,
        event_sealed_artifact_envelope_digest: row.get(14)?,
        event_sealed_artifact_key_id: row.get(15)?,
        event_sealed_artifact_schema_version: row.get(16)?,
        event_sealed_artifact_encryption_algorithm: row.get(17)?,
        event_sealed_artifact_canonicalization_algorithm: row.get(18)?,
        event_sealed_artifact_canonicalization_version: row.get(19)?,
        event_sealed_artifact_redactor_version: row.get(20)?,
        attempt_count: row.get(21)?,
        max_attempts: row.get(22)?,
        lease_generation: row.get(23)?,
        state: row.get(24)?,
        lease_token: row.get(25)?,
        lease_expires_at_ms: row.get(26)?,
    })
}

fn raw_outbox_fence(row: &Row<'_>) -> rusqlite::Result<RawOutboxFence> {
    Ok(RawOutboxFence {
        outbox_id: row.get(0)?,
        run_id: row.get(1)?,
        event_id: row.get(2)?,
        required: row.get(3)?,
        attempt_count: row.get(4)?,
        max_attempts: row.get(5)?,
        lease_generation: row.get(6)?,
        lease_token: row.get(7)?,
        state: row.get(8)?,
    })
}

fn raw_run_status(row: &Row<'_>) -> rusqlite::Result<RawRunStatus> {
    Ok(RawRunStatus {
        exit_event_id: row.get(0)?,
        exit_terminal_reason: row.get(1)?,
        candidate_outcome: row.get(2)?,
        final_outcome: row.get(3)?,
        required_dead_lettered: row.get(4)?,
        pending_required_delivery: row.get(5)?,
        exact_replay: row.get(6)?,
        exact_replay_state: row.get(7)?,
        exact_replay_reason: row.get(8)?,
        exact_replay_transition_sequence: row.get(9)?,
        exact_replay_latched_at_ms: row.get(10)?,
        exact_replay_latched_by_event_id: row.get(11)?,
    })
}

fn raw_run_header(row: &Row<'_>) -> rusqlite::Result<RawRunHeader> {
    Ok(RawRunHeader {
        start_event_id: row.get(0)?,
        execution_mode: row.get(1)?,
        execution_mode_source: row.get(2)?,
        started_at_ms: row.get(3)?,
        owner_id: row.get(4)?,
        owner_lease_generation: row.get(5)?,
        owner_lease_expires_at_ms: row.get(6)?,
        provenance_canonicalization_algorithm: row.get(7)?,
        provenance_canonicalization_version: row.get(8)?,
        session_id: row.get(9)?,
        parent_run_id: row.get(10)?,
        branch_id: row.get(11)?,
        tenant_id: row.get(12)?,
        action_id: row.get(13)?,
        objective_id: row.get(14)?,
        task_id: row.get(15)?,
        model_id: row.get(16)?,
        provider_id: row.get(17)?,
        config_digest: row.get(18)?,
        prompt_digest: row.get(19)?,
        prompt_digest_absence: row.get(20)?,
        input_digest: row.get(21)?,
        input_digest_absence: row.get(22)?,
        output_digest: row.get(23)?,
        output_digest_absence: row.get(24)?,
        tool_digest: row.get(25)?,
        tool_digest_absence: row.get(26)?,
        skill_digest: row.get(27)?,
        skill_digest_absence: row.get(28)?,
        policy_digest: row.get(29)?,
        policy_digest_absence: row.get(30)?,
        sandbox_digest: row.get(31)?,
        sandbox_digest_absence: row.get(32)?,
    })
}

fn raw_event(row: &Row<'_>) -> rusqlite::Result<RawEvent> {
    Ok(RawEvent {
        event_id: row.get(0)?,
        run_id: row.get(1)?,
        sequence: row.get(2)?,
        kind: row.get(3)?,
        append_key: row.get(4)?,
        effect_intent: row.get(5)?,
        artifact_degradation_reason: row.get(6)?,
        public_identity: row.get(7)?,
        fingerprint_algorithm: row.get(8)?,
        fingerprint_algorithm_version: row.get(9)?,
        fingerprint_key_id: row.get(10)?,
        fingerprint_key_version: row.get(11)?,
        fingerprint: row.get(12)?,
        sealed_artifact_envelope_digest: row.get(13)?,
        sealed_artifact_key_id: row.get(14)?,
        sealed_artifact_schema_version: row.get(15)?,
        sealed_artifact_encryption_algorithm: row.get(16)?,
        sealed_artifact_canonicalization_algorithm: row.get(17)?,
        sealed_artifact_canonicalization_version: row.get(18)?,
        sealed_artifact_redactor_version: row.get(19)?,
        run_provenance: row.get(20)?,
        public_payload: row.get(21)?,
        manifest_hash: row.get(22)?,
        previous_hash: row.get(23)?,
        event_hash: row.get(24)?,
        created_at_ms: row.get(25)?,
    })
}

fn raw_outbox_record(row: &Row<'_>) -> rusqlite::Result<RawOutboxRecord> {
    Ok(RawOutboxRecord {
        outbox_id: row.get(0)?,
        run_id: row.get(1)?,
        event_id: row.get(2)?,
        destination: row.get(3)?,
        required: row.get(4)?,
        idempotency_key: row.get(5)?,
        public_payload: row.get(6)?,
        sealed_artifact_envelope_digest: row.get(7)?,
        sealed_artifact_key_id: row.get(8)?,
        sealed_artifact_schema_version: row.get(9)?,
        sealed_artifact_encryption_algorithm: row.get(10)?,
        sealed_artifact_canonicalization_algorithm: row.get(11)?,
        sealed_artifact_canonicalization_version: row.get(12)?,
        sealed_artifact_redactor_version: row.get(13)?,
        state: row.get(14)?,
        attempt_count: row.get(15)?,
        max_attempts: row.get(16)?,
        lease_generation: row.get(17)?,
        delivery_receipt: row.get(18)?,
        delivery_error: row.get(19)?,
    })
}

fn raw_append_receipt(row: &Row<'_>) -> rusqlite::Result<RawAppendReceipt> {
    Ok(RawAppendReceipt {
        event_id: row.get(0)?,
        run_id: row.get(1)?,
        event_sequence: row.get(2)?,
        event_hash: row.get(3)?,
        sealed_artifact_envelope_digest: row.get(4)?,
        sealed_artifact_key_id: row.get(5)?,
        sealed_artifact_schema_version: row.get(6)?,
        sealed_artifact_encryption_algorithm: row.get(7)?,
        sealed_artifact_canonicalization_algorithm: row.get(8)?,
        sealed_artifact_canonicalization_version: row.get(9)?,
        sealed_artifact_redactor_version: row.get(10)?,
        event_kind: row.get(11)?,
        artifact_degradation_reason: row.get(12)?,
    })
}

fn load_outbox_records_for_run(
    connection: &rusqlite::Connection,
    run_id: RunId,
) -> LedgerResult<Vec<OutboxRecord>> {
    let mut statement = connection.prepare(
        "SELECT outbox_id, run_id, event_id, destination, required, idempotency_key,
                public_payload_jcs,
                sealed_artifact_envelope_digest, sealed_artifact_key_id,
                sealed_artifact_schema_version, sealed_artifact_encryption_algorithm,
                sealed_artifact_canonicalization_algorithm,
                sealed_artifact_canonicalization_version, sealed_artifact_redactor_version,
                state, attempt_count, max_attempts, lease_generation,
                delivery_receipt, delivery_error
         FROM session_run_outbox WHERE run_id = ?1 ORDER BY event_id, manifest_order",
    )?;
    let rows = statement
        .query_map([run_id.to_string()], raw_outbox_record)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().map(OutboxRecord::try_from).collect()
}

fn load_outbox_records_for_event(
    connection: &rusqlite::Connection,
    event_id: EventId,
) -> LedgerResult<Vec<OutboxRecord>> {
    let mut statement = connection.prepare(
        "SELECT outbox_id, run_id, event_id, destination, required, idempotency_key,
                public_payload_jcs,
                sealed_artifact_envelope_digest, sealed_artifact_key_id,
                sealed_artifact_schema_version, sealed_artifact_encryption_algorithm,
                sealed_artifact_canonicalization_algorithm,
                sealed_artifact_canonicalization_version, sealed_artifact_redactor_version,
                state, attempt_count, max_attempts, lease_generation,
                delivery_receipt, delivery_error
         FROM session_run_outbox WHERE event_id = ?1 ORDER BY manifest_order",
    )?;
    let rows = statement
        .query_map([event_id.to_string()], raw_outbox_record)?
        .collect::<Result<Vec<_>, _>>()?;
    rows.into_iter().map(OutboxRecord::try_from).collect()
}

pub(crate) fn verify_event_delivery_projection(
    event: &SessionRunEvent,
    manifest_jcs: &CanonicalJson,
    manifest_hash: &str,
    actual_outbox: &[OutboxRecord],
) -> LedgerResult<()> {
    let expected = DispatchPolicyV1::derive(
        event.event_id,
        event.effect_intent,
        &event.public_payload,
        event.sealed_artifact_ref.as_ref(),
    )
    .map_err(|error| LedgerError::IntegrityViolation(error.to_string()))?;
    verify_event_delivery_projection_with_manifest(
        event,
        manifest_jcs,
        manifest_hash,
        &expected,
        actual_outbox.iter(),
    )
}

fn verify_event_delivery_projection_with_manifest<'a>(
    event: &SessionRunEvent,
    manifest_jcs: &CanonicalJson,
    manifest_hash: &str,
    expected: &DeliveryManifest,
    actual_outbox: impl Iterator<Item = &'a OutboxRecord>,
) -> LedgerResult<()> {
    if manifest_hash != event.manifest_hash
        || sha256_hex(manifest_jcs.as_str().as_bytes()) != manifest_hash
        || *manifest_jcs != expected.canonical_json
    {
        return Err(LedgerError::IntegrityViolation(
            "event manifest does not equal the policy-derived canonical manifest".to_string(),
        ));
    }
    let mut actual_outbox = actual_outbox;
    for expected in &expected.entries {
        let actual = actual_outbox.next().ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "event outbox cardinality does not equal its manifest".to_string(),
            )
        })?;
        if actual.run_id != event.run_id
            || actual.event_id != event.event_id
            || actual.destination != expected.destination
            || actual.required != expected.required
            || actual.idempotency_key != expected.idempotency_key
            || actual.public_payload != expected.public_payload
            || actual.sealed_artifact_ref != expected.sealed_artifact_ref
            || event.sealed_artifact_ref.as_ref() != Some(&actual.sealed_artifact_ref)
        {
            return Err(LedgerError::IntegrityViolation(
                "outbox reference or projection does not equal the event manifest".to_string(),
            ));
        }
    }
    if actual_outbox.next().is_some() {
        return Err(LedgerError::IntegrityViolation(
            "event outbox cardinality does not equal its manifest".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn verify_event_nonresumable_projection(
    event: &SessionRunEvent,
    effects: &[NonResumableEffectSnapshot],
    all_events: &[SessionRunEvent],
) -> LedgerResult<()> {
    let matching = effects
        .iter()
        .filter(|effect| effect.record.event_id == event.event_id)
        .collect::<Vec<_>>();
    if event.effect_intent != EffectIntent::LocalOneShotV1 {
        if matching.is_empty() {
            return Ok(());
        }
        return Err(LedgerError::IntegrityViolation(
            "non-local event has a non-resumable effect projection".to_string(),
        ));
    }
    if matching.len() != 1 {
        return Err(LedgerError::IntegrityViolation(
            "local one-shot event must have exactly one effect projection".to_string(),
        ));
    }
    let effect = matching[0];
    let record = &effect.record;
    if record.run_id != event.run_id
        || record.idempotency_key != derive_nonresumable_idempotency_key(event.event_id)?
    {
        return Err(LedgerError::IntegrityViolation(
            "non-resumable effect identity does not match its source event".to_string(),
        ));
    }
    let shape_valid = match record.state {
        NonResumableEffectState::Armed => {
            effect.claim_binding.is_none()
                && record.claimed_at_ms.is_none()
                && record.result_payload_digest.is_none()
                && record.result_event_id.is_none()
                && record.terminal_at_ms.is_none()
        }
        NonResumableEffectState::Executing => {
            effect.claim_binding.is_some()
                && record.claimed_at_ms.is_some()
                && record.result_event_id.is_none()
                && record.terminal_at_ms.is_none()
        }
        NonResumableEffectState::Completed => {
            effect.claim_binding.is_some()
                && record.claimed_at_ms.is_some()
                && record.result_payload_digest.is_some()
                && record.result_event_id.is_some()
                && record.terminal_at_ms.is_some()
        }
        NonResumableEffectState::Abandoned => {
            effect.claim_binding.is_none()
                && record.claimed_at_ms.is_none()
                && record.result_payload_digest.is_none()
                && record.result_event_id.is_none()
                && record.terminal_at_ms.is_some()
        }
        NonResumableEffectState::UnknownNonResumable => {
            effect.claim_binding.is_some()
                && record.claimed_at_ms.is_some()
                && record.result_event_id.is_none()
                && record.terminal_at_ms.is_some()
        }
    };
    if !shape_valid {
        return Err(LedgerError::IntegrityViolation(
            "non-resumable effect state has inconsistent evidence fields".to_string(),
        ));
    }
    if record.state == NonResumableEffectState::Completed {
        let result_event_id = record.result_event_id.ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "completed non-resumable effect lacks a result event".to_string(),
            )
        })?;
        let result = all_events
            .iter()
            .find(|candidate| candidate.event_id == result_event_id)
            .ok_or_else(|| {
                LedgerError::IntegrityViolation(
                    "non-resumable result event is outside the source run".to_string(),
                )
            })?;
        let claim_binding = effect.claim_binding.as_ref().ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "completed non-resumable effect lacks a claim binding".to_string(),
            )
        })?;
        let result_payload_digest = record.result_payload_digest.as_ref().ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "completed non-resumable effect lacks a result commitment".to_string(),
            )
        })?;
        verify_nonresumable_result_projection(event, result, claim_binding, result_payload_digest)?;
    }
    Ok(())
}

fn verify_nonresumable_result_projection(
    source: &SessionRunEvent,
    result: &SessionRunEvent,
    claim_binding: &Sha256Digest,
    result_payload_digest: &Sha256Digest,
) -> LedgerResult<()> {
    let expected_append_key =
        derive_nonresumable_result_append_key(source.event_id, claim_binding)?;
    let payload_value = result.public_payload.as_value()?;
    let observed_payload = payload_value
        .as_object()
        .and_then(|object| object.get("payload"))
        .ok_or_else(|| {
            LedgerError::IntegrityViolation(
                "non-resumable result payload lacks its wrapped observation".to_string(),
            )
        })?;
    let expected_payload = CanonicalJson::from_value(&json!({
        "schema": NONRESUMABLE_RESULT_SCHEMA,
        "schema_version": RUN_EVENT_BINDING_SCHEMA_VERSION,
        "source_event_id": source.event_id,
        "claim_binding": claim_binding,
        "payload": observed_payload,
    }))?;
    let actual_result_digest =
        Sha256Digest::new(sha256_hex(result.public_payload.as_str().as_bytes()))?;
    if source.effect_intent != EffectIntent::LocalOneShotV1
        || result.run_id != source.run_id
        || result.sequence <= source.sequence
        || result.kind != SessionRunEventKind::Append
        || result.append_key.as_ref() != Some(&expected_append_key)
        || result.effect_intent != EffectIntent::None
        || result.sealed_artifact_ref.is_none()
        || result.public_payload != expected_payload
        || actual_result_digest != *result_payload_digest
    {
        return Err(LedgerError::IntegrityViolation(
            "non-resumable result event does not match its claim-bound source".to_string(),
        ));
    }
    Ok(())
}

fn verify_event_delivery_projection_in_db(
    factory: &LedgerConnectionFactory,
    event: &SessionRunEvent,
) -> LedgerResult<()> {
    let connection = factory.open()?;
    let (manifest_jcs, manifest_hash): (String, String) = connection.query_row(
        "SELECT manifest_jcs, manifest_hash FROM session_run_manifests WHERE event_id = ?1",
        [event.event_id.to_string()],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let manifest_jcs = CanonicalJson::from_jcs_text(manifest_jcs).map_err(|_| {
        LedgerError::IntegrityViolation("manifest is not canonical JCS-v1".to_string())
    })?;
    let actual_outbox = load_outbox_records_for_event(&connection, event.event_id)?;
    verify_event_delivery_projection(event, &manifest_jcs, &manifest_hash, &actual_outbox)
}

fn verify_run_snapshot_contents(
    header: &RunHeader,
    status: &RunStatus,
    head_hash: &str,
    events: &[SessionRunEvent],
    manifests: &[LedgerManifestSnapshot],
    outbox: &[LedgerOutboxSnapshot],
    nonresumable_effects: &[NonResumableEffectSnapshot],
) -> LedgerResult<()> {
    if header.run_id != status.run_id {
        return Err(LedgerError::IntegrityViolation(
            "snapshot header and status name different runs".to_string(),
        ));
    }
    let first = events.first().ok_or_else(|| {
        LedgerError::IntegrityViolation("snapshot has no immutable run-start event".to_string())
    })?;
    if first.event_id != header.start_event_id || first.kind != SessionRunEventKind::RunStart {
        return Err(LedgerError::IntegrityViolation(
            "snapshot first event does not equal the run header start event".to_string(),
        ));
    }
    if events.len() != manifests.len() {
        return Err(LedgerError::IntegrityViolation(
            "snapshot event and manifest counts differ".to_string(),
        ));
    }

    let expected_provenance = header.provenance.canonical_json()?;
    let mut previous_hash = genesis_hash();
    let mut run_start_count = 0_u64;
    let mut accounted_outbox = 0_usize;
    let mut accounted_nonresumable = 0_usize;
    for (index, (event, manifest)) in events.iter().zip(manifests.iter()).enumerate() {
        let expected_sequence = u64::try_from(index)
            .map_err(|_| LedgerError::NumericOverflow)?
            .checked_add(1)
            .ok_or(LedgerError::NumericOverflow)?;
        if event.run_id != header.run_id
            || event.sequence != expected_sequence
            || event.previous_hash != previous_hash
            || manifest.event_id != event.event_id
        {
            return Err(LedgerError::IntegrityViolation(
                "snapshot event ordering, chain link, or manifest linkage is invalid".to_string(),
            ));
        }
        if event.kind == SessionRunEventKind::RunStart {
            run_start_count = run_start_count
                .checked_add(1)
                .ok_or(LedgerError::NumericOverflow)?;
            if event.run_provenance.as_ref() != Some(&expected_provenance) {
                return Err(LedgerError::IntegrityViolation(
                    "run-start provenance does not equal the immutable run header".to_string(),
                ));
            }
        }
        if recompute_event_hash(event)? != event.event_hash {
            return Err(LedgerError::IntegrityViolation(
                "snapshot event hash does not match its canonical body".to_string(),
            ));
        }

        let expected_manifest = DispatchPolicyV1::derive(
            event.event_id,
            event.effect_intent,
            &event.public_payload,
            event.sealed_artifact_ref.as_ref(),
        )?;
        let mut event_outbox = outbox
            .iter()
            .filter(|entry| entry.record.event_id == event.event_id);
        for expected in &expected_manifest.entries {
            let actual = event_outbox.next().ok_or_else(|| {
                LedgerError::IntegrityViolation(
                    "snapshot outbox cardinality does not equal its manifest".to_string(),
                )
            })?;
            if actual.manifest_order != expected.order {
                return Err(LedgerError::IntegrityViolation(
                    "snapshot outbox manifest order does not equal the derived policy".to_string(),
                ));
            }
        }
        if event_outbox.next().is_some() {
            return Err(LedgerError::IntegrityViolation(
                "snapshot outbox cardinality does not equal its manifest".to_string(),
            ));
        }
        accounted_outbox = accounted_outbox
            .checked_add(expected_manifest.entries.len())
            .ok_or(LedgerError::NumericOverflow)?;
        verify_event_delivery_projection_with_manifest(
            event,
            &manifest.canonical_json,
            &manifest.hash,
            &expected_manifest,
            outbox
                .iter()
                .filter(|entry| entry.record.event_id == event.event_id)
                .map(|entry| &entry.record),
        )?;
        verify_event_nonresumable_projection(event, nonresumable_effects, events)?;
        if event.effect_intent == EffectIntent::LocalOneShotV1 {
            accounted_nonresumable = accounted_nonresumable
                .checked_add(1)
                .ok_or(LedgerError::NumericOverflow)?;
        }
        previous_hash = event.event_hash.clone();
    }
    if run_start_count != 1
        || accounted_outbox != outbox.len()
        || accounted_nonresumable != nonresumable_effects.len()
        || previous_hash != head_hash
    {
        return Err(LedgerError::IntegrityViolation(
            "snapshot start count, projection linkage, or head watermark is invalid".to_string(),
        ));
    }
    verify_run_status_projection(status, events, outbox)?;
    let unknown_count = nonresumable_effects
        .iter()
        .filter(|effect| effect.record.state == NonResumableEffectState::UnknownNonResumable)
        .count();
    let has_active = nonresumable_effects.iter().any(|effect| {
        matches!(
            effect.record.state,
            NonResumableEffectState::Armed | NonResumableEffectState::Executing
        )
    });
    let terminal_reason = status.exit_terminal_reason;
    if terminal_reason.is_some() && has_active {
        return Err(LedgerError::IntegrityViolation(
            "terminal run retains an armed or executing non-resumable effect".to_string(),
        ));
    }
    if terminal_reason == Some(TerminalReason::Completed)
        && nonresumable_effects
            .iter()
            .any(|effect| effect.record.state != NonResumableEffectState::Completed)
    {
        return Err(LedgerError::IntegrityViolation(
            "completed run contains an incomplete non-resumable effect".to_string(),
        ));
    }
    if terminal_reason.is_none()
        && nonresumable_effects.iter().any(|effect| {
            matches!(
                effect.record.state,
                NonResumableEffectState::Abandoned | NonResumableEffectState::UnknownNonResumable
            )
        })
    {
        return Err(LedgerError::IntegrityViolation(
            "non-terminal run contains a terminalized non-resumable effect".to_string(),
        ));
    }
    if terminal_reason.is_some()
        && ((terminal_reason == Some(TerminalReason::EffectNonResumableUnknown))
            != (unknown_count > 0))
    {
        return Err(LedgerError::IntegrityViolation(
            "terminal reason and unknown non-resumable effect projection disagree".to_string(),
        ));
    }
    Ok(())
}

fn verify_run_status_projection(
    status: &RunStatus,
    events: &[SessionRunEvent],
    outbox: &[LedgerOutboxSnapshot],
) -> LedgerResult<()> {
    let exit_events = events
        .iter()
        .filter(|event| event.kind == SessionRunEventKind::RunExit)
        .collect::<Vec<_>>();
    let required_dead_lettered = outbox
        .iter()
        .any(|entry| entry.record.required && entry.record.state == OutboxState::DeadLettered);
    let Some(terminal_reason) = status.exit_terminal_reason else {
        let expected_final = required_dead_lettered.then_some(RunOutcome::DeadLettered);
        if !exit_events.is_empty()
            || status.exit_event_id.is_some()
            || status.candidate_outcome.is_some()
            || status.required_dead_lettered != required_dead_lettered
            || status.pending_required_delivery
            || status.final_outcome != expected_final
        {
            return Err(LedgerError::IntegrityViolation(
                "non-terminal run status disagrees with its event and outbox projections"
                    .to_string(),
            ));
        }
        return Ok(());
    };
    if exit_events.len() != 1 || status.exit_event_id != Some(exit_events[0].event_id) {
        return Err(LedgerError::IntegrityViolation(
            "terminal run must contain exactly one run-exit event".to_string(),
        ));
    }
    let binding = exit_events[0].public_payload.as_value()?;
    let object = binding.as_object().ok_or_else(|| {
        LedgerError::IntegrityViolation("run-exit binding is not an object".to_string())
    })?;
    if object.len() != 4
        || object.get("schema").and_then(serde_json::Value::as_str) != Some(RUN_EXIT_BINDING_SCHEMA)
        || object
            .get("schema_version")
            .and_then(serde_json::Value::as_u64)
            != Some(u64::from(RUN_EVENT_BINDING_SCHEMA_VERSION))
        || object
            .get("terminal_reason")
            .and_then(serde_json::Value::as_str)
            != Some(terminal_reason.as_str())
        || !object.contains_key("payload")
    {
        return Err(LedgerError::IntegrityViolation(
            "run status terminal reason is not bound by its run-exit event".to_string(),
        ));
    }

    let unresolved_required = outbox
        .iter()
        .any(|entry| entry.record.required && entry.record.state != OutboxState::Acknowledged);
    let (pending_required_delivery, final_outcome) = if required_dead_lettered {
        (false, Some(RunOutcome::DeadLettered))
    } else if unresolved_required {
        (true, None)
    } else {
        (false, Some(terminal_reason.outcome()))
    };
    if status.candidate_outcome != Some(terminal_reason.outcome())
        || status.required_dead_lettered != required_dead_lettered
        || status.pending_required_delivery != pending_required_delivery
        || status.final_outcome != final_outcome
    {
        return Err(LedgerError::IntegrityViolation(
            "run status outcomes do not reconcile with its exit and outbox".to_string(),
        ));
    }
    Ok(())
}

fn parse_optional_artifact_ref(
    envelope_digest: Option<String>,
    key_id: Option<String>,
    schema_version: Option<i64>,
    encryption_algorithm: Option<String>,
    canonicalization_algorithm: Option<String>,
    canonicalization_version: Option<i64>,
    redactor_version: Option<String>,
) -> LedgerResult<Option<SealedArtifactRef>> {
    match (
        envelope_digest,
        key_id,
        schema_version,
        encryption_algorithm,
        canonicalization_algorithm,
        canonicalization_version,
        redactor_version,
    ) {
        (None, None, None, None, None, None, None) => Ok(None),
        (
            Some(envelope_digest),
            Some(key_id),
            Some(schema_version),
            Some(encryption_algorithm),
            Some(canonicalization_algorithm),
            Some(canonicalization_version),
            Some(redactor_version),
        ) => Ok(Some(SealedArtifactRef::from_db(
            envelope_digest,
            key_id,
            schema_version,
            encryption_algorithm,
            canonicalization_algorithm,
            canonicalization_version,
            redactor_version,
        )?)),
        _ => Err(LedgerError::InvalidStoredState {
            kind: "sealed artifact reference",
            value: "partially populated".to_string(),
        }),
    }
}

fn parse_run_id(value: String) -> LedgerResult<RunId> {
    value.parse().map_err(|_| LedgerError::InvalidStoredState {
        kind: "run id",
        value,
    })
}

fn parse_event_id(value: String) -> LedgerResult<EventId> {
    value.parse().map_err(|_| LedgerError::InvalidStoredState {
        kind: "event id",
        value,
    })
}

fn parse_outbox_id(value: String) -> LedgerResult<OutboxRowId> {
    value.parse().map_err(|_| LedgerError::InvalidStoredState {
        kind: "outbox id",
        value,
    })
}

fn parse_replay_operation_instance_id(value: String) -> LedgerResult<String> {
    if value.is_empty() || value.len() > 256 {
        return Err(LedgerError::InvalidStoredState {
            kind: "replay operation instance id",
            value,
        });
    }
    Ok(value)
}

fn parse_bool(kind: &'static str, value: i64) -> LedgerResult<bool> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(LedgerError::InvalidStoredState {
            kind,
            value: value.to_string(),
        }),
    }
}

fn parse_nonnegative_u64(kind: &'static str, value: i64) -> LedgerResult<u64> {
    u64::try_from(value).map_err(|_| LedgerError::InvalidStoredState {
        kind,
        value: value.to_string(),
    })
}

fn parse_positive_u64(kind: &'static str, value: i64) -> LedgerResult<u64> {
    let value = parse_nonnegative_u64(kind, value)?;
    if value == 0 {
        return Err(LedgerError::InvalidStoredState {
            kind,
            value: "0".to_string(),
        });
    }
    Ok(value)
}

fn parse_nonnegative_u32(kind: &'static str, value: i64) -> LedgerResult<u32> {
    u32::try_from(value).map_err(|_| LedgerError::InvalidStoredState {
        kind,
        value: value.to_string(),
    })
}

fn parse_positive_u32(kind: &'static str, value: i64) -> LedgerResult<u32> {
    let value = parse_nonnegative_u32(kind, value)?;
    if value == 0 {
        return Err(LedgerError::InvalidStoredState {
            kind,
            value: "0".to_string(),
        });
    }
    Ok(value)
}

fn validated_digest_string(kind: &'static str, value: String) -> LedgerResult<String> {
    Sha256Digest::new(value)
        .map(|digest| digest.as_str().to_string())
        .map_err(|_| LedgerError::InvalidStoredState {
            kind,
            value: "not a full lowercase SHA-256 digest".to_string(),
        })
}
