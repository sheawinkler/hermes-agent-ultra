//! Provider- and tool-free reconstruction of one already-verified run export.
//!
//! This module deliberately has no provider, handler, delivery, or tool-registry
//! dependency. A [`VerifiedRunReplayPlan`] can only be built by the run-export
//! verifier from the exact canonical bytes it accepted. Reconstruction reduces
//! those immutable events and, when present, verifies sealed private evidence
//! through an explicitly read-only capability.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use serde_json::Value;
use thiserror::Error;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use crate::run_export::VerifiedRunExport;
use crate::run_ledger::{
    ArtifactPhysicalState, ArtifactReferenceState, BranchId, CanonicalJson,
    Destination, DownstreamIdempotencyKey, EventId, ExactReplayState, OutboxRecord, OutboxRowId,
    OutboxState, PrivateFingerprint, ReplayObligationFidelity, ReplayObligationSnapshot,
    ReplayOperationKind, ReplayOperationOutcome, ReplayOperationPhase, RunId, RunLedger,
    RunOutcome, RunOwnerId, SealedArtifactRef, SessionId, SessionRunEvent, SessionRunEventKind,
    TenantId, TerminalReason,
};
use crate::sealed_artifact::{
    validate_private_jcs_commitment, validate_replay_artifact_private_jcs, ArtifactSealer,
    FilesystemSealedArtifactStore, OsArtifactKeyManager, ReconstructedPrivateFragment,
    ReplayArtifactPrivateClassification, SealedArtifactRef as StoreSealedArtifactRef,
};

/// A count is present only when canonical typed replay obligations classify the
/// relevant operation set. Historical/unclassified evidence is never zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayEvidenceCount {
    Available(u64),
    Unavailable,
}

impl Default for ReplayEvidenceCount {
    fn default() -> Self {
        Self::Unavailable
    }
}

/// Typed replay-operation counters. They remain unavailable until the export
/// includes the canonical operation-kind/phase obligation projection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReplayEvidenceCounters {
    pub delivery_request_count: ReplayEvidenceCount,
    pub delivery_attempt_count: ReplayEvidenceCount,
    pub model_call_count: ReplayEvidenceCount,
    pub tool_call_count: ReplayEvidenceCount,
    pub delegation_count: ReplayEvidenceCount,
    pub other_effect_count: ReplayEvidenceCount,
    pub failure_count: ReplayEvidenceCount,
}

/// One canonical delivery projection retained from the verified export. It is
/// evidence of a reconstructed delivery intent/result, never a delivery to be
/// executed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayOutboxEntry {
    outbox_id: OutboxRowId,
    operation_instance_id: String,
    source_event_id: EventId,
    manifest_order: u32,
    destination: Destination,
    idempotency_key: DownstreamIdempotencyKey,
    state: OutboxState,
    required: bool,
    public_payload: CanonicalJson,
    sealed_source_artifact: SealedArtifactRef,
}

impl ReplayOutboxEntry {
    pub(crate) fn from_outbox_record(record: &OutboxRecord, manifest_order: u32) -> Self {
        Self {
            outbox_id: record.outbox_id,
            operation_instance_id: record.outbox_id.to_string(),
            source_event_id: record.event_id,
            manifest_order,
            destination: record.destination,
            idempotency_key: record.idempotency_key.clone(),
            state: record.state,
            required: record.required,
            public_payload: record.public_payload.clone(),
            sealed_source_artifact: record.sealed_artifact_ref.clone(),
        }
    }

    pub const fn outbox_id(&self) -> OutboxRowId {
        self.outbox_id
    }

    pub fn operation_instance_id(&self) -> &str {
        &self.operation_instance_id
    }

    pub const fn source_event_id(&self) -> EventId {
        self.source_event_id
    }

    pub const fn manifest_order(&self) -> u32 {
        self.manifest_order
    }

    pub const fn destination(&self) -> Destination {
        self.destination
    }

    pub fn idempotency_key(&self) -> &DownstreamIdempotencyKey {
        &self.idempotency_key
    }

    /// Terminal outbox state recorded by the verified export. This is a
    /// projection for boundary validation only; replay never dispatches it.
    pub const fn state(&self) -> OutboxState {
        self.state
    }

    pub const fn required(&self) -> bool {
        self.required
    }

    pub fn public_payload(&self) -> &CanonicalJson {
        &self.public_payload
    }

    pub fn sealed_source_artifact(&self) -> &SealedArtifactRef {
        &self.sealed_source_artifact
    }
}

/// An immutable event-local sealed-artifact commitment needed for reconstruction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayArtifactReference {
    event_id: EventId,
    event_kind: SessionRunEventKind,
    artifact: SealedArtifactRef,
    public_identity: CanonicalJson,
    private_fingerprint: PrivateFingerprint,
}

impl ReplayArtifactReference {
    pub const fn event_id(&self) -> EventId {
        self.event_id
    }

    pub const fn event_kind(&self) -> SessionRunEventKind {
        self.event_kind
    }

    pub fn artifact(&self) -> &SealedArtifactRef {
        &self.artifact
    }

    pub fn envelope_digest(&self) -> &str {
        self.artifact.envelope_digest().as_str()
    }
}

/// A runtime projection that has no one-to-one typed replay obligation. It is
/// intentionally retained as structural unavailable evidence rather than
/// silently omitted or counted as an executed effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayProjectionGapKind {
    NonresumableIntent,
    NonresumableResult,
    DeliveryRequest,
    DeliveryResult,
}

impl ReplayProjectionGapKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::NonresumableIntent => "nonresumable_intent",
            Self::NonresumableResult => "nonresumable_result",
            Self::DeliveryRequest => "delivery_request",
            Self::DeliveryResult => "delivery_result",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReplayProjectionGap {
    source_event_id: EventId,
    source_event_sequence: u64,
    operation: ReplayOperationKind,
    phase: ReplayOperationPhase,
    operation_instance_id: String,
    resolved_event_id: Option<EventId>,
    kind: ReplayProjectionGapKind,
}

impl ReplayProjectionGap {
    pub(crate) fn new(
        source_event_id: EventId,
        source_event_sequence: u64,
        operation: ReplayOperationKind,
        phase: ReplayOperationPhase,
        operation_instance_id: String,
        resolved_event_id: Option<EventId>,
        kind: ReplayProjectionGapKind,
    ) -> Self {
        Self {
            source_event_id,
            source_event_sequence,
            operation,
            phase,
            operation_instance_id,
            resolved_event_id,
            kind,
        }
    }

    pub const fn source_event_id(&self) -> EventId {
        self.source_event_id
    }

    pub const fn source_event_sequence(&self) -> u64 {
        self.source_event_sequence
    }

    pub const fn operation(&self) -> ReplayOperationKind {
        self.operation
    }

    pub const fn phase(&self) -> ReplayOperationPhase {
        self.phase
    }

    pub fn operation_instance_id(&self) -> &str {
        &self.operation_instance_id
    }

    pub const fn resolved_event_id(&self) -> Option<EventId> {
        self.resolved_event_id
    }

    pub const fn kind(&self) -> ReplayProjectionGapKind {
        self.kind
    }
}

/// A parsed, verified, path-independent reconstruction plan.
///
/// Its fields are private so a caller cannot synthesize evidence or substitute
/// bytes after verification. Use `verify_run_export_replay_plan[_bytes]` to
/// obtain one.
#[derive(Clone)]
pub struct VerifiedRunReplayPlan {
    summary: VerifiedRunExport,
    canonical_bytes: Vec<u8>,
    events: Vec<SessionRunEvent>,
    outbox: Vec<ReplayOutboxEntry>,
    replay_obligations: Vec<ReplayObligationSnapshot>,
    projection_gaps: Vec<ReplayProjectionGap>,
    session_id: SessionId,
    tenant_id: TenantId,
    owner_id: RunOwnerId,
    branch_id: BranchId,
    terminal_reason: TerminalReason,
    final_outcome: RunOutcome,
    recorded_exact_replay_state: ExactReplayState,
    artifact_references: Vec<ReplayArtifactReference>,
    counters: ReplayEvidenceCounters,
}

impl fmt::Debug for VerifiedRunReplayPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedRunReplayPlan")
            .field("summary", &self.summary)
            .field("canonical_bytes_len", &self.canonical_bytes.len())
            .field("events", &self.events.len())
            .field("outbox", &self.outbox.len())
            .field("replay_obligations", &self.replay_obligations.len())
            .field("projection_gaps", &self.projection_gaps.len())
            .field("session_id", &self.session_id)
            .field("tenant_id", &self.tenant_id)
            .field("owner_id", &self.owner_id)
            .field("branch_id", &self.branch_id)
            .field("terminal_reason", &self.terminal_reason)
            .field("final_outcome", &self.final_outcome)
            .field(
                "recorded_exact_replay_state",
                &self.recorded_exact_replay_state,
            )
            .field("artifact_references", &self.artifact_references.len())
            .field("counters", &self.counters)
            .finish()
    }
}

impl VerifiedRunReplayPlan {
    pub(crate) fn from_verified_export(
        summary: VerifiedRunExport,
        canonical_bytes: Vec<u8>,
        events: Vec<SessionRunEvent>,
        replay_obligations: Vec<ReplayObligationSnapshot>,
        outbox: Vec<ReplayOutboxEntry>,
        projection_gaps: Vec<ReplayProjectionGap>,
        session_id: SessionId,
        tenant_id: TenantId,
        owner_id: RunOwnerId,
        branch_id: BranchId,
        terminal_reason: TerminalReason,
        final_outcome: RunOutcome,
        recorded_exact_replay_state: ExactReplayState,
    ) -> Result<Self, String> {
        let event_count = u64::try_from(events.len())
            .map_err(|_| "replay event count exceeds u64".to_string())?;
        if summary.event_count != event_count {
            return Err("verified replay summary event count does not match parsed events".to_string());
        }
        if !summary.evidence_complete() {
            return Err("verified export relies on legacy delivery evidence".to_string());
        }

        let mut artifact_references = Vec::new();
        for event in &events {
            let Some(artifact) = event.sealed_artifact_ref.as_ref() else {
                continue;
            };
            let public_identity = event.public_identity.clone().ok_or_else(|| {
                "sealed replay artifact is missing its public identity commitment".to_string()
            })?;
            let private_fingerprint = event.private_fingerprint.clone().ok_or_else(|| {
                "sealed replay artifact is missing its private fingerprint commitment".to_string()
            })?;
            artifact_references.push(ReplayArtifactReference {
                event_id: event.event_id,
                event_kind: event.kind,
                artifact: artifact.clone(),
                public_identity,
                private_fingerprint,
            });
        }

        let counters = replay_evidence_counters(&replay_obligations)?;
        Ok(Self {
            summary,
            canonical_bytes,
            events,
            outbox,
            replay_obligations,
            projection_gaps,
            session_id,
            tenant_id,
            owner_id,
            branch_id,

            terminal_reason,
            final_outcome,
            recorded_exact_replay_state,
            artifact_references,
            counters,
        })
    }

    pub fn summary(&self) -> &VerifiedRunExport {
        &self.summary
    }

    /// The canonical export digest committed by the immutable verification
    /// parse that minted this plan.
    pub fn export_digest(&self) -> &str {
        &self.summary.digest
    }

    /// The exact canonical document bytes accepted during verification.
    pub fn canonical_bytes(&self) -> &[u8] {
        &self.canonical_bytes
    }

    pub fn events(&self) -> &[SessionRunEvent] {
        &self.events
    }

    /// Canonical delivery projections retained from the same verified export
    /// parse as this plan.
    pub fn outbox(&self) -> &[ReplayOutboxEntry] {
        &self.outbox
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn tenant_id(&self) -> &TenantId {
        &self.tenant_id
    }

    /// Immutable export provenance only. A run owner is a lease holder, not an
    /// authenticated principal and is never used for replay authorization.
    pub fn owner_id(&self) -> &RunOwnerId {
        &self.owner_id
    }

    pub fn branch_id(&self) -> &BranchId {
        &self.branch_id
    }

    pub const fn terminal_reason(&self) -> TerminalReason {
        self.terminal_reason
    }

    pub const fn final_outcome(&self) -> RunOutcome {
        self.final_outcome
    }

    /// Historical latch state from the export. It is evidence only; current
    /// reconstruction fidelity is determined from actual unsealed payloads.
    pub const fn recorded_exact_replay_state(&self) -> ExactReplayState {
        self.recorded_exact_replay_state
    }

    pub const fn recorded_exact_replay(&self) -> bool {
        self.recorded_exact_replay_state.exact_replay()
    }

    pub fn artifact_references(&self) -> &[ReplayArtifactReference] {
        &self.artifact_references
    }

    pub fn replay_obligations(&self) -> &[ReplayObligationSnapshot] {
        &self.replay_obligations
    }

    pub fn projection_gaps(&self) -> &[ReplayProjectionGap] {
        &self.projection_gaps
    }

    pub const fn counters(&self) -> ReplayEvidenceCounters {
        self.counters
    }

    pub const fn integrity_verified(&self) -> bool {
        true
    }

    /// Run exports currently carry integrity evidence but no signer proof.
    pub const fn authentication_verified(&self) -> bool {
        false
    }

    pub const fn terminal_valid(&self) -> bool {
        true
    }
}

/// Caller-selected event boundary for branching an immutable verified source.
/// It is only an input; [`VerifiedReplayBoundary`] proves it names an event in
/// the canonical source sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayBranchBoundary {
    event_id: EventId,
    sequence: u64,
}

impl ReplayBranchBoundary {
    pub const fn new(event_id: EventId, sequence: u64) -> Self {
        Self { event_id, sequence }
    }

    pub const fn event_id(&self) -> EventId {
        self.event_id
    }

    pub const fn sequence(&self) -> u64 {
        self.sequence
    }
}

/// A branch boundary verified against one immutable run-export parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedReplayBoundary {
    event_id: EventId,
    sequence: u64,
    event_hash: String,
}

impl VerifiedReplayBoundary {
    pub const fn event_id(&self) -> EventId {
        self.event_id
    }

    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Canonical chain hash of the selected source boundary event.
    pub fn event_hash(&self) -> &str {
        &self.event_hash
    }
}

/// Immutable source head committed by a verified export. The lifecycle
/// coordinator must recheck this snapshot in its child-mint transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedReplaySourceHead {
    event_id: EventId,
    sequence: u64,
    event_hash: String,
}

impl VerifiedReplaySourceHead {
    pub const fn event_id(&self) -> EventId {
        self.event_id
    }

    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    pub fn event_hash(&self) -> &str {
        &self.event_hash
    }
}

/// A source-owned, exact replay branch plan. It authorizes no external work:
/// the lifecycle coordinator may only mint a fresh child lineage after
/// atomically rechecking its source boundary, head, and selected artifacts.
#[derive(Debug)]
pub struct VerifiedBranchReplayPlan {
    source: VerifiedRunReplayPlan,
    boundary: VerifiedReplayBoundary,
    source_head: VerifiedReplaySourceHead,
    source_artifact_references: Vec<ReplayArtifactReference>,
}

impl VerifiedBranchReplayPlan {
    pub fn source_plan(&self) -> &VerifiedRunReplayPlan {
        &self.source
    }

    pub const fn source_run_id(&self) -> RunId {
        self.source.summary.run_id
    }

    pub fn source_tenant_id(&self) -> &TenantId {
        self.source.tenant_id()
    }

    pub fn source_branch_id(&self) -> &BranchId {
        self.source.branch_id()
    }

    pub fn source_export_digest(&self) -> &str {
        self.source.export_digest()
    }

    pub fn boundary(&self) -> &VerifiedReplayBoundary {
        &self.boundary
    }

    pub fn source_head(&self) -> &VerifiedReplaySourceHead {
        &self.source_head
    }

    pub const fn source_head_event_id(&self) -> EventId {
        self.source_head.event_id
    }

    pub const fn source_head_sequence(&self) -> u64 {
        self.source_head.sequence
    }

    pub fn source_head_event_hash(&self) -> &str {
        &self.source_head.event_hash
    }

    /// Exact source artifacts selected by the verified prefix. The lifecycle
    /// transaction must recheck each reference rather than reopening exports.
    pub fn source_artifact_references(&self) -> &[ReplayArtifactReference] {
        &self.source_artifact_references
    }
}

/// Verify that a branch cut is an operation-complete prefix of one immutable,
/// exact source export. This only constructs a plan; it never mints a child
/// run, copies effects, resolves artifacts, or causes a delivery.
pub fn verify_branch_replay_plan(
    source: VerifiedRunReplayPlan,
    requested_boundary: ReplayBranchBoundary,
) -> DeterministicReplayResult<VerifiedBranchReplayPlan> {
    validate_plan(&source)?;
    if !source.recorded_exact_replay() || !source.projection_gaps.is_empty() {
        return Err(DeterministicReplayError::InvalidPlan(
            "branch source lacks complete exact replay evidence".to_string(),
        ));
    }
    let source_head_event = source.events.last().ok_or_else(|| {
        DeterministicReplayError::InvalidPlan(
            "branch source has no canonical event head".to_string(),
        )
    })?;
    let boundary_event = source
        .events
        .iter()
        .find(|event| event.event_id == requested_boundary.event_id)
        .ok_or_else(|| {
            DeterministicReplayError::InvalidPlan(
                "branch boundary event is absent from the verified source".to_string(),
            )
        })?;
    if boundary_event.sequence != requested_boundary.sequence {
        return Err(DeterministicReplayError::InvalidPlan(
            "branch boundary event identity and sequence disagree".to_string(),
        ));
    }

    let event_sequences = source
        .events
        .iter()
        .map(|event| (event.event_id, event.sequence))
        .collect::<BTreeMap<_, _>>();
    for obligation in &source.replay_obligations {
        if obligation.fidelity != ReplayObligationFidelity::ExactPrivate
            || obligation.artifact.is_none()
        {
            return Err(DeterministicReplayError::InvalidPlan(
                "branch source contains non-exact replay obligation evidence".to_string(),
            ));
        }
        let resolved_event_id = obligation.resolved_event_id.ok_or_else(|| {
            DeterministicReplayError::InvalidPlan(
                "branch source contains an unresolved replay obligation".to_string(),
            )
        })?;
        let resolved_sequence = event_sequences.get(&resolved_event_id).copied().ok_or_else(|| {
            DeterministicReplayError::InvalidPlan(
                "branch source obligation resolution is absent from verified events".to_string(),
            )
        })?;
        let source_in_prefix = obligation.source_event_sequence <= requested_boundary.sequence;
        let resolved_in_prefix = resolved_sequence <= requested_boundary.sequence;
        if source_in_prefix != resolved_in_prefix {
            return Err(DeterministicReplayError::InvalidPlan(
                "branch boundary splits a request/result replay operation".to_string(),
            ));
        }
        let artifact = obligation
            .artifact
            .as_ref()
            .expect("exact replay obligation artifact was checked above");
        if !source.artifact_references.iter().any(|reference| {
            reference.event_id == resolved_event_id && reference.artifact == *artifact
        }) {
            return Err(DeterministicReplayError::InvalidPlan(
                "branch source exact replay obligation lacks an event-local artifact reference"
                    .to_string(),
            ));
        }
    }

    for outbox in source
        .outbox
        .iter()
        .filter(|entry| {
            event_sequences
                .get(&entry.source_event_id())
                .is_some_and(|sequence| *sequence <= requested_boundary.sequence)
        })
    {
        if !matches!(outbox.state(), OutboxState::Acknowledged | OutboxState::DeadLettered) {
            return Err(DeterministicReplayError::InvalidPlan(
                "branch boundary leaves canonical outbox work pending".to_string(),
            ));
        }
        let request_present = source.replay_obligations.iter().any(|obligation| {
            obligation.operation == ReplayOperationKind::Delivery
                && obligation.phase == ReplayOperationPhase::Request
                && obligation.operation_instance_id == outbox.operation_instance_id()
                && obligation.source_event_id == outbox.source_event_id()
                && obligation.resolved_event_id == Some(outbox.source_event_id())
        });
        let result_sequences = source
            .replay_obligations
            .iter()
            .filter(|obligation| {
                obligation.operation == ReplayOperationKind::Delivery
                    && obligation.phase == ReplayOperationPhase::Result
                    && obligation.operation_instance_id == outbox.operation_instance_id()
                    && obligation.resolved_event_id == Some(obligation.source_event_id)
            })
            .map(|obligation| {
                event_sequences
                    .get(&obligation.source_event_id)
                    .copied()
                    .ok_or_else(|| {
                        DeterministicReplayError::InvalidPlan(
                            "delivery result replay obligation source is absent".to_string(),
                        )
                    })
            })
            .collect::<DeterministicReplayResult<Vec<_>>>()?;
        if !request_present
            || result_sequences.is_empty()
            || result_sequences
                .iter()
                .any(|sequence| *sequence > requested_boundary.sequence)
        {
            return Err(DeterministicReplayError::InvalidPlan(
                "branch boundary splits or omits canonical outbox replay evidence".to_string(),
            ));
        }
    }

    let source_artifact_references = source
        .artifact_references
        .iter()
        .filter(|reference| {
            event_sequences
                .get(&reference.event_id)
                .is_some_and(|sequence| *sequence <= requested_boundary.sequence)
        })
        .cloned()
        .collect();
    Ok(VerifiedBranchReplayPlan {
        boundary: VerifiedReplayBoundary {
            event_id: boundary_event.event_id,
            sequence: boundary_event.sequence,
            event_hash: boundary_event.event_hash.clone(),
        },
        source_head: VerifiedReplaySourceHead {
            event_id: source_head_event.event_id,
            sequence: source_head_event.sequence,
            event_hash: source_head_event.event_hash.clone(),
        },
        source,
        source_artifact_references,
    })
}

/// Fidelity actually proven by reconstruction, never inferred from a ledger
/// availability latch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayFidelity {
    /// Every replay-critical private artifact was a closed exact payload and
    /// matched its event-local identity commitment.
    Exact,
    /// At least one verified private artifact is only a source commitment plus
    /// metadata summary. The report is a reconstruction, not an exact replay.
    MetadataOnly,
    /// A replay-critical runtime projection has no typed private reconstruction
    /// evidence. It is structurally visible but cannot be exact.
    Unavailable,
}

impl ReplayFidelity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::MetadataOnly => "metadata_only",
            Self::Unavailable => "unavailable",
        }
    }

    pub const fn exact(self) -> bool {
        matches!(self, Self::Exact)
    }
}

/// Current availability observed through the read-only artifact capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplayArtifactAvailability {
    Verified,
}

impl ReplayArtifactAvailability {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Verified => "verified",
        }
    }
}
/// One ordered transcript node reconstructed solely from verified obligations,
/// sanitized public projections, and (when exact) an opaque scrub-on-drop
/// private fragment. It never invokes a provider, tool, delivery, or handler.
pub struct ReconstructedReplayEvent {
    source_event_id: EventId,
    source_event_sequence: u64,
    operation: ReplayOperationKind,
    phase: ReplayOperationPhase,
    operation_instance_id: String,
    outcome: ReplayOperationOutcome,
    fidelity: ReplayObligationFidelity,
    resolved_event_id: Option<EventId>,
    artifact: Option<ReconstructedArtifact>,
    delivery: Option<ReplayOutboxEntry>,
    public_projection: CanonicalJson,
    private_fragment: Option<Arc<ReconstructedPrivateFragment>>,
    projection_gap: Option<ReplayProjectionGapKind>,
}

impl ReconstructedReplayEvent {
    pub const fn source_event_id(&self) -> EventId {
        self.source_event_id
    }

    pub const fn source_event_sequence(&self) -> u64 {
        self.source_event_sequence
    }

    pub const fn operation(&self) -> ReplayOperationKind {
        self.operation
    }

    pub const fn phase(&self) -> ReplayOperationPhase {
        self.phase
    }

    /// Stable typed operation identity. Delivery records use the outbox row ID;
    /// all other operations use their source event ID.
    pub fn operation_instance_id(&self) -> &str {
        &self.operation_instance_id
    }

    pub const fn outcome(&self) -> ReplayOperationOutcome {
        self.outcome
    }

    pub const fn fidelity(&self) -> ReplayObligationFidelity {
        self.fidelity
    }

    pub const fn resolved_event_id(&self) -> Option<EventId> {
        self.resolved_event_id
    }

    pub fn artifact(&self) -> Option<&ReconstructedArtifact> {
        self.artifact.as_ref()
    }

    /// The canonical outbox entry referenced by this reconstructed delivery
    /// obligation. It is never sent during replay.
    pub fn delivery(&self) -> Option<&ReplayOutboxEntry> {
        self.delivery.as_ref()
    }

    pub fn public_projection(&self) -> &CanonicalJson {
        &self.public_projection
    }

    /// Runs an authorized, non-escaping inspection over exact private evidence.
    /// Metadata/unavailable structural records intentionally return `None`.
    pub fn with_private_fragment<R>(
        &self,
        inspect: impl for<'fragment> FnOnce(&'fragment ReconstructedPrivateFragment) -> R,
    ) -> Option<R> {
        self.private_fragment.as_deref().map(inspect)
    }

    pub const fn projection_gap(&self) -> Option<ReplayProjectionGapKind> {
        self.projection_gap
    }
}

impl fmt::Debug for ReconstructedReplayEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ReconstructedReplayEvent")
            .field("source_event_id", &self.source_event_id)
            .field("source_event_sequence", &self.source_event_sequence)
            .field("operation", &self.operation)
            .field("phase", &self.phase)
            .field("operation_instance_id", &self.operation_instance_id)
            .field("outcome", &self.outcome)
            .field("fidelity", &self.fidelity)
            .field("resolved_event_id", &self.resolved_event_id)
            .field("artifact", &self.artifact)
            .field("delivery", &self.delivery)
            .field("public_projection_len", &self.public_projection.as_str().len())
            .field("has_private_fragment", &self.private_fragment.is_some())
            .field("projection_gap", &self.projection_gap)
            .finish()
    }
}


/// Verified artifact evidence used in a reconstruction report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconstructedArtifact {
    pub event_id: EventId,
    pub envelope_digest: String,
    pub availability: ReplayArtifactAvailability,
    pub fidelity: ReplayFidelity,
}

/// Provider/tool-free terminal reconstruction evidence.
pub struct DeterministicReplayReport {
    run_id: RunId,
    export_digest: String,
    event_count: u64,
    artifact_count: u64,
    artifacts: Vec<ReconstructedArtifact>,
    transcript: Vec<ReconstructedReplayEvent>,
    terminal_reason: TerminalReason,
    final_outcome: RunOutcome,
    fidelity: ReplayFidelity,
    obligations_complete: bool,
    reconstruction_digest: String,
    counters: ReplayEvidenceCounters,
}

impl DeterministicReplayReport {
    pub const fn run_id(&self) -> RunId {
        self.run_id
    }

    pub fn export_digest(&self) -> &str {
        &self.export_digest
    }

    pub const fn event_count(&self) -> u64 {
        self.event_count
    }

    pub const fn artifact_count(&self) -> u64 {
        self.artifact_count
    }

    pub fn artifacts(&self) -> &[ReconstructedArtifact] {
        &self.artifacts
    }

    /// Ordered typed operations reconstructed from verified evidence. Exact
    /// records retain their private payload only behind the event-local
    /// authorized closure API.
    pub fn transcript(&self) -> &[ReconstructedReplayEvent] {
        &self.transcript
    }

    pub const fn terminal_reason(&self) -> TerminalReason {
        self.terminal_reason
    }

    pub const fn final_outcome(&self) -> RunOutcome {
        self.final_outcome
    }

    pub const fn fidelity(&self) -> ReplayFidelity {
        self.fidelity
    }

    /// True only when every runtime projection has exactly one typed,
    /// non-pending obligation. Structural projection gaps fail closed.
    pub const fn obligations_complete(&self) -> bool {
        self.obligations_complete
    }

    /// SHA-256 over the public, typed reconstructed transcript and terminal
    /// evidence. It intentionally excludes private artifact payloads.
    pub fn reconstruction_digest(&self) -> &str {
        &self.reconstruction_digest
    }

    pub const fn counters(&self) -> ReplayEvidenceCounters {
        self.counters
    }

    pub const fn exact(&self) -> bool {
        self.fidelity.exact()
    }
}

impl fmt::Debug for DeterministicReplayReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeterministicReplayReport")
            .field("run_id", &self.run_id)
            .field("export_digest", &self.export_digest)
            .field("event_count", &self.event_count)
            .field("artifact_count", &self.artifact_count)
            .field("artifacts", &self.artifacts)
            .field("transcript_len", &self.transcript.len())
            .field(
                "private_fragment_count",
                &self
                    .transcript
                    .iter()
                    .filter(|event| event.private_fragment.is_some())
                    .count(),
            )
            .field("terminal_reason", &self.terminal_reason)
            .field("final_outcome", &self.final_outcome)
            .field("fidelity", &self.fidelity)
            .field("obligations_complete", &self.obligations_complete)
            .field("reconstruction_digest", &self.reconstruction_digest)
            .field("counters", &self.counters)
            .finish()
    }
}

/// Failure returned by a read-only exact-artifact capability.
#[derive(Debug, Error)]
pub enum ExactArtifactReadError {
    #[error("exact artifact is unavailable: {0}")]
    Unavailable(String),
    #[error("exact artifact reference is no longer authoritatively available")]
    ReferenceUnavailable,
    #[error("exact artifact is outside the verified replay plan scope")]
    UnauthorizedScope,
}

impl ExactArtifactReadError {
    fn unavailable(error: impl fmt::Display) -> Self {
        Self::Unavailable(error.to_string())
    }
}

/// Private JCS recovered only after the capability verified envelope integrity,
/// event-local identity commitment, and artifact authorization.
///
/// There is intentionally no public constructor: external consumers receive a
/// concrete read-only capability from [`crate::run_runtime::RunRuntime`] rather
/// than fabricating successful artifact evidence.
pub struct VerifiedReplayArtifact {
    private_jcs: Zeroizing<Vec<u8>>,
}

impl fmt::Debug for VerifiedReplayArtifact {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedReplayArtifact")
            .field("private_jcs_len", &self.private_jcs.len())
            .finish()
    }
}

impl VerifiedReplayArtifact {
    pub(crate) fn from_verified_canonical_bytes(private_jcs: Vec<u8>) -> Self {
        Self {
            private_jcs: Zeroizing::new(private_jcs),
        }
    }

    fn as_bytes(&self) -> &[u8] {
        self.private_jcs.as_slice()
    }
}

/// Capability boundary for reading one event-local private artifact. It has no
/// provider, handler, delivery, or tool-registry affordance.
pub trait ExactArtifactReader: Send + Sync {
    fn read_exact(
        &self,
        plan: &VerifiedRunReplayPlan,
        artifact: &ReplayArtifactReference,
    ) -> Result<VerifiedReplayArtifact, ExactArtifactReadError>;
}

/// A runtime-created, immutable capability scoped to one verified tenant, run,
/// export digest, and complete event/reference allowlist. Its constructor is
/// crate-visible so external callers cannot mint a local authorization grant.
pub struct BoundExactArtifactReader<'a> {
    ledger: &'a RunLedger,
    keys: &'a OsArtifactKeyManager,
    store: &'a FilesystemSealedArtifactStore,
    tenant_id: TenantId,
    run_id: RunId,
    export_digest: String,
    allowed_artifacts: Vec<ReplayArtifactReference>,
}

impl fmt::Debug for BoundExactArtifactReader<'_> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("BoundExactArtifactReader")
            .field("tenant_id", &self.tenant_id)
            .field("run_id", &self.run_id)
            .field("export_digest", &self.export_digest)
            .field("allowed_artifacts", &self.allowed_artifacts.len())
            .finish()
    }
}

impl<'a> BoundExactArtifactReader<'a> {
    pub(crate) fn from_verified_plan(
        ledger: &'a RunLedger,
        keys: &'a OsArtifactKeyManager,
        store: &'a FilesystemSealedArtifactStore,
        plan: &VerifiedRunReplayPlan,
    ) -> Self {
        Self {
            ledger,
            keys,
            store,
            tenant_id: plan.tenant_id.clone(),
            run_id: plan.summary.run_id,
            export_digest: plan.summary.digest.clone(),
            allowed_artifacts: plan.artifact_references.clone(),
        }
    }

    pub fn run_id(&self) -> RunId {
        self.run_id
    }

    pub fn tenant_id(&self) -> &TenantId {
        &self.tenant_id
    }

    pub fn export_digest(&self) -> &str {
        &self.export_digest
    }
}

impl ExactArtifactReader for BoundExactArtifactReader<'_> {
    fn read_exact(
        &self,
        plan: &VerifiedRunReplayPlan,
        artifact: &ReplayArtifactReference,
    ) -> Result<VerifiedReplayArtifact, ExactArtifactReadError> {
        if plan.summary.run_id != self.run_id
            || plan.tenant_id != self.tenant_id
            || plan.summary.digest != self.export_digest
            || !self
                .allowed_artifacts
                .iter()
                .any(|candidate| candidate == artifact)
        {
            return Err(ExactArtifactReadError::UnauthorizedScope);
        }

        let store_reference = StoreSealedArtifactRef::try_from(artifact.artifact())
            .map_err(ExactArtifactReadError::unavailable)?;
        let private_jcs = {
            // The artifact guard must precede the ledger read: retirement and
            // CAS collection use the same order, so a reference cannot be
            // retired between authoritative status verification and read.
            let guard = self
                .store
                .acquire_shared_operation_guard()
                .map_err(ExactArtifactReadError::unavailable)?;
            let status = self
                .ledger
                .artifact_reference_status(self.run_id, artifact.event_id, artifact.artifact())
                .map_err(ExactArtifactReadError::unavailable)?;
            if status.run_id != self.run_id
                || status.event_id != artifact.event_id
                || status.artifact != *artifact.artifact()
                || status.reference_state != ArtifactReferenceState::Available
                || status.physical_state != ArtifactPhysicalState::Available
            {
                return Err(ExactArtifactReadError::ReferenceUnavailable);
            }
            let envelope = guard
                .locked_read(&store_reference.envelope_digest)
                .map_err(ExactArtifactReadError::unavailable)?;
            ArtifactSealer::new(self.keys, self.store)
                .unseal_verified_envelope_bytes(&store_reference, &envelope)
                .map_err(ExactArtifactReadError::unavailable)?
        };
        verify_artifact_commitment(self.keys, artifact, private_jcs.as_slice())?;
        Ok(VerifiedReplayArtifact { private_jcs })
    }
}

#[derive(Debug, Error)]
pub enum DeterministicReplayError {
    #[error("verified replay plan is internally inconsistent: {0}")]
    InvalidPlan(String),
    #[error("verified replay obligation for event {event_id} is incomplete or unavailable")]
    IncompleteReplayEvidence { event_id: EventId },
    #[error("verified replay obligation fidelity does not match artifact evidence for event {event_id}")]
    ReplayFidelityMismatch { event_id: EventId },
    #[error("verified replay plan requires a private exact-artifact reader")]
    ArtifactReaderRequired,
    #[error("exact artifact for event {event_id} ({digest}) could not be verified: {source}")]
    ArtifactRead {
        event_id: EventId,
        digest: String,
        #[source]
        source: ExactArtifactReadError,
    },
    #[error("verified artifact for event {event_id} ({digest}) has invalid replay payload: {reason}")]
    InvalidArtifactPayload {
        event_id: EventId,
        digest: String,
        reason: String,
    },
    #[error("exact replay is unavailable because event {event_id} ({digest}) has metadata-only evidence")]
    ExactFidelityRequired { event_id: EventId, digest: String },
    #[error("exact replay is unavailable because event {event_id} has unavailable structural evidence")]
    ExactEvidenceRequired { event_id: EventId },
}

pub type DeterministicReplayResult<T> = Result<T, DeterministicReplayError>;

/// Reduce one immutable verified plan without invoking a provider, tool,
/// handler, delivery route, or other effectful runtime path.
pub fn reconstruct_verified_run(
    plan: &VerifiedRunReplayPlan,
    reader: Option<&dyn ExactArtifactReader>,
) -> DeterministicReplayResult<DeterministicReplayReport> {
    validate_plan(plan)?;

    let artifact_count = u64::try_from(plan.artifact_references.len()).map_err(|_| {
        DeterministicReplayError::InvalidPlan("artifact reference count exceeds u64".to_string())
    })?;
    if artifact_count > 0 && reader.is_none() {
        return Err(DeterministicReplayError::ArtifactReaderRequired);
    }

    let mut artifacts = Vec::with_capacity(plan.artifact_references.len());
    let mut private_fragments = BTreeMap::<EventId, Arc<ReconstructedPrivateFragment>>::new();
    for artifact in &plan.artifact_references {
        let verified = reader
            .expect("reader is required whenever artifact references exist")
            .read_exact(plan, artifact)
            .map_err(|source| DeterministicReplayError::ArtifactRead {
                event_id: artifact.event_id,
                digest: artifact.envelope_digest().to_string(),
                source,
            })?;
        let classification = classify_artifact_private(artifact, verified.as_bytes())?;
        let artifact_fidelity = match classification {
            ReplayArtifactPrivateClassification::Exact(fragment) => {
                if private_fragments.insert(artifact.event_id, Arc::new(fragment)).is_some() {
                    return Err(DeterministicReplayError::InvalidPlan(
                        "verified artifact identities are not unique".to_string(),
                    ));
                }
                ReplayFidelity::Exact
            }
            ReplayArtifactPrivateClassification::MetadataOnly { .. } => {
                ReplayFidelity::MetadataOnly
            }
        };
        artifacts.push(ReconstructedArtifact {
            event_id: artifact.event_id,
            envelope_digest: artifact.envelope_digest().to_string(),
            availability: ReplayArtifactAvailability::Verified,
            fidelity: artifact_fidelity,
        });
    }
    let transcript = reconstruct_transcript(plan, &artifacts, &private_fragments)?;
    let fidelity = if transcript.iter().any(|event| {
        event.fidelity == ReplayObligationFidelity::Unavailable || event.projection_gap.is_some()
    }) {
        ReplayFidelity::Unavailable
    } else if transcript
        .iter()
        .any(|event| event.fidelity == ReplayObligationFidelity::MetadataOnly)
    {
        ReplayFidelity::MetadataOnly
    } else {
        ReplayFidelity::Exact
    };
    let obligations_complete = plan.projection_gaps.is_empty()
        && transcript
            .iter()
            .all(|event| event.projection_gap.is_none());
    let reconstruction_digest = reconstruction_digest(
        plan,
        &artifacts,
        &transcript,
        fidelity,
        obligations_complete,
    );


    Ok(DeterministicReplayReport {
        run_id: plan.summary.run_id,
        export_digest: plan.summary.digest.clone(),
        event_count: plan.summary.event_count,
        artifact_count,
        artifacts,
        transcript,
        terminal_reason: plan.terminal_reason,
        final_outcome: plan.final_outcome,
        obligations_complete,
        reconstruction_digest,
        fidelity,
        counters: plan.counters,
    })
}
fn reconstruction_digest(
    plan: &VerifiedRunReplayPlan,
    artifacts: &[ReconstructedArtifact],
    transcript: &[ReconstructedReplayEvent],
    fidelity: ReplayFidelity,
    obligations_complete: bool,
) -> String {
    let mut hasher = Sha256::new();
    update_reconstruction_digest_bytes(&mut hasher, b"hermes-deterministic-replay-report-v1");
    update_reconstruction_digest_bytes(&mut hasher, plan.summary.run_id.to_string().as_bytes());
    update_reconstruction_digest_bytes(&mut hasher, plan.export_digest().as_bytes());
    hasher.update(plan.summary.event_count.to_be_bytes());
    update_reconstruction_digest_bytes(&mut hasher, plan.terminal_reason.as_str().as_bytes());
    update_reconstruction_digest_bytes(&mut hasher, plan.final_outcome.as_str().as_bytes());
    update_reconstruction_digest_bytes(&mut hasher, fidelity.as_str().as_bytes());
    hasher.update([u8::from(obligations_complete)]);

    hasher.update(
        u64::try_from(artifacts.len())
            .expect("artifact count was checked before report digesting")
            .to_be_bytes(),
    );
    for artifact in artifacts {
        update_reconstruction_digest_bytes(&mut hasher, artifact.event_id.to_string().as_bytes());
        update_reconstruction_digest_bytes(&mut hasher, artifact.envelope_digest.as_bytes());
        update_reconstruction_digest_bytes(&mut hasher, artifact.availability.as_str().as_bytes());
        update_reconstruction_digest_bytes(&mut hasher, artifact.fidelity.as_str().as_bytes());
    }

    hasher.update(
        u64::try_from(transcript.len())
            .expect("transcript length fits u64")
            .to_be_bytes(),
    );
    for event in transcript {
        update_reconstruction_digest_bytes(&mut hasher, event.source_event_id.to_string().as_bytes());
        hasher.update(event.source_event_sequence.to_be_bytes());
        update_reconstruction_digest_bytes(&mut hasher, event.operation.as_str().as_bytes());
        update_reconstruction_digest_bytes(&mut hasher, event.phase.as_str().as_bytes());
        update_reconstruction_digest_bytes(&mut hasher, event.operation_instance_id.as_bytes());
        update_reconstruction_digest_bytes(&mut hasher, event.outcome.as_str().as_bytes());
        update_reconstruction_digest_bytes(&mut hasher, event.fidelity.as_str().as_bytes());
        match event.resolved_event_id {
            Some(event_id) => {
                hasher.update([1]);
                update_reconstruction_digest_bytes(&mut hasher, event_id.to_string().as_bytes());
            }
            None => hasher.update([0]),
        }
        match event.artifact.as_ref() {
            Some(artifact) => {
                hasher.update([1]);
                update_reconstruction_digest_bytes(&mut hasher, artifact.event_id.to_string().as_bytes());
                update_reconstruction_digest_bytes(&mut hasher, artifact.envelope_digest.as_bytes());
                update_reconstruction_digest_bytes(
                    &mut hasher,
                    artifact.availability.as_str().as_bytes(),
                );
                update_reconstruction_digest_bytes(&mut hasher, artifact.fidelity.as_str().as_bytes());
            }
            None => hasher.update([0]),
        }
        update_reconstruction_digest_bytes(&mut hasher, event.public_projection.as_str().as_bytes());
        match event.projection_gap {
            Some(gap) => {
                hasher.update([1]);
                update_reconstruction_digest_bytes(&mut hasher, gap.as_str().as_bytes());
            }
            None => hasher.update([0]),
        }
    }
    hex::encode(hasher.finalize())
}

fn update_reconstruction_digest_bytes(hasher: &mut Sha256, value: &[u8]) {
    hasher.update(
        u64::try_from(value.len())
            .expect("digest field length fits u64")
            .to_be_bytes(),
    );
    hasher.update(value);
}


fn reconstruct_transcript(
    plan: &VerifiedRunReplayPlan,
    artifacts: &[ReconstructedArtifact],
    private_fragments: &BTreeMap<EventId, Arc<ReconstructedPrivateFragment>>,
) -> DeterministicReplayResult<Vec<ReconstructedReplayEvent>> {
    let mut transcript =
        Vec::with_capacity(plan.replay_obligations.len() + plan.projection_gaps.len());
    for obligation in &plan.replay_obligations {
        let source = plan
            .events
            .iter()
            .find(|event| event.event_id == obligation.source_event_id)
            .ok_or_else(|| {
                DeterministicReplayError::InvalidPlan(
                    "replay obligation source event is absent from the verified event sequence"
                        .to_string(),
                )
            })?;
        if source.sequence != obligation.source_event_sequence {
            return Err(DeterministicReplayError::InvalidPlan(
                "replay obligation source sequence does not match the verified event".to_string(),
            ));
        }
        let resolved = obligation
            .resolved_event_id
            .and_then(|event_id| plan.events.iter().find(|event| event.event_id == event_id));
        if obligation.resolved_event_id.is_some() && resolved.is_none() {
            return Err(DeterministicReplayError::InvalidPlan(
                "replay obligation resolution event is absent from the verified event sequence"
                    .to_string(),
            ));
        }
        let delivery = delivery_projection_for(
            plan,
            source,
            obligation.operation,
            obligation.phase,
            &obligation.operation_instance_id,
        )?;
        let public_projection = delivery
            .as_ref()
            .map(|entry| entry.public_payload.clone())
            .unwrap_or_else(|| resolved.unwrap_or(source).public_payload.clone());
        let (artifact, private_fragment) = match obligation.fidelity {
            ReplayObligationFidelity::ExactPrivate => {
                let resolved_id = obligation
                    .resolved_event_id
                    .ok_or(DeterministicReplayError::IncompleteReplayEvidence {
                        event_id: obligation.source_event_id,
                    })?;
                let required = obligation
                    .artifact
                    .as_ref()
                    .ok_or(DeterministicReplayError::IncompleteReplayEvidence {
                        event_id: obligation.source_event_id,
                    })?;
                if plan
                    .artifact_references
                    .iter()
                    .find(|reference| reference.event_id == resolved_id)
                    .is_none_or(|reference| &reference.artifact != required)
                {
                    return Err(DeterministicReplayError::IncompleteReplayEvidence {
                        event_id: obligation.source_event_id,
                    });
                }
                let artifact = artifacts
                    .iter()
                    .find(|artifact| {
                        artifact.event_id == resolved_id
                            && artifact.envelope_digest == required.envelope_digest().as_str()
                    })
                    .cloned()
                    .ok_or(DeterministicReplayError::IncompleteReplayEvidence {
                        event_id: obligation.source_event_id,
                    })?;
                if artifact.fidelity != ReplayFidelity::Exact {
                    return Err(DeterministicReplayError::ReplayFidelityMismatch {
                        event_id: obligation.source_event_id,
                    });
                }
                let fragment = private_fragments.get(&resolved_id).cloned().ok_or(
                    DeterministicReplayError::ReplayFidelityMismatch {
                        event_id: obligation.source_event_id,
                    },
                )?;
                (Some(artifact), Some(fragment))
            }
            ReplayObligationFidelity::MetadataOnly => {
                let artifact = if let Some(required) = obligation.artifact.as_ref() {
                    let resolved_id = obligation
                        .resolved_event_id
                        .ok_or(DeterministicReplayError::IncompleteReplayEvidence {
                            event_id: obligation.source_event_id,
                        })?;
                    let artifact = artifacts
                        .iter()
                        .find(|artifact| {
                            artifact.event_id == resolved_id
                                && artifact.envelope_digest
                                    == required.envelope_digest().as_str()
                        })
                        .cloned()
                        .ok_or(DeterministicReplayError::IncompleteReplayEvidence {
                            event_id: obligation.source_event_id,
                        })?;
                    if artifact.fidelity != ReplayFidelity::MetadataOnly {
                        return Err(DeterministicReplayError::ReplayFidelityMismatch {
                            event_id: obligation.source_event_id,
                        });
                    }
                    Some(artifact)
                } else {
                    None
                };
                (artifact, None)
            }
            ReplayObligationFidelity::Unavailable => (None, None),
            ReplayObligationFidelity::Pending => {
                return Err(DeterministicReplayError::IncompleteReplayEvidence {
                    event_id: obligation.source_event_id,
                });
            }
        };
        transcript.push(ReconstructedReplayEvent {
            source_event_id: obligation.source_event_id,
            source_event_sequence: obligation.source_event_sequence,
            operation: obligation.operation,
            phase: obligation.phase,
            operation_instance_id: obligation.operation_instance_id.clone(),
            outcome: obligation.outcome,
            fidelity: obligation.fidelity,
            resolved_event_id: obligation.resolved_event_id,
            artifact,
            delivery,
            public_projection,
            private_fragment,
            projection_gap: None,
        });
    }
    for gap in &plan.projection_gaps {
        let source = plan
            .events
            .iter()
            .find(|event| event.event_id == gap.source_event_id)
            .ok_or_else(|| {
                DeterministicReplayError::InvalidPlan(
                    "replay projection gap source event is absent from verified events".to_string(),
                )
            })?;
        let delivery = delivery_projection_for(
            plan,
            source,
            gap.operation,
            gap.phase,
            gap.operation_instance_id(),
        )?;
        let public_projection = delivery
            .as_ref()
            .map(|entry| entry.public_payload.clone())
            .unwrap_or_else(|| source.public_payload.clone());
        transcript.push(ReconstructedReplayEvent {
            source_event_id: gap.source_event_id,
            source_event_sequence: gap.source_event_sequence,
            operation: gap.operation,
            phase: gap.phase,
            operation_instance_id: gap.operation_instance_id().to_string(),
            outcome: ReplayOperationOutcome::Unavailable,
            fidelity: ReplayObligationFidelity::Unavailable,
            resolved_event_id: gap.resolved_event_id,
            artifact: None,
            delivery,
            public_projection,
            private_fragment: None,
            projection_gap: Some(gap.kind),
        });
    }
    transcript.sort_by(|left, right| {
        (
            left.source_event_sequence,
            left.operation.as_str(),
            left.phase.as_str(),
            left.operation_instance_id.as_str(),
        )
            .cmp(&(
                right.source_event_sequence,
                right.operation.as_str(),
                right.phase.as_str(),
                right.operation_instance_id.as_str(),
            ))
    });
    Ok(transcript)
}

fn delivery_projection_for(
    plan: &VerifiedRunReplayPlan,
    source: &SessionRunEvent,
    operation: ReplayOperationKind,
    phase: ReplayOperationPhase,
    operation_instance_id: &str,
) -> DeterministicReplayResult<Option<ReplayOutboxEntry>> {
    if operation != ReplayOperationKind::Delivery {
        return Ok(None);
    }
    let entry = plan
        .outbox
        .iter()
        .find(|entry| entry.operation_instance_id() == operation_instance_id)
        .ok_or_else(|| {
            DeterministicReplayError::InvalidPlan(
                "delivery operation instance does not identify a verified outbox row".to_string(),
            )
        })?;
    match phase {
        ReplayOperationPhase::Request => {
            if source.event_id != entry.source_event_id
                || source.sealed_artifact_ref.as_ref() != Some(&entry.sealed_source_artifact)
            {
                return Err(DeterministicReplayError::InvalidPlan(
                    "delivery request does not match its canonical outbox source projection"
                        .to_string(),
                ));
            }
        }
        ReplayOperationPhase::Result => {
            if source.kind != SessionRunEventKind::DeliveryResult {
                return Err(DeterministicReplayError::InvalidPlan(
                    "delivery result does not use its own delivery-result source event".to_string(),
                ));
            }
        }
    }
    Ok(Some(entry.clone()))
}
/// Require every replay-critical artifact to be a closed exact payload. This
/// rejects metadata-only reconstructions rather than mislabeling them replayed.
pub fn reconstruct_exact_verified_run(
    plan: &VerifiedRunReplayPlan,
    reader: Option<&dyn ExactArtifactReader>,
) -> DeterministicReplayResult<DeterministicReplayReport> {
    let report = reconstruct_verified_run(plan, reader)?;
    if report.exact() {
        return Ok(report);
    }
    if let Some(artifact) = report
        .artifacts()
        .iter()
        .find(|artifact| artifact.fidelity == ReplayFidelity::MetadataOnly)
    {
        return Err(DeterministicReplayError::ExactFidelityRequired {
            event_id: artifact.event_id,
            digest: artifact.envelope_digest.clone(),
        });
    }
    let event = report
        .transcript()
        .iter()
        .find(|event| {
            event.fidelity != ReplayObligationFidelity::ExactPrivate
                || event.projection_gap.is_some()
        })
        .ok_or_else(|| {
            DeterministicReplayError::InvalidPlan(
                "non-exact replay report has no unavailable evidence".to_string(),
            )
        })?;
    Err(DeterministicReplayError::ExactEvidenceRequired {
        event_id: event.source_event_id,
    })
}

fn validate_plan(plan: &VerifiedRunReplayPlan) -> DeterministicReplayResult<()> {
    let event_count = u64::try_from(plan.events.len()).map_err(|_| {
        DeterministicReplayError::InvalidPlan("event count exceeds u64".to_string())
    })?;
    if plan.summary.event_count != event_count || event_count == 0 {
        return Err(DeterministicReplayError::InvalidPlan(
            "event count is not a complete verified sequence".to_string(),
        ));
    }
    let outbox_count = u64::try_from(plan.outbox.len()).map_err(|_| {
        DeterministicReplayError::InvalidPlan("outbox count exceeds u64".to_string())
    })?;
    if plan.summary.outbox_count != outbox_count {
        return Err(DeterministicReplayError::InvalidPlan(
            "outbox count does not match the verified summary".to_string(),
        ));
    }

    for (index, event) in plan.events.iter().enumerate() {
        let expected_sequence = u64::try_from(index)
            .map_err(|_| {
                DeterministicReplayError::InvalidPlan("event index exceeds u64".to_string())
            })?
            .checked_add(1)
            .ok_or_else(|| {
                DeterministicReplayError::InvalidPlan("event sequence overflow".to_string())
            })?;
        if event.run_id != plan.summary.run_id || event.sequence != expected_sequence {
            return Err(DeterministicReplayError::InvalidPlan(
                "event run identity or sequence is not contiguous".to_string(),
            ));
        }
    }

    let mut outbox_instances = BTreeSet::new();
    let mut previous_outbox_order = None;
    for entry in &plan.outbox {
        if !outbox_instances.insert(entry.operation_instance_id()) {
            return Err(DeterministicReplayError::InvalidPlan(
                "canonical outbox operation instance is duplicated".to_string(),
            ));
        }
        if entry.operation_instance_id() != entry.outbox_id().to_string() {
            return Err(DeterministicReplayError::InvalidPlan(
                "canonical outbox operation instance does not equal its outbox identifier"
                    .to_string(),
            ));
        }
        let source = plan
            .events
            .iter()
            .find(|event| event.event_id == entry.source_event_id())
            .ok_or_else(|| {
                DeterministicReplayError::InvalidPlan(
                    "canonical outbox source event is absent from verified events".to_string(),
                )
            })?;
        if source.sealed_artifact_ref.as_ref() != Some(entry.sealed_source_artifact()) {
            return Err(DeterministicReplayError::InvalidPlan(
                "canonical outbox sealed source reference does not match its event".to_string(),
            ));
        }
        let order = (source.sequence, entry.manifest_order());
        if previous_outbox_order.is_some_and(|previous| previous >= order) {
            return Err(DeterministicReplayError::InvalidPlan(
                "canonical outbox rows are not in immutable event/manifest order".to_string(),
            ));
        }
        previous_outbox_order = Some(order);
    }

    let expected_artifacts = plan
        .events
        .iter()
        .filter_map(|event| {
            event.sealed_artifact_ref.as_ref().map(|artifact| {
                let public_identity = event.public_identity.clone().ok_or_else(|| {
                    DeterministicReplayError::InvalidPlan(
                        "sealed artifact lacks a public identity commitment".to_string(),
                    )
                })?;
                let private_fingerprint = event.private_fingerprint.clone().ok_or_else(|| {
                    DeterministicReplayError::InvalidPlan(
                        "sealed artifact lacks a private fingerprint commitment".to_string(),
                    )
                })?;
                Ok(ReplayArtifactReference {
                    event_id: event.event_id,
                    event_kind: event.kind,
                    artifact: artifact.clone(),
                    public_identity,
                    private_fingerprint,
                })
            })
        })
        .collect::<Result<Vec<_>, DeterministicReplayError>>()?;
    if expected_artifacts != plan.artifact_references {
        return Err(DeterministicReplayError::InvalidPlan(
            "artifact references do not match the verified event sequence".to_string(),
        ));
    }
    let counters = replay_evidence_counters(&plan.replay_obligations)
        .map_err(DeterministicReplayError::InvalidPlan)?;
    if counters != plan.counters {
        return Err(DeterministicReplayError::InvalidPlan(
            "replay evidence counters do not match typed obligations".to_string(),
        ));
    }
    let mut identities = BTreeSet::new();
    let mut previous_order = None;
    let mut saw_run_start_request = false;
    for obligation in &plan.replay_obligations {
        let source = plan
            .events
            .iter()
            .find(|event| event.event_id == obligation.source_event_id)
            .ok_or_else(|| {
                DeterministicReplayError::InvalidPlan(
                    "replay obligation source event is absent from verified events".to_string(),
                )
            })?;
        if source.sequence != obligation.source_event_sequence {
            return Err(DeterministicReplayError::InvalidPlan(
                "replay obligation source sequence is inconsistent".to_string(),
            ));
        }
        validate_operation_instance_binding(
            plan,
            source,
            obligation.operation,
            obligation.phase,
            &obligation.operation_instance_id,
        )?;
        let identity = (
            obligation.source_event_id,
            obligation.operation.as_str(),
            obligation.phase.as_str(),
            obligation.operation_instance_id.as_str(),
        );
        if !identities.insert(identity) {
            return Err(DeterministicReplayError::InvalidPlan(
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
            return Err(DeterministicReplayError::InvalidPlan(
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
        if obligation.resolved_event_id.is_some_and(|event_id| {
            !plan.events.iter().any(|event| event.event_id == event_id)
        }) {
            return Err(DeterministicReplayError::InvalidPlan(
                "replay obligation resolution event is absent from verified events".to_string(),
            ));
        }
    }
    if !saw_run_start_request {
        return Err(DeterministicReplayError::InvalidPlan(
            "replay obligations omit run-start request evidence".to_string(),
        ));
    }
    for gap in &plan.projection_gaps {
        let source = plan
            .events
            .iter()
            .find(|event| event.event_id == gap.source_event_id)
            .ok_or_else(|| {
                DeterministicReplayError::InvalidPlan(
                    "replay projection gap source event is absent from verified events".to_string(),
                )
            })?;
        if source.sequence != gap.source_event_sequence {
            return Err(DeterministicReplayError::InvalidPlan(
                "replay projection gap source sequence is inconsistent".to_string(),
            ));
        }
        validate_operation_instance_binding(
            plan,
            source,
            gap.operation,
            gap.phase,
            gap.operation_instance_id(),
        )?;
        let identity = (
            gap.source_event_id,
            gap.operation.as_str(),
            gap.phase.as_str(),
            gap.operation_instance_id(),
        );
        if identities.contains(&identity) {
            return Err(DeterministicReplayError::InvalidPlan(
                "replay projection gap duplicates a mapped obligation".to_string(),
            ));
        }
        if gap.resolved_event_id.is_some_and(|event_id| {
            !plan.events.iter().any(|event| event.event_id == event_id)
        }) {
            return Err(DeterministicReplayError::InvalidPlan(
                "replay projection gap resolution event is absent from verified events".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_operation_instance_binding(
    plan: &VerifiedRunReplayPlan,
    source: &SessionRunEvent,
    operation: ReplayOperationKind,
    phase: ReplayOperationPhase,
    operation_instance_id: &str,
) -> DeterministicReplayResult<()> {
    if operation == ReplayOperationKind::Delivery {
        let _ = delivery_projection_for(plan, source, operation, phase, operation_instance_id)?;
    } else if operation_instance_id != source.event_id.to_string() {
        return Err(DeterministicReplayError::InvalidPlan(
            "non-delivery operation instance does not equal its source event identity".to_string(),
        ));
    }
    Ok(())
}

fn replay_evidence_counters(
    obligations: &[ReplayObligationSnapshot],
) -> Result<ReplayEvidenceCounters, String> {
    if obligations.is_empty() {
        return Err("typed replay obligations are missing".to_string());
    }
    let mut model_call_count = 0_u64;
    let mut tool_call_count = 0_u64;
    let mut delivery_request_count = 0_u64;
    let mut delivery_attempt_count = 0_u64;
    let mut delegation_count = 0_u64;
    let mut other_effect_count = 0_u64;
    let mut failure_count = 0_u64;
    for obligation in obligations {
        if !obligation.required || obligation.fidelity == ReplayObligationFidelity::Pending {
            return Err("replay obligation is incomplete".to_string());
        }
        if obligation.operation == ReplayOperationKind::Delivery {
            let counter = match obligation.phase {
                ReplayOperationPhase::Request => &mut delivery_request_count,
                ReplayOperationPhase::Result => &mut delivery_attempt_count,
            };
            *counter = (*counter)
                .checked_add(1)
                .ok_or_else(|| "delivery replay operation count overflow".to_string())?;
        } else if obligation.phase == ReplayOperationPhase::Request {
            let counter = match obligation.operation {
                ReplayOperationKind::Model => &mut model_call_count,
                ReplayOperationKind::Tool => &mut tool_call_count,
                ReplayOperationKind::Delegation => &mut delegation_count,
                ReplayOperationKind::Run => continue,
                ReplayOperationKind::Oauth | ReplayOperationKind::AgentOutput => {
                    &mut other_effect_count
                }
                ReplayOperationKind::Delivery => unreachable!("handled above"),
            };
            *counter = (*counter)
                .checked_add(1)
                .ok_or_else(|| "replay operation count overflow".to_string())?;
        }
        if obligation.phase == ReplayOperationPhase::Result
            && matches!(
                obligation.outcome,
                ReplayOperationOutcome::HandlerError
                    | ReplayOperationOutcome::EffectNonResumableUnknown
                    | ReplayOperationOutcome::Cancelled
                    | ReplayOperationOutcome::TimedOut
                    | ReplayOperationOutcome::Unavailable
            )
        {
            failure_count = failure_count
                .checked_add(1)
                .ok_or_else(|| "replay failure count overflow".to_string())?;
        }
    }
    Ok(ReplayEvidenceCounters {
        delivery_request_count: ReplayEvidenceCount::Available(delivery_request_count),
        delivery_attempt_count: ReplayEvidenceCount::Available(delivery_attempt_count),
        model_call_count: ReplayEvidenceCount::Available(model_call_count),
        tool_call_count: ReplayEvidenceCount::Available(tool_call_count),
        delegation_count: ReplayEvidenceCount::Available(delegation_count),
        other_effect_count: ReplayEvidenceCount::Available(other_effect_count),
        failure_count: ReplayEvidenceCount::Available(failure_count),
    })
}

fn classify_artifact_private(
    artifact: &ReplayArtifactReference,
    private_jcs: &[u8],
) -> DeterministicReplayResult<ReplayArtifactPrivateClassification> {
    // The event identity is public, canonical evidence. The shared classifier
    // parses private JCS once and returns either an opaque scrub-on-drop exact
    // fragment or metadata-only binding; no raw JSON enters a report/error.
    let public_identity = serde_json::from_str::<Value>(artifact.public_identity.as_str())
        .map_err(|_| invalid_artifact_payload(artifact, "invalid public identity".to_string()))?;
    validate_replay_artifact_private_jcs(&public_identity, private_jcs).map_err(|_| {
        invalid_artifact_payload(
            artifact,
            "private replay artifact has unsupported closed evidence".to_string(),
        )
    })
}

fn verify_artifact_commitment(
    keys: &OsArtifactKeyManager,
    artifact: &ReplayArtifactReference,
    private_jcs: &[u8],
) -> Result<(), ExactArtifactReadError> {
    let fingerprint_key = keys
        .resolve_fingerprint_key(
            artifact.private_fingerprint.key_id().as_str(),
            artifact.private_fingerprint.key_version(),
        )
        .map_err(ExactArtifactReadError::unavailable)?;
    validate_private_jcs_commitment(
        artifact.public_identity.as_str().as_bytes(),
        private_jcs,
        &fingerprint_key,
        &artifact.private_fingerprint.as_hex(),
    )
    .map_err(ExactArtifactReadError::unavailable)
}

fn invalid_artifact_payload(
    artifact: &ReplayArtifactReference,
    reason: String,
) -> DeterministicReplayError {
    DeterministicReplayError::InvalidArtifactPayload {
        event_id: artifact.event_id,
        digest: artifact.envelope_digest().to_string(),
        reason,
    }
}

fn is_lowercase_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}
