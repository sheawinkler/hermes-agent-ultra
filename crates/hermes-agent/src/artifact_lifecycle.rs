//! Atomic redaction, sealing, reservation, publication, and ledger commit.

use hermes_core::execution_mode::{ExecutionMode, ResolvedExecutionMode};
use serde_json::Value;

use crate::run_ledger::{
    AppendIdentity as LedgerAppendIdentity, AppendKey, AppendReceipt, AppendRequest,
    AppendReservation, ArtifactDegradationReason, CanonicalJson, EffectIntent, EventId,
    LedgerError, ReplayAppendContract, ReplayArtifactFidelity, RunId, RunLedger, RunOwnerLease,
    SealedArtifactRef as LedgerSealedArtifactRef, SessionRunEventKind,
};
use crate::sealed_artifact::{
    prepare_artifact, prepare_artifact_bound, prepare_exact_runtime_artifact_bound,
    ArtifactFreeSpaceGuard, ArtifactRedactor, ArtifactSealer, ExactRuntimeEvidenceSchema,
    FilesystemSealedArtifactStore, OsArtifactKeyManager, SealedArtifactError, SealedArtifactRef,
    SealedArtifactStore,
};

#[derive(Debug, thiserror::Error)]
pub enum ArtifactLifecycleError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("artifact preparation failed before reservation: {0}")]
    ArtifactBeforeReservation(SealedArtifactError),
    #[error("artifact lifecycle failed after reservation: {0}")]
    ArtifactAfterReservation(SealedArtifactError),
}

pub type ArtifactLifecycleResult<T> = Result<T, ArtifactLifecycleError>;

pub struct ArtifactAppendRequest<'a> {
    pub mode: ResolvedExecutionMode,
    pub owner_lease: &'a RunOwnerLease,
    pub append_key: AppendKey,
    pub effect_intent: EffectIntent,
    pub public_payload: CanonicalJson,
    pub raw_artifact: &'a Value,
    pub redactor: &'a dyn ArtifactRedactor,
}

impl std::fmt::Debug for ArtifactAppendRequest<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ArtifactAppendRequest")
            .field("mode", &self.mode)
            .field("run_id", &self.owner_lease.run_id)
            .field("append_key", &self.append_key)
            .field("effect_intent", &self.effect_intent)
            .field("public_payload", &self.public_payload)
            .field("raw_artifact", &"[REDACTED]")
            .field("redactor", &self.redactor.version())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactAppendOutcome {
    Committed(AppendReceipt),
    InFlight {
        run_id: RunId,
        event_id: EventId,
        lease_generation: u64,
        lease_expires_at_ms: i64,
        published_sealed_artifact_ref: Option<LedgerSealedArtifactRef>,
    },
    ManualDegraded {
        reason: ArtifactDegradationReason,
        receipt: AppendReceipt,
    },
}

/// Typed replay contract for a runtime append whose private evidence is
/// required to reconstruct a run. Exact preparation is attempted only for the
/// selected closed schema; a rejection falls back to sealed metadata and
/// records that fidelity atomically with the append event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ReplayCriticalArtifactAppend {
    pub exact_schema: ExactRuntimeEvidenceSchema,
    pub obligation: ReplayAppendContract,
}

impl ReplayCriticalArtifactAppend {
    pub const fn new(
        exact_schema: ExactRuntimeEvidenceSchema,
        obligation: ReplayAppendContract,
    ) -> Self {
        Self {
            exact_schema,
            obligation,
        }
    }
}

pub struct ArtifactAppendCoordinator<'a> {
    ledger: &'a RunLedger,
    keys: &'a OsArtifactKeyManager,
    store: &'a dyn SealedArtifactStore,
    free_space_guard: Option<ArtifactFreeSpaceGuard<'a>>,
}

impl std::fmt::Debug for ArtifactAppendCoordinator<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ArtifactAppendCoordinator")
            .field("keys", &self.keys)
            .finish_non_exhaustive()
    }
}

impl<'a> ArtifactAppendCoordinator<'a> {
    /// Construct for a non-filesystem store or a test seam. Production
    /// filesystem callsites must use [`Self::new_with_filesystem_admission`].
    pub fn new(
        ledger: &'a RunLedger,
        keys: &'a OsArtifactKeyManager,
        store: &'a dyn SealedArtifactStore,
    ) -> Self {
        Self {
            ledger,
            keys,
            store,
            free_space_guard: None,
        }
    }

    /// Construct the production coordinator with real filesystem capacity
    /// admission. Successful writes reserve capacity before reservation or
    /// sealing; a manual bound I/O outage reserves only to record degradation.
    pub fn new_with_filesystem_admission(
        ledger: &'a RunLedger,
        keys: &'a OsArtifactKeyManager,
        store: &'a FilesystemSealedArtifactStore,
    ) -> Self {
        let free_space_guard = ArtifactFreeSpaceGuard::for_filesystem_store(store);
        Self {
            ledger,
            keys,
            store,
            free_space_guard: Some(free_space_guard),
        }
    }

    #[cfg(test)]
    pub(crate) fn new_with_free_space_guard(
        ledger: &'a RunLedger,
        keys: &'a OsArtifactKeyManager,
        store: &'a dyn SealedArtifactStore,
        free_space_guard: ArtifactFreeSpaceGuard<'a>,
    ) -> Self {
        Self {
            ledger,
            keys,
            store,
            free_space_guard: Some(free_space_guard),
        }
    }

    /// Append exactly once. Reservation losers never seal. Reclaimed leases
    /// verify and reuse the immutable published reference. New publication
    /// holds the store guard through the authoritative ledger commit.
    pub fn append(
        &self,
        request: &ArtifactAppendRequest<'_>,
    ) -> ArtifactLifecycleResult<ArtifactAppendOutcome> {
        self.append_inner(request, None, false, None)
    }

    /// Seal the canonical credential-free metadata wrapper while exposing only
    /// its closed identity projection to the authoritative ledger. After
    /// redaction and identity fingerprinting succeed, only a manual run may
    /// terminalize an owned unpublished reservation as a typed degradation
    /// when the sealing key or CAS store is unavailable.
    pub(crate) fn append_bound(
        &self,
        request: &ArtifactAppendRequest<'_>,
        private_evidence: &Value,
    ) -> ArtifactLifecycleResult<ArtifactAppendOutcome> {
        self.append_inner(request, Some(private_evidence), true, None)
    }

    pub(crate) fn append_bound_replay_critical(
        &self,
        request: &ArtifactAppendRequest<'_>,
        private_evidence: &Value,
        replay: ReplayCriticalArtifactAppend,
    ) -> ArtifactLifecycleResult<ArtifactAppendOutcome> {
        self.append_inner(request, Some(private_evidence), true, Some(replay))
    }

    fn append_inner(
        &self,
        request: &ArtifactAppendRequest<'_>,
        private_evidence: Option<&Value>,
        allow_manual_degradation: bool,
        replay: Option<ReplayCriticalArtifactAppend>,
    ) -> ArtifactLifecycleResult<ArtifactAppendOutcome> {
        // A brand-new successful append is admitted before its reservation
        // exists because capacity is sized from the actual private JCS payload.
        // A manual bound I/O outage is the narrow exception: it reserves only
        // to record a typed terminal degradation, never to attempt a write.
        let mut existing_identity = self.ledger.append_identity_for_key(&request.append_key)?;
        let fingerprint_key = match existing_identity.as_ref() {
            Some(identity) => self.keys.resolve_fingerprint_key(
                identity.private_fingerprint.key_id().as_str(),
                identity.private_fingerprint.key_version(),
            ),
            None => self.keys.active_fingerprint_key(),
        }
        .map_err(|error| {
            if existing_identity.is_some() {
                ArtifactLifecycleError::ArtifactAfterReservation(error)
            } else {
                ArtifactLifecycleError::ArtifactBeforeReservation(error)
            }
        })?;
        let (mut prepared, replay_fidelity) = match (private_evidence, replay) {
            (Some(private_evidence), Some(replay)) => {
                match prepare_exact_runtime_artifact_bound(
                    replay.exact_schema,
                    request.raw_artifact,
                    private_evidence,
                    request.redactor,
                    &fingerprint_key,
                ) {
                    Ok(prepared) => (prepared, ReplayArtifactFidelity::ExactPrivate),
                    Err(_) => (
                        prepare_artifact_bound(
                            request.raw_artifact,
                            private_evidence,
                            request.redactor,
                            &fingerprint_key,
                        )
                        .map_err(|error| {
                            if existing_identity.is_some() {
                                ArtifactLifecycleError::ArtifactAfterReservation(error)
                            } else {
                                ArtifactLifecycleError::ArtifactBeforeReservation(error)
                            }
                        })?,
                        ReplayArtifactFidelity::MetadataOnly,
                    ),
                }
            }
            (Some(private_evidence), None) => (
                prepare_artifact_bound(
                    request.raw_artifact,
                    private_evidence,
                    request.redactor,
                    &fingerprint_key,
                )
                .map_err(|error| {
                    if existing_identity.is_some() {
                        ArtifactLifecycleError::ArtifactAfterReservation(error)
                    } else {
                        ArtifactLifecycleError::ArtifactBeforeReservation(error)
                    }
                })?,
                ReplayArtifactFidelity::MetadataOnly,
            ),
            (None, Some(_)) => {
                return Err(ArtifactLifecycleError::ArtifactBeforeReservation(
                    SealedArtifactError::InvalidMetadata(
                        "replay-critical artifact lacks private evidence".to_string(),
                    ),
                ));
            }
            (None, None) => (
                prepare_artifact(request.raw_artifact, request.redactor, &fingerprint_key)
                    .map_err(|error| {
                        if existing_identity.is_some() {
                            ArtifactLifecycleError::ArtifactAfterReservation(error)
                        } else {
                            ArtifactLifecycleError::ArtifactBeforeReservation(error)
                        }
                    })?,
                ReplayArtifactFidelity::MetadataOnly,
            ),
        };
        let mut capacity_reservation = None;
        // A manual bound append may use a failed capacity probe only to create
        // its durable degradation audit. It must never continue to a write.
        let mut pre_reservation_degradation = None;
        if existing_identity.is_none() {
            if let Some(free_space_guard) = self.free_space_guard.as_ref() {
                match free_space_guard.reserve_for_prepared(&prepared) {
                    Ok(reservation) => capacity_reservation = Some(reservation),
                    Err(error) => {
                        if let Some(concurrent_identity) =
                            self.ledger.append_identity_for_key(&request.append_key)?
                        {
                            let historical_fingerprint_key = self
                                .keys
                                .resolve_fingerprint_key(
                                    concurrent_identity.private_fingerprint.key_id().as_str(),
                                    concurrent_identity.private_fingerprint.key_version(),
                                )
                                .map_err(ArtifactLifecycleError::ArtifactAfterReservation)?;
                            prepared
                                .rebind_fingerprint(&historical_fingerprint_key)
                                .map_err(ArtifactLifecycleError::ArtifactAfterReservation)?;
                            existing_identity = Some(concurrent_identity);
                        } else if let Some(reason) = degradation_reason(&error) {
                            if allow_manual_degradation
                                && request.mode.mode == ExecutionMode::Manual
                            {
                                pre_reservation_degradation = Some(reason);
                            } else {
                                return Err(ArtifactLifecycleError::ArtifactBeforeReservation(
                                    error,
                                ));
                            }
                        } else {
                            return Err(ArtifactLifecycleError::ArtifactBeforeReservation(error));
                        }
                    }
                }
            }
        }
        let identity = LedgerAppendIdentity::try_from(prepared.identity())?;
        let append_request = AppendRequest::new(
            request.owner_lease.run_id,
            request.append_key.clone(),
            identity.clone(),
            request.effect_intent,
            request.public_payload.clone(),
        );
        let append_request = match replay {
            Some(replay) => append_request.with_replay_contract(replay.obligation, replay_fidelity),
            None => append_request,
        };
        let reservation = self.ledger.reserve_append(
            request.mode,
            request.owner_lease,
            append_request,
        )?;

        match reservation {
            AppendReservation::Committed(receipt) => outcome_from_receipt(receipt),
            AppendReservation::InFlight {
                run_id,
                event_id,
                lease_generation,
                lease_expires_at_ms,
                published_sealed_artifact_ref,
            } => Ok(ArtifactAppendOutcome::InFlight {
                run_id,
                event_id,
                lease_generation,
                lease_expires_at_ms,
                published_sealed_artifact_ref,
            }),
            AppendReservation::Reserved(lease) => {
                if let Some(reason) = pre_reservation_degradation {
                    let receipt = self.ledger.degrade_reserved_append(
                        request.mode,
                        request.owner_lease,
                        &lease,
                        reason,
                    )?;
                    return outcome_from_receipt(receipt);
                }
                let sealer = ArtifactSealer::new(self.keys, self.store);
                if let Some(published) = lease.published_sealed_artifact_ref.as_ref() {
                    let artifact_reference = SealedArtifactRef::try_from(published)
                        .map_err(ArtifactLifecycleError::ArtifactAfterReservation)?;
                    sealer
                        .verify_exact_integrity(&artifact_reference)
                        .map_err(ArtifactLifecycleError::ArtifactAfterReservation)?;
                    let receipt = self.ledger.commit_reserved_append(
                        request.mode,
                        request.owner_lease,
                        &lease,
                        Some(published),
                    )?;
                    return outcome_from_receipt(receipt);
                }

                // A reclaimed unpublished reservation will write a new
                // envelope, so reserve capacity immediately before sealing and
                // retain that cross-process guard through the ledger commit.
                // Published references and committed/in-flight reservations do
                // not write and deliberately bypass this guard.
                if existing_identity.is_some() && capacity_reservation.is_none() {
                    if let Some(free_space_guard) = self.free_space_guard.as_ref() {
                        capacity_reservation = Some(
                            free_space_guard
                                .reserve_for_prepared(&prepared)
                                .map_err(ArtifactLifecycleError::ArtifactAfterReservation)?,
                        );
                    }
                }

                let pending = match sealer.seal(prepared) {
                    Ok(pending) => pending,
                    Err(error) => {
                        return self.handle_unpublished_reservation_failure(
                            request,
                            &lease,
                            error,
                            allow_manual_degradation,
                        );
                    }
                };
                let receipt = pending.commit_with(|sealed_identity, artifact_reference| {
                    let sealed_identity = LedgerAppendIdentity::try_from(&sealed_identity)?;
                    if sealed_identity != identity {
                        return Err(LedgerError::SealedArtifactIdentity(
                            "sealed append identity changed after reservation".to_string(),
                        ));
                    }
                    let ledger_reference = LedgerSealedArtifactRef::try_from(&artifact_reference)?;
                    let published = self.ledger.publish_sealed_ref(
                        request.mode,
                        request.owner_lease,
                        &lease,
                        ledger_reference,
                    )?;
                    self.ledger.commit_reserved_append(
                        request.mode,
                        request.owner_lease,
                        &lease,
                        Some(&published),
                    )
                })?;
                outcome_from_receipt(receipt)
            }
        }
    }

    fn handle_unpublished_reservation_failure(
        &self,
        request: &ArtifactAppendRequest<'_>,
        lease: &crate::run_ledger::AppendLease,
        error: SealedArtifactError,
        allow_manual_degradation: bool,
    ) -> ArtifactLifecycleResult<ArtifactAppendOutcome> {
        let Some(reason) = degradation_reason(&error) else {
            return Err(ArtifactLifecycleError::ArtifactAfterReservation(error));
        };
        if !allow_manual_degradation || request.mode.mode != ExecutionMode::Manual {
            return Err(ArtifactLifecycleError::ArtifactAfterReservation(error));
        }
        let receipt = self.ledger.degrade_reserved_append(
            request.mode,
            request.owner_lease,
            lease,
            reason,
        )?;
        outcome_from_receipt(receipt)
    }
}

fn outcome_from_receipt(receipt: AppendReceipt) -> ArtifactLifecycleResult<ArtifactAppendOutcome> {
    match receipt.event_kind {
        SessionRunEventKind::Append => Ok(ArtifactAppendOutcome::Committed(receipt)),
        SessionRunEventKind::ManualAppendDegradedAudit => {
            let reason = receipt.artifact_degradation_reason.ok_or_else(|| {
                LedgerError::IntegrityViolation(
                    "manual artifact degradation receipt lacks a typed reason".to_string(),
                )
            })?;
            Ok(ArtifactAppendOutcome::ManualDegraded { reason, receipt })
        }
        kind => Err(LedgerError::IntegrityViolation(format!(
            "append receipt resolved to unexpected event kind {}",
            kind.as_str()
        ))
        .into()),
    }
}

fn degradation_reason(error: &SealedArtifactError) -> Option<ArtifactDegradationReason> {
    match error {
        SealedArtifactError::KeyUnavailable(_) => {
            Some(ArtifactDegradationReason::SealingKeyUnavailable)
        }
        SealedArtifactError::Io(_) => Some(ArtifactDegradationReason::ArtifactStoreUnavailable),
        SealedArtifactError::Redaction(_)
        | SealedArtifactError::InvalidMetadata(_)
        | SealedArtifactError::Canonicalization(_)
        | SealedArtifactError::Cryptography(_)
        | SealedArtifactError::InvalidEnvelope(_)
        | SealedArtifactError::Unauthorized(_)
        | SealedArtifactError::ReferenceSnapshot(_)
        | SealedArtifactError::CasConflict(_)
        | SealedArtifactError::InjectedFailure(_)
        | SealedArtifactError::TooLarge { .. }
        | SealedArtifactError::InsufficientFreeSpace { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io;
    use std::path::Path;
    use std::sync::atomic::{AtomicBool, AtomicI64, AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, Mutex};
    use std::time::{Duration, SystemTime};

    use hermes_core::execution_mode::{ExecutionModeSource, ResolvedExecutionMode};
    use serde_json::json;
    use tempfile::TempDir;
    use zeroize::Zeroizing;

    use super::*;
    use crate::run_ledger::{
        ActionId, BranchId, DigestAbsence, LedgerClock, LedgerConnectionFactory, ModelId,
        ObjectiveId, ProvenanceDigest, ProviderId, RunLedgerConfig, RunOwnerId, RunProvenance,
        RunStart, SessionId, Sha256Digest, TaskId, TenantId,
    };
    use crate::sealed_artifact::{
        sealed_envelope_required_free_bytes, ArtifactCredentialProvisionGuard,
        ArtifactCredentialStore, ArtifactFreeSpaceGuard, ArtifactFreeSpaceProvider,
        ArtifactPublicationGuard, FilesystemSealedArtifactStore, OsArtifactKeyManagerConfig,
        RedactedArtifact,
    };

    #[derive(Debug)]
    struct TestClock(AtomicI64);

    impl TestClock {
        fn new(now: i64) -> Self {
            Self(AtomicI64::new(now))
        }

        fn advance(&self, millis: i64) {
            self.0.fetch_add(millis, Ordering::SeqCst);
        }
    }

    impl LedgerClock for TestClock {
        fn now_unix_ms(&self) -> Result<i64, LedgerError> {
            Ok(self.0.load(Ordering::SeqCst))
        }
    }

    struct TestCredentialGuard;
    impl ArtifactCredentialProvisionGuard for TestCredentialGuard {}

    #[derive(Default)]
    struct MemoryCredentialStore {
        unavailable: AtomicBool,
        secrets: Mutex<HashMap<(String, String), Vec<u8>>>,
    }

    impl MemoryCredentialStore {
        fn set_unavailable(&self, unavailable: bool) {
            self.unavailable.store(unavailable, Ordering::SeqCst);
        }
    }

    impl ArtifactCredentialStore for MemoryCredentialStore {
        fn load_secret(
            &self,
            service: &str,
            account: &str,
        ) -> Result<Option<Zeroizing<Vec<u8>>>, SealedArtifactError> {
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(SealedArtifactError::KeyUnavailable(
                    "injected credential outage".to_string(),
                ));
            }
            Ok(self
                .secrets
                .lock()
                .unwrap()
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
            self.secrets
                .lock()
                .unwrap()
                .entry((service.to_string(), account.to_string()))
                .or_insert_with(|| secret.to_vec());
            Ok(())
        }

        fn acquire_provision_lock(
            &self,
            _service: &str,
            _store_namespace: &str,
        ) -> Result<Box<dyn ArtifactCredentialProvisionGuard>, SealedArtifactError> {
            Ok(Box::new(TestCredentialGuard))
        }
    }

    struct TestPublicationGuard;
    impl ArtifactPublicationGuard for TestPublicationGuard {}

    #[derive(Default)]
    struct CountingStore {
        fail_publish: AtomicBool,
        publishes: AtomicUsize,
        entries: Mutex<HashMap<String, Vec<u8>>>,
    }

    impl CountingStore {
        fn set_fail_publish(&self, fail: bool) {
            self.fail_publish.store(fail, Ordering::SeqCst);
        }

        fn publish_count(&self) -> usize {
            self.publishes.load(Ordering::SeqCst)
        }

        fn corrupt_only_entry(&self) {
            let mut entries = self.entries.lock().unwrap();
            let bytes = entries.values_mut().next().expect("published artifact");
            bytes[0] ^= 0xff;
        }
    }

    impl SealedArtifactStore for CountingStore {
        fn publish_for_commit(
            &self,
            envelope_digest: &str,
            bytes: &[u8],
        ) -> Result<Box<dyn ArtifactPublicationGuard>, SealedArtifactError> {
            self.publishes.fetch_add(1, Ordering::SeqCst);
            if self.fail_publish.load(Ordering::SeqCst) {
                return Err(SealedArtifactError::Io(io::Error::other(
                    "injected artifact store outage",
                )));
            }
            let mut entries = self.entries.lock().unwrap();
            match entries.get(envelope_digest) {
                Some(existing) if existing != bytes => {
                    return Err(SealedArtifactError::CasConflict(
                        envelope_digest.to_string(),
                    ));
                }
                Some(_) => {}
                None => {
                    entries.insert(envelope_digest.to_string(), bytes.to_vec());
                }
            }
            Ok(Box::new(TestPublicationGuard))
        }

        fn read(&self, envelope_digest: &str) -> Result<Vec<u8>, SealedArtifactError> {
            self.entries
                .lock()
                .unwrap()
                .get(envelope_digest)
                .cloned()
                .ok_or_else(|| {
                    SealedArtifactError::Io(io::Error::new(
                        io::ErrorKind::NotFound,
                        "artifact not found",
                    ))
                })
        }

        fn remove(&self, envelope_digest: &str) -> Result<(), SealedArtifactError> {
            self.entries.lock().unwrap().remove(envelope_digest);
            Ok(())
        }
    }

    #[derive(Debug)]
    struct FixedFreeSpace {
        available_bytes: u64,
        calls: AtomicUsize,
    }

    impl FixedFreeSpace {
        fn new(available_bytes: u64) -> Self {
            Self {
                available_bytes,
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl ArtifactFreeSpaceProvider for FixedFreeSpace {
        fn available_bytes(&self, _root: &Path) -> Result<u64, SealedArtifactError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.available_bytes)
        }
    }

    struct UnavailableFreeSpace;

    impl ArtifactFreeSpaceProvider for UnavailableFreeSpace {
        fn available_bytes(&self, _root: &Path) -> Result<u64, SealedArtifactError> {
            Err(SealedArtifactError::Io(io::Error::new(
                io::ErrorKind::NotFound,
                "artifact root unavailable",
            )))
        }
    }

    struct OneEnvelopeFreeSpace {
        first_available_bytes: u64,
        calls: AtomicUsize,
    }

    impl OneEnvelopeFreeSpace {
        fn new(first_available_bytes: u64) -> Self {
            Self {
                first_available_bytes,
                calls: AtomicUsize::new(0),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    impl ArtifactFreeSpaceProvider for OneEnvelopeFreeSpace {
        fn available_bytes(&self, _root: &Path) -> Result<u64, SealedArtifactError> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(if call == 0 {
                self.first_available_bytes
            } else {
                0
            })
        }
    }

    struct TestRedactor;

    impl ArtifactRedactor for TestRedactor {
        fn version(&self) -> &str {
            "test-redactor-v1"
        }

        fn redact(&self, raw: &Value) -> Result<RedactedArtifact, SealedArtifactError> {
            Ok(RedactedArtifact {
                public_identity: raw.get("identity").cloned().unwrap_or(Value::Null),
                credential_free_private: raw.get("private").cloned().unwrap_or(Value::Null),
            })
        }
    }

    struct Fixture {
        _directory: TempDir,
        ledger: Arc<RunLedger>,
        clock: Arc<TestClock>,
        mode: ResolvedExecutionMode,
        owner_lease: RunOwnerLease,
    }

    fn fixture(execution_mode: ExecutionMode) -> Fixture {
        let directory = tempfile::tempdir().unwrap();
        let factory = LedgerConnectionFactory::new(directory.path().join("sessions.db"));
        factory.migrate_and_cut_over_v0_to_v1().unwrap();
        let clock = Arc::new(TestClock::new(1_700_000_000_000));
        let ledger = Arc::new(
            RunLedger::open_with_clock(
                factory,
                RunLedgerConfig {
                    append_lease_duration: Duration::from_millis(10),
                    outbox_lease_duration: Duration::from_millis(10),
                    owner_lease_duration: Duration::from_secs(30),
                    max_delivery_attempts: 3,
                },
                clock.clone(),
            )
            .unwrap(),
        );
        let mode = ResolvedExecutionMode {
            mode: execution_mode,
            source: ExecutionModeSource::BuiltInDefault,
        };
        let run_id = RunId::new();
        let created = ledger
            .create_run_and_start(
                mode,
                RunOwnerId::new("artifact-test-owner").unwrap(),
                RunStart::new(
                    run_id,
                    provenance(),
                    canonical(json!({"start": "artifact-lifecycle-test"})),
                ),
            )
            .unwrap();
        Fixture {
            _directory: directory,
            ledger,
            clock,
            mode,
            owner_lease: created.owner_lease,
        }
    }

    fn provenance() -> RunProvenance {
        RunProvenance::new(
            SessionId::new("artifact-session").unwrap(),
            None,
            BranchId::new("main").unwrap(),
            TenantId::new("tenant").unwrap(),
            ActionId::new("artifact-append").unwrap(),
            ObjectiveId::new("objective").unwrap(),
            TaskId::new("task").unwrap(),
            ModelId::new("model").unwrap(),
            ProviderId::new("provider").unwrap(),
            digest(1),
            ProvenanceDigest::present(digest(2)),
            ProvenanceDigest::present(digest(3)),
            ProvenanceDigest::absent(DigestAbsence::Unavailable),
            ProvenanceDigest::present(digest(4)),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::present(digest(5)),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
        )
    }

    fn digest(byte: u8) -> Sha256Digest {
        Sha256Digest::new(hex::encode([byte; 32])).unwrap()
    }

    fn canonical(value: Value) -> CanonicalJson {
        CanonicalJson::from_value(&value).unwrap()
    }

    fn key_manager(
        credentials: Arc<dyn ArtifactCredentialStore>,
        sealing_key: &str,
        fingerprint_key: &str,
        fingerprint_version: u32,
    ) -> OsArtifactKeyManager {
        OsArtifactKeyManager::with_store(
            OsArtifactKeyManagerConfig::for_namespace(
                "artifact-test-service",
                "artifact-test-namespace",
                sealing_key,
                fingerprint_key,
                fingerprint_version,
            )
            .unwrap(),
            credentials,
        )
    }

    fn raw_artifact() -> Value {
        json!({
            "identity": {"tenant": "public"},
            "private": {"credential_free": "secret-derived-value"},
        })
    }

    fn committed(outcome: ArtifactAppendOutcome) -> AppendReceipt {
        match outcome {
            ArtifactAppendOutcome::Committed(receipt) => receipt,
            other => panic!("expected committed append, got {other:?}"),
        }
    }

    fn degraded(outcome: ArtifactAppendOutcome) -> AppendReceipt {
        match outcome {
            ArtifactAppendOutcome::ManualDegraded { receipt, .. } => receipt,
            other => panic!("expected manual degradation, got {other:?}"),
        }
    }

    #[test]
    fn committed_retry_bypasses_low_space_without_republication() {
        let fixture = fixture(ExecutionMode::Manual);
        let credentials = Arc::new(MemoryCredentialStore::default());
        let keys = key_manager(credentials, "seal-v1", "fingerprint-v1", 1);
        keys.provision_active_keys().unwrap();
        let store = CountingStore::default();
        let coordinator = ArtifactAppendCoordinator::new(&fixture.ledger, &keys, &store);
        let append_key = AppendKey::new("artifact-once").unwrap();
        let public_payload = canonical(json!({"request": 1}));
        let raw = raw_artifact();
        let redactor = TestRedactor;
        let request = ArtifactAppendRequest {
            mode: fixture.mode,
            owner_lease: &fixture.owner_lease,
            append_key,
            effect_intent: EffectIntent::None,
            public_payload,
            raw_artifact: &raw,
            redactor: &redactor,
        };

        let first = committed(coordinator.append(&request).unwrap());
        let low_space = FixedFreeSpace::new(0);
        let guard = ArtifactFreeSpaceGuard::with_provider(fixture._directory.path(), &low_space);
        let retry = ArtifactAppendCoordinator::new_with_free_space_guard(
            &fixture.ledger,
            &keys,
            &store,
            guard,
        );
        let second = committed(retry.append(&request).unwrap());

        assert_eq!(first, second);
        assert!(first.sealed_artifact_ref.is_some());
        assert_eq!(store.publish_count(), 1);
        assert_eq!(low_space.calls(), 0);
        assert_eq!(
            fixture
                .ledger
                .events_for_run(fixture.owner_lease.run_id)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn in_flight_retry_bypasses_low_space_without_side_effects() {
        let fixture = fixture(ExecutionMode::Manual);
        let credentials = Arc::new(MemoryCredentialStore::default());
        let keys = key_manager(credentials, "seal-v1", "fingerprint-v1", 1);
        keys.provision_active_keys().unwrap();
        let store = CountingStore::default();
        let append_key = AppendKey::new("disk-threshold-in-flight").unwrap();
        let public_payload = canonical(json!({"request": "in-flight-retry"}));
        let raw = raw_artifact();
        let redactor = TestRedactor;
        let fingerprint_key = keys.active_fingerprint_key().unwrap();
        let prepared = prepare_artifact_bound(&raw, &raw, &redactor, &fingerprint_key).unwrap();
        let identity = LedgerAppendIdentity::try_from(prepared.identity()).unwrap();

        assert!(matches!(
            fixture
                .ledger
                .reserve_append(
                    fixture.mode,
                    &fixture.owner_lease,
                    AppendRequest::new(
                        fixture.owner_lease.run_id,
                        append_key.clone(),
                        identity,
                        EffectIntent::None,
                        public_payload.clone(),
                    ),
                )
                .unwrap(),
            AppendReservation::Reserved(_)
        ));

        let low_space = FixedFreeSpace::new(0);
        let guard = ArtifactFreeSpaceGuard::with_provider(fixture._directory.path(), &low_space);
        let coordinator = ArtifactAppendCoordinator::new_with_free_space_guard(
            &fixture.ledger,
            &keys,
            &store,
            guard,
        );
        let request = ArtifactAppendRequest {
            mode: fixture.mode,
            owner_lease: &fixture.owner_lease,
            append_key,
            effect_intent: EffectIntent::None,
            public_payload,
            raw_artifact: &raw,
            redactor: &redactor,
        };

        assert!(matches!(
            coordinator.append_bound(&request, &raw).unwrap(),
            ArtifactAppendOutcome::InFlight {
                published_sealed_artifact_ref: None,
                ..
            }
        ));
        assert_eq!(low_space.calls(), 0);
        assert_eq!(store.publish_count(), 0);
        assert_eq!(
            fixture
                .ledger
                .events_for_run(fixture.owner_lease.run_id)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn bound_store_outage_terminalizes_reserved_manual_append_and_retry() {
        let fixture = fixture(ExecutionMode::Manual);
        let credentials = Arc::new(MemoryCredentialStore::default());
        let keys = key_manager(credentials, "seal-v1", "fingerprint-v1", 1);
        keys.provision_active_keys().unwrap();
        let store = CountingStore::default();
        store.set_fail_publish(true);
        let coordinator = ArtifactAppendCoordinator::new(&fixture.ledger, &keys, &store);
        let append_key = AppendKey::new("manual-store-outage").unwrap();
        let public_payload = canonical(json!({"request": 2}));
        let raw = raw_artifact();
        let redactor = TestRedactor;
        let request = ArtifactAppendRequest {
            mode: fixture.mode,
            owner_lease: &fixture.owner_lease,
            append_key: append_key.clone(),
            effect_intent: EffectIntent::AlphaRuntimeV1,
            public_payload,
            raw_artifact: &raw,
            redactor: &redactor,
        };

        let first = degraded(coordinator.append_bound(&request, &raw).unwrap());
        store.set_fail_publish(false);
        let second = degraded(coordinator.append_bound(&request, &raw).unwrap());

        assert_eq!(first, second);
        assert_eq!(
            first.artifact_degradation_reason,
            Some(ArtifactDegradationReason::ArtifactStoreUnavailable)
        );
        assert_eq!(
            first.event_kind,
            SessionRunEventKind::ManualAppendDegradedAudit
        );
        assert!(first.sealed_artifact_ref.is_none());
        assert!(first.outbox_ids.is_empty());
        assert_eq!(store.publish_count(), 1);
        let events = fixture
            .ledger
            .events_for_run(fixture.owner_lease.run_id)
            .unwrap();
        assert_eq!(events.len(), 2);
        assert_ne!(events[1].append_key.as_ref(), Some(&append_key));
        assert!(events[1]
            .append_key
            .as_ref()
            .expect("manual degradation audit key")
            .as_str()
            .starts_with("manual-degradation-"));
        assert_eq!(events[1].effect_intent, EffectIntent::None);
        assert!(fixture
            .ledger
            .outbox_for_run(fixture.owner_lease.run_id)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn manual_bound_pre_reservation_store_outage_is_audited_without_write() {
        let fixture = fixture(ExecutionMode::Manual);
        let credentials = Arc::new(MemoryCredentialStore::default());
        let keys = key_manager(credentials, "seal-v1", "fingerprint-v1", 1);
        keys.provision_active_keys().unwrap();
        let store = CountingStore::default();
        let unavailable = UnavailableFreeSpace;
        let guard = ArtifactFreeSpaceGuard::with_provider(fixture._directory.path(), &unavailable);
        let coordinator = ArtifactAppendCoordinator::new_with_free_space_guard(
            &fixture.ledger,
            &keys,
            &store,
            guard,
        );
        let append_key = AppendKey::new("manual-pre-reservation-store-outage").unwrap();
        let raw = raw_artifact();
        let redactor = TestRedactor;
        let request = ArtifactAppendRequest {
            mode: fixture.mode,
            owner_lease: &fixture.owner_lease,
            append_key,
            effect_intent: EffectIntent::None,
            public_payload: canonical(json!({"request": "manual-store-outage"})),
            raw_artifact: &raw,
            redactor: &redactor,
        };

        let receipt = degraded(coordinator.append_bound(&request, &raw).unwrap());
        assert_eq!(
            receipt.artifact_degradation_reason,
            Some(ArtifactDegradationReason::ArtifactStoreUnavailable)
        );
        assert_eq!(store.publish_count(), 0);
        assert_eq!(
            fixture
                .ledger
                .events_for_run(fixture.owner_lease.run_id)
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn nonmanual_bound_pre_reservation_store_outage_fails_without_audit() {
        let fixture = fixture(ExecutionMode::Manual);
        let credentials = Arc::new(MemoryCredentialStore::default());
        let keys = key_manager(credentials, "seal-v1", "fingerprint-v1", 1);
        keys.provision_active_keys().unwrap();
        let store = CountingStore::default();
        let unavailable = UnavailableFreeSpace;
        let guard = ArtifactFreeSpaceGuard::with_provider(fixture._directory.path(), &unavailable);
        let coordinator = ArtifactAppendCoordinator::new_with_free_space_guard(
            &fixture.ledger,
            &keys,
            &store,
            guard,
        );
        let raw = raw_artifact();
        let redactor = TestRedactor;
        let request = ArtifactAppendRequest {
            mode: ResolvedExecutionMode {
                mode: ExecutionMode::Reviewed,
                source: ExecutionModeSource::Cli,
            },
            owner_lease: &fixture.owner_lease,
            append_key: AppendKey::new("reviewed-pre-reservation-store-outage").unwrap(),
            effect_intent: EffectIntent::None,
            public_payload: canonical(json!({"request": "must-fail-closed"})),
            raw_artifact: &raw,
            redactor: &redactor,
        };

        assert!(matches!(
            coordinator.append_bound(&request, &raw),
            Err(ArtifactLifecycleError::ArtifactBeforeReservation(
                SealedArtifactError::Io(_)
            ))
        ));
        assert_eq!(store.publish_count(), 0);
        assert_eq!(
            fixture
                .ledger
                .events_for_run(fixture.owner_lease.run_id)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn bound_sealing_key_outage_tombstone_survives_recovery_and_rejects_mismatch() {
        let fixture = fixture(ExecutionMode::Manual);
        let credentials = Arc::new(MemoryCredentialStore::default());
        let provisioned = key_manager(credentials.clone(), "seal-v1", "fingerprint-v1", 1);
        provisioned.provision_active_keys().unwrap();
        let missing_sealing_key = key_manager(credentials, "seal-missing", "fingerprint-v1", 1);
        let store = CountingStore::default();
        let coordinator =
            ArtifactAppendCoordinator::new(&fixture.ledger, &missing_sealing_key, &store);
        let append_key = AppendKey::new("manual-sealing-key-outage").unwrap();
        let public_payload = canonical(json!({"request": 3}));
        let raw = raw_artifact();
        let redactor = TestRedactor;
        let request = ArtifactAppendRequest {
            mode: fixture.mode,
            owner_lease: &fixture.owner_lease,
            append_key: append_key.clone(),
            effect_intent: EffectIntent::AlphaRuntimeV1,
            public_payload: public_payload.clone(),
            raw_artifact: &raw,
            redactor: &redactor,
        };

        let first = degraded(coordinator.append_bound(&request, &raw).unwrap());
        fixture.clock.advance(30_001);
        let during_outage_with_expired_owner =
            degraded(coordinator.append_bound(&request, &raw).unwrap());
        let second = degraded(coordinator.append_bound(&request, &raw).unwrap());

        assert_eq!(first, second);
        assert_eq!(first, during_outage_with_expired_owner);
        assert_eq!(
            first.artifact_degradation_reason,
            Some(ArtifactDegradationReason::SealingKeyUnavailable)
        );
        assert_eq!(store.publish_count(), 0);
        let events = fixture
            .ledger
            .events_for_run(fixture.owner_lease.run_id)
            .unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[1].effect_intent, EffectIntent::None);
        assert!(events[1].sealed_artifact_ref.is_none());
        assert!(fixture
            .ledger
            .outbox_for_run(fixture.owner_lease.run_id)
            .unwrap()
            .is_empty());
        let mismatched_payload = canonical(json!({"request": "different"}));
        let mismatch = ArtifactAppendRequest {
            public_payload: mismatched_payload,
            ..request
        };
        assert!(matches!(
            coordinator.append_bound(&mismatch, &raw),
            Err(ArtifactLifecycleError::Ledger(
                LedgerError::AppendKeyConflict { .. }
            ))
        ));
    }

    #[test]
    fn unbound_append_cannot_write_manual_degradation_audit() {
        let fixture = fixture(ExecutionMode::Manual);
        let credentials = Arc::new(MemoryCredentialStore::default());
        let provisioned = key_manager(credentials.clone(), "seal-v1", "fingerprint-v1", 1);
        provisioned.provision_active_keys().unwrap();
        let missing_sealing_key = key_manager(credentials, "seal-missing", "fingerprint-v1", 1);
        let store = CountingStore::default();
        let coordinator =
            ArtifactAppendCoordinator::new(&fixture.ledger, &missing_sealing_key, &store);
        let raw = raw_artifact();
        let request = ArtifactAppendRequest {
            mode: fixture.mode,
            owner_lease: &fixture.owner_lease,
            append_key: AppendKey::new("unbound-no-manual-degradation").unwrap(),
            effect_intent: EffectIntent::None,
            public_payload: canonical(json!({"request": "must-fail-closed"})),
            raw_artifact: &raw,
            redactor: &TestRedactor,
        };

        assert!(matches!(
            coordinator.append(&request),
            Err(ArtifactLifecycleError::ArtifactAfterReservation(
                SealedArtifactError::KeyUnavailable(_)
            ))
        ));
        assert_eq!(
            fixture
                .ledger
                .events_for_run(fixture.owner_lease.run_id)
                .unwrap()
                .len(),
            1
        );
        assert!(fixture
            .ledger
            .outbox_for_run(fixture.owner_lease.run_id)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn manual_identity_key_outage_is_fatal_before_reservation() {
        let fixture = fixture(ExecutionMode::Manual);
        let credentials = Arc::new(MemoryCredentialStore::default());
        let provisioned = key_manager(credentials.clone(), "seal-v1", "fingerprint-v1", 1);
        provisioned.provision_active_keys().unwrap();
        let missing_identity_key = key_manager(credentials, "seal-v1", "fingerprint-missing", 1);
        let store = CountingStore::default();
        let coordinator =
            ArtifactAppendCoordinator::new(&fixture.ledger, &missing_identity_key, &store);
        let raw = raw_artifact();
        let request = ArtifactAppendRequest {
            mode: fixture.mode,
            owner_lease: &fixture.owner_lease,
            append_key: AppendKey::new("manual-identity-key-outage").unwrap(),
            effect_intent: EffectIntent::None,
            public_payload: canonical(json!({"request": "identity-required"})),
            raw_artifact: &raw,
            redactor: &TestRedactor,
        };

        assert!(matches!(
            coordinator.append_bound(&request, &raw),
            Err(ArtifactLifecycleError::ArtifactBeforeReservation(
                SealedArtifactError::KeyUnavailable(_)
            ))
        ));
        assert_eq!(store.publish_count(), 0);
        assert_eq!(
            fixture
                .ledger
                .events_for_run(fixture.owner_lease.run_id)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn all_nonmanual_modes_fail_before_degradation() {
        let fixture = fixture(ExecutionMode::Manual);
        let credentials = Arc::new(MemoryCredentialStore::default());
        let keys = key_manager(credentials.clone(), "seal-v1", "fingerprint-v1", 1);
        keys.provision_active_keys().unwrap();
        credentials.set_unavailable(true);
        let store = CountingStore::default();
        let coordinator = ArtifactAppendCoordinator::new(&fixture.ledger, &keys, &store);
        let redactor = TestRedactor;

        for execution_mode in [
            ExecutionMode::Reviewed,
            ExecutionMode::AutonomousLocal,
            ExecutionMode::Ci,
            ExecutionMode::RemoteWorker,
        ] {
            let raw = raw_artifact();
            let request = ArtifactAppendRequest {
                mode: ResolvedExecutionMode {
                    mode: execution_mode,
                    source: ExecutionModeSource::Cli,
                },
                owner_lease: &fixture.owner_lease,
                append_key: AppendKey::new(format!("nonmanual-key-outage-{execution_mode}"))
                    .unwrap(),
                effect_intent: EffectIntent::None,
                public_payload: canonical(json!({"request": "identity-required"})),
                raw_artifact: &raw,
                redactor: &redactor,
            };

            assert!(matches!(
                coordinator.append_bound(&request, &raw),
                Err(ArtifactLifecycleError::ArtifactBeforeReservation(
                    SealedArtifactError::KeyUnavailable(_)
                ))
            ));
        }

        assert_eq!(store.publish_count(), 0);
        assert_eq!(
            fixture
                .ledger
                .events_for_run(fixture.owner_lease.run_id)
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn disk_threshold_fails_before_reservation_in_every_mode() {
        let fixture = fixture(ExecutionMode::Manual);
        let credentials = Arc::new(MemoryCredentialStore::default());
        let keys = key_manager(credentials, "seal-v1", "fingerprint-v1", 1);
        keys.provision_active_keys().unwrap();
        let store = CountingStore::default();
        let free_space = FixedFreeSpace::new(0);
        let guard = ArtifactFreeSpaceGuard::with_provider(fixture._directory.path(), &free_space);
        let coordinator = ArtifactAppendCoordinator::new_with_free_space_guard(
            &fixture.ledger,
            &keys,
            &store,
            guard,
        );
        let redactor = TestRedactor;

        for execution_mode in [
            ExecutionMode::Manual,
            ExecutionMode::Reviewed,
            ExecutionMode::AutonomousLocal,
            ExecutionMode::Ci,
            ExecutionMode::RemoteWorker,
        ] {
            let raw = raw_artifact();
            let append_key = AppendKey::new(format!("disk-threshold-{execution_mode}")).unwrap();
            let request = ArtifactAppendRequest {
                mode: ResolvedExecutionMode {
                    mode: execution_mode,
                    source: ExecutionModeSource::Cli,
                },
                owner_lease: &fixture.owner_lease,
                append_key: append_key.clone(),
                effect_intent: EffectIntent::None,
                public_payload: canonical(json!({"request": "disk-admission"})),
                raw_artifact: &raw,
                redactor: &redactor,
            };

            match coordinator.append_bound(&request, &raw) {
                Err(ArtifactLifecycleError::ArtifactBeforeReservation(
                    SealedArtifactError::InsufficientFreeSpace {
                        available_bytes,
                        minimum_bytes,
                    },
                )) => {
                    assert_eq!(available_bytes, 0);
                    assert!(minimum_bytes > 0);
                }
                other => panic!("expected pre-reservation capacity failure, got {other:?}"),
            }
            assert!(fixture
                .ledger
                .append_identity_for_key(&append_key)
                .unwrap()
                .is_none());
        }

        assert_eq!(free_space.calls(), 5);
        assert_eq!(store.publish_count(), 0);
        assert_eq!(
            fixture
                .ledger
                .events_for_run(fixture.owner_lease.run_id)
                .unwrap()
                .len(),
            1
        );
        assert!(fixture
            .ledger
            .outbox_for_run(fixture.owner_lease.run_id)
            .unwrap()
            .is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn capacity_reservation_allows_only_one_of_two_barrier_writers() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let fixture = fixture(ExecutionMode::Manual);
        let artifact_root = fixture._directory.path().join("artifact-cas");
        fs::create_dir(&artifact_root).unwrap();
        fs::set_permissions(&artifact_root, fs::Permissions::from_mode(0o700)).unwrap();
        let store = Arc::new(FilesystemSealedArtifactStore::open(&artifact_root).unwrap());
        let credentials = Arc::new(MemoryCredentialStore::default());
        let key_config = OsArtifactKeyManagerConfig::new(
            "artifact-test-service",
            "seal-v1",
            "fingerprint-v1",
            1,
            store.as_ref(),
        )
        .unwrap();
        let keys = Arc::new(OsArtifactKeyManager::with_store(key_config, credentials));
        keys.provision_active_keys_for_store(store.as_ref())
            .unwrap();

        let sizing_raw = raw_artifact();
        let sizing_redactor = TestRedactor;
        let sizing_key = keys.active_fingerprint_key().unwrap();
        let sizing_prepared = prepare_artifact(&sizing_raw, &sizing_redactor, &sizing_key).unwrap();
        let one_envelope_capacity =
            sealed_envelope_required_free_bytes(sizing_prepared.private_jcs_len()).unwrap();
        let free_space = Arc::new(OneEnvelopeFreeSpace::new(one_envelope_capacity));
        let start = Arc::new(Barrier::new(3));
        let mode = fixture.mode;
        let owner_lease = fixture.owner_lease.clone();
        let ledger = fixture.ledger.clone();

        let (left, right) = std::thread::scope(|scope| {
            let start_left = start.clone();
            let ledger_left = ledger.clone();
            let keys_left = keys.clone();
            let store_left = store.clone();
            let free_space_left = free_space.clone();
            let root_left = artifact_root.clone();
            let owner_left = owner_lease.clone();
            let left = scope.spawn(move || {
                start_left.wait();
                let guard =
                    ArtifactFreeSpaceGuard::with_provider(&root_left, free_space_left.as_ref());
                let coordinator = ArtifactAppendCoordinator::new_with_free_space_guard(
                    &ledger_left,
                    &keys_left,
                    store_left.as_ref(),
                    guard,
                );
                let raw = raw_artifact();
                let redactor = TestRedactor;
                let request = ArtifactAppendRequest {
                    mode,
                    owner_lease: &owner_left,
                    append_key: AppendKey::new("capacity-race-left").unwrap(),
                    effect_intent: EffectIntent::None,
                    public_payload: canonical(json!({"request": "capacity-race-left"})),
                    raw_artifact: &raw,
                    redactor: &redactor,
                };
                coordinator.append(&request)
            });

            let start_right = start.clone();
            let ledger_right = ledger.clone();
            let keys_right = keys.clone();
            let store_right = store.clone();
            let free_space_right = free_space.clone();
            let root_right = artifact_root.clone();
            let owner_right = owner_lease.clone();
            let right = scope.spawn(move || {
                start_right.wait();
                let guard =
                    ArtifactFreeSpaceGuard::with_provider(&root_right, free_space_right.as_ref());
                let coordinator = ArtifactAppendCoordinator::new_with_free_space_guard(
                    &ledger_right,
                    &keys_right,
                    store_right.as_ref(),
                    guard,
                );
                let raw = raw_artifact();
                let redactor = TestRedactor;
                let request = ArtifactAppendRequest {
                    mode,
                    owner_lease: &owner_right,
                    append_key: AppendKey::new("capacity-race-right").unwrap(),
                    effect_intent: EffectIntent::None,
                    public_payload: canonical(json!({"request": "capacity-race-right"})),
                    raw_artifact: &raw,
                    redactor: &redactor,
                };
                coordinator.append(&request)
            });

            start.wait();
            (left.join().unwrap(), right.join().unwrap())
        });

        let outcomes = [left, right];
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| {
                    matches!(outcome.as_ref(), Ok(ArtifactAppendOutcome::Committed(_)))
                })
                .count(),
            1
        );
        assert_eq!(
            outcomes
                .iter()
                .filter(|outcome| {
                    matches!(
                        outcome.as_ref(),
                        Err(ArtifactLifecycleError::ArtifactBeforeReservation(
                            SealedArtifactError::InsufficientFreeSpace {
                                available_bytes: 0,
                                ..
                            }
                        ))
                    )
                })
                .count(),
            1
        );
        assert_eq!(free_space.calls(), 2);
        assert_eq!(
            fixture
                .ledger
                .events_for_run(fixture.owner_lease.run_id)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            usize::from(
                fixture
                    .ledger
                    .append_identity_for_key(&AppendKey::new("capacity-race-left").unwrap())
                    .unwrap()
                    .is_some(),
            ) + usize::from(
                fixture
                    .ledger
                    .append_identity_for_key(&AppendKey::new("capacity-race-right").unwrap())
                    .unwrap()
                    .is_some(),
            ),
            1
        );
        assert!(fixture
            .ledger
            .outbox_for_run(fixture.owner_lease.run_id)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn reclaimed_lease_uses_historical_fingerprint_key_after_rotation() {
        let fixture = fixture(ExecutionMode::Manual);
        let credentials = Arc::new(MemoryCredentialStore::default());
        let old_keys = key_manager(credentials.clone(), "seal-old", "fingerprint-old", 1);
        old_keys.provision_active_keys().unwrap();
        let store = CountingStore::default();
        let append_key = AppendKey::new("rotated-fingerprint").unwrap();
        let public_payload = canonical(json!({"request": 5}));
        let raw = raw_artifact();
        let redactor = TestRedactor;
        let fingerprint_key = old_keys.active_fingerprint_key().unwrap();
        let prepared = prepare_artifact(&raw, &redactor, &fingerprint_key).unwrap();
        let identity = LedgerAppendIdentity::try_from(prepared.identity()).unwrap();
        assert!(matches!(
            fixture
                .ledger
                .reserve_append(
                    fixture.mode,
                    &fixture.owner_lease,
                    AppendRequest::new(
                        fixture.owner_lease.run_id,
                        append_key.clone(),
                        identity,
                        EffectIntent::None,
                        public_payload.clone(),
                    ),
                )
                .unwrap(),
            AppendReservation::Reserved(_)
        ));
        drop(prepared);
        fixture.clock.advance(11);

        let new_keys = key_manager(credentials, "seal-new", "fingerprint-new", 2);
        new_keys.provision_active_keys().unwrap();
        let coordinator = ArtifactAppendCoordinator::new(&fixture.ledger, &new_keys, &store);
        let request = ArtifactAppendRequest {
            mode: fixture.mode,
            owner_lease: &fixture.owner_lease,
            append_key,
            effect_intent: EffectIntent::None,
            public_payload,
            raw_artifact: &raw,
            redactor: &redactor,
        };
        let receipt = committed(coordinator.append(&request).unwrap());

        assert_eq!(
            receipt.sealed_artifact_ref.unwrap().key_id().as_str(),
            "seal-new"
        );
        let events = fixture
            .ledger
            .events_for_run(fixture.owner_lease.run_id)
            .unwrap();
        assert_eq!(
            events[1]
                .private_fingerprint
                .as_ref()
                .unwrap()
                .key_id()
                .as_str(),
            "fingerprint-old"
        );
        assert_eq!(store.publish_count(), 1);
    }

    #[test]
    fn crash_after_cas_reuses_reference_and_corruption_fails_closed() {
        let fixture = fixture(ExecutionMode::Manual);
        let credentials = Arc::new(MemoryCredentialStore::default());
        let keys = key_manager(credentials, "seal-v1", "fingerprint-v1", 1);
        keys.provision_active_keys().unwrap();
        let store = CountingStore::default();
        let append_key = AppendKey::new("crash-after-cas").unwrap();
        let public_payload = canonical(json!({"request": 6}));
        let raw = raw_artifact();
        let redactor = TestRedactor;
        let fingerprint_key = keys.active_fingerprint_key().unwrap();
        let prepared = prepare_artifact_bound(&raw, &raw, &redactor, &fingerprint_key).unwrap();
        let identity = LedgerAppendIdentity::try_from(prepared.identity()).unwrap();
        let lease = match fixture
            .ledger
            .reserve_append(
                fixture.mode,
                &fixture.owner_lease,
                AppendRequest::new(
                    fixture.owner_lease.run_id,
                    append_key.clone(),
                    identity,
                    EffectIntent::None,
                    public_payload.clone(),
                ),
            )
            .unwrap()
        {
            AppendReservation::Reserved(lease) => lease,
            other => panic!("expected lease, got {other:?}"),
        };
        let pending = ArtifactSealer::new(&keys, &store).seal(prepared).unwrap();
        let injected: Result<(), LedgerError> =
            pending.commit_with(|_identity, artifact_reference| {
                let ledger_reference = LedgerSealedArtifactRef::try_from(&artifact_reference)?;
                fixture.ledger.publish_sealed_ref(
                    fixture.mode,
                    &fixture.owner_lease,
                    &lease,
                    ledger_reference,
                )?;
                Err(LedgerError::IntegrityViolation(
                    "injected crash after CAS".to_string(),
                ))
            });
        assert!(injected.is_err());
        assert_eq!(store.publish_count(), 1);
        store.corrupt_only_entry();
        fixture.clock.advance(11);

        let low_space = FixedFreeSpace::new(0);
        let guard = ArtifactFreeSpaceGuard::with_provider(fixture._directory.path(), &low_space);
        let coordinator = ArtifactAppendCoordinator::new_with_free_space_guard(
            &fixture.ledger,
            &keys,
            &store,
            guard,
        );
        let request = ArtifactAppendRequest {
            mode: fixture.mode,
            owner_lease: &fixture.owner_lease,
            append_key,
            effect_intent: EffectIntent::None,
            public_payload,
            raw_artifact: &raw,
            redactor: &redactor,
        };
        assert!(matches!(
            coordinator.append_bound(&request, &raw),
            Err(ArtifactLifecycleError::ArtifactAfterReservation(_))
        ));
        assert_eq!(
            fixture
                .ledger
                .events_for_run(fixture.owner_lease.run_id)
                .unwrap()
                .len(),
            1
        );
        assert_eq!(store.publish_count(), 1);
        assert_eq!(low_space.calls(), 0);
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_keeps_published_uncommitted_reservation_artifact_live() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;
        let fixture = fixture(ExecutionMode::Manual);
        let artifact_root = tempfile::tempdir().unwrap();
        fs::set_permissions(artifact_root.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let store = FilesystemSealedArtifactStore::open(artifact_root.path()).unwrap();
        let credentials = Arc::new(MemoryCredentialStore::default());
        let keys = OsArtifactKeyManager::with_store(
            OsArtifactKeyManagerConfig::new(
                "artifact-test-service",
                "seal-v1",
                "fingerprint-v1",
                1,
                &store,
            )
            .unwrap(),
            credentials,
        );
        keys.provision_active_keys_for_store(&store).unwrap();
        let append_key = AppendKey::new("cleanup-published-reservation").unwrap();
        let public_payload = canonical(json!({"request": "cleanup-race"}));
        let raw = raw_artifact();
        let redactor = TestRedactor;
        let fingerprint_key = keys.active_fingerprint_key().unwrap();
        let prepared = prepare_artifact(&raw, &redactor, &fingerprint_key).unwrap();
        let identity = LedgerAppendIdentity::try_from(prepared.identity()).unwrap();
        let lease = match fixture
            .ledger
            .reserve_append(
                fixture.mode,
                &fixture.owner_lease,
                AppendRequest::new(
                    fixture.owner_lease.run_id,
                    append_key,
                    identity,
                    EffectIntent::None,
                    public_payload,
                ),
            )
            .unwrap()
        {
            AppendReservation::Reserved(lease) => lease,
            other => panic!("expected reservation, got {other:?}"),
        };
        let pending = ArtifactSealer::new(&keys, &store).seal(prepared).unwrap();
        let digest = pending.reference().envelope_digest.clone();
        let artifact_path = artifact_root.path().join(format!("{digest}.sealed"));
        let injected: Result<(), LedgerError> =
            pending.commit_with(|_identity, artifact_reference| {
                let ledger_reference = LedgerSealedArtifactRef::try_from(&artifact_reference)?;
                fixture.ledger.publish_sealed_ref(
                    fixture.mode,
                    &fixture.owner_lease,
                    &lease,
                    ledger_reference,
                )?;
                Err(LedgerError::IntegrityViolation(
                    "injected crash after published reference".to_string(),
                ))
            });
        assert!(injected.is_err());
        assert!(artifact_path.exists());
        assert!(fixture
            .ledger
            .referenced_sealed_envelope_digests()
            .unwrap()
            .iter()
            .any(|referenced| referenced.as_str() == digest));

        let cutoff = SystemTime::now() + Duration::from_secs(60);
        let maintenance = store
            .cleanup_stale_from_ledger(cutoff, cutoff, &fixture.ledger)
            .unwrap();
        assert_eq!(maintenance.stale_unreferenced_files_removed, 0);
        assert!(artifact_path.exists());
    }

    #[test]
    fn degradation_fence_and_reservation_race_has_one_terminal_event() {
        let fixture = fixture(ExecutionMode::Manual);
        let credentials = Arc::new(MemoryCredentialStore::default());
        let good_keys = Arc::new(key_manager(
            credentials.clone(),
            "seal-v1",
            "fingerprint-v1",
            1,
        ));
        good_keys.provision_active_keys().unwrap();
        let missing_sealing_keys = Arc::new(key_manager(
            credentials,
            "seal-missing",
            "fingerprint-v1",
            1,
        ));
        let store = Arc::new(CountingStore::default());
        let barrier = Arc::new(Barrier::new(2));
        let append_key = AppendKey::new("degrade-reserve-race").unwrap();
        let public_payload = canonical(json!({"request": 7}));
        let ledger_a = fixture.ledger.clone();
        let ledger_b = fixture.ledger.clone();
        let owner_a = fixture.owner_lease.clone();
        let owner_b = fixture.owner_lease.clone();
        let store_a = store.clone();
        let store_b = store.clone();
        let barrier_a = barrier.clone();
        let barrier_b = barrier;
        let key_a = append_key.clone();
        let key_b = append_key.clone();
        let payload_a = public_payload.clone();
        let payload_b = public_payload;
        let mode = fixture.mode;

        let degrading = std::thread::spawn(move || {
            let raw = raw_artifact();
            let redactor = TestRedactor;
            let coordinator =
                ArtifactAppendCoordinator::new(&ledger_a, &missing_sealing_keys, &*store_a);
            let request = ArtifactAppendRequest {
                mode,
                owner_lease: &owner_a,
                append_key: key_a,
                effect_intent: EffectIntent::None,
                public_payload: payload_a,
                raw_artifact: &raw,
                redactor: &redactor,
            };
            barrier_a.wait();
            coordinator.append_bound(&request, &raw)
        });
        let appending = std::thread::spawn(move || {
            let raw = raw_artifact();
            let redactor = TestRedactor;
            let coordinator = ArtifactAppendCoordinator::new(&ledger_b, &good_keys, &*store_b);
            let request = ArtifactAppendRequest {
                mode,
                owner_lease: &owner_b,
                append_key: key_b,
                effect_intent: EffectIntent::None,
                public_payload: payload_b,
                raw_artifact: &raw,
                redactor: &redactor,
            };
            barrier_b.wait();
            coordinator.append_bound(&request, &raw)
        });

        let degrading_result = degrading.join().unwrap().unwrap();
        let appending_result = appending.join().unwrap().unwrap();
        assert!(matches!(
            degrading_result,
            ArtifactAppendOutcome::ManualDegraded { .. }
                | ArtifactAppendOutcome::InFlight { .. }
                | ArtifactAppendOutcome::Committed(_)
        ));
        assert!(matches!(
            appending_result,
            ArtifactAppendOutcome::ManualDegraded { .. }
                | ArtifactAppendOutcome::InFlight { .. }
                | ArtifactAppendOutcome::Committed(_)
        ));
        let events = fixture
            .ledger
            .events_for_run(fixture.owner_lease.run_id)
            .unwrap();
        let terminal = events
            .iter()
            .filter(|event| {
                matches!(
                    event.kind,
                    SessionRunEventKind::Append | SessionRunEventKind::ManualAppendDegradedAudit
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(terminal.len(), 1);
        match terminal[0].kind {
            SessionRunEventKind::Append => {
                assert_eq!(terminal[0].append_key.as_ref(), Some(&append_key));
            }
            SessionRunEventKind::ManualAppendDegradedAudit => {
                let audit_key = terminal[0]
                    .append_key
                    .as_ref()
                    .expect("degradation audit key");
                assert_ne!(audit_key, &append_key);
                assert!(audit_key.as_str().starts_with("manual-degradation-"));
                let payload = terminal[0].public_payload.as_value().unwrap();
                assert_eq!(
                    payload["public_context"]["source_append_key"],
                    append_key.as_str()
                );
            }
            other => panic!("unexpected terminal event: {other:?}"),
        }
        assert!(store.publish_count() <= 1);
    }
}
