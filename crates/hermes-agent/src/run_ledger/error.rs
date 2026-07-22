use thiserror::Error;

pub type LedgerResult<T> = Result<T, LedgerError>;

#[derive(Debug, Error)]
pub enum LedgerError {
    #[error("ledger SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    #[error("ledger busy timeout must be between 1 ms and 30,000 ms (received {milliseconds} ms)")]
    InvalidBusyTimeout { milliseconds: u128 },

    #[error(
        "ledger lease duration must be positive and at most 24 hours (received {milliseconds} ms)"
    )]
    InvalidLeaseDuration { milliseconds: u128 },

    #[error("ledger maximum delivery attempts must be between 1 and 1,000 (received {attempts})")]
    InvalidMaximumDeliveryAttempts { attempts: u32 },

    #[error("ledger has not completed the v5 cutover")]
    NotMigrated,

    #[error("ledger marker mismatch: schema={schema:?}, primary={primary:?}")]
    MarkerMismatch {
        schema: Option<String>,
        primary: Option<String>,
    },

    #[error("ledger schema version {version} is newer than supported version 5")]
    NewerSchemaVersion { version: String },

    #[error("ledger schema validation failed: {0}")]
    SchemaValidation(String),
    #[error("ledger schema fingerprint marker is unknown or does not match: {fingerprint:?}")]
    SchemaFingerprintMismatch { fingerprint: Option<String> },

    #[error("ledger migration was intentionally aborted before commit")]
    MigrationAborted,

    #[error("canonical JSON rejected: {0}")]
    CanonicalJson(String),

    #[error("invalid {kind}: {value:?}")]
    InvalidIdentifier { kind: &'static str, value: String },
    #[error("invalid full lowercase SHA-256 {kind}: {value:?}")]
    InvalidDigest { kind: &'static str, value: String },

    #[error("invalid stored {kind} state: {value:?}")]
    InvalidStoredState { kind: &'static str, value: String },

    #[error("numeric overflow while updating ledger state")]
    NumericOverflow,

    #[error("run {run_id} was not found")]
    RunNotFound { run_id: String },

    #[error("run {run_id} already exists")]
    RunAlreadyExists { run_id: String },

    #[error("run {run_id} is already terminal")]
    RunAlreadyTerminal { run_id: String },

    #[error("parent run {parent_run_id} was not found")]
    ParentRunNotFound { parent_run_id: String },

    #[error("parent run {parent_run_id} is not in tenant {tenant_id}")]
    ParentRunTenantMismatch {
        parent_run_id: String,
        tenant_id: String,
    },

    #[error("branch replay child for source run {source_run_id} has invalid lineage: {reason}")]
    BranchReplayChildLineage {
        source_run_id: String,
        reason: &'static str,
    },

    #[error("verified branch replay source run {source_run_id} no longer matches: {reason}")]
    BranchReplaySourceMismatch {
        source_run_id: String,
        reason: &'static str,
    },

    #[error("execution mode does not match run {run_id}")]
    ExecutionModeMismatch { run_id: String },

    #[error("closed execution mode {mode} requires a configured certified backend")]
    MissingExecutionCertification { mode: &'static str },
    #[error(
        "execution containment mode does not match resolved mode: expected {expected}, received {received}"
    )]
    ExecutionCertificationModeMismatch {
        expected: &'static str,
        received: &'static str,
    },
    #[error("execution containment certification is invalid: {reason}")]
    InvalidExecutionCertification { reason: &'static str },

    #[error("append key {append_key:?} conflicts with a different append identity")]
    AppendKeyConflict { append_key: String },

    #[error("append key {append_key:?} conflicts with a different private fingerprint")]
    PrivateFingerprintMismatch { append_key: String },

    #[error("append reservation for {append_key:?} is not owned by this lease")]
    StaleAppendLease { append_key: String },

    #[error("append reservation for {append_key:?} was not found")]
    AppendReservationNotFound { append_key: String },
    #[error("sealed artifact reference is required before committing effect-capable append {append_key:?}")]
    SealedArtifactRefRequired { append_key: String },

    #[error("sealed artifact reference does not match the published reservation reference for {append_key:?}")]
    SealedArtifactRefMismatch { append_key: String },

    #[error("sealed artifact reference for {append_key:?} is already published and immutable")]
    SealedArtifactRefConflict { append_key: String },

    #[error("sealed artifact append identity is invalid: {0}")]
    SealedArtifactIdentity(String),

    #[error("sealed artifact reference is invalid: {0}")]
    SealedArtifactReference(String),
    #[error("sealed artifact digest {digest} aliases conflicting immutable reference metadata")]
    ArtifactReferenceAliasConflict { digest: String },

    #[error(
        "sealed artifact reference {event_id}/{digest} was not found for run {run_id}"
    )]
    ArtifactReferenceNotFound {
        run_id: String,
        event_id: String,
        digest: String,
    },

    #[error(
        "sealed artifact lifecycle transition conflicts for {event_id}/{digest}: existing {existing}, requested {requested}"
    )]
    ArtifactStateConflict {
        event_id: String,
        digest: String,
        existing: String,
        requested: String,
    },

    #[error(
        "sealed artifact lifecycle transition is illegal for {event_id}/{digest}: {from} -> {to}"
    )]
    ArtifactStateTransitionInvalid {
        event_id: String,
        digest: String,
        from: String,
        to: String,
    },

    #[error("run {run_id} already recorded a different terminal reason")]
    RunExitConflict { run_id: String },

    #[error("run owner lease for {run_id} is stale, expired, terminal, or does not match")]
    StaleRunOwnerLease { run_id: String },

    #[error("artifact degradation audit is permitted only for manual run {run_id}")]
    ManualArtifactDegradationRequiresManual { run_id: String },

    #[error("append {append_key:?} already published a sealed artifact and cannot degrade")]
    PublishedArtifactCannotDegrade { append_key: String },

    #[error("non-resumable effect for event {event_id} was not found")]
    NonResumableEffectNotFound { event_id: String },

    #[error("non-resumable effect for event {event_id} cannot be claimed from state {state}")]
    NonResumableEffectNotArmed {
        event_id: String,
        state: &'static str,
    },

    #[error("non-resumable effect lease for event {event_id} is stale or already consumed")]
    StaleNonResumableEffectLease { event_id: String },

    #[error("result event {result_event_id} does not match non-resumable effect {event_id}")]
    NonResumableEffectResultMismatch {
        event_id: String,
        result_event_id: String,
    },

    #[error("terminal reason effect_non_resumable_unknown for run {run_id} requires an executing effect with unknown outcome")]
    EffectNonResumableUnknownRequiresEffect { run_id: String },

    #[error("run {run_id} cannot complete with {count} unclaimed non-resumable effect(s)")]
    UnclaimedNonResumableEffects { run_id: String, count: usize },

    #[error("outbox lease for {outbox_id} is stale or does not exist")]
    StaleOutboxLease { outbox_id: String },

    #[error("outbox handler is already registered for destination {destination}")]
    DuplicateOutboxDestination { destination: String },
    #[error("outbox destination {destination} lacks the closed durable-dedupe certification")]
    UncertifiedOutboxDestination { destination: String },

    #[error("delivery payload is invalid for destination {destination}")]
    InvalidOutboxPayload { destination: String },

    #[error("ledger event chain integrity failed: {0}")]
    IntegrityViolation(String),

    #[error("clock did not provide a valid Unix millisecond timestamp")]
    Clock,
}
