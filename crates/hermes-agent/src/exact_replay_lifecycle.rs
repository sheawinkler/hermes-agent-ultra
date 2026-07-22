//! Authoritative exact-replay reads and retention over the private CAS.
//!
//! The coordinator is deliberately the only place where a concrete filesystem
//! operation guard is held across a ledger call.  Its lock order is always
//! **CAS store guard -> ledger transaction**; the ledger never calls back into
//! the store, and ordinary store methods must not be used while one of these
//! guards is live.

use std::io;

use serde_json::Value;
use zeroize::Zeroizing;

use crate::run_ledger::{
    AppendIdentity, ArtifactPhysicalState, ArtifactReferenceState, ArtifactStateReason,
    ArtifactStateTransition, CreatedRun, EventId, ExactReplayState, LedgerError,
    ReplayArtifactFidelity, RunId, RunLedger, RunOwnerId, RunStart, SealedArtifactRef,
};
use crate::sealed_artifact::{
    prepare_artifact_bound, prepare_exact_artifact_bound, ArtifactRedactor, ArtifactSealer,
    ExactArtifactAuthorization, FilesystemSealedArtifactStore, OsArtifactKeyManager,
    SealedArtifactError, SealedArtifactRef as StoreSealedArtifactRef,
};
use hermes_core::execution_mode::ResolvedExecutionMode;

/// Typed terminal result for an exact private-artifact read.
#[derive(Debug, thiserror::Error)]
pub enum ExactReplayLifecycleError {
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error("exact artifact access is unauthorized")]
    Unauthorized,
    #[error("exact artifact is unavailable: {reason:?}")]
    Unavailable { reason: ArtifactStateReason },
    #[error("exact artifact has been retired (state {state:?})")]
    Retired { state: ExactReplayState },
    #[error(transparent)]
    Artifact(#[from] SealedArtifactError),
}

pub type ExactReplayLifecycleResult<T> = Result<T, ExactReplayLifecycleError>;

/// Result of retiring one event-local reference.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactReplayRetirement {
    /// The durable logical transition into `retiring`, when this invocation
    /// performed it. A retry that starts from `retiring`/`retired` has none.
    pub began: Option<ArtifactStateTransition>,
    /// The durable logical transition into `retired` (or its original
    /// idempotent transition on retry).
    pub finished: ArtifactStateTransition,
    /// The physical state observed after any required unlink observation.
    pub physical_state: ArtifactPhysicalState,
    /// True when this call found a durable tombstone and issued the locked
    /// unlink/observe sequence. It can be true on an idempotent crash retry.
    pub tombstone_reclaimed: bool,
}

/// Coordinates authoritative RunStart sealing, exact reads, and retirement.
///
/// This facade deliberately accepts a concrete filesystem CAS instead of a
/// trait object: exact read and retention need its cross-process guard APIs.
pub struct ExactReplayLifecycleCoordinator<'a> {
    ledger: &'a RunLedger,
    keys: &'a OsArtifactKeyManager,
    store: &'a FilesystemSealedArtifactStore,
}

impl std::fmt::Debug for ExactReplayLifecycleCoordinator<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ExactReplayLifecycleCoordinator")
            .field("keys", self.keys)
            .field("store", self.store)
            .finish_non_exhaustive()
    }
}

impl<'a> ExactReplayLifecycleCoordinator<'a> {
    pub fn new(
        ledger: &'a RunLedger,
        keys: &'a OsArtifactKeyManager,
        store: &'a FilesystemSealedArtifactStore,
    ) -> Self {
        Self {
            ledger,
            keys,
            store,
        }
    }

    /// Seal one credential-redacted, bound RunStart artifact and retain its CAS
    /// publication guard through the authoritative RunStart transaction.
    ///
    /// `private_evidence` is the immutable prepared evidence from the same
    /// runtime input that will execute; callers must not synthesize it later.
    /// The sealed payload remains the redactor's credential-free projection,
    /// never raw credentials or plaintext digest material.
    pub(crate) fn create_run_and_start_bound_artifact(
        &self,
        mode: ResolvedExecutionMode,
        owner_id: RunOwnerId,
        start: RunStart,
        raw_artifact: &Value,
        private_evidence: &Value,
        redactor: &dyn ArtifactRedactor,
    ) -> ExactReplayLifecycleResult<CreatedRun> {
        let fingerprint_key = self.keys.active_fingerprint_key()?;
        let (prepared, fidelity) = match prepare_exact_artifact_bound(
            raw_artifact,
            private_evidence,
            redactor,
            &fingerprint_key,
        ) {
            Ok(prepared) => (prepared, ReplayArtifactFidelity::ExactPrivate),
            Err(_) => (
                prepare_artifact_bound(
                    raw_artifact,
                    private_evidence,
                    redactor,
                    &fingerprint_key,
                )?,
                ReplayArtifactFidelity::MetadataOnly,
            ),
        };
        let identity = AppendIdentity::try_from(prepared.identity())?;
        let sealer = ArtifactSealer::new(self.keys, self.store);
        let pending = sealer.seal(prepared)?;
        pending
            .commit_with(|sealed_identity, store_reference| {
                let sealed_identity = AppendIdentity::try_from(&sealed_identity)?;
                if sealed_identity != identity {
                    return Err(LedgerError::SealedArtifactIdentity(
                        "sealed run-start identity changed after preparation".to_string(),
                    ));
                }
                let ledger_reference = SealedArtifactRef::try_from(&store_reference)?;
                self.ledger.create_run_and_start_with_bound_artifact(
                    mode,
                    owner_id,
                    start,
                    sealed_identity,
                    ledger_reference,
                    fidelity,
                )
            })
            .map_err(ExactReplayLifecycleError::Ledger)
    }

    /// Authorize first, then hold the shared CAS guard across the ledger query
    /// and verified decrypt.  Unauthorized requests mutate neither the CAS nor
    /// the ledger.
    pub fn read_exact(
        &self,
        run_id: RunId,
        event_id: EventId,
        artifact: &SealedArtifactRef,
        authorization: &dyn ExactArtifactAuthorization,
    ) -> ExactReplayLifecycleResult<Zeroizing<Vec<u8>>> {
        let store_reference = StoreSealedArtifactRef::try_from(artifact)?;
        match authorization.authorize_exact_read(&store_reference) {
            Ok(()) => {}
            Err(SealedArtifactError::Unauthorized(_)) => {
                return Err(ExactReplayLifecycleError::Unauthorized);
            }
            Err(error) => return Err(error.into()),
        }

        let verified: ExactReplayLifecycleResult<Zeroizing<Vec<u8>>> = (|| {
            // CAS guard first, then every ledger operation in this scope.
            let guard = self.store.acquire_shared_operation_guard()?;
            let status = self
                .ledger
                .artifact_reference_status(run_id, event_id, artifact)?;
            ensure_readable_status(&status)?;
            let envelope = guard.locked_read(&store_reference.envelope_digest)?;
            let sealer = ArtifactSealer::new(self.keys, self.store);
            let plaintext = sealer.unseal_verified_envelope_bytes(&store_reference, &envelope)?;
            // Historical v3 imports start as unverified. Promotion is safe
            // under this same shared guard and cannot reverse a terminal
            // lifecycle state.
            if matches!(status.reference_state, ArtifactReferenceState::Unverified) {
                self.ledger.mark_sealed_artifact_verified(artifact)?;
            }
            Ok(plaintext)
        })();

        match verified {
            Ok(plaintext) => Ok(plaintext),
            Err(ExactReplayLifecycleError::Artifact(error)) => {
                let reason =
                    self.record_verified_read_failure(run_id, event_id, artifact, error)?;
                Err(ExactReplayLifecycleError::Unavailable { reason })
            }
            Err(error) => Err(error),
        }
    }

    /// Retire one logical reference. The exclusive CAS guard covers every
    /// ledger transition and the final unlink so a concurrent exact read or
    /// publication cannot observe a split lifecycle.
    pub fn retire_exact_reference(
        &self,
        run_id: RunId,
        event_id: EventId,
        artifact: &SealedArtifactRef,
    ) -> ExactReplayLifecycleResult<ExactReplayRetirement> {
        // CAS guard first, then every ledger operation in this scope.
        let guard = self.store.acquire_exclusive_operation_guard()?;
        let initial = self
            .ledger
            .artifact_reference_status(run_id, event_id, artifact)?;
        let began = match initial.reference_state {
            ArtifactReferenceState::Retired | ArtifactReferenceState::Retiring => None,
            _ => Some(
                self.ledger
                    .begin_exact_artifact_retirement(run_id, event_id, artifact)?,
            ),
        };
        let finished = self
            .ledger
            .finish_exact_artifact_retirement(run_id, event_id, artifact)?;

        let tombstoned = self
            .ledger
            .tombstoned_exact_artifacts()?
            .into_iter()
            .any(|candidate| candidate == *artifact);
        let physical_state = if tombstoned {
            // `locked_remove` fsyncs the directory. The tombstone was committed
            // before this unlink; a crash can therefore leave only safe residue.
            guard.locked_remove(artifact.envelope_digest().as_str())?;
            self.ledger.observe_exact_artifact_unlinked(artifact)?
        } else {
            self.ledger
                .artifact_reference_status(run_id, event_id, artifact)?
                .physical_state
        };
        Ok(ExactReplayRetirement {
            began,
            finished,
            physical_state,
            tombstone_reclaimed: tombstoned,
        })
    }

    fn record_verified_read_failure(
        &self,
        run_id: RunId,
        event_id: EventId,
        artifact: &SealedArtifactRef,
        error: SealedArtifactError,
    ) -> ExactReplayLifecycleResult<ArtifactStateReason> {
        let Some(reason) = read_failure_reason(&error) else {
            return Err(ExactReplayLifecycleError::Artifact(error));
        };
        // The shared guard has been released before acquiring exclusive. This
        // serializes the durable failure fan-out against retirement/unlink.
        let _guard = self.store.acquire_exclusive_operation_guard()?;
        let status = self
            .ledger
            .artifact_reference_status(run_id, event_id, artifact)?;
        ensure_readable_status(&status)?;
        self.ledger
            .mark_sealed_artifact_unavailable(artifact, reason)?;
        Ok(reason)
    }
}

fn ensure_readable_status(
    status: &crate::run_ledger::ArtifactReferenceStatus,
) -> ExactReplayLifecycleResult<()> {
    if !status.exact_replay.exact_replay {
        return Err(ExactReplayLifecycleError::Unavailable {
            reason: status
                .exact_replay
                .reason
                .unwrap_or(ArtifactStateReason::Sanitized),
        });
    }
    if matches!(
        status.reference_state,
        ArtifactReferenceState::Retiring | ArtifactReferenceState::Retired
    ) || matches!(
        status.physical_state,
        ArtifactPhysicalState::Retiring | ArtifactPhysicalState::Retired
    ) {
        return Err(ExactReplayLifecycleError::Retired {
            state: status.exact_replay.state,
        });
    }
    if let Some(reason) = status.reference_reason.or(status.physical_reason) {
        return Err(ExactReplayLifecycleError::Unavailable { reason });
    }
    if !matches!(
        status.reference_state,
        ArtifactReferenceState::Unverified | ArtifactReferenceState::Available
    ) {
        return Err(ExactReplayLifecycleError::Unavailable {
            reason: ArtifactStateReason::Corrupt,
        });
    }
    Ok(())
}

fn read_failure_reason(error: &SealedArtifactError) -> Option<ArtifactStateReason> {
    match error {
        SealedArtifactError::Io(error) if error.kind() == io::ErrorKind::NotFound => {
            Some(ArtifactStateReason::Missing)
        }
        SealedArtifactError::KeyUnavailable(_) => Some(ArtifactStateReason::KeyRevoked),
        SealedArtifactError::CasConflict(_)
        | SealedArtifactError::InvalidEnvelope(_)
        | SealedArtifactError::Cryptography(_)
        | SealedArtifactError::Canonicalization(_)
        | SealedArtifactError::TooLarge { .. } => Some(ArtifactStateReason::Corrupt),
        SealedArtifactError::Io(_)
        | SealedArtifactError::Redaction(_)
        | SealedArtifactError::InvalidMetadata(_)
        | SealedArtifactError::Unauthorized(_)
        | SealedArtifactError::ReferenceSnapshot(_)
        | SealedArtifactError::InjectedFailure(_)
        // Capacity admission runs before a write reservation, never during an
        // exact read; it cannot honestly diagnose stored artifact state.
        | SealedArtifactError::InsufficientFreeSpace { .. } => None,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::io;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{mpsc, Arc, Mutex};
    use std::thread;
    use std::time::Duration;

    use hermes_core::execution_mode::{ExecutionMode, ExecutionModeSource};
    use serde_json::json;

    use super::*;
    use crate::run_ledger::{
        ActionId, BranchId, CanonicalJson, DigestAbsence, LedgerConnectionFactory, ModelId,
        ObjectiveId, ProvenanceDigest, ProviderId, RunProvenance, SessionId, SessionRunEventKind,
        Sha256Digest, TaskId, TenantId,
    };
    use crate::sealed_artifact::{
        ArtifactCredentialProvisionGuard, ArtifactCredentialStore, OsArtifactKeyManagerConfig,
        RedactedArtifact, SealedArtifactStore,
    };

    struct ProvisionGuard;
    impl ArtifactCredentialProvisionGuard for ProvisionGuard {}

    #[derive(Default)]
    struct MemoryCredentials {
        unavailable: AtomicBool,
        secrets: Mutex<HashMap<(String, String), Vec<u8>>>,
    }

    impl MemoryCredentials {
        fn set_unavailable(&self, unavailable: bool) {
            self.unavailable.store(unavailable, Ordering::SeqCst);
        }
    }

    impl ArtifactCredentialStore for MemoryCredentials {
        fn load_secret(
            &self,
            service: &str,
            account: &str,
        ) -> Result<Option<Zeroizing<Vec<u8>>>, SealedArtifactError> {
            if self.unavailable.load(Ordering::SeqCst) {
                return Err(SealedArtifactError::KeyUnavailable(
                    "test key revoked".to_string(),
                ));
            }
            Ok(self
                .secrets
                .lock()
                .expect("credentials mutex")
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
                .expect("credentials mutex")
                .entry((service.to_string(), account.to_string()))
                .or_insert_with(|| secret.to_vec());
            Ok(())
        }

        fn acquire_provision_lock(
            &self,
            _service: &str,
            _store_namespace: &str,
        ) -> Result<Box<dyn ArtifactCredentialProvisionGuard>, SealedArtifactError> {
            Ok(Box::new(ProvisionGuard))
        }
    }

    struct TestRedactor;

    impl ArtifactRedactor for TestRedactor {
        fn version(&self) -> &str {
            "exact-replay-test-v1"
        }

        fn redact(&self, raw: &Value) -> Result<RedactedArtifact, SealedArtifactError> {
            Ok(RedactedArtifact {
                public_identity: raw.get("identity").cloned().ok_or_else(|| {
                    SealedArtifactError::Redaction("identity missing".to_string())
                })?,
                credential_free_private: raw
                    .get("private")
                    .cloned()
                    .ok_or_else(|| SealedArtifactError::Redaction("private missing".to_string()))?,
            })
        }
    }

    struct Allow;

    impl ExactArtifactAuthorization for Allow {
        fn authorize_exact_read(
            &self,
            _artifact: &StoreSealedArtifactRef,
        ) -> Result<(), SealedArtifactError> {
            Ok(())
        }
    }

    struct Deny;

    impl ExactArtifactAuthorization for Deny {
        fn authorize_exact_read(
            &self,
            _artifact: &StoreSealedArtifactRef,
        ) -> Result<(), SealedArtifactError> {
            Err(SealedArtifactError::Unauthorized("test deny".to_string()))
        }
    }

    struct Fixture {
        _directory: tempfile::TempDir,
        factory: LedgerConnectionFactory,
        ledger: RunLedger,
        store: FilesystemSealedArtifactStore,
        keys: OsArtifactKeyManager,
        credentials: Arc<MemoryCredentials>,
        mode: ResolvedExecutionMode,
    }

    fn fixture() -> Fixture {
        let directory = tempfile::tempdir().expect("temporary directory");
        let factory = LedgerConnectionFactory::new(directory.path().join("ledger.sqlite"));
        factory
            .migrate_and_cut_over_v0_to_v1()
            .expect("ledger migration");
        let ledger = RunLedger::open(factory.clone()).expect("ledger open");
        std::fs::create_dir(directory.path().join("artifacts")).expect("artifact store directory");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(
                directory.path().join("artifacts"),
                std::fs::Permissions::from_mode(0o700),
            )
            .expect("artifact store permissions");
        }
        let store =
            FilesystemSealedArtifactStore::open(directory.path().join("artifacts")).expect("store");
        let credentials = Arc::new(MemoryCredentials::default());
        let keys = OsArtifactKeyManager::with_store(
            OsArtifactKeyManagerConfig::new(
                "exact-replay-test",
                "sealing-v1",
                "fingerprint-v1",
                1,
                &store,
            )
            .expect("key config"),
            credentials.clone(),
        );
        keys.provision_active_keys_for_store(&store)
            .expect("key provisioning");
        Fixture {
            _directory: directory,
            factory,
            ledger,
            store,
            keys,
            credentials,
            mode: ResolvedExecutionMode {
                mode: ExecutionMode::Manual,
                source: ExecutionModeSource::BuiltInDefault,
            },
        }
    }

    fn digest(byte: u8) -> Sha256Digest {
        Sha256Digest::new(hex::encode([byte; 32])).expect("digest")
    }

    fn provenance() -> RunProvenance {
        RunProvenance::new(
            SessionId::new("exact-replay-session").expect("session"),
            None,
            BranchId::new("main").expect("branch"),
            TenantId::new("tenant").expect("tenant"),
            ActionId::new("exact-replay").expect("action"),
            ObjectiveId::new("objective").expect("objective"),
            TaskId::new("task").expect("task"),
            ModelId::new("model").expect("model"),
            ProviderId::new("provider").expect("provider"),
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

    fn private_evidence() -> Value {
        json!({
            "schema": "hermes.agent-run-start-private/v1",
            "system_prompt": "You are the exact prepared system prompt.",
            "system_prompt_source": "fresh",
            "messages": [{"role": "user", "content": "hello"}],
            "injected_skills_block": "skill instructions",
            "advertised_tools": [],
            "tool_contract": [],
            "config_projection": {
                "api_mode": null,
                "background_review_enabled": null,
                "background_review_metrics_enabled": null,
                "budget": null,
                "checkpoint_interval_turns": null,
                "code_index_enabled": null,
                "code_index_max_files": null,
                "code_index_max_symbols": null,
                "execution_mode": null,
                "lsp_context_enabled": null,
                "lsp_context_max_chars": null,
                "max_concurrent_delegates": null,
                "max_cost_usd": null,
                "max_delegate_depth": null,
                "max_tokens": null,
                "max_turns": null,
                "memory_flush_interval": null,
                "memory_nudge_interval": null,
                "preflight_context_compress": null,
                "quiet_mode": null,
                "rollback_on_tool_error_threshold": null,
                "skill_creation_nudge_interval": null,
                "skip_context_files": null,
                "skip_memory": null,
                "stream": null
            },
            "execution_containment": {
                "schema": "hermes.execution-containment-provenance/v1",
                "admission": "manual_host",
                "backend_kind": null,
                "scope": null,
                "policy_version": null,
                "configuration_digest": null
            }
        })
    }

    fn raw_artifact(private: &Value) -> Value {
        json!({
            "identity": {
                "artifact_kind": "request",
                "evidence_id": "run-start",
                "evidence_subject": "run-provenance",
            },
            "private": private,
        })
    }

    fn run_start() -> RunStart {
        RunStart::new(
            RunId::new(),
            provenance(),
            CanonicalJson::from_value(&json!({"schema": "test-run-start"}))
                .expect("canonical start"),
        )
    }

    fn create_bound(fixture: &Fixture) -> (CreatedRun, EventId, SealedArtifactRef, Value) {
        let private = private_evidence();
        let raw = raw_artifact(&private);
        let coordinator =
            ExactReplayLifecycleCoordinator::new(&fixture.ledger, &fixture.keys, &fixture.store);
        let created = coordinator
            .create_run_and_start_bound_artifact(
                fixture.mode,
                RunOwnerId::new("exact-replay-owner").expect("owner"),
                run_start(),
                &raw,
                &private,
                &TestRedactor,
            )
            .expect("bound run start");
        let event_id = created.header.start_event_id;
        let artifact = fixture
            .ledger
            .events_for_run(created.header.run_id)
            .expect("events")
            .into_iter()
            .find(|event| event.event_id == event_id)
            .and_then(|event| event.sealed_artifact_ref)
            .expect("run-start sealed ref");
        (created, event_id, artifact, private)
    }

    #[test]
    fn direct_exact_prepare_rejects_credentials_in_all_free_text_evidence() {
        let fixture = fixture();
        for location in ["system_prompt", "injected_skills_block", "messages"] {
            let mut private = private_evidence();
            match location {
                "system_prompt" => {
                    private["system_prompt"] = json!("api_key=sk-live-secret-token");
                }
                "injected_skills_block" => {
                    private["injected_skills_block"] = json!("Bearer sk-live-secret-token");
                }
                "messages" => {
                    private["messages"][0]["content"] = json!("token=sk-live-secret-token");
                }
                _ => unreachable!("fixed free-text fixture"),
            }
            let raw = raw_artifact(&private);
            assert!(
                prepare_exact_artifact_bound(
                    &raw,
                    &private,
                    &TestRedactor,
                    &fixture
                        .keys
                        .active_fingerprint_key()
                        .expect("fingerprint key"),
                )
                .is_err(),
                "{location} credential must fail before CAS publication"
            );
        }
    }

    #[test]
    fn exact_tool_schema_preserves_property_names_but_rejects_secret_defaults() {
        let fixture = fixture();
        let mut private = private_evidence();
        let schema = json!({
            "name": "tool_probe",
            "description": "Probe the input.",
            "parameters": {
                "type": "object",
                "properties": {
                    "api_key": {"type": "string"}
                }
            }
        });
        private["advertised_tools"] = json!([schema.clone()]);
        private["tool_contract"] = json!([{
            "schema": schema,
            "execution_trust": "registered"
        }]);
        let raw = raw_artifact(&private);
        let prepared = prepare_exact_artifact_bound(
            &raw,
            &private,
            &TestRedactor,
            &fixture
                .keys
                .active_fingerprint_key()
                .expect("fingerprint key"),
        )
        .expect("property names are schema metadata, not credentials");
        let sealer = ArtifactSealer::new(&fixture.keys, &fixture.store);
        let pending = sealer.seal(prepared).expect("seal schema metadata");
        let reference = pending.reference().clone();
        pending
            .commit_with(|_, _| Ok::<_, SealedArtifactError>(()))
            .expect("commit publication");
        let recovered = sealer
            .unseal_exact(&reference, &Allow)
            .expect("unseal schema metadata");
        let recovered: Value = serde_json::from_slice(&recovered).expect("exact JSON");
        assert_eq!(
            recovered["advertised_tools"][0]["parameters"]["properties"]["api_key"]["type"],
            "string"
        );

        let mut with_secret_default = private;
        with_secret_default["advertised_tools"][0]["parameters"]["properties"]["api_key"]
            ["default"] = json!("sk-live-secret-token");
        let raw = raw_artifact(&with_secret_default);
        assert!(
            prepare_exact_artifact_bound(
                &raw,
                &with_secret_default,
                &TestRedactor,
                &fixture
                    .keys
                    .active_fingerprint_key()
                    .expect("fingerprint key"),
            )
            .is_err(),
            "credential-bearing schema defaults must fail before CAS publication"
        );
    }

    #[test]
    fn unavailable_prompt_source_keeps_sanitized_blocked_input_messages_exact() {
        let fixture = fixture();
        let mut private = private_evidence();
        private["system_prompt_source"] = json!("unavailable");
        private["system_prompt"] = Value::Null;
        private["injected_skills_block"] = Value::Null;
        let raw = raw_artifact(&private);
        let prepared = prepare_exact_artifact_bound(
            &raw,
            &private,
            &TestRedactor,
            &fixture
                .keys
                .active_fingerprint_key()
                .expect("fingerprint key"),
        )
        .expect("blocked input messages remain exact sealed evidence");
        let pending = ArtifactSealer::new(&fixture.keys, &fixture.store)
            .seal(prepared)
            .expect("seal blocked evidence");
        let reference = pending.reference().clone();
        pending
            .commit_with(|_, _| Ok::<_, SealedArtifactError>(()))
            .expect("commit blocked evidence");
        let recovered = ArtifactSealer::new(&fixture.keys, &fixture.store)
            .unseal_exact(&reference, &Allow)
            .expect("unseal blocked evidence");
        let recovered: Value = serde_json::from_slice(&recovered).expect("exact JSON");
        assert_eq!(recovered["system_prompt_source"], "unavailable");
        assert_eq!(recovered["messages"][0]["content"], "hello");
    }

    #[test]
    fn authorized_read_returns_sealed_exact_prepared_evidence() {
        let fixture = fixture();
        let (created, event_id, artifact, _) = create_bound(&fixture);
        let coordinator =
            ExactReplayLifecycleCoordinator::new(&fixture.ledger, &fixture.keys, &fixture.store);

        let plaintext = coordinator
            .read_exact(created.header.run_id, event_id, &artifact, &Allow)
            .expect("authorized exact read");
        let recovered: Value = serde_json::from_slice(&plaintext).expect("exact JSON");
        assert_eq!(
            recovered["system_prompt"],
            "You are the exact prepared system prompt."
        );
        assert_eq!(recovered["injected_skills_block"], "skill instructions");
        assert_eq!(recovered["messages"][0]["content"], "hello");
        assert!(
            fixture
                .ledger
                .artifact_reference_status(created.header.run_id, event_id, &artifact)
                .expect("status")
                .exact_replay
                .exact_replay
        );
    }

    #[test]
    fn unauthorized_read_mutates_no_lifecycle_state() {
        let fixture = fixture();
        let (created, event_id, artifact, _) = create_bound(&fixture);
        let before = fixture
            .ledger
            .artifact_reference_status(created.header.run_id, event_id, &artifact)
            .expect("before");
        let coordinator =
            ExactReplayLifecycleCoordinator::new(&fixture.ledger, &fixture.keys, &fixture.store);

        assert!(matches!(
            coordinator.read_exact(created.header.run_id, event_id, &artifact, &Deny),
            Err(ExactReplayLifecycleError::Unauthorized)
        ));
        assert_eq!(
            fixture
                .ledger
                .artifact_reference_status(created.header.run_id, event_id, &artifact)
                .expect("after"),
            before
        );
    }

    #[test]
    fn missing_corrupt_and_key_revoked_reads_latch_typed_unavailability_durably() {
        let fixture = fixture();
        let (created, event_id, artifact, _) = create_bound(&fixture);
        fixture
            .store
            .remove(artifact.envelope_digest().as_str())
            .expect("remove exact object");
        let coordinator =
            ExactReplayLifecycleCoordinator::new(&fixture.ledger, &fixture.keys, &fixture.store);
        assert!(matches!(
            coordinator.read_exact(created.header.run_id, event_id, &artifact, &Allow),
            Err(ExactReplayLifecycleError::Unavailable {
                reason: ArtifactStateReason::Missing
            })
        ));
        let reopened = RunLedger::open(fixture.factory.clone()).expect("restart ledger");
        assert_eq!(
            reopened
                .artifact_reference_status(created.header.run_id, event_id, &artifact)
                .expect("durable status")
                .reference_state,
            ArtifactReferenceState::UnavailableMissing
        );
        assert!(
            !reopened
                .artifact_reference_status(created.header.run_id, event_id, &artifact)
                .expect("durable missing replay latch")
                .exact_replay
                .exact_replay
        );

        let corrupt_fixture = self::fixture();
        let (created, event_id, artifact, _) = create_bound(&corrupt_fixture);
        std::fs::write(
            corrupt_fixture
                .store
                .root()
                .join(format!("{}.sealed", artifact.envelope_digest().as_str())),
            b"corrupt sealed envelope",
        )
        .expect("corrupt exact object");
        let coordinator = ExactReplayLifecycleCoordinator::new(
            &corrupt_fixture.ledger,
            &corrupt_fixture.keys,
            &corrupt_fixture.store,
        );
        assert!(matches!(
            coordinator.read_exact(created.header.run_id, event_id, &artifact, &Allow),
            Err(ExactReplayLifecycleError::Unavailable {
                reason: ArtifactStateReason::Corrupt
            })
        ));
        let reopened = RunLedger::open(corrupt_fixture.factory.clone()).expect("restart ledger");
        assert_eq!(
            reopened
                .artifact_reference_status(created.header.run_id, event_id, &artifact)
                .expect("durable corrupt status")
                .reference_state,
            ArtifactReferenceState::UnavailableCorrupt
        );
        assert!(
            !reopened
                .artifact_reference_status(created.header.run_id, event_id, &artifact)
                .expect("durable corrupt replay latch")
                .exact_replay
                .exact_replay
        );
        let key_revoked_fixture = self::fixture();
        let (created, event_id, artifact, _) = create_bound(&key_revoked_fixture);
        key_revoked_fixture.credentials.set_unavailable(true);
        let coordinator = ExactReplayLifecycleCoordinator::new(
            &key_revoked_fixture.ledger,
            &key_revoked_fixture.keys,
            &key_revoked_fixture.store,
        );
        assert!(matches!(
            coordinator.read_exact(created.header.run_id, event_id, &artifact, &Allow),
            Err(ExactReplayLifecycleError::Unavailable {
                reason: ArtifactStateReason::KeyRevoked
            })
        ));
        assert_eq!(
            key_revoked_fixture
                .ledger
                .artifact_reference_status(created.header.run_id, event_id, &artifact)
                .expect("key-revoked status")
                .reference_state,
            ArtifactReferenceState::UnavailableKeyRevoked
        );
        let reopened = RunLedger::open(key_revoked_fixture.factory.clone())
            .expect("restart key-revoked ledger");
        assert_eq!(
            reopened
                .artifact_reference_status(created.header.run_id, event_id, &artifact)
                .expect("durable key-revoked status")
                .reference_state,
            ArtifactReferenceState::UnavailableKeyRevoked
        );
        assert!(
            !reopened
                .artifact_reference_status(created.header.run_id, event_id, &artifact)
                .expect("durable key-revoked replay latch")
                .exact_replay
                .exact_replay
        );
    }

    #[test]
    fn one_shared_reference_retires_without_deleting_the_other_then_last_unlinks() {
        let fixture = fixture();
        let private = private_evidence();
        let raw = raw_artifact(&private);
        let prepared = prepare_exact_artifact_bound(
            &raw,
            &private,
            &TestRedactor,
            &fixture
                .keys
                .active_fingerprint_key()
                .expect("fingerprint key"),
        )
        .expect("prepared exact artifact");
        let identity = AppendIdentity::try_from(prepared.identity()).expect("ledger identity");
        let sealer = ArtifactSealer::new(&fixture.keys, &fixture.store);
        let pending = sealer.seal(prepared).expect("sealed artifact");
        let (first, second, artifact) = pending
            .commit_with(|sealed_identity, store_ref| {
                let sealed_identity =
                    AppendIdentity::try_from(&sealed_identity).expect("sealed identity");
                assert_eq!(sealed_identity, identity);
                let artifact = SealedArtifactRef::try_from(&store_ref).expect("ledger ref");
                let first = fixture.ledger.create_run_and_start_with_sealed_artifact(
                    fixture.mode,
                    RunOwnerId::new("shared-owner-one").expect("owner"),
                    run_start(),
                    sealed_identity.clone(),
                    artifact.clone(),
                )?;
                let second = fixture.ledger.create_run_and_start_with_sealed_artifact(
                    fixture.mode,
                    RunOwnerId::new("shared-owner-two").expect("owner"),
                    run_start(),
                    sealed_identity,
                    artifact.clone(),
                )?;
                Ok::<_, LedgerError>((first, second, artifact))
            })
            .expect("two linked starts");
        let coordinator =
            ExactReplayLifecycleCoordinator::new(&fixture.ledger, &fixture.keys, &fixture.store);

        let first_retirement = coordinator
            .retire_exact_reference(first.header.run_id, first.header.start_event_id, &artifact)
            .expect("first retirement");
        assert!(!first_retirement.tombstone_reclaimed);
        let first_status = fixture
            .ledger
            .artifact_reference_status(first.header.run_id, first.header.start_event_id, &artifact)
            .expect("retired first-reference status");
        let second_status = fixture
            .ledger
            .artifact_reference_status(
                second.header.run_id,
                second.header.start_event_id,
                &artifact,
            )
            .expect("live second-reference status");
        assert!(!first_status.exact_replay.exact_replay);
        assert!(second_status.exact_replay.exact_replay);
        let first_events = fixture
            .ledger
            .events_for_run(first.header.run_id)
            .expect("first run events");
        let audit = first_events.last().expect("retirement audit");
        assert_eq!(audit.kind, SessionRunEventKind::ArtifactStateAudit);
        assert!(!audit
            .public_payload
            .as_str()
            .contains("You are the exact prepared system prompt."));
        assert!(fixture
            .ledger
            .outbox_for_run(first.header.run_id)
            .expect("retirement outbox")
            .is_empty());
        fixture
            .ledger
            .verify_run_chain(first.header.run_id)
            .expect("audit event is chain-bound");
        coordinator
            .read_exact(
                second.header.run_id,
                second.header.start_event_id,
                &artifact,
                &Allow,
            )
            .expect("other shared ref remains readable");

        let last_retirement = coordinator
            .retire_exact_reference(
                second.header.run_id,
                second.header.start_event_id,
                &artifact,
            )
            .expect("last retirement");
        assert!(last_retirement.tombstone_reclaimed);
        assert_eq!(
            last_retirement.physical_state,
            ArtifactPhysicalState::Retired
        );
        assert!(matches!(
            fixture.store.read(artifact.envelope_digest().as_str()),
            Err(SealedArtifactError::Io(error)) if error.kind() == io::ErrorKind::NotFound
        ));
        assert!(matches!(
            coordinator.read_exact(
                second.header.run_id,
                second.header.start_event_id,
                &artifact,
                &Allow,
            ),
            Err(ExactReplayLifecycleError::Retired { .. })
        ));

        let retry = coordinator
            .retire_exact_reference(
                second.header.run_id,
                second.header.start_event_id,
                &artifact,
            )
            .expect("idempotent retirement");
        assert_eq!(
            retry.finished.audit_event_id,
            last_retirement.finished.audit_event_id
        );
        assert_eq!(retry.physical_state, ArtifactPhysicalState::Retired);
    }

    #[test]
    fn lifecycle_transitions_are_idempotent_and_reversals_fail_closed() {
        let fixture = fixture();
        let (created, event_id, artifact, _) = create_bound(&fixture);

        {
            // Store guard precedes every lifecycle transaction in this scope.
            let _guard = fixture
                .store
                .acquire_exclusive_operation_guard()
                .expect("exclusive guard");
            let began = fixture
                .ledger
                .begin_exact_artifact_retirement(created.header.run_id, event_id, &artifact)
                .expect("begin retirement");
            let retried_begin = fixture
                .ledger
                .begin_exact_artifact_retirement(created.header.run_id, event_id, &artifact)
                .expect("idempotent begin");
            assert_eq!(retried_begin.audit_event_id, began.audit_event_id);

            let finished = fixture
                .ledger
                .finish_exact_artifact_retirement(created.header.run_id, event_id, &artifact)
                .expect("finish retirement");
            let retried_finish = fixture
                .ledger
                .finish_exact_artifact_retirement(created.header.run_id, event_id, &artifact)
                .expect("idempotent finish");
            assert_eq!(retried_finish.audit_event_id, finished.audit_event_id);
            assert!(matches!(
                fixture.ledger.begin_exact_artifact_retirement(
                    created.header.run_id,
                    event_id,
                    &artifact,
                ),
                Err(LedgerError::ArtifactStateTransitionInvalid { .. })
            ));
        }

        let coordinator =
            ExactReplayLifecycleCoordinator::new(&fixture.ledger, &fixture.keys, &fixture.store);
        let observed = coordinator
            .retire_exact_reference(created.header.run_id, event_id, &artifact)
            .expect("reclaim durable tombstone");
        assert!(observed.tombstone_reclaimed);
        assert_eq!(observed.physical_state, ArtifactPhysicalState::Retired);
    }

    #[test]
    fn retire_guard_waits_for_an_inflight_exact_read_guard() {
        let fixture = fixture();
        let competing_store =
            FilesystemSealedArtifactStore::open(fixture.store.root()).expect("second store handle");
        let read_guard = fixture
            .store
            .acquire_shared_operation_guard()
            .expect("shared exact-read guard");
        let (entered_tx, entered_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _retire_guard = competing_store
                .acquire_exclusive_operation_guard()
                .expect("exclusive retirement guard");
            entered_tx.send(()).expect("retirement entered");
        });

        assert!(
            entered_rx.recv_timeout(Duration::from_millis(50)).is_err(),
            "retirement must wait while the exact-read guard is live"
        );
        drop(read_guard);
        entered_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("retirement entered after read released");
        worker.join().expect("retirement worker");
    }

    #[test]
    fn publication_waits_for_an_inflight_retirement_guard() {
        let fixture = fixture();
        let (_created, _event_id, artifact, _) = create_bound(&fixture);
        let envelope = fixture
            .store
            .read(artifact.envelope_digest().as_str())
            .expect("sealed envelope");
        let competing_store =
            FilesystemSealedArtifactStore::open(fixture.store.root()).expect("second store handle");
        let retirement_guard = fixture
            .store
            .acquire_exclusive_operation_guard()
            .expect("exclusive retirement guard");
        let digest = artifact.envelope_digest().as_str().to_string();
        let (published_tx, published_rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let publication = competing_store
                .publish_for_commit(&digest, &envelope)
                .expect("publication guard");
            published_tx.send(()).expect("publication entered");
            drop(publication);
        });

        assert!(
            published_rx
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "publication must wait while retirement owns the store guard"
        );
        drop(retirement_guard);
        published_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("publication entered after retirement released");
        worker.join().expect("publication worker");
    }

    #[test]
    fn restart_recovers_each_retirement_durable_boundary() {
        for boundary in ["retiring", "tombstone", "unlink"] {
            let fixture = fixture();
            let (created, event_id, artifact, _) = create_bound(&fixture);
            {
                // Simulate a process death immediately after this durable
                // boundary by releasing the guard without the next step.
                let guard = fixture
                    .store
                    .acquire_exclusive_operation_guard()
                    .expect("exclusive guard");
                fixture
                    .ledger
                    .begin_exact_artifact_retirement(created.header.run_id, event_id, &artifact)
                    .expect("durable retiring audit");
                if boundary != "retiring" {
                    fixture
                        .ledger
                        .finish_exact_artifact_retirement(
                            created.header.run_id,
                            event_id,
                            &artifact,
                        )
                        .expect("durable tombstone");
                }
                if boundary == "unlink" {
                    guard
                        .locked_remove(artifact.envelope_digest().as_str())
                        .expect("durable unlink");
                }
            }

            let restarted = RunLedger::open(fixture.factory.clone()).expect("restart ledger");
            let coordinator =
                ExactReplayLifecycleCoordinator::new(&restarted, &fixture.keys, &fixture.store);
            let recovered = coordinator
                .retire_exact_reference(created.header.run_id, event_id, &artifact)
                .expect("restart recovery");
            assert_eq!(
                recovered.physical_state,
                ArtifactPhysicalState::Retired,
                "boundary={boundary}"
            );
            assert!(matches!(
                fixture.store.read(artifact.envelope_digest().as_str()),
                Err(SealedArtifactError::Io(error)) if error.kind() == io::ErrorKind::NotFound
            ));
        }
    }
}
