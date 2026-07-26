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
    BranchReplaySourceSnapshot, CanonicalJson, ClaimNext, CreatedRun, DeliveryCompletion,
    DeliveryCompletionReceipt, DeliveryEvidence, DeliveryFailureReason, Destination, DigestAbsence,
    DownstreamIdempotencyKey, DurableDeliveryReceipt, EffectIntent, EventId, ExactReplayState,
    ExactReplayStatus, LeaseToken, NonResumableEffectAttempt, NonResumableEffectLease,
    NonResumableEffectRecord, NonResumableEffectSnapshot, NonResumableEffectState,
    NonResumableIdempotencyKey, OutboxLease, OutboxRecord, OutboxRowId, OutboxState,
    PrivateFingerprint, ProvenanceDigest, ReplayAppendContract, ReplayArtifactFidelity,
    ReplayObligationFidelity, ReplayObligationSnapshot, ReplayOperationKind,
    ReplayOperationOutcome, ReplayOperationPhase, RunExecutionCertification, RunExit, RunHeader,
    RunId, RunOutcome, RunOwnerId, RunOwnerLease, RunProvenance, RunStart, RunStatus,
    SealedArtifactRef, SessionId, SessionRunEvent, SessionRunEventKind, Sha256Digest,
    StaleRunRecovery, TerminalReason, CANONICALIZATION_ALGORITHM, CANONICALIZATION_VERSION,
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
        ReplayArtifactFidelity::MetadataOnly => {
            delivery_result_evidence_identity(lease, completion)
        }
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
        let created =
            self.create_run_and_start_tx(&transaction, mode, owner_id, start, start_artifact)?;
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
            Some((identity, reference, fidelity)) => {
                (Some(identity), Some(reference), Some(fidelity))
            }
            None => (None, None, None),
        };
        let start_exact_private = start_fidelity == Some(ReplayArtifactFidelity::ExactPrivate)
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
                params![
                    run_id.to_string(),
                    started_at_ms,
                    start_event_id.to_string()
                ],
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
                    contract
                        .resolution_source_event_id
                        .map(|value| value.to_string()),
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

include!("ledger_event_artifact_helpers.rs");
include!("ledger_replay_storage_helpers.rs");
include!("ledger_row_verification_helpers.rs");
