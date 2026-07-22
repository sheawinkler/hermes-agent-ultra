mod connection;
mod error;
mod ledger;
mod policy;
mod types;

pub use connection::{LedgerConnectionFactory, DEFAULT_BUSY_TIMEOUT, LEDGER_SCHEMA_VERSION};
pub use error::{LedgerError, LedgerResult};
pub(crate) use ledger::{
    delivery_result_evidence_identity, delivery_result_evidence_identity_for,
    delivery_result_exact_evidence_identity, delivery_result_exact_evidence_identity_for,
    genesis_hash, recompute_event_hash, verify_event_delivery_projection,
    verify_event_nonresumable_projection, DELIVERY_RESULT_BINDING_SCHEMA,
    DELIVERY_RESULT_EVIDENCE_IDENTITY_SCHEMA, LEGACY_DELIVERY_RESULT_BINDING_SCHEMA,
    RUN_EVENT_BINDING_SCHEMA_VERSION, RUN_EXIT_BINDING_SCHEMA, RUN_START_BINDING_SCHEMA,
};
pub use ledger::{LedgerClock, RunLedger, RunLedgerConfig, SystemLedgerClock};
pub use policy::{
    DeliveryManifest, DispatchPolicyV1, DurableOutboxDestinationHandler, ManifestEntry,
    OutboxRouteRegistry,
};
pub(crate) use policy::CertifiedOutboxDestinationHandler;
pub(crate) use types::{
    BranchReplayArtifactBinding, BranchReplaySourceSnapshot, NonResumableEffectLease,
    NonResumableEffectSnapshot, RunExecutionCertification,
};
pub use types::{
    ActionId, AppendIdentity, AppendKey, AppendLease, AppendReceipt, AppendRequest,
    AppendReservation, ArtifactDegradationReason, ArtifactKeyId, ArtifactPhysicalState,
    ArtifactReferenceState, ArtifactReferenceStatus, ArtifactStateReason, ArtifactStateTransition,
    BranchId, CanonicalJson, ClaimNext, CreatedRun, DeliveryCompletion,
    DeliveryCompletionReceipt, DeliveryEvidence, DeliveryFailureReason, Destination,
    DigestAbsence, DownstreamIdempotencyKey, DurableDeliveryReceipt, EffectIntent,
    EventId, ExactReplayState, ExactReplayStatus, LedgerIdentityKeyId, ModelId,
    NonResumableEffectAttempt, NonResumableEffectRecord, NonResumableEffectState,
    NonResumableIdempotencyKey, ObjectiveId, OutboxLease, OutboxRecord, OutboxRowId,
    OutboxState, PrivateFingerprint, PrivateFingerprintAlgorithm, ProvenanceDigest, ProviderId,
    RedactorVersion, ReplayAppendContract, ReplayArtifactFidelity, ReplayObligationFidelity,
    ReplayObligationSnapshot, ReplayOperationKind, ReplayOperationOutcome, ReplayOperationPhase,
    RunExit, RunHeader, RunId, RunOutcome, RunOwnerId, RunOwnerLease, RunProvenance, RunStart,
    RunStatus, SealedArtifactRef, SessionId, SessionRunEvent, SessionRunEventKind, Sha256Digest,
    StaleRunRecovery, TaskId, TenantId, TerminalReason,
    CANONICALIZATION_ALGORITHM, CANONICALIZATION_VERSION, PRIVATE_FINGERPRINT_ALGORITHM,
    PRIVATE_FINGERPRINT_VERSION,
};

#[cfg(test)]
mod tests;
