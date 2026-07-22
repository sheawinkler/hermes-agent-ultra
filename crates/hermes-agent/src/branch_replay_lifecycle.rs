//! Admission preparation for an immutable, effect-free branch replay child.
//!
//! This module deliberately has no provider, tool, or outbox dispatch surface.
//! It binds a verified source snapshot into a fresh child run-start payload; the
//! ledger transaction consumes the prepared admission only after revalidating
//! the same snapshot in one SQLite transaction.

use hermes_core::execution_mode::ResolvedExecutionMode;
use serde_json::{json, Value};
use thiserror::Error;

use crate::deterministic_replay::VerifiedBranchReplayPlan;
use crate::run_ledger::{
    BranchReplayArtifactBinding, BranchReplaySourceSnapshot, CanonicalJson, CreatedRun,
    LedgerError, RunLedger, RunOwnerId, RunProvenance, RunStart, Sha256Digest,
};

pub(crate) const BRANCH_REPLAY_START_SCHEMA: &str = "hermes.branch-replay-start/v1";


/// Caller-supplied child identity and execution admission. The coordinator
/// validates lineage and replaces its public payload with a source-bound
/// envelope before the ledger creates the child.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BranchReplayStart {
    pub(crate) mode: ResolvedExecutionMode,
    pub(crate) owner_id: RunOwnerId,
    pub(crate) start: RunStart,
}

impl BranchReplayStart {
    pub(crate) fn new(mode: ResolvedExecutionMode, owner_id: RunOwnerId, start: RunStart) -> Self {
        Self {
            mode,
            owner_id,
            start,
        }
    }
}

/// A validated child start ready for the ledger's atomic branch-mint path.
///
/// It has no outbox, effect, append-key, or idempotency material to copy from
/// the source run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PreparedBranchReplayStart {
    source: BranchReplaySourceSnapshot,
    admission: BranchReplayStart,
}

impl PreparedBranchReplayStart {
    pub(crate) fn source(&self) -> &BranchReplaySourceSnapshot {
        &self.source
    }

    pub(crate) fn admission(&self) -> &BranchReplayStart {
        &self.admission
    }

    pub(crate) fn into_parts(self) -> (BranchReplaySourceSnapshot, BranchReplayStart) {
        (self.source, self.admission)
    }
}

/// Typed failure for branch replay admission preparation.
#[derive(Debug, Error)]
pub(crate) enum BranchReplayLifecycleError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),

    #[error("invalid verified branch source snapshot: {reason}")]
    InvalidSourceSnapshot { reason: &'static str },

    #[error("invalid branch replay child lineage: {reason}")]
    InvalidChildLineage { reason: &'static str },
}

pub(crate) type BranchReplayLifecycleResult<T> = Result<T, BranchReplayLifecycleError>;

/// Prepares an effect-free child run start from verified source facts.
///
/// The later ledger call must revalidate `source` and insert the child in one
/// transaction; this function never reads mutable ledger state and therefore
/// never claims to provide that atomicity itself.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct BranchReplayLifecycleCoordinator;

impl BranchReplayLifecycleCoordinator {
    pub(crate) const fn new() -> Self {
        Self
    }

    pub(crate) fn prepare_child_start(
        &self,
        source: BranchReplaySourceSnapshot,
        mut admission: BranchReplayStart,
    ) -> BranchReplayLifecycleResult<PreparedBranchReplayStart> {
        validate_child_lineage(&source, &admission.start)?;

        let runtime_start = admission.start.public_payload.as_value()?;
        admission.start.public_payload = branch_replay_start_payload(&source, runtime_start)?;

        Ok(PreparedBranchReplayStart { source, admission })
    }

    /// Admit an effect-free child from an immutable verified export plan.
    ///
    /// Current ledger state is sampled only to form the recheck snapshot. The
    /// source chain, exact-replay latch, artifact states, obligations, and
    /// outbox are all checked again by the ledger in its one child-mint
    /// transaction, so this method cannot race source mutation into a child.
    pub(crate) fn admit_verified_branch_child(
        &self,
        ledger: &RunLedger,
        plan: &VerifiedBranchReplayPlan,
        admission: BranchReplayStart,
    ) -> BranchReplayLifecycleResult<CreatedRun> {
        let source = source_snapshot_from_verified_plan(ledger, plan)?;
        let (source, admission) = self.prepare_child_start(source, admission)?.into_parts();
        ledger
            .create_branch_run_from_verified_source(
                &source,
                admission.mode,
                admission.owner_id,
                admission.start,
            )
            .map_err(Into::into)
    }
}

fn source_snapshot_from_verified_plan(
    ledger: &RunLedger,
    plan: &VerifiedBranchReplayPlan,
) -> BranchReplayLifecycleResult<BranchReplaySourceSnapshot> {
    let source_plan = plan.source_plan();
    let source_status = ledger.run_status(plan.source_run_id())?;
    if !source_status.exact_replay.exact_replay
        || source_status.exact_replay.state != source_plan.recorded_exact_replay_state()
    {
        return Err(BranchReplayLifecycleError::InvalidSourceSnapshot {
            reason: "source exact-replay latch changed after export verification",
        });
    }

    let mut artifacts = Vec::with_capacity(plan.source_artifact_references().len());
    for reference in plan.source_artifact_references() {
        let status = ledger.artifact_reference_status(
            plan.source_run_id(),
            reference.event_id(),
            reference.artifact(),
        )?;
        artifacts.push(BranchReplayArtifactBinding::new(
            reference.event_id(),
            reference.artifact().clone(),
            status.physical_state,
            status.reference_state,
        ));
    }

    let boundary = plan.boundary();
    let source_head = plan.source_head();
    let source_next_event_sequence = source_head
        .sequence()
        .checked_add(1)
        .ok_or(LedgerError::NumericOverflow)?;
    BranchReplaySourceSnapshot::new(
        plan.source_run_id(),
        source_plan.session_id().clone(),
        source_plan.tenant_id().clone(),
        source_plan.branch_id().clone(),
        Sha256Digest::new(plan.source_export_digest().to_string())?,
        boundary.event_id(),
        boundary.sequence(),
        Sha256Digest::new(boundary.event_hash().to_string())?,
        source_head.event_id(),
        source_head.sequence(),
        Sha256Digest::new(source_head.event_hash().to_string())?,
        source_next_event_sequence,
        source_status.exact_replay.state,
        source_status.exact_replay.transition_sequence,
        artifacts,
    )
    .map_err(Into::into)
}

fn validate_child_lineage(
    source: &BranchReplaySourceSnapshot,
    start: &RunStart,
) -> BranchReplayLifecycleResult<()> {
    let provenance: &RunProvenance = &start.provenance;
    if start.run_id == source.source_run_id() {
        return Err(BranchReplayLifecycleError::InvalidChildLineage {
            reason: "child run id must differ from the source run id",
        });
    }
    if provenance.parent_run_id() != Some(source.source_run_id()) {
        return Err(BranchReplayLifecycleError::InvalidChildLineage {
            reason: "child parent run id does not bind the verified source run",
        });
    }
    if provenance.session_id() != source.source_session_id() {
        return Err(BranchReplayLifecycleError::InvalidChildLineage {
            reason: "child session id does not preserve source lineage",
        });
    }
    if provenance.tenant_id() != source.source_tenant_id() {
        return Err(BranchReplayLifecycleError::InvalidChildLineage {
            reason: "child tenant differs from the verified source tenant",
        });
    }
    if provenance.branch_id().as_str() == "main" {
        return Err(BranchReplayLifecycleError::InvalidChildLineage {
            reason: "branch replay child must use a non-main branch id",
        });
    }
    if provenance.branch_id() == source.source_branch_id() {
        return Err(BranchReplayLifecycleError::InvalidChildLineage {
            reason: "branch replay child must use a fresh branch id",
        });
    }
    Ok(())
}

fn branch_replay_start_payload(
    source: &BranchReplaySourceSnapshot,
    runtime_start: Value,
) -> BranchReplayLifecycleResult<CanonicalJson> {
    CanonicalJson::from_value(&json!({
        "branch_replay": {
            "boundary_event_hash": source.boundary_event_hash().as_str(),
            "boundary_event_id": source.boundary_event_id().to_string(),
            "boundary_event_sequence": source.boundary_event_sequence(),
            "source_branch_id": source.source_branch_id().as_str(),
            "source_export_digest": source.source_export_digest().as_str(),
            "source_head_event_id": source.source_head_event_id().to_string(),
            "source_head_event_sequence": source.source_head_event_sequence(),
            "source_head_hash": source.source_head_hash().as_str(),
            "source_run_id": source.source_run_id().to_string(),
            "source_session_id": source.source_session_id().as_str(),
            "source_tenant_id": source.source_tenant_id().as_str(),
            "source_next_event_sequence": source.source_next_event_sequence(),
        },
        "runtime_start": runtime_start,
        "schema": BRANCH_REPLAY_START_SCHEMA,
    }))
    .map_err(Into::into)
}

#[cfg(test)]
mod tests {
    use hermes_core::execution_mode::{ExecutionMode, ExecutionModeSource};
    use serde_json::json;
    use tempfile::tempdir;

    use super::*;
    use crate::deterministic_replay::{verify_branch_replay_plan, ReplayBranchBoundary};
    use crate::run_export::{export_run, verify_run_export_replay_plan};
    use crate::run_ledger::{
        ActionId, AppendIdentity, BranchId, DigestAbsence, EventId, ExactReplayState,
        LedgerConnectionFactory, LedgerIdentityKeyId, ModelId, ObjectiveId, PrivateFingerprint,
        ProviderId, ProvenanceDigest, ReplayArtifactFidelity, RunExit, RunId, RunLedger,
        RunOwnerId, RunProvenance, SessionId, Sha256Digest, TaskId, TenantId, TerminalReason,
    };

    fn digest(byte: u8) -> Sha256Digest {
        Sha256Digest::new(hex::encode([byte; 32])).expect("digest")
    }

    fn mode() -> ResolvedExecutionMode {
        ResolvedExecutionMode {
            mode: ExecutionMode::Manual,
            source: ExecutionModeSource::BuiltInDefault,
        }
    }

    fn source() -> BranchReplaySourceSnapshot {
        BranchReplaySourceSnapshot::new(
            RunId::new(),
            SessionId::new("source-session").expect("session"),
            TenantId::new("tenant-source").expect("tenant"),
            BranchId::new("branch-source").expect("branch"),
            digest(1),
            EventId::new(),
            3,
            digest(2),
            EventId::new(),
            7,
            digest(3),
            8,
            ExactReplayState::Available,
            0,
            Vec::new(),
        )
        .expect("source")
    }

    fn child_start(source: &BranchReplaySourceSnapshot) -> RunStart {
        RunStart::new(
            RunId::new(),
            RunProvenance::new(
                source.source_session_id().clone(),
                Some(source.source_run_id()),
                BranchId::new("branch-replay-a").expect("branch"),
                source.source_tenant_id().clone(),
                ActionId::new("branch-replay").expect("action"),
                ObjectiveId::new("branch-objective").expect("objective"),
                TaskId::new("branch-task").expect("task"),
                ModelId::new("replay-model").expect("model"),
                ProviderId::new("replay-provider").expect("provider"),
                digest(4),
                ProvenanceDigest::absent(DigestAbsence::NotApplicable),
                ProvenanceDigest::absent(DigestAbsence::NotApplicable),
                ProvenanceDigest::absent(DigestAbsence::NotApplicable),
                ProvenanceDigest::absent(DigestAbsence::NotApplicable),
                ProvenanceDigest::absent(DigestAbsence::NotApplicable),
                ProvenanceDigest::absent(DigestAbsence::NotApplicable),
                ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ),
            CanonicalJson::from_value(&json!({"runtime": "fresh"})).expect("payload"),
        )
    }

    fn concrete_source_provenance() -> RunProvenance {
        RunProvenance::new(
            SessionId::new("source-session").expect("session"),
            None,
            BranchId::new("branch-source").expect("branch"),
            TenantId::new("tenant-source").expect("tenant"),
            ActionId::new("source-action").expect("action"),
            ObjectiveId::new("source-objective").expect("objective"),
            TaskId::new("source-task").expect("task"),
            ModelId::new("source-model").expect("model"),
            ProviderId::new("source-provider").expect("provider"),
            digest(10),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
        )
    }

    fn exact_identity() -> AppendIdentity {
        AppendIdentity::new(
            CanonicalJson::from_value(&json!({"subject":"source","tenant":"tenant-source"}))
                .expect("public identity"),
            PrivateFingerprint::hmac_sha256_v1(
                LedgerIdentityKeyId::new("branch-replay-ledger-key-v1").expect("key id"),
                1,
                [42; 32],
            )
            .expect("private fingerprint"),
        )
    }

    fn exact_artifact_ref() -> crate::run_ledger::SealedArtifactRef {
        crate::run_ledger::SealedArtifactRef::try_from(
            crate::sealed_artifact::SealedArtifactRef {
                envelope_digest: hex::encode([43; 32]),
                key_id: "branch-replay-artifact-key-v1".to_string(),
                schema_version: crate::sealed_artifact::SEALED_ARTIFACT_SCHEMA_VERSION,
                encryption_algorithm: crate::sealed_artifact::ARTIFACT_ENCRYPTION_ALGORITHM
                    .to_string(),
                canonicalization_algorithm: crate::sealed_artifact::ARTIFACT_CANONICALIZATION
                    .to_string(),
                canonicalization_version:
                    crate::sealed_artifact::ARTIFACT_CANONICALIZATION_VERSION,
                redactor_version: "branch-replay-redactor-v1".to_string(),
            },
        )
        .expect("artifact reference")
    }

    #[test]
    fn admits_child_from_current_verified_exact_export() {
        let directory = tempdir().expect("ledger directory");
        let factory = LedgerConnectionFactory::new(directory.path().join("sessions.db"));
        factory
            .migrate_and_cut_over_v0_to_v1()
            .expect("migrate ledger");
        let ledger = RunLedger::open(factory).expect("open ledger");
        let source_run_id = RunId::new();
        let source = ledger
            .create_run_and_start_with_bound_artifact(
                mode(),
                RunOwnerId::new("source-owner").expect("owner"),
                RunStart::new(
                    source_run_id,
                    concrete_source_provenance(),
                    CanonicalJson::from_value(&json!({"start":"exact-source"}))
                        .expect("source payload"),
                ),
                exact_identity(),
                exact_artifact_ref(),
                ReplayArtifactFidelity::ExactPrivate,
            )
            .expect("create exact source");
        ledger
            .finalize_run_exit(
                mode(),
                &source.owner_lease,
                RunExit::new(
                    TerminalReason::Completed,
                    CanonicalJson::from_value(&json!({"exit":"complete"}))
                        .expect("exit payload"),
                ),
            )
            .expect("finalize source");

        let export = export_run(&ledger, directory.path().join("exports"), source_run_id)
            .expect("export exact source");
        let replay = verify_run_export_replay_plan(&export.path).expect("verify source export");
        let (head_event_id, head_sequence) = {
            let head = replay.events().last().expect("source head");
            (head.event_id, head.sequence)
        };
        let plan = verify_branch_replay_plan(
            replay,
            ReplayBranchBoundary::new(head_event_id, head_sequence),
        )
        .expect("verify branch plan");

        let child_start = RunStart::new(
            RunId::new(),
            RunProvenance::new(
                SessionId::new("source-session").expect("session"),
                Some(source_run_id),
                BranchId::new("branch-replay-child").expect("branch"),
                TenantId::new("tenant-source").expect("tenant"),
                ActionId::new("branch-replay").expect("action"),
                ObjectiveId::new("branch-objective").expect("objective"),
                TaskId::new("branch-task").expect("task"),
                ModelId::new("replay-model").expect("model"),
                ProviderId::new("replay-provider").expect("provider"),
                digest(11),
                ProvenanceDigest::absent(DigestAbsence::NotApplicable),
                ProvenanceDigest::absent(DigestAbsence::NotApplicable),
                ProvenanceDigest::absent(DigestAbsence::NotApplicable),
                ProvenanceDigest::absent(DigestAbsence::NotApplicable),
                ProvenanceDigest::absent(DigestAbsence::NotApplicable),
                ProvenanceDigest::absent(DigestAbsence::NotApplicable),
                ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ),
            CanonicalJson::from_value(&json!({"runtime":"fresh"})).expect("child payload"),
        );
        let child = BranchReplayLifecycleCoordinator::new()
            .admit_verified_branch_child(
                &ledger,
                &plan,
                BranchReplayStart::new(
                    mode(),
                    RunOwnerId::new("branch-owner").expect("owner"),
                    child_start,
                ),
            )
            .expect("admit child");

        let events = ledger.events_for_run(child.header.run_id).expect("child events");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].kind, crate::run_ledger::SessionRunEventKind::RunStart);
        assert!(ledger
            .outbox_for_run(child.header.run_id)
            .expect("child outbox")
            .is_empty());
        let payload = events[0].public_payload.as_value().expect("child payload json");
        assert_eq!(
            payload["branch_replay"]["source_run_id"],
            source_run_id.to_string()
        );
        assert_eq!(payload["runtime_start"]["runtime"], "fresh");
    }

    #[test]
    fn prepares_source_bound_effect_free_child_start() {
        let source = source();
        let admission = BranchReplayStart::new(
            mode(),
            RunOwnerId::new("branch-owner").expect("owner"),
            child_start(&source),
        );

        let prepared = BranchReplayLifecycleCoordinator::new()
            .prepare_child_start(source.clone(), admission)
            .expect("prepare child");
        let payload = prepared
            .admission()
            .start
            .public_payload
            .as_value()
            .expect("payload json");

        assert_eq!(payload["schema"], BRANCH_REPLAY_START_SCHEMA);
        assert_eq!(
            payload["branch_replay"]["source_run_id"],
            source.source_run_id().to_string()
        );
        assert_eq!(
            payload["branch_replay"]["boundary_event_sequence"],
            source.boundary_event_sequence()
        );
        assert_eq!(payload["runtime_start"]["runtime"], "fresh");
        assert!(payload.get("outbox").is_none());
        assert!(payload.get("idempotency_key").is_none());
    }

    #[test]
    fn rejects_main_or_reused_branch_and_cross_tenant_lineage() {
        let source = source();
        let coordinator = BranchReplayLifecycleCoordinator::new();
        let base = child_start(&source);

        let mut main = base.clone();
        main.provenance = RunProvenance::new(
            source.source_session_id().clone(),
            Some(source.source_run_id()),
            BranchId::new("main").expect("branch"),
            source.source_tenant_id().clone(),
            ActionId::new("branch-replay").expect("action"),
            ObjectiveId::new("branch-objective").expect("objective"),
            TaskId::new("branch-task").expect("task"),
            ModelId::new("replay-model").expect("model"),
            ProviderId::new("replay-provider").expect("provider"),
            digest(4),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
        );
        let error = coordinator
            .prepare_child_start(
                source.clone(),
                BranchReplayStart::new(mode(), RunOwnerId::new("owner-main").unwrap(), main),
            )
            .expect_err("main must fail");
        assert!(matches!(
            error,
            BranchReplayLifecycleError::InvalidChildLineage {
                reason: "branch replay child must use a non-main branch id"
            }
        ));

        let mut reused = base.clone();
        reused.provenance = RunProvenance::new(
            source.source_session_id().clone(),
            Some(source.source_run_id()),
            source.source_branch_id().clone(),
            source.source_tenant_id().clone(),
            ActionId::new("branch-replay").expect("action"),
            ObjectiveId::new("branch-objective").expect("objective"),
            TaskId::new("branch-task").expect("task"),
            ModelId::new("replay-model").expect("model"),
            ProviderId::new("replay-provider").expect("provider"),
            digest(4),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
        );
        let error = coordinator
            .prepare_child_start(
                source.clone(),
                BranchReplayStart::new(mode(), RunOwnerId::new("owner-reused").unwrap(), reused),
            )
            .expect_err("reused branch must fail");
        assert!(matches!(
            error,
            BranchReplayLifecycleError::InvalidChildLineage {
                reason: "branch replay child must use a fresh branch id"
            }
        ));

        let mut cross_tenant = base;
        cross_tenant.provenance = RunProvenance::new(
            source.source_session_id().clone(),
            Some(source.source_run_id()),
            BranchId::new("branch-replay-b").expect("branch"),
            TenantId::new("tenant-other").expect("tenant"),
            ActionId::new("branch-replay").expect("action"),
            ObjectiveId::new("branch-objective").expect("objective"),
            TaskId::new("branch-task").expect("task"),
            ModelId::new("replay-model").expect("model"),
            ProviderId::new("replay-provider").expect("provider"),
            digest(4),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
        );
        let error = coordinator
            .prepare_child_start(
                source,
                BranchReplayStart::new(
                    mode(),
                    RunOwnerId::new("owner-cross-tenant").unwrap(),
                    cross_tenant,
                ),
            )
            .expect_err("cross-tenant must fail");
        assert!(matches!(
            error,
            BranchReplayLifecycleError::InvalidChildLineage {
                reason: "child tenant differs from the verified source tenant"
            }
        ));
    }

    #[test]
    fn rejects_incoherent_source_sequence() {
        let error = BranchReplaySourceSnapshot::new(
            RunId::new(),
            SessionId::new("source-session").unwrap(),
            TenantId::new("tenant-source").unwrap(),
            BranchId::new("main").unwrap(),
            digest(1),
            EventId::new(),
            8,
            digest(2),
            EventId::new(),
            7,
            digest(3),
            8,
            ExactReplayState::Available,
            0,
            Vec::new(),
        )
        .expect_err("boundary after head must fail");
        assert!(matches!(error, LedgerError::IntegrityViolation(_)));
    }
}
