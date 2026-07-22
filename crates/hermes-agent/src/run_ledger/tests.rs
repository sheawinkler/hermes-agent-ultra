use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Barrier};
use std::process::Command;
use std::time::Duration;

use hermes_core::execution_mode::{ExecutionMode, ExecutionModeSource, ResolvedExecutionMode};
use rusqlite::{params, types::ValueRef, OptionalExtension};
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

use crate::sealed_artifact;
use crate::session_persistence::SessionPersistence;
use crate::run_runtime::CertifiedEffectIntent;

use super::ledger::{
    derive_nonresumable_claim_binding, derive_nonresumable_idempotency_key,
    derive_nonresumable_result_append_key,
};
use super::policy::DurableOutboxDestinationHandler;
use super::types::LeaseToken;
use super::{
    ActionId, AppendIdentity, AppendKey, AppendLease, AppendReceipt, AppendRequest,
    AppendReservation, ArtifactDegradationReason, BranchId, CanonicalJson,
    CertifiedOutboxDestinationHandler, ClaimNext, CreatedRun, DeliveryCompletion,
    DeliveryEvidence, DeliveryFailureReason, Destination, DigestAbsence, DispatchPolicyV1,
    DurableDeliveryReceipt, EffectIntent, EventId, LedgerClock, LedgerConnectionFactory,
    LedgerError, ModelId, NonResumableEffectAttempt, NonResumableEffectState, ObjectiveId,
    OutboxRouteRegistry, OutboxState, PrivateFingerprint, ProvenanceDigest, ProviderId, RunExit,
    RunId, RunLedger, RunLedgerConfig, RunOutcome, RunOwnerId, RunOwnerLease, RunProvenance,
    RunStart, SealedArtifactRef, SessionId, SessionRunEventKind, Sha256Digest, StaleRunRecovery,
    TaskId, TenantId, TerminalReason,
};

#[derive(Debug)]
struct TestClock {
    millis: AtomicI64,
}

impl TestClock {
    fn new(millis: i64) -> Self {
        Self {
            millis: AtomicI64::new(millis),
        }
    }

    fn advance(&self, milliseconds: i64) {
        self.millis.fetch_add(milliseconds, Ordering::SeqCst);
    }
}

impl LedgerClock for TestClock {
    fn now_unix_ms(&self) -> Result<i64, LedgerError> {
        Ok(self.millis.load(Ordering::SeqCst))
    }
}

struct DedupeHandler;

impl DurableOutboxDestinationHandler for DedupeHandler {
    fn destination(&self) -> Destination {
        Destination::AlphaRuntimeV1
    }

    fn deliver(
        &self,
        _idempotency_key: &super::DownstreamIdempotencyKey,
        _public_payload: &CanonicalJson,
    ) -> Result<DurableDeliveryReceipt, DeliveryFailureReason> {
        DurableDeliveryReceipt::new("durable-receipt")
            .map_err(|_| DeliveryFailureReason::HandlerRejected)
    }
}

fn begin_observed_one_shot(
    ledger: &RunLedger,
    owner_lease: &RunOwnerLease,
    intent: &AppendReceipt,
) -> NonResumableEffectAttempt {
    let certified = CertifiedEffectIntent::from_receipt_for_test(intent).unwrap();
    let lease = ledger
        .begin_nonresumable_effect(mode(), owner_lease, &certified)
        .unwrap();
    let attempt = RunLedger::observed_nonresumable_attempt(lease, "returned").unwrap();
    ledger.persist_nonresumable_result(mode(), &attempt).unwrap();
    attempt
}


struct NonDedupeHandler;

impl DurableOutboxDestinationHandler for NonDedupeHandler {
    fn destination(&self) -> Destination {
        Destination::AlphaRuntimeV1
    }

    fn deliver(
        &self,
        _idempotency_key: &super::DownstreamIdempotencyKey,
        _public_payload: &CanonicalJson,
    ) -> Result<DurableDeliveryReceipt, DeliveryFailureReason> {
        DurableDeliveryReceipt::new("forged-receipt")
            .map_err(|_| DeliveryFailureReason::HandlerRejected)
    }
}

fn mode() -> ResolvedExecutionMode {
    ResolvedExecutionMode {
        mode: ExecutionMode::Manual,
        source: ExecutionModeSource::BuiltInDefault,
    }
}

fn owner_id() -> RunOwnerId {
    RunOwnerId::new("test-owner").unwrap()
}

fn object(value: serde_json::Value) -> CanonicalJson {
    CanonicalJson::from_value(&value).unwrap()
}

fn digest(byte: u8) -> Sha256Digest {
    Sha256Digest::new(hex::encode([byte; 32])).unwrap()
}

fn provenance() -> RunProvenance {
    RunProvenance::new(
        SessionId::new("session-01").unwrap(),
        None,
        BranchId::new("branch-main").unwrap(),
        TenantId::new("tenant-public").unwrap(),
        ActionId::new("mission-enqueue").unwrap(),
        ObjectiveId::new("objective-42").unwrap(),
        TaskId::new("task-42").unwrap(),
        ModelId::new("gpt-5.6").unwrap(),
        ProviderId::new("openai").unwrap(),
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

fn identity(byte: u8) -> AppendIdentity {
    AppendIdentity::new(
        object(json!({"tenant": "public-tenant", "subject": "public-subject"})),
        PrivateFingerprint::hmac_sha256_v1(
            super::LedgerIdentityKeyId::new("test-ledger-key-v1").unwrap(),
            7,
            [byte; 32],
        )
        .unwrap(),
    )
}

fn artifact_ref(byte: u8) -> SealedArtifactRef {
    SealedArtifactRef::try_from(sealed_artifact::SealedArtifactRef {
        envelope_digest: hex::encode([byte; 32]),
        key_id: "sealed-artifact-key-v1".to_string(),
        schema_version: sealed_artifact::SEALED_ARTIFACT_SCHEMA_VERSION,
        encryption_algorithm: sealed_artifact::ARTIFACT_ENCRYPTION_ALGORITHM.to_string(),
        canonicalization_algorithm: sealed_artifact::ARTIFACT_CANONICALIZATION.to_string(),
        canonicalization_version: sealed_artifact::ARTIFACT_CANONICALIZATION_VERSION,
        redactor_version: "redactor-v1".to_string(),
    })
    .unwrap()
}

fn delivery_evidence(
    lease: &super::OutboxLease,
    completion: &DeliveryCompletion,
    byte: u8,
) -> DeliveryEvidence {
    DeliveryEvidence::new(
        AppendIdentity::new(
            CanonicalJson::from_value(&super::delivery_result_evidence_identity(
                lease, completion,
            ))
            .unwrap(),
            PrivateFingerprint::hmac_sha256_v1(
                super::LedgerIdentityKeyId::new("test-ledger-key-v1").unwrap(),
                7,
                [byte; 32],
            )
            .unwrap(),
        ),
        artifact_ref(byte),
    )
}

fn fixture(max_attempts: u32) -> (TempDir, LedgerConnectionFactory, RunLedger, Arc<TestClock>) {
    let directory = tempfile::tempdir().unwrap();
    let factory = LedgerConnectionFactory::new(directory.path().join("sessions.db"));
    factory.migrate_and_cut_over_v0_to_v1().unwrap();
    let clock = Arc::new(TestClock::new(1_700_000_000_000));
    let ledger = RunLedger::open_with_clock(
        factory.clone(),
        RunLedgerConfig {
            append_lease_duration: Duration::from_millis(10),
            outbox_lease_duration: Duration::from_millis(10),
            owner_lease_duration: Duration::from_secs(30),
            max_delivery_attempts: max_attempts,
        },
        clock.clone(),
    )
    .unwrap();
    (directory, factory, ledger, clock)
}

fn create_owned_run(ledger: &RunLedger) -> CreatedRun {
    let run_id = RunId::new();
    ledger
        .create_run_and_start(
            mode(),
            owner_id(),
            RunStart::new(run_id, provenance(), object(json!({"start": "ok"}))),
        )
        .unwrap()
}

fn create_run(ledger: &RunLedger) -> CreatedRun {
    create_owned_run(ledger)
}

#[test]
fn closed_mode_requires_execution_certification_before_an_authoritative_ledger_run() {
    let (_directory, factory, ledger, _clock) = fixture(3);

    for execution_mode in [
        ExecutionMode::Reviewed,
        ExecutionMode::AutonomousLocal,
        ExecutionMode::Ci,
        ExecutionMode::RemoteWorker,
    ] {
        let error = ledger
            .create_run_and_start(
                ResolvedExecutionMode {
                    mode: execution_mode,
                    source: ExecutionModeSource::Cli,
                },
                owner_id(),
                RunStart::new(
                    RunId::new(),
                    provenance(),
                    object(json!({"start": "must-not-write"})),
                ),
            )
            .unwrap_err();
        assert!(matches!(
            error,
            LedgerError::MissingExecutionCertification { mode }
                if mode == execution_mode.as_str()
        ));
    }
    let connection = factory.open().unwrap();
    let persisted_runs: i64 = connection
        .query_row("SELECT COUNT(*) FROM session_runs", [], |row| row.get(0))
        .unwrap();
    assert_eq!(persisted_runs, 0);
}

fn append_request(
    run_id: RunId,
    key: &str,
    effect_intent: EffectIntent,
    fingerprint: u8,
) -> AppendRequest {
    AppendRequest::new(
        run_id,
        AppendKey::new(key).unwrap(),
        identity(fingerprint),
        effect_intent,
        object(json!({"request": "public"})),
    )
}

fn reserve(ledger: &RunLedger, owner_lease: &RunOwnerLease, request: AppendRequest) -> AppendLease {
    match ledger.reserve_append(mode(), owner_lease, request).unwrap() {
        AppendReservation::Reserved(lease) => lease,
        other => panic!("expected reservation, got {other:?}"),
    }
}

fn commit_none(
    ledger: &RunLedger,
    owner_lease: &RunOwnerLease,
    request: AppendRequest,
) -> AppendReceipt {
    let lease = reserve(ledger, owner_lease, request);
    ledger
        .commit_reserved_append(mode(), owner_lease, &lease, None)
        .expect("non-effect append must commit without artifact")
}

fn commit_effect(
    ledger: &RunLedger,
    owner_lease: &RunOwnerLease,
    request: AppendRequest,
    reference: SealedArtifactRef,
) -> AppendReceipt {
    let lease = reserve(ledger, owner_lease, request);
    let published = ledger
        .publish_sealed_ref(mode(), owner_lease, &lease, reference)
        .expect("publish sealed artifact reference");
    ledger
        .commit_reserved_append(mode(), owner_lease, &lease, Some(&published))
        .expect("effect append must commit after publication")
}

fn registered_routes() -> OutboxRouteRegistry {
    let mut routes = OutboxRouteRegistry::default();
    routes
        .register_certified(
            CertifiedOutboxDestinationHandler::alpha_runtime_v1(Arc::new(DedupeHandler)).unwrap(),
        )
        .unwrap();
    routes
}

const LEDGER_DATA_TABLES: &[&str] = &[
    "session_runs",
    "session_run_events",
    "session_run_manifests",
    "session_run_append_reservations",
    "session_run_nonresumable_effects",
    "session_run_outbox",
];

fn raw_digest(byte: u8) -> String {
    hex::encode([byte; 32])
}

fn insert_schema_event(
    connection: &rusqlite::Connection,
    event_id: &str,
    run_id: &str,
    event_sequence: i64,
    event_kind: &str,
    append_key: Option<&str>,
    effect_intent: &str,
    artifact_degradation_reason: Option<&str>,
    identity: bool,
    sealed_ref: bool,
    run_provenance_jcs: Option<&str>,
) -> rusqlite::Result<usize> {
    let public_identity_jcs = identity.then_some(r#"{"subject":"delivery","tenant":"public"}"#);
    let private_fingerprint_algorithm = identity.then_some("hmac-sha256");
    let private_fingerprint_algorithm_version = identity.then_some(1_i64);
    let private_fingerprint_key_id = identity.then_some("delivery-fingerprint-key");
    let private_fingerprint_key_version = identity.then_some(1_i64);
    let private_fingerprint = identity.then(|| raw_digest(0x41));
    let sealed_artifact_envelope_digest = sealed_ref.then(|| raw_digest(0x51));
    let sealed_artifact_key_id = sealed_ref.then_some("delivery-artifact-key");
    let sealed_artifact_schema_version = sealed_ref.then_some(1_i64);
    let sealed_artifact_encryption_algorithm = sealed_ref.then_some("aes-256-gcm");
    let sealed_artifact_canonicalization_algorithm = sealed_ref.then_some("rfc8785-jcs");
    let sealed_artifact_canonicalization_version = sealed_ref.then_some(1_i64);
    let sealed_artifact_redactor_version = sealed_ref.then_some("delivery-redactor-v1");
    let sequence_byte = u8::try_from(event_sequence).unwrap();

    connection.execute(
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
            event_id,
            run_id,
            event_sequence,
            event_kind,
            append_key,
            effect_intent,
            artifact_degradation_reason,
            public_identity_jcs,
            private_fingerprint_algorithm,
            private_fingerprint_algorithm_version,
            private_fingerprint_key_id,
            private_fingerprint_key_version,
            private_fingerprint,
            sealed_artifact_envelope_digest,
            sealed_artifact_key_id,
            sealed_artifact_schema_version,
            sealed_artifact_encryption_algorithm,
            sealed_artifact_canonicalization_algorithm,
            sealed_artifact_canonicalization_version,
            sealed_artifact_redactor_version,
            run_provenance_jcs,
            r#"{"delivery":"result"}"#,
            raw_digest(sequence_byte.wrapping_add(0x61)),
            raw_digest(sequence_byte.wrapping_add(0x71)),
            raw_digest(sequence_byte.wrapping_add(0x81)),
            "rfc8785-jcs",
            1_i64,
            1_700_000_000_000_i64,
        ],
    )
}

fn seed_populated_v1(factory: &LedgerConnectionFactory) {
    factory.create_v1_schema_for_test().unwrap();
    seed_populated_legacy_rows(factory);
}

fn seed_populated_v2(factory: &LedgerConnectionFactory) {
    factory.create_v2_schema_for_test().unwrap();
    seed_populated_legacy_rows(factory);
}

fn seed_populated_legacy_rows(factory: &LedgerConnectionFactory) {
    let connection = factory.open_configured().unwrap();
    connection.execute_batch("PRAGMA foreign_keys=OFF;").unwrap();

    connection
        .execute(
            "INSERT INTO session_runs (
                 run_id, session_id, parent_run_id, branch_id, tenant_id, action_id,
                 objective_id, task_id, model_id, provider_id, config_digest, prompt_digest,
                 prompt_digest_absence, input_digest, input_digest_absence, output_digest,
                 output_digest_absence, tool_digest, tool_digest_absence, skill_digest,
                 skill_digest_absence, policy_digest, policy_digest_absence, sandbox_digest,
                 sandbox_digest_absence, provenance_canonicalization_algorithm,
                 provenance_canonicalization_version, execution_mode, execution_mode_source,
                 owner_id, owner_lease_generation, owner_lease_token, owner_lease_expires_at_ms,
                 start_event_id, started_at_ms, next_event_sequence, head_hash, exit_event_id,
                 exit_terminal_reason, candidate_outcome, final_outcome, required_dead_lettered,
                 pending_required_delivery
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                 ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30,
                 ?31, ?32, ?33, ?34, ?35, ?36, ?37, ?38, ?39, ?40, ?41, ?42, ?43
             )",
            params![
                "run-v1",
                "session-v1",
                Option::<String>::None,
                "branch-v1",
                "tenant-v1",
                "action-v1",
                "objective-v1",
                "task-v1",
                "model-v1",
                "provider-v1",
                raw_digest(0x01),
                Option::<String>::None,
                "unavailable",
                Option::<String>::None,
                "unavailable",
                Option::<String>::None,
                "unavailable",
                Option::<String>::None,
                "not_applicable",
                Option::<String>::None,
                "not_applicable",
                Option::<String>::None,
                "unavailable",
                Option::<String>::None,
                "not_applicable",
                "rfc8785-jcs",
                1_i64,
                "manual",
                "built_in_default",
                "v1-owner",
                1_i64,
                vec![0x11_u8; 16],
                1_700_000_000_100_i64,
                "event-v1-start",
                1_700_000_000_000_i64,
                3_i64,
                raw_digest(0x02),
                Option::<String>::None,
                Option::<String>::None,
                Option::<String>::None,
                Option::<String>::None,
                0_i64,
                0_i64,
            ],
        )
        .unwrap();
    insert_schema_event(
        &connection,
        "event-v1-start",
        "run-v1",
        1,
        "run_start",
        None,
        "none",
        None,
        false,
        false,
        Some(r#"{"schema":"hermes.session-run.start-binding/v1"}"#),
    )
    .unwrap();
    insert_schema_event(
        &connection,
        "event-v1-append",
        "run-v1",
        2,
        "append",
        Some("append-v1"),
        "alpha_runtime_v1",
        None,
        true,
        true,
        None,
    )
    .unwrap();
    insert_schema_event(
        &connection,
        "event-v1-delivery-result",
        "run-v1",
        3,
        "delivery_result",
        None,
        "none",
        None,
        false,
        false,
        None,
    )
    .unwrap();
    connection
        .execute(
            "INSERT INTO session_run_manifests (
                 event_id, manifest_jcs, manifest_hash, canonicalization_algorithm,
                 canonicalization_version
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                "event-v1-append",
                r#"{"manifest":"v1"}"#,
                raw_digest(0x03),
                "rfc8785-jcs",
                1_i64,
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session_run_append_reservations (
                 append_key, run_id, event_id, frozen_event_at_ms, public_identity_jcs,
                 private_fingerprint_algorithm, private_fingerprint_algorithm_version,
                 private_fingerprint_key_id, private_fingerprint_key_version,
                 private_fingerprint, effect_intent, public_payload_jcs,
                 sealed_artifact_envelope_digest, sealed_artifact_key_id,
                 sealed_artifact_schema_version, sealed_artifact_encryption_algorithm,
                 sealed_artifact_canonicalization_algorithm,
                 sealed_artifact_canonicalization_version, sealed_artifact_redactor_version,
                 lease_generation, lease_token, lease_expires_at_ms, committed_event_id,
                 committed_at_ms, terminal_audit_event_id, terminal_degraded_at_ms
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                 ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26
             )",
            params![
                "append-v1",
                "run-v1",
                "reservation-event-v1",
                1_700_000_000_001_i64,
                r#"{"subject":"delivery","tenant":"public"}"#,
                "hmac-sha256",
                1_i64,
                "v1-fingerprint-key",
                1_i64,
                raw_digest(0x04),
                "alpha_runtime_v1",
                r#"{"append":"v1"}"#,
                raw_digest(0x05),
                "v1-artifact-key",
                1_i64,
                "aes-256-gcm",
                "rfc8785-jcs",
                1_i64,
                "redactor-v1",
                1_i64,
                vec![0x12_u8; 16],
                1_700_000_000_100_i64,
                "event-v1-append",
                1_700_000_000_002_i64,
                Option::<String>::None,
                Option::<i64>::None,
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session_run_nonresumable_effects (
                 event_id, run_id, state, idempotency_key, claim_token, claimed_at_ms,
                 result_payload_digest, result_event_id, terminal_at_ms
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                "event-v1-append",
                "run-v1",
                "armed",
                raw_digest(0x06),
                Option::<Vec<u8>>::None,
                Option::<i64>::None,
                Option::<String>::None,
                Option::<String>::None,
                Option::<i64>::None,
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session_run_outbox (
                 outbox_id, run_id, event_id, manifest_order, destination, required,
                 idempotency_key, public_payload_jcs, sealed_artifact_envelope_digest,
                 sealed_artifact_key_id, sealed_artifact_schema_version,
                 sealed_artifact_encryption_algorithm,
                 sealed_artifact_canonicalization_algorithm,
                 sealed_artifact_canonicalization_version, sealed_artifact_redactor_version,
                 state, attempt_count, max_attempts, lease_generation, lease_token,
                 lease_expires_at_ms, delivery_receipt, delivery_error, terminal_at_ms
             ) VALUES (
                 ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                 ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24
             )",
            params![
                "outbox-v1",
                "run-v1",
                "event-v1-append",
                0_i64,
                "alpha_runtime_v1",
                1_i64,
                raw_digest(0x07),
                r#"{"outbox":"v1"}"#,
                raw_digest(0x08),
                "v1-outbox-artifact-key",
                1_i64,
                "aes-256-gcm",
                "rfc8785-jcs",
                1_i64,
                "redactor-v1",
                "acknowledged",
                1_i64,
                3_i64,
                1_i64,
                Option::<Vec<u8>>::None,
                Option::<i64>::None,
                "old-v1-ack",
                Option::<String>::None,
                1_700_000_000_003_i64,
            ],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO state_meta (key, value) VALUES (?1, ?2)",
            params!["retained-v1-marker", "retain-me"],
        )
        .unwrap();
    connection.execute_batch("PRAGMA foreign_keys=ON;").unwrap();
}

fn table_counts(connection: &rusqlite::Connection) -> Vec<(String, i64)> {
    LEDGER_DATA_TABLES
        .iter()
        .map(|table| {
            let count = connection
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| row.get(0))
                .unwrap();
            ((*table).to_string(), count)
        })
        .collect()
}

fn event_hash_rows(
    connection: &rusqlite::Connection,
) -> Vec<(String, String, String, String, String)> {
    connection
        .prepare(
            "SELECT event_id, public_payload_jcs, manifest_hash, previous_hash, event_hash
             FROM session_run_events ORDER BY event_id",
        )
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn state_meta_rows(connection: &rusqlite::Connection) -> Vec<(String, String)> {
    connection
        .prepare("SELECT key, value FROM state_meta ORDER BY key")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn schema_sql_rows(connection: &rusqlite::Connection) -> Vec<(String, String, String)> {
    connection
        .prepare(
            "SELECT type, name, sql
             FROM sqlite_master
             WHERE sql IS NOT NULL
               AND (
                   (type = 'table' AND name IN (
                       'state_meta', 'session_runs', 'session_run_events',
                       'session_run_manifests', 'session_run_append_reservations',
                       'session_run_nonresumable_effects', 'session_run_outbox'
                   ))
                   OR (
                       type = 'index' AND tbl_name IN (
                           'session_runs', 'session_run_events', 'session_run_manifests',
                           'session_run_append_reservations',
                           'session_run_nonresumable_effects', 'session_run_outbox'
                       )
                   )
               )
             ORDER BY type, name",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
}

fn sqlite_value_snapshot(value: ValueRef<'_>) -> String {
    match value {
        ValueRef::Null => "null".to_string(),
        ValueRef::Integer(value) => format!("integer:{value}"),
        ValueRef::Real(value) => format!("real:{:016x}", value.to_bits()),
        ValueRef::Text(value) => format!("text:{}", hex::encode(value)),
        ValueRef::Blob(value) => format!("blob:{}", hex::encode(value)),
    }
}

fn outbox_row_snapshots(connection: &rusqlite::Connection) -> Vec<Vec<String>> {
    let mut statement = connection
        .prepare("SELECT * FROM session_run_outbox ORDER BY outbox_id")
        .unwrap();
    let column_count = statement.column_count();
    let mut rows = statement.query([]).unwrap();
    let mut snapshots = Vec::new();
    while let Some(row) = rows.next().unwrap() {
        snapshots.push(
            (0..column_count)
                .map(|column| sqlite_value_snapshot(row.get_ref(column).unwrap()))
                .collect(),
        );
    }
    snapshots
}

#[derive(Debug, PartialEq, Eq)]
struct V1MigrationSnapshot {
    schema: Vec<(String, String, String)>,
    counts: Vec<(String, i64)>,
    events: Vec<(String, String, String, String, String)>,
    outbox: Vec<Vec<String>>,
    meta: Vec<(String, String)>,
    foreign_key_violation: Option<String>,
    settings: (String, i64, i64),
}

fn v1_migration_snapshot(connection: &rusqlite::Connection) -> V1MigrationSnapshot {
    let foreign_key_violation = connection
        .query_row("PRAGMA foreign_key_check", [], |row| row.get(0))
        .optional()
        .unwrap();
    let journal_mode = connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    let synchronous = connection
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .unwrap();
    let foreign_keys = connection
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    V1MigrationSnapshot {
        schema: schema_sql_rows(connection),
        counts: table_counts(connection),
        events: event_hash_rows(connection),
        outbox: outbox_row_snapshots(connection),
        meta: state_meta_rows(connection),
        foreign_key_violation,
        settings: (journal_mode, synchronous, foreign_keys),
    }
}

const V1_MIGRATION_CRASH_DATABASE_ENV: &str = "HERMES_V1_MIGRATION_CRASH_DATABASE";
const V1_MIGRATION_CRASH_STEP_ENV: &str = "HERMES_V1_MIGRATION_CRASH_STEP";
const V1_MIGRATION_CRASH_BOUNDARY_ENV: &str = "HERMES_V1_MIGRATION_CRASH_BOUNDARY";
const V2_MIGRATION_CRASH_DATABASE_ENV: &str = "HERMES_V2_MIGRATION_CRASH_DATABASE";
const V2_MIGRATION_CRASH_STEP_ENV: &str = "HERMES_V2_MIGRATION_CRASH_STEP";
const V2_MIGRATION_CRASH_BOUNDARY_ENV: &str = "HERMES_V2_MIGRATION_CRASH_BOUNDARY";

fn meta_value<'a>(rows: &'a [(String, String)], key: &str) -> Option<&'a str> {
    rows.iter()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, value)| value.as_str())
}

fn assert_complete_v6_recovery(
    factory: &LedgerConnectionFactory,
    before: &V1MigrationSnapshot,
) {
    factory.validate_cutover().unwrap();
    let connection = factory.open().unwrap();
    assert_eq!(table_counts(&connection), before.counts);
    assert_eq!(event_hash_rows(&connection), before.events);
    assert_eq!(outbox_row_snapshots(&connection), before.outbox);

    let after_meta = state_meta_rows(&connection);
    assert_eq!(
        meta_value(&after_meta, "retained-v1-marker"),
        meta_value(&before.meta, "retained-v1-marker")
    );
    assert_eq!(meta_value(&after_meta, "run_ledger_schema_version"), Some("6"));
    assert_eq!(meta_value(&after_meta, "ledger_primary"), Some("1"));
    let expected_fingerprint = super::connection::expected_schema_fingerprint();
    assert_eq!(
        meta_value(&after_meta, "run_ledger_schema_fingerprint"),
        Some(expected_fingerprint.as_str())
    );

    let recovered = v1_migration_snapshot(&connection);
    assert!(recovered.foreign_key_violation.is_none());
    assert_eq!(recovered.settings, before.settings);
}

#[test]
fn fresh_v6_schema_validates_exactly() {
    let directory = tempfile::tempdir().unwrap();
    let factory = LedgerConnectionFactory::new(directory.path().join("fresh-v6.db"));

    factory.migrate_and_cut_over_v0_to_v1().unwrap();
    factory.validate_cutover().unwrap();
    let connection = factory.open().unwrap();

    let version: String = connection
        .query_row(
            "SELECT value FROM state_meta WHERE key = 'run_ledger_schema_version'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let fingerprint: String = connection
        .query_row(
            "SELECT value FROM state_meta WHERE key = 'run_ledger_schema_fingerprint'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(version, "6");
    assert_eq!(fingerprint, super::connection::expected_schema_fingerprint());
    assert_eq!(super::LEDGER_SCHEMA_VERSION, 6);
}

#[test]
fn populated_v5_replay_obligations_upgrade_and_reopen() {
    let (_directory, factory, ledger, _clock) = fixture(3);
    let created = create_run(&ledger);
    drop(ledger);

    let connection = factory.open_configured().unwrap();
    connection
        .execute_batch(
            r#"
            DROP INDEX idx_session_run_replay_obligations_run;
            ALTER TABLE session_run_replay_obligations
                RENAME TO session_run_replay_obligations_v6_legacy;
            CREATE TABLE session_run_replay_obligations (
                run_id TEXT NOT NULL REFERENCES session_runs(run_id) ON DELETE RESTRICT,
                source_event_id TEXT NOT NULL REFERENCES session_run_events(event_id) ON DELETE RESTRICT,
                operation_kind TEXT NOT NULL CHECK (operation_kind IN ('run', 'model', 'tool', 'delegation', 'oauth', 'delivery', 'agent_output')),
                phase TEXT NOT NULL CHECK (phase IN ('request', 'result')),
                replay_fidelity TEXT NOT NULL CHECK (replay_fidelity IN ('pending', 'exact_private', 'metadata_only', 'unavailable')),
                resolved_event_id TEXT REFERENCES session_run_events(event_id) ON DELETE RESTRICT,
                created_at_ms INTEGER NOT NULL,
                resolved_at_ms INTEGER,
                PRIMARY KEY (run_id, source_event_id, operation_kind, phase),
                CHECK (
                    (replay_fidelity = 'pending' AND resolved_event_id IS NULL AND resolved_at_ms IS NULL)
                    OR (replay_fidelity IN ('exact_private', 'metadata_only', 'unavailable')
                        AND resolved_event_id IS NOT NULL AND resolved_at_ms IS NOT NULL)
                )
            );
            CREATE INDEX idx_session_run_replay_obligations_run
            ON session_run_replay_obligations(run_id, replay_fidelity, source_event_id);
            "#,
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO session_run_replay_obligations (
                run_id, source_event_id, operation_kind, phase, replay_fidelity,
                resolved_event_id, created_at_ms, resolved_at_ms
             ) VALUES (?1, ?2, 'run', 'request', 'exact_private', ?2, 17, 17)",
            params![
                created.header.run_id.to_string(),
                created.header.start_event_id.to_string(),
            ],
        )
        .unwrap();
    connection
        .execute_batch("DROP TABLE session_run_replay_obligations_v6_legacy")
        .unwrap();
    connection
        .execute(
            "UPDATE state_meta SET value = '5' WHERE key = 'run_ledger_schema_version'",
            [],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE state_meta SET value = ?1 WHERE key = 'run_ledger_schema_fingerprint'",
            [super::connection::expected_v5_schema_fingerprint_for_test()],
        )
        .unwrap();
    drop(connection);

    factory.migrate_and_cut_over_v0_to_v1().unwrap();
    factory.validate_cutover().unwrap();

    let reopened = factory.open().unwrap();
    let index_count: i64 = reopened
        .query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'index' AND name = 'idx_session_run_replay_obligations_run'
               AND tbl_name = 'session_run_replay_obligations'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(index_count, 1);
    let migrated: (String, String, String, String) = reopened
        .query_row(
            "SELECT operation_instance_id, replay_fidelity, outcome, resolved_event_id
             FROM session_run_replay_obligations
             WHERE run_id = ?1 AND source_event_id = ?2
               AND operation_kind = 'run' AND phase = 'request'",
            params![
                created.header.run_id.to_string(),
                created.header.start_event_id.to_string(),
            ],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .unwrap();
    assert_eq!(
        migrated,
        (
            created.header.start_event_id.to_string(),
            "unavailable".to_string(),
            "not_applicable".to_string(),
            created.header.start_event_id.to_string(),
        )
    );
    let exact: (i64, String) = reopened
        .query_row(
            "SELECT exact_replay, state FROM session_run_exact_replay WHERE run_id = ?1",
            [created.header.run_id.to_string()],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!(exact, (0, "unverified".to_string()));
    drop(reopened);
    factory.open().unwrap();
}

#[test]
fn v1_to_v6_migration_preserves_populated_rows_hashes_and_settings() {
    let directory = tempfile::tempdir().unwrap();
    let factory = LedgerConnectionFactory::new(directory.path().join("populated-v1.db"));
    seed_populated_v1(&factory);

    let before = factory.open_configured().unwrap();
    let counts_before = table_counts(&before);
    let hashes_before = event_hash_rows(&before);
    let outbox_before = outbox_row_snapshots(&before);
    let retained_marker_before = state_meta_rows(&before)
        .into_iter()
        .find(|(key, _)| key == "retained-v1-marker")
        .unwrap()
        .1;
    drop(before);

    factory.migrate_and_cut_over_v0_to_v1().unwrap();
    let after = factory.open().unwrap();
    assert_eq!(table_counts(&after), counts_before);
    assert_eq!(event_hash_rows(&after), hashes_before);
    assert_eq!(outbox_row_snapshots(&after), outbox_before);
    assert_eq!(
        after
            .query_row(
                "SELECT value FROM state_meta WHERE key = 'retained-v1-marker'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        retained_marker_before
    );
    assert_eq!(
        after
            .query_row(
                "SELECT value FROM state_meta WHERE key = 'run_ledger_schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "6"
    );
    let retained_ack: (String, i64, i64, Option<String>, Option<String>, Option<i64>) = after
        .query_row(
            "SELECT state, attempt_count, lease_generation, delivery_receipt, delivery_error,
                    terminal_at_ms
             FROM session_run_outbox WHERE outbox_id = 'outbox-v1'",
            [],
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
        .unwrap();
    assert_eq!(
        retained_ack,
        (
            "acknowledged".to_string(),
            1,
            1,
            Some("old-v1-ack".to_string()),
            None,
            Some(1_700_000_000_003),
        )
    );
    let foreign_key_violation: Option<String> = after
        .query_row("PRAGMA foreign_key_check", [], |row| row.get(0))
        .optional()
        .unwrap();
    assert!(foreign_key_violation.is_none());
    let journal_mode: String = after
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    let synchronous: i64 = after
        .query_row("PRAGMA synchronous", [], |row| row.get(0))
        .unwrap();
    let foreign_keys: i64 = after
        .query_row("PRAGMA foreign_keys", [], |row| row.get(0))
        .unwrap();
    assert!(journal_mode.eq_ignore_ascii_case("wal"));
    assert_eq!(synchronous, 2);
    assert_eq!(foreign_keys, 1);
}

#[test]
fn v4_artifact_state_sql_rejects_unknown_literals() {
    let directory = tempfile::tempdir().unwrap();
    let factory = LedgerConnectionFactory::new(directory.path().join("state-literals-v4.db"));
    factory.migrate_and_cut_over_v0_to_v1().unwrap();
    let connection = factory.open().unwrap();

    for (digest_byte, state, reason) in [
        (0xd1_u8, "unavailable_deleted", Option::<&str>::None),
        (0xd2_u8, "available", Some("gone")),
    ] {
        assert!(
            connection
                .execute(
                    "INSERT INTO session_run_artifact_objects (
                        envelope_digest, key_id, schema_version, encryption_algorithm,
                        canonicalization_algorithm, canonicalization_version, redactor_version,
                        state, state_reason, transition_sequence, tombstoned_at_ms, retired_at_ms
                     ) VALUES (?1, 'test-key', 1, 'aes-256-gcm', 'rfc8785-jcs', 1,
                               'test-redactor', ?2, ?3, 0, NULL, NULL)",
                    params![raw_digest(digest_byte), state, reason],
                )
                .is_err(),
            "unknown artifact state/reason literal must be rejected: state={state}, reason={reason:?}"
        );
    }
}

#[test]
fn v4_reference_and_replay_state_sql_reject_unknown_literals() {
    let (_directory, factory, ledger, _clock) = fixture(3);
    let created = create_run(&ledger);
    let connection = factory.open().unwrap();
    let digest = raw_digest(0xd3);
    connection
        .execute(
            "INSERT INTO session_run_artifact_objects (
                envelope_digest, key_id, schema_version, encryption_algorithm,
                canonicalization_algorithm, canonicalization_version, redactor_version,
                state, state_reason, transition_sequence, tombstoned_at_ms, retired_at_ms
             ) VALUES (?1, 'test-key', 1, 'aes-256-gcm', 'rfc8785-jcs', 1,
                       'test-redactor', 'available', NULL, 0, NULL, NULL)",
            [&digest],
        )
        .unwrap();

    for (state, reason) in [
        ("unavailable_deleted", Option::<&str>::None),
        ("available", Some("gone")),
    ] {
        assert!(
            connection
                .execute(
                    "INSERT INTO session_run_artifact_refs (
                        event_id, run_id, envelope_digest, state, state_reason,
                        transition_sequence, state_changed_at_ms
                     ) VALUES (?1, ?2, ?3, ?4, ?5, 0, 1)",
                    params![
                        created.header.start_event_id.to_string(),
                        created.header.run_id.to_string(),
                        digest.as_str(),
                        state,
                        reason,
                    ],
                )
                .is_err(),
            "unknown reference state/reason literal must be rejected: state={state}, reason={reason:?}"
        );
    }

    for (state, reason) in [
        ("unavailable_deleted", Option::<&str>::None),
        ("available", Some("gone")),
    ] {
        assert!(
            connection
                .execute(
                    "UPDATE session_run_exact_replay
                     SET state = ?1, state_reason = ?2
                     WHERE run_id = ?3",
                    params![state, reason, created.header.run_id.to_string()],
                )
                .is_err(),
            "unknown exact-replay state/reason literal must be rejected: state={state}, reason={reason:?}"
        );
    }
}

#[test]
fn v2_to_v6_migration_preserves_rows_and_marks_historical_artifacts_unverified() {
    let directory = tempfile::tempdir().unwrap();
    let factory = LedgerConnectionFactory::new(directory.path().join("populated-v2.db"));
    seed_populated_v2(&factory);

    let before = factory.open_configured().unwrap();
    let counts_before = table_counts(&before);
    let hashes_before = event_hash_rows(&before);
    let outbox_before = outbox_row_snapshots(&before);
    drop(before);

    factory.migrate_and_cut_over_v0_to_v1().unwrap();
    let after = factory.open().unwrap();
    assert_eq!(table_counts(&after), counts_before);
    assert_eq!(event_hash_rows(&after), hashes_before);
    assert_eq!(outbox_row_snapshots(&after), outbox_before);
    assert_eq!(
        after
            .query_row(
                "SELECT value FROM state_meta WHERE key = 'run_ledger_schema_version'",
                [],
                |row| row.get::<_, String>(0),
            )
            .unwrap(),
        "6"
    );
    assert_eq!(
        after
            .query_row(
                "SELECT state, state_reason, transition_sequence
                 FROM session_run_artifact_objects",
                [],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .unwrap(),
        ("unverified".to_string(), None, 0)
    );
    assert_eq!(
        after
            .query_row(
                "SELECT exact_replay, state, state_reason, transition_sequence
                 FROM session_run_exact_replay WHERE run_id = 'run-v1'",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .unwrap(),
        (0, "unverified".to_string(), None, 0)
    );
    let foreign_key_violation: Option<String> = after
        .query_row("PRAGMA foreign_key_check", [], |row| row.get(0))
        .optional()
        .unwrap();
    assert!(foreign_key_violation.is_none());
}

#[test]
fn every_v2_migration_checkpoint_rolls_back_exactly_and_retries() {
    let operation_count = LedgerConnectionFactory::v2_migration_operation_count_for_test();
    let commit_boundary_count =
        LedgerConnectionFactory::v2_migration_commit_boundary_count_for_test();
    let checkpoint_count = LedgerConnectionFactory::v2_migration_checkpoint_count_for_test();
    assert_eq!(operation_count, 24);
    assert_eq!(commit_boundary_count, 1);
    assert_eq!(
        checkpoint_count,
        operation_count * 2 + commit_boundary_count
    );

    for checkpoint_index in 0..checkpoint_count {
        let directory = tempfile::tempdir().unwrap();
        let factory = LedgerConnectionFactory::new(
            directory
                .path()
                .join(format!("abort-v2-{checkpoint_index}.db")),
        );
        seed_populated_v2(&factory);
        let before_connection = factory.open_configured().unwrap();
        let before = v1_migration_snapshot(&before_connection);
        drop(before_connection);

        assert!(
            matches!(
                factory.migrate_and_abort_at_v2_checkpoint_for_test(checkpoint_index),
                Err(LedgerError::MigrationAborted)
            ),
            "checkpoint {checkpoint_index} did not abort"
        );
        let after_abort = factory.open_configured().unwrap();
        assert_eq!(
            v1_migration_snapshot(&after_abort),
            before,
            "checkpoint {checkpoint_index} changed the v2 database"
        );
        drop(after_abort);

        factory.migrate_and_cut_over_v0_to_v1().unwrap();
        assert_complete_v6_recovery(&factory, &before);
    }
}

#[test]
fn every_v2_migration_crash_boundary_recovers_exactly_and_converges() {
    let operation_count = LedgerConnectionFactory::v2_migration_operation_count_for_test();
    let commit_boundary_count =
        LedgerConnectionFactory::v2_migration_commit_boundary_count_for_test();
    let checkpoint_count = LedgerConnectionFactory::v2_migration_checkpoint_count_for_test();
    assert_eq!(operation_count, 24);
    assert_eq!(commit_boundary_count, 1);
    assert_eq!(
        checkpoint_count,
        operation_count * 2 + commit_boundary_count
    );

    for checkpoint_index in 0..checkpoint_count {
        let (step, boundary) =
            LedgerConnectionFactory::v2_migration_checkpoint_case_for_test(checkpoint_index)
                .expect("v2 crash checkpoint");
        let directory = tempfile::tempdir().unwrap();
        let factory = LedgerConnectionFactory::new(
            directory
                .path()
                .join(format!("hard-abort-v2-{checkpoint_index}.db")),
        );
        seed_populated_v2(&factory);
        let before_connection = factory.open_configured().unwrap();
        let before = v1_migration_snapshot(&before_connection);
        drop(before_connection);

        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "run_ledger::tests::v2_migration_hard_crash_child",
                "--ignored",
            ])
            .env(V2_MIGRATION_CRASH_DATABASE_ENV, factory.database_path())
            .env(V2_MIGRATION_CRASH_STEP_ENV, &step)
            .env(V2_MIGRATION_CRASH_BOUNDARY_ENV, boundary)
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "hard-crash child unexpectedly returned for step={step}, boundary={boundary}; stdout={:?}; stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );

        let recovered_connection = factory.open_configured().unwrap();
        let recovered_v2 = v1_migration_snapshot(&recovered_connection) == before;
        drop(recovered_connection);
        if !recovered_v2 {
            assert_complete_v6_recovery(&factory, &before);
        }

        factory.migrate_and_cut_over_v0_to_v1().unwrap();
        factory.migrate_and_cut_over_v0_to_v1().unwrap();
        assert_complete_v6_recovery(&factory, &before);
    }
}

#[test]
#[ignore = "subprocess-only v2 migration crash helper"]
fn v2_migration_hard_crash_child() {
    let Some(database_path) = std::env::var_os(V2_MIGRATION_CRASH_DATABASE_ENV) else {
        return;
    };
    let step = std::env::var(V2_MIGRATION_CRASH_STEP_ENV).unwrap();
    let boundary = std::env::var(V2_MIGRATION_CRASH_BOUNDARY_ENV).unwrap();
    let factory = LedgerConnectionFactory::new(std::path::PathBuf::from(database_path));
    let _ = factory.migrate_and_hard_abort_at_v2_checkpoint_for_test(&step, &boundary);
}

#[test]
fn v2_outbox_accepts_every_delivery_failure_reason() {
    let directory = tempfile::tempdir().unwrap();
    let factory = LedgerConnectionFactory::new(directory.path().join("outbox-reasons-v2.db"));
    seed_populated_v1(&factory);
    factory.migrate_and_cut_over_v0_to_v1().unwrap();
    let connection = factory.open().unwrap();

    for reason in [
        "missing_route",
        "invalid_payload",
        "route_decode_failed",
        "invalid_variant",
        "sealed_task_mismatch",
        "durable_store_failure",
        "handler_rejected",
        "attempts_exhausted",
    ] {
        assert_eq!(
            connection
                .execute(
                    "UPDATE session_run_outbox
                     SET state = 'dead_lettered', delivery_receipt = NULL, delivery_error = ?1,
                         terminal_at_ms = 1_700_000_000_004
                     WHERE outbox_id = 'outbox-v1'",
                    [reason],
                )
                .unwrap(),
            1
        );
    }
}

#[test]
fn every_v1_migration_checkpoint_rolls_back_exactly_and_retries() {
    let operation_count = LedgerConnectionFactory::v1_migration_operation_count_for_test();
    let commit_boundary_count =
        LedgerConnectionFactory::v1_migration_commit_boundary_count_for_test();
    let checkpoint_count = LedgerConnectionFactory::v1_migration_checkpoint_count_for_test();
    assert_eq!(operation_count, 44);
    assert_eq!(commit_boundary_count, 1);
    assert_eq!(
        checkpoint_count,
        operation_count * 2 + commit_boundary_count
    );

    for checkpoint_index in 0..checkpoint_count {
        let directory = tempfile::tempdir().unwrap();
        let factory = LedgerConnectionFactory::new(
            directory
                .path()
                .join(format!("abort-v1-{checkpoint_index}.db")),
        );
        seed_populated_v1(&factory);

        let before_connection = factory.open_configured().unwrap();
        let before = v1_migration_snapshot(&before_connection);
        assert!(before.foreign_key_violation.is_none());
        assert!(before.settings.0.eq_ignore_ascii_case("wal"));
        assert_eq!(before.settings.1, 2);
        assert_eq!(before.settings.2, 1);
        drop(before_connection);

        assert!(
            matches!(
                factory.migrate_and_abort_at_v1_checkpoint_for_test(checkpoint_index),
                Err(LedgerError::MigrationAborted)
            ),
            "checkpoint {checkpoint_index} did not abort"
        );

        let after_abort = factory.open_configured().unwrap();
        assert_eq!(
            v1_migration_snapshot(&after_abort),
            before,
            "checkpoint {checkpoint_index} changed the v1 database"
        );
        drop(after_abort);

        factory.migrate_and_cut_over_v0_to_v1().unwrap();
        factory.validate_cutover().unwrap();
        let retry = factory.open().unwrap();
        let version: String = retry
            .query_row(
                "SELECT value FROM state_meta WHERE key = 'run_ledger_schema_version'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(version, "6", "checkpoint {checkpoint_index} retry");
    }
}

#[test]
fn every_v1_migration_crash_boundary_recovers_exactly_and_converges() {
    let operation_count = LedgerConnectionFactory::v1_migration_operation_count_for_test();
    let commit_boundary_count =
        LedgerConnectionFactory::v1_migration_commit_boundary_count_for_test();
    let checkpoint_count = LedgerConnectionFactory::v1_migration_checkpoint_count_for_test();
    assert_eq!(operation_count, 44);
    assert_eq!(commit_boundary_count, 1);
    assert_eq!(
        checkpoint_count,
        operation_count * 2 + commit_boundary_count
    );

    for checkpoint_index in 0..checkpoint_count {
        let (step, boundary) =
            LedgerConnectionFactory::v1_migration_checkpoint_case_for_test(checkpoint_index)
                .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let factory = LedgerConnectionFactory::new(
            directory
                .path()
                .join(format!("hard-abort-v1-{checkpoint_index}.db")),
        );
        seed_populated_v1(&factory);

        let before_connection = factory.open_configured().unwrap();
        let before = v1_migration_snapshot(&before_connection);
        drop(before_connection);

        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "run_ledger::tests::v1_migration_hard_crash_child",
                "--ignored",
            ])
            .env(V1_MIGRATION_CRASH_DATABASE_ENV, factory.database_path())
            .env(V1_MIGRATION_CRASH_STEP_ENV, &step)
            .env(V1_MIGRATION_CRASH_BOUNDARY_ENV, boundary)
            .output()
            .unwrap();
        assert!(
            !output.status.success(),
            "hard-crash child unexpectedly returned for step={step}, boundary={boundary}; stdout={:?}; stderr={:?}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );

        let recovered_connection = factory.open_configured().unwrap();
        let recovered_v1 = v1_migration_snapshot(&recovered_connection) == before;
        drop(recovered_connection);
        if !recovered_v1 {
            assert_complete_v6_recovery(&factory, &before);
        }

        factory.migrate_and_cut_over_v0_to_v1().unwrap();
        factory.migrate_and_cut_over_v0_to_v1().unwrap();
        assert_complete_v6_recovery(&factory, &before);
    }
}

#[test]
#[ignore = "subprocess-only v1 migration crash helper"]
fn v1_migration_hard_crash_child() {
    let Some(database_path) = std::env::var_os(V1_MIGRATION_CRASH_DATABASE_ENV) else {
        return;
    };
    let step = std::env::var(V1_MIGRATION_CRASH_STEP_ENV).unwrap();
    let boundary = std::env::var(V1_MIGRATION_CRASH_BOUNDARY_ENV).unwrap();
    let factory = LedgerConnectionFactory::new(std::path::PathBuf::from(database_path));
    let _ = factory.migrate_and_hard_abort_at_v1_checkpoint_for_test(&step, &boundary);
}

#[test]
fn delivery_result_and_exact_run_start_evidence_require_complete_pairs() {
    let (_directory, factory, ledger, _clock) = fixture(3);
    let created = create_run(&ledger);
    let connection = factory.open().unwrap();
    let run_id = created.header.run_id.to_string();

    assert_eq!(
        insert_schema_event(
            &connection,
            "delivery-evidence-complete",
            &run_id,
            2,
            "delivery_result",
            None,
            "none",
            None,
            true,
            true,
            None,
        )
        .unwrap(),
        1
    );
    assert_eq!(
        insert_schema_event(
            &connection,
            "delivery-internal-no-evidence",
            &run_id,
            3,
            "delivery_result",
            None,
            "none",
            None,
            false,
            false,
            None,
        )
        .unwrap(),
        1
    );
    assert!(
        insert_schema_event(
            &connection,
            "delivery-evidence-missing-ref",
            &run_id,
            4,
            "delivery_result",
            None,
            "none",
            None,
            true,
            false,
            None,
        )
        .is_err()
    );
    assert!(
        insert_schema_event(
            &connection,
            "delivery-evidence-missing-identity",
            &run_id,
            5,
            "delivery_result",
            None,
            "none",
            None,
            false,
            true,
            None,
        )
        .is_err()
    );
    assert_eq!(
        insert_schema_event(
            &connection,
            "run-start-exact-evidence",
            &run_id,
            6,
            "run_start",
            None,
            "none",
            None,
            true,
            true,
            Some(r#"{"schema":"hermes.session-run.start-binding/v1"}"#),
        )
        .unwrap(),
        1
    );
    assert!(
        insert_schema_event(
            &connection,
            "run-start-evidence-missing-ref",
            &run_id,
            7,
            "run_start",
            None,
            "none",
            None,
            true,
            false,
            Some(r#"{"schema":"hermes.session-run.start-binding/v1"}"#),
        )
        .is_err()
    );
    assert!(
        insert_schema_event(
            &connection,
            "run-start-evidence-missing-identity",
            &run_id,
            8,
            "run_start",
            None,
            "none",
            None,
            false,
            true,
            Some(r#"{"schema":"hermes.session-run.start-binding/v1"}"#),
        )
        .is_err()
    );
    assert!(
        insert_schema_event(
            &connection,
            "run-exit-evidence",
            &run_id,
            9,
            "run_exit",
            None,
            "none",
            None,
            true,
            true,
            None,
        )
        .is_err()
    );
}

#[test]
fn v1_tampering_and_uppercase_quoted_literals_fail_closed() {
    let directory = tempfile::tempdir().unwrap();

    let fingerprint_tamper =
        LedgerConnectionFactory::new(directory.path().join("v1-fingerprint-tamper.db"));
    fingerprint_tamper.create_v1_schema_for_test().unwrap();
    fingerprint_tamper
        .open_configured()
        .unwrap()
        .execute(
            "UPDATE state_meta SET value = 'tampered-v1-fingerprint'
             WHERE key = 'run_ledger_schema_fingerprint'",
            [],
        )
        .unwrap();
    assert!(matches!(
        fingerprint_tamper.migrate_and_cut_over_v0_to_v1(),
        Err(LedgerError::SchemaFingerprintMismatch { .. })
    ));

    let schema_tamper = LedgerConnectionFactory::new(directory.path().join("v1-schema-tamper.db"));
    schema_tamper.create_v1_schema_for_test().unwrap();
    alter_table_sql(&schema_tamper, "session_run_outbox", "'ready'", "'READY'");
    assert!(matches!(
        schema_tamper.migrate_and_cut_over_v0_to_v1(),
        Err(LedgerError::SchemaValidation(_))
    ));

    let uppercase_v2 = LedgerConnectionFactory::new(directory.path().join("v2-uppercase.db"));
    uppercase_v2.migrate_and_cut_over_v0_to_v1().unwrap();
    alter_table_sql(&uppercase_v2, "session_run_outbox", "'ready'", "'READY'");
    assert!(matches!(
        uppercase_v2.open(),
        Err(LedgerError::SchemaValidation(_))
    ));
}
#[test]
fn run_ledger_schema_is_exact_fingerprinted_and_migration_rolls_back() {
    let directory = tempfile::tempdir().unwrap();
    for name in ["abort-before-markers", "abort-after-markers"] {
        let factory = LedgerConnectionFactory::new(directory.path().join(format!("{name}.db")));
        let result = if name == "abort-before-markers" {
            factory.migrate_and_abort_for_test()
        } else {
            factory.migrate_and_abort_after_markers_for_test()
        };
        assert!(matches!(result, Err(LedgerError::MigrationAborted)));
        assert!(matches!(factory.open(), Err(LedgerError::NotMigrated)));
        let connection = factory.open_configured().unwrap();
        let run_table: Option<i64> = connection
            .query_row(
                "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'session_runs'",
                [],
                |row| row.get(0),
            )
            .optional()
            .unwrap();
        assert!(run_table.is_none(), "rollback left v1 table {name}");
    }

    let wrong_shape = LedgerConnectionFactory::new(directory.path().join("wrong-shape.db"));
    wrong_shape.migrate_and_cut_over_v0_to_v1().unwrap();
    {
        let connection = wrong_shape.open_configured().unwrap();
        connection.execute_batch("PRAGMA foreign_keys=OFF; DROP TABLE session_runs; CREATE TABLE session_runs (run_id TEXT PRIMARY KEY);").unwrap();
    }
    assert!(matches!(
        wrong_shape.open(),
        Err(LedgerError::SchemaValidation(_))
    ));

    let missing_fk = LedgerConnectionFactory::new(directory.path().join("missing-fk.db"));
    missing_fk.migrate_and_cut_over_v0_to_v1().unwrap();
    alter_table_sql(
        &missing_fk,
        "session_run_outbox",
        " REFERENCES session_runs(run_id) ON DELETE RESTRICT",
        "",
    );
    assert!(matches!(
        missing_fk.open(),
        Err(LedgerError::SchemaValidation(_))
    ));

    let missing_check = LedgerConnectionFactory::new(directory.path().join("missing-check.db"));
    missing_check.migrate_and_cut_over_v0_to_v1().unwrap();
    alter_table_sql(
        &missing_check,
        "session_run_outbox",
        "CHECK (attempt_count <= max_attempts)",
        "CHECK (attempt_count >= 0)",
    );
    assert!(matches!(
        missing_check.open(),
        Err(LedgerError::SchemaValidation(_))
    ));

    let missing_index = LedgerConnectionFactory::new(directory.path().join("missing-index.db"));
    missing_index.migrate_and_cut_over_v0_to_v1().unwrap();
    missing_index
        .open_configured()
        .unwrap()
        .execute("DROP INDEX idx_session_run_outbox_claim", [])
        .unwrap();
    assert!(matches!(
        missing_index.open(),
        Err(LedgerError::SchemaValidation(_))
    ));

    let unknown_marker = LedgerConnectionFactory::new(directory.path().join("unknown-marker.db"));
    unknown_marker.migrate_and_cut_over_v0_to_v1().unwrap();
    unknown_marker
        .open_configured()
        .unwrap()
        .execute(
            "UPDATE state_meta SET value = 'not-the-v1-schema' WHERE key = 'run_ledger_schema_fingerprint'",
            [],
        )
        .unwrap();
    assert!(matches!(
        unknown_marker.open(),
        Err(LedgerError::SchemaFingerprintMismatch { .. })
    ));
}

#[test]
fn ledger_cutover_upgrades_real_session_state_meta_without_data_loss() {
    let home = tempfile::tempdir().unwrap();
    let sessions = SessionPersistence::new(home.path());
    sessions.ensure_db().unwrap();
    sessions
        .set_meta("session-retention-marker", "preserve-me")
        .unwrap();

    let factory = LedgerConnectionFactory::new(sessions.db_path());
    factory.migrate_and_cut_over_v0_to_v1().unwrap();
    factory.validate_cutover().unwrap();

    assert_eq!(
        sessions
            .get_meta("session-retention-marker")
            .unwrap()
            .as_deref(),
        Some("preserve-me")
    );
    let connection = factory.open().unwrap();
    let columns = connection
        .prepare("PRAGMA table_info(state_meta)")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(1)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(
        columns,
        vec![("key".to_string(), 1, 1), ("value".to_string(), 1, 0)]
    );
}

#[test]
fn ledger_cutover_rejects_unknown_real_session_state_meta_atomically() {
    let home = tempfile::tempdir().unwrap();
    let sessions = SessionPersistence::new(home.path());
    sessions.ensure_db().unwrap();
    sessions.set_meta("legacy-marker", "unchanged").unwrap();
    let connection = rusqlite::Connection::open(sessions.db_path()).unwrap();
    connection
        .execute_batch("ALTER TABLE state_meta ADD COLUMN ledger_collision TEXT;")
        .unwrap();
    let ddl_before: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'state_meta'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let rows_before = connection
        .prepare("SELECT key, value, ledger_collision FROM state_meta ORDER BY key")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    drop(connection);

    let factory = LedgerConnectionFactory::new(sessions.db_path());
    assert!(matches!(
        factory.migrate_and_cut_over_v0_to_v1(),
        Err(LedgerError::SchemaValidation(_))
    ));

    let connection = rusqlite::Connection::open(sessions.db_path()).unwrap();
    let ddl_after: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'state_meta'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    let rows_after = connection
        .prepare("SELECT key, value, ledger_collision FROM state_meta ORDER BY key")
        .unwrap()
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(ddl_after, ddl_before);
    assert_eq!(rows_after, rows_before);
    let ledger_marker: Option<String> = connection
        .query_row(
            "SELECT value FROM state_meta WHERE key = 'run_ledger_schema_fingerprint'",
            [],
            |row| row.get(0),
        )
        .optional()
        .unwrap();
    assert!(ledger_marker.is_none());
    let ledger_tables: i64 = connection
        .query_row(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name LIKE 'session_run%'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(ledger_tables, 0);
}

fn alter_table_sql(factory: &LedgerConnectionFactory, table: &str, from: &str, to: &str) {
    let connection = factory.open_configured().unwrap();
    connection
        .execute_batch("PRAGMA writable_schema=ON;")
        .unwrap();
    let changed = connection
        .execute(
            "UPDATE sqlite_master SET sql = replace(sql, ?1, ?2) WHERE type = 'table' AND name = ?3",
            params![from, to, table],
        )
        .unwrap();
    assert_eq!(changed, 1);
    connection
        .execute_batch("PRAGMA writable_schema=OFF;")
        .unwrap();
}

#[test]
fn run_ledger_provenance_round_trips_and_validates_ids_and_digests() {
    let (_directory, factory, ledger, _clock) = fixture(3);
    let run_id = RunId::new();
    let provenance = provenance();
    let created = ledger
        .create_run_and_start(
            mode(),
            owner_id(),
            RunStart::new(run_id, provenance.clone(), object(json!({"start": "full"}))),
        )
        .unwrap();
    assert_eq!(created.header.provenance, provenance);
    let reopened = RunLedger::open(factory).unwrap();
    assert_eq!(reopened.run_header(run_id).unwrap(), created.header);
    let events = reopened.events_for_run(run_id).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, SessionRunEventKind::RunStart);
    assert_eq!(
        events[0].run_provenance,
        Some(provenance.canonical_json().unwrap())
    );
    assert!(SessionId::new("not an opaque id").is_err());
    assert!(Sha256Digest::new("A".repeat(64)).is_err());
    assert!(Sha256Digest::new("a".repeat(63)).is_err());
    reopened.verify_run_chain(run_id).unwrap();
}

#[test]
fn owner_heartbeat_fences_old_generation_without_invalidating_inflight_append() {
    let (_directory, _factory, ledger, clock) = fixture(3);
    let created = create_owned_run(&ledger);
    let run_id = created.header.run_id;
    let request = append_request(run_id, "heartbeat-fence", EffectIntent::AlphaRuntimeV1, 13);
    let append_lease = reserve(&ledger, &created.owner_lease, request);
    clock.advance(5);
    let renewed = ledger
        .heartbeat_run_owner(mode(), &created.owner_lease)
        .expect("heartbeat current owner");
    assert_eq!(
        renewed.lease_generation,
        created.owner_lease.lease_generation + 1
    );
    assert_eq!(
        renewed.lease_expires_at_ms,
        created.owner_lease.lease_expires_at_ms + 5
    );

    assert!(matches!(
        ledger.publish_sealed_ref(
            mode(),
            &created.owner_lease,
            &append_lease,
            artifact_ref(0x91),
        ),
        Err(LedgerError::StaleRunOwnerLease { .. })
    ));
    let reference = ledger
        .publish_sealed_ref(mode(), &renewed, &append_lease, artifact_ref(0x91))
        .expect("current owner publishes reserved artifact");
    assert!(matches!(
        ledger.commit_reserved_append(
            mode(),
            &created.owner_lease,
            &append_lease,
            Some(&reference),
        ),
        Err(LedgerError::StaleRunOwnerLease { .. })
    ));
    ledger
        .commit_reserved_append(mode(), &renewed, &append_lease, Some(&reference))
        .expect("current owner commits the pre-heartbeat append reservation");
    assert!(matches!(
        ledger.finalize_run_exit(
            mode(),
            &created.owner_lease,
            RunExit::new(TerminalReason::Completed, object(json!({"stale": true}))),
        ),
        Err(LedgerError::StaleRunOwnerLease { .. })
    ));
    ledger
        .finalize_run_exit(
            mode(),
            &renewed,
            RunExit::new(TerminalReason::Completed, object(json!({"current": true}))),
        )
        .expect("current owner finalizes");
}

#[test]
fn committed_retry_is_read_only_idempotent_for_same_run_stale_owner_only() {
    let (_directory, _factory, ledger, _clock) = fixture(3);
    let created = create_owned_run(&ledger);
    let request = append_request(
        created.header.run_id,
        "committed-retry",
        EffectIntent::None,
        14,
    );
    let original = commit_none(&ledger, &created.owner_lease, request.clone());
    let renewed = ledger
        .heartbeat_run_owner(mode(), &created.owner_lease)
        .expect("rotate owner fence");
    ledger
        .finalize_run_exit(
            mode(),
            &renewed,
            RunExit::new(TerminalReason::Completed, object(json!({"done": true}))),
        )
        .expect("finalize run");

    assert_eq!(
        ledger
            .reserve_append(mode(), &created.owner_lease, request.clone())
            .expect("same-run committed retry is read-only"),
        AppendReservation::Committed(original)
    );
    let other = create_owned_run(&ledger);
    assert!(matches!(
        ledger.reserve_append(mode(), &other.owner_lease, request),
        Err(LedgerError::StaleRunOwnerLease { .. })
    ));
}

#[test]
fn duplicate_creation_and_exact_expiry_recovery_are_single_writer_and_single_exit() {
    let (_directory, _factory, ledger, clock) = fixture(3);
    let created = create_owned_run(&ledger);
    let run_id = created.header.run_id;
    assert!(matches!(
        ledger.create_run_and_start(
            mode(),
            RunOwnerId::new("competing-owner").unwrap(),
            RunStart::new(run_id, provenance(), object(json!({"duplicate": true}))),
        ),
        Err(LedgerError::RunAlreadyExists { .. })
    ));

    clock.advance(30_000);
    let request = append_request(run_id, "expired-owner", EffectIntent::None, 15);
    assert!(matches!(
        ledger.reserve_append(mode(), &created.owner_lease, request),
        Err(LedgerError::StaleRunOwnerLease { .. })
    ));
    assert!(matches!(
        ledger.heartbeat_run_owner(mode(), &created.owner_lease),
        Err(LedgerError::StaleRunOwnerLease { .. })
    ));
    assert!(matches!(
        ledger.finalize_run_exit(
            mode(),
            &created.owner_lease,
            RunExit::new(TerminalReason::Completed, object(json!({"late": true}))),
        ),
        Err(LedgerError::StaleRunOwnerLease { .. })
    ));

    let barrier = Arc::new(Barrier::new(3));
    let mut workers = Vec::new();
    for _ in 0..2 {
        let worker_ledger = ledger.clone();
        let worker_barrier = Arc::clone(&barrier);
        workers.push(std::thread::spawn(move || {
            worker_barrier.wait();
            worker_ledger
                .recover_stale_run(run_id)
                .expect("recover stale run")
        }));
    }
    barrier.wait();
    let recoveries = workers
        .into_iter()
        .map(|worker| worker.join().expect("recovery worker"))
        .collect::<Vec<_>>();
    assert_eq!(
        recoveries
            .iter()
            .filter(|recovery| matches!(recovery, StaleRunRecovery::Recovered(_)))
            .count(),
        1
    );
    assert_eq!(
        recoveries
            .iter()
            .filter(|recovery| matches!(recovery, StaleRunRecovery::AlreadyFinalized(_)))
            .count(),
        1
    );
    let status = ledger.run_status(run_id).unwrap();
    assert_eq!(
        status.exit_terminal_reason,
        Some(TerminalReason::ProcessDeath)
    );
    assert_eq!(status.final_outcome, Some(RunOutcome::Failure));
    assert_eq!(
        ledger
            .events_for_run(run_id)
            .unwrap()
            .iter()
            .filter(|event| event.kind == SessionRunEventKind::RunExit)
            .count(),
        1
    );
    ledger.verify_run_chain(run_id).unwrap();
}

#[test]
fn sealed_artifact_identity_conversion_is_exact_and_key_versioned() {
    let wire = sealed_artifact::AppendIdentity {
        public_identity_jcs: br#"{"a":1,"b":2}"#.to_vec(),
        private_fingerprint: "a5".repeat(32),
        fingerprint_algorithm: "hmac-sha256".to_string(),
        fingerprint_version: 1,
        fingerprint_key_id: "ledger-key-v7".to_string(),
        fingerprint_key_version: 7,
        redactor_version: "redactor-v1".to_string(),
        canonicalization_algorithm: "rfc8785-jcs".to_string(),
        canonicalization_version: 1,
    };
    let converted = AppendIdentity::try_from(&wire).unwrap();
    assert_eq!(converted.public_identity.as_str(), r#"{"a":1,"b":2}"#);
    assert_eq!(converted.private_fingerprint.algorithm_version(), 1);
    assert_eq!(converted.private_fingerprint.key_version(), 7);
    assert_eq!(
        converted.private_fingerprint.key_id().as_str(),
        "ledger-key-v7"
    );

    let mut invalid = wire.clone();
    invalid.public_identity_jcs = br#"{"b":2,"a":1}"#.to_vec();
    assert!(matches!(
        AppendIdentity::try_from(&invalid),
        Err(LedgerError::SealedArtifactIdentity(_))
    ));
    let mut invalid = wire.clone();
    invalid.fingerprint_algorithm = "sha256".to_string();
    assert!(matches!(
        AppendIdentity::try_from(&invalid),
        Err(LedgerError::SealedArtifactIdentity(_))
    ));
    let mut invalid = wire.clone();
    invalid.fingerprint_key_version = 0;
    assert!(matches!(
        AppendIdentity::try_from(&invalid),
        Err(LedgerError::SealedArtifactIdentity(_))
    ));
    let mut invalid = wire.clone();
    invalid.redactor_version = "not a version".to_string();
    assert!(matches!(
        AppendIdentity::try_from(&invalid),
        Err(LedgerError::SealedArtifactIdentity(_))
    ));
    let mut invalid = wire;
    invalid.private_fingerprint = "A5".repeat(32);
    assert!(matches!(
        AppendIdentity::try_from(&invalid),
        Err(LedgerError::SealedArtifactIdentity(_))
    ));
    let mut invalid_ref = sealed_artifact::SealedArtifactRef {
        envelope_digest: "A5".repeat(32),
        key_id: "sealed-artifact-key-v1".to_string(),
        schema_version: sealed_artifact::SEALED_ARTIFACT_SCHEMA_VERSION,
        encryption_algorithm: sealed_artifact::ARTIFACT_ENCRYPTION_ALGORITHM.to_string(),
        canonicalization_algorithm: sealed_artifact::ARTIFACT_CANONICALIZATION.to_string(),
        canonicalization_version: sealed_artifact::ARTIFACT_CANONICALIZATION_VERSION,
        redactor_version: "redactor-v1".to_string(),
    };
    assert!(SealedArtifactRef::try_from(&invalid_ref).is_err());
    invalid_ref.envelope_digest = "a5".repeat(32);
    invalid_ref.canonicalization_version = 2;
    assert!(matches!(
        SealedArtifactRef::try_from(&invalid_ref),
        Err(LedgerError::SealedArtifactReference(_))
    ));
}

#[test]
fn reservation_publish_commit_is_fenced_and_effect_refs_are_equal_everywhere() {
    let (_directory, _factory, ledger, _clock) = fixture(3);
    let created = create_run(&ledger);
    let run_id = created.header.run_id;
    let request = append_request(run_id, "effect-fence", EffectIntent::AlphaRuntimeV1, 7);
    let lease = reserve(&ledger, &created.owner_lease, request);

    assert!(matches!(
        ledger.commit_reserved_append(mode(), &created.owner_lease, &lease, None),
        Err(LedgerError::SealedArtifactRefRequired { .. })
    ));
    assert_eq!(ledger.events_for_run(run_id).unwrap().len(), 1);
    assert!(ledger.outbox_for_run(run_id).unwrap().is_empty());

    let reference = artifact_ref(0x11);
    let published = ledger
        .publish_sealed_ref(mode(), &created.owner_lease, &lease, reference.clone())
        .unwrap();
    assert_eq!(published, reference);
    assert!(matches!(
        ledger.publish_sealed_ref(mode(), &created.owner_lease, &lease, artifact_ref(0x12),),
        Err(LedgerError::SealedArtifactRefConflict { .. })
    ));
    assert!(matches!(
        ledger.commit_reserved_append(
            mode(),
            &created.owner_lease,
            &lease,
            Some(&artifact_ref(0x12)),
        ),
        Err(LedgerError::SealedArtifactRefMismatch { .. })
    ));

    let receipt = ledger
        .commit_reserved_append(mode(), &created.owner_lease, &lease, Some(&published))
        .unwrap();
    assert_eq!(receipt.sealed_artifact_ref, Some(reference.clone()));
    let event = ledger.events_for_run(run_id).unwrap().pop().unwrap();
    assert_eq!(event.sealed_artifact_ref, Some(reference.clone()));
    let outbox = ledger.outbox_for_run(run_id).unwrap();
    assert_eq!(outbox.len(), 1);
    assert_eq!(outbox[0].sealed_artifact_ref, reference);
    ledger.verify_run_chain(run_id).unwrap();
}

#[test]
fn protected_digest_snapshot_keeps_event_outbox_and_published_reservation_refs() {
    let (_directory, _factory, ledger, _clock) = fixture(3);
    let created = create_run(&ledger);
    let committed_ref = artifact_ref(0x61);
    let receipt = commit_effect(
        &ledger,
        &created.owner_lease,
        append_request(
            created.header.run_id,
            "snapshot-committed-effect",
            EffectIntent::AlphaRuntimeV1,
            0x61,
        ),
        committed_ref.clone(),
    );
    let event = ledger
        .events_for_run(created.header.run_id)
        .unwrap()
        .into_iter()
        .find(|event| event.event_id == receipt.event_id)
        .expect("committed effect event");
    assert_eq!(event.sealed_artifact_ref, Some(committed_ref.clone()));
    let outbox = ledger.outbox_for_run(created.header.run_id).unwrap();
    assert_eq!(outbox.len(), 1);
    assert_eq!(outbox[0].sealed_artifact_ref, committed_ref.clone());

    let reservation_ref = artifact_ref(0x62);
    let reservation = reserve(
        &ledger,
        &created.owner_lease,
        append_request(
            created.header.run_id,
            "snapshot-published-reservation",
            EffectIntent::AlphaRuntimeV1,
            0x62,
        ),
    );
    assert_eq!(
        ledger
            .publish_sealed_ref(
                mode(),
                &created.owner_lease,
                &reservation,
                reservation_ref.clone(),
            )
            .unwrap(),
        reservation_ref
    );

    assert_eq!(
        ledger.referenced_sealed_envelope_digests().unwrap(),
        vec![
            committed_ref.envelope_digest().clone(),
            reservation_ref.envelope_digest().clone(),
        ]
    );
}

#[test]
fn failed_publication_and_stale_tokens_leave_no_event_or_reference() {
    let (_directory, _factory, ledger, clock) = fixture(3);
    let created = create_run(&ledger);
    let run_id = created.header.run_id;
    let request = append_request(
        run_id,
        "publication-failure",
        EffectIntent::AlphaRuntimeV1,
        9,
    );
    let first = reserve(&ledger, &created.owner_lease, request.clone());
    clock.advance(11);
    let second = reserve(&ledger, &created.owner_lease, request);
    assert!(matches!(
        ledger.publish_sealed_ref(mode(), &created.owner_lease, &first, artifact_ref(0x21),),
        Err(LedgerError::StaleAppendLease { .. })
    ));
    assert!(second.published_sealed_artifact_ref.is_none());
    assert!(matches!(
        ledger.commit_reserved_append(mode(), &created.owner_lease, &second, None),
        Err(LedgerError::SealedArtifactRefRequired { .. })
    ));
    assert_eq!(ledger.events_for_run(run_id).unwrap().len(), 1);
    assert!(ledger.outbox_for_run(run_id).unwrap().is_empty());
}

#[test]
fn published_references_are_immutable_and_reused_by_reclaimed_reservations() {
    let (_directory, _factory, ledger, clock) = fixture(3);
    let created = create_run(&ledger);
    let run_id = created.header.run_id;
    let request = append_request(run_id, "reused-ref", EffectIntent::AlphaRuntimeV1, 4);
    let first = reserve(&ledger, &created.owner_lease, request.clone());
    let reference = ledger
        .publish_sealed_ref(mode(), &created.owner_lease, &first, artifact_ref(0x31))
        .unwrap();
    clock.advance(11);
    let second = reserve(&ledger, &created.owner_lease, request);
    assert_eq!(second.event_id, first.event_id);
    assert_eq!(
        second.published_sealed_artifact_ref,
        Some(reference.clone())
    );
    assert_eq!(
        ledger
            .publish_sealed_ref(mode(), &created.owner_lease, &second, reference.clone(),)
            .unwrap(),
        reference
    );
    assert!(matches!(
        ledger.commit_reserved_append(
            mode(),
            &created.owner_lease,
            &first,
            Some(&artifact_ref(0x31)),
        ),
        Err(LedgerError::StaleAppendLease { .. })
    ));
    let receipt = ledger
        .commit_reserved_append(
            mode(),
            &created.owner_lease,
            &second,
            Some(&artifact_ref(0x31)),
        )
        .unwrap();
    assert_eq!(receipt.event_id, first.event_id);
}

#[test]
fn non_effect_append_is_explicit_reserve_then_commit_and_chain_is_contiguous() {
    let (_directory, _factory, ledger, _clock) = fixture(3);
    let created = create_owned_run(&ledger);
    let run_id = created.header.run_id;
    for key in ["sequence-a", "sequence-b"] {
        commit_none(
            &ledger,
            &created.owner_lease,
            append_request(run_id, key, EffectIntent::None, 5),
        );
    }
    ledger
        .finalize_run_exit(
            mode(),
            &created.owner_lease,
            RunExit::new(TerminalReason::Completed, object(json!({"exit": "ok"}))),
        )
        .unwrap();
    assert_eq!(
        ledger
            .events_for_run(run_id)
            .unwrap()
            .iter()
            .map(|event| event.sequence)
            .collect::<Vec<_>>(),
        vec![1, 2, 3, 4]
    );
    ledger.verify_run_chain(run_id).unwrap();
}

#[test]
fn outbox_registration_is_closed_and_claims_are_fenced() {
    let (_directory, _factory, ledger, clock) = fixture(3);
    let mut routes = OutboxRouteRegistry::default();
    assert!(matches!(
        routes.register(Arc::new(NonDedupeHandler)),
        Err(LedgerError::UncertifiedOutboxDestination { .. })
    ));
    routes
        .register_certified(
            CertifiedOutboxDestinationHandler::alpha_runtime_v1(Arc::new(DedupeHandler)).unwrap(),
        )
        .unwrap();

    let created = create_run(&ledger);
    let run_id = created.header.run_id;
    commit_effect(
        &ledger,
        &created.owner_lease,
        append_request(run_id, "outbox-fence", EffectIntent::AlphaRuntimeV1, 4),
        artifact_ref(0x41),
    );
    let first = match ledger.claim_next(mode(), &routes).unwrap() {
        ClaimNext::Claimed(lease) => lease,
        other => panic!("expected a claim, got {other:?}"),
    };
    clock.advance(11);
    let second = match ledger.claim_next(mode(), &routes).unwrap() {
        ClaimNext::Claimed(lease) => lease,
        other => panic!("expected reclaimed claim, got {other:?}"),
    };
    assert!(second.lease_generation > first.lease_generation);
    assert!(matches!(
        ledger.ack(
            mode(),
            &first,
            DurableDeliveryReceipt::new("too-late").unwrap(),
        ),
        Err(LedgerError::StaleOutboxLease { .. })
    ));
    let completion =
        DeliveryCompletion::Acknowledged(DurableDeliveryReceipt::new("durable-final").unwrap());
    let evidence = delivery_evidence(&second, &completion, 0xb1);
    assert_eq!(
        ledger
            .complete_delivery_with_evidence(mode(), &second, completion, evidence)
            .unwrap()
            .state,
        OutboxState::Acknowledged
    );
}

#[test]
fn route_failures_dead_letter_deterministically_without_unsealed_effects() {
    let (_directory, factory, ledger, _clock) = fixture(2);
    let created = create_run(&ledger);
    let run_id = created.header.run_id;
    commit_effect(
        &ledger,
        &created.owner_lease,
        append_request(run_id, "missing-route", EffectIntent::AlphaRuntimeV1, 6),
        artifact_ref(0x51),
    );
    let no_routes = OutboxRouteRegistry::default();
    assert!(matches!(
        ledger.claim_next(mode(), &no_routes).unwrap(),
        ClaimNext::Rejected {
            state: OutboxState::Ready,
            reason: DeliveryFailureReason::MissingRoute,
            ..
        }
    ));
    assert!(matches!(
        ledger.claim_next(mode(), &no_routes).unwrap(),
        ClaimNext::Rejected {
            state: OutboxState::DeadLettered,
            reason: DeliveryFailureReason::MissingRoute,
            ..
        }
    ));
    assert_eq!(
        ledger.run_status(run_id).unwrap().final_outcome,
        Some(RunOutcome::DeadLettered)
    );

    let invalid_created = create_run(&ledger);
    let invalid_run = invalid_created.header.run_id;
    let receipt = commit_effect(
        &ledger,
        &invalid_created.owner_lease,
        append_request(
            invalid_run,
            "invalid-route",
            EffectIntent::AlphaRuntimeV1,
            9,
        ),
        artifact_ref(0x52),
    );
    factory
        .open()
        .unwrap()
        .execute(
            "UPDATE session_run_outbox SET public_payload_jcs = ?1 WHERE outbox_id = ?2",
            params!["\"not-an-object\"", receipt.outbox_ids[0].to_string()],
        )
        .unwrap();
    let routes = registered_routes();
    assert!(matches!(
        ledger.claim_next(mode(), &routes).unwrap(),
        ClaimNext::Rejected {
            state: OutboxState::Ready,
            reason: DeliveryFailureReason::InvalidPayload,
            ..
        }
    ));
    assert!(matches!(
        ledger.claim_next(mode(), &routes).unwrap(),
        ClaimNext::Rejected {
            state: OutboxState::DeadLettered,
            reason: DeliveryFailureReason::InvalidPayload,
            ..
        }
    ));
}

#[test]
fn expired_max_attempt_lease_dead_letters_atomically_without_attempt_plus_one() {
    let (_directory, _factory, ledger, clock) = fixture(3);
    let created = create_run(&ledger);
    let run_id = created.header.run_id;
    commit_effect(
        &ledger,
        &created.owner_lease,
        append_request(run_id, "worker-death", EffectIntent::AlphaRuntimeV1, 11),
        artifact_ref(0x61),
    );
    let routes = registered_routes();
    for expected_attempt in 1..=3 {
        let lease = match ledger.claim_next(mode(), &routes).unwrap() {
            ClaimNext::Claimed(lease) => lease,
            other => panic!("expected worker lease {expected_attempt}, got {other:?}"),
        };
        assert_eq!(lease.attempt_count, expected_attempt);
        clock.advance(11);
    }
    assert!(matches!(
        ledger.claim_next(mode(), &routes).unwrap(),
        ClaimNext::Rejected {
            reason: DeliveryFailureReason::AttemptsExhausted,
            state: OutboxState::DeadLettered,
            ..
        }
    ));
    let outbox = ledger.outbox_for_run(run_id).unwrap();
    assert_eq!(outbox[0].attempt_count, 3);
    assert_eq!(outbox[0].state, OutboxState::DeadLettered);
    assert_eq!(
        outbox[0].delivery_error,
        Some(DeliveryFailureReason::AttemptsExhausted)
    );
    assert_eq!(
        ledger.run_status(run_id).unwrap().final_outcome,
        Some(RunOutcome::DeadLettered)
    );
    assert!(matches!(
        ledger.claim_next(mode(), &routes).unwrap(),
        ClaimNext::Empty
    ));
    let delivery_events = ledger
        .events_for_run(run_id)
        .unwrap()
        .into_iter()
        .filter(|event| event.kind == SessionRunEventKind::DeliveryResult)
        .collect::<Vec<_>>();
    assert_eq!(delivery_events.len(), 1);
    assert!(delivery_events[0]
        .public_payload
        .as_str()
        .contains("attempts_exhausted"));
}

#[test]
fn run_reconciles_zero_rows_all_ack_and_early_dead_letter() {
    let (_directory, _factory, ledger, _clock) = fixture(1);
    let zero_row = create_owned_run(&ledger);
    assert_eq!(
        ledger
            .finalize_run_exit(
                mode(),
                &zero_row.owner_lease,
                RunExit::new(TerminalReason::Completed, object(json!({"zero": true}))),
            )
            .unwrap()
            .final_outcome,
        Some(RunOutcome::Success)
    );

    let all_ack = create_owned_run(&ledger);
    commit_effect(
        &ledger,
        &all_ack.owner_lease,
        append_request(
            all_ack.header.run_id,
            "all-ack",
            EffectIntent::AlphaRuntimeV1,
            10,
        ),
        artifact_ref(0x71),
    );
    assert!(
        ledger
            .finalize_run_exit(
                mode(),
                &all_ack.owner_lease,
                RunExit::new(TerminalReason::Completed, object(json!({"all_ack": true}))),
            )
            .unwrap()
            .pending_required_delivery
    );
    let routes = registered_routes();
    let lease = match ledger.claim_next(mode(), &routes).unwrap() {
        ClaimNext::Claimed(lease) => lease,
        other => panic!("expected all-ack lease, got {other:?}"),
    };
    let completion =
        DeliveryCompletion::Acknowledged(DurableDeliveryReceipt::new("all-ack-receipt").unwrap());
    let evidence = delivery_evidence(&lease, &completion, 0xb2);
    assert_eq!(
        ledger
            .complete_delivery_with_evidence(mode(), &lease, completion, evidence)
            .unwrap()
            .run_status
            .final_outcome,
        Some(RunOutcome::Success)
    );

    let early_dead = create_owned_run(&ledger);
    commit_effect(
        &ledger,
        &early_dead.owner_lease,
        append_request(
            early_dead.header.run_id,
            "early-dead",
            EffectIntent::AlphaRuntimeV1,
            12,
        ),
        artifact_ref(0x72),
    );
    let no_routes = OutboxRouteRegistry::default();
    assert!(matches!(
        ledger.claim_next(mode(), &no_routes).unwrap(),
        ClaimNext::Rejected {
            state: OutboxState::DeadLettered,
            ..
        }
    ));
    let final_status = ledger
        .finalize_run_exit(
            mode(),
            &early_dead.owner_lease,
            RunExit::new(
                TerminalReason::Completed,
                object(json!({"late_exit": true})),
            ),
        )
        .unwrap();
    assert_eq!(final_status.final_outcome, Some(RunOutcome::DeadLettered));
    assert_eq!(
        final_status.exit_terminal_reason,
        Some(TerminalReason::Completed)
    );
}

#[test]
fn no_private_sentinel_or_raw_private_sha256_enters_any_ledger_column() {
    let (_directory, factory, ledger, _clock) = fixture(3);
    let created = create_run(&ledger);
    let run_id = created.header.run_id;
    let private_sentinel = "credential-free-private-sentinel-should-never-persist";
    let raw_private_sha256 = hex::encode(Sha256::digest(private_sentinel.as_bytes()));
    let keyed_fingerprint = [0xA5; 32];
    commit_none(
        &ledger,
        &created.owner_lease,
        AppendRequest::new(
            run_id,
            AppendKey::new("private-scan").unwrap(),
            AppendIdentity::new(
                object(json!({"tenant": "public-tenant", "subject": "public-subject"})),
                PrivateFingerprint::hmac_sha256_v1(
                    super::LedgerIdentityKeyId::new("test-ledger-key-v1").unwrap(),
                    7,
                    keyed_fingerprint,
                )
                .unwrap(),
            ),
            EffectIntent::None,
            object(json!({"public": "safe"})),
        ),
    );

    let connection = factory.open().unwrap();
    let mut observed_keyed_fingerprint = false;
    for table in [
        "state_meta",
        "session_runs",
        "session_run_events",
        "session_run_manifests",
        "session_run_append_reservations",
        "session_run_nonresumable_effects",
        "session_run_outbox",
    ] {
        let query = format!("SELECT * FROM {table}");
        let mut statement = connection.prepare(&query).unwrap();
        let column_count = statement.column_count();
        let mut rows = statement.query([]).unwrap();
        while let Some(row) = rows.next().unwrap() {
            for index in 0..column_count {
                let value = row.get_ref(index).unwrap();
                let bytes = match value {
                    ValueRef::Text(value) | ValueRef::Blob(value) => value,
                    _ => continue,
                };
                let text = String::from_utf8_lossy(bytes);
                assert!(
                    !text.contains(private_sentinel),
                    "private sentinel in {table}"
                );
                assert!(
                    !text.contains(&raw_private_sha256),
                    "raw private SHA-256 in {table}"
                );
                if text.contains(&hex::encode(keyed_fingerprint)) {
                    observed_keyed_fingerprint = true;
                }
            }
        }
    }
    assert!(
        observed_keyed_fingerprint,
        "keyed fingerprint metadata was not persisted"
    );
}

#[test]
fn dispatch_policy_binds_effect_manifest_to_exact_artifact_reference() {
    let event_id = EventId::new();
    let payload = object(json!({"mission": "public-work"}));
    let reference = artifact_ref(0x81);
    let first = DispatchPolicyV1::derive(
        event_id,
        EffectIntent::AlphaRuntimeV1,
        &payload,
        Some(&reference),
    )
    .unwrap();
    let second = DispatchPolicyV1::derive(
        event_id,
        EffectIntent::AlphaRuntimeV1,
        &payload,
        Some(&reference),
    )
    .unwrap();
    assert_eq!(first, second);
    assert_eq!(first.entries.len(), 1);
    assert_eq!(first.entries[0].sealed_artifact_ref, reference);
    assert!(
        DispatchPolicyV1::derive(event_id, EffectIntent::AlphaRuntimeV1, &payload, None).is_err()
    );
}

#[test]
fn manual_degradation_requires_the_owned_source_reservation() {
    let (_directory, _factory, ledger, _clock) = fixture(3);
    let created = create_owned_run(&ledger);
    let append_key = AppendKey::new("reserved-source-key").unwrap();
    let lease = reserve(
        &ledger,
        &created.owner_lease,
        AppendRequest::new(
            created.header.run_id,
            append_key.clone(),
            identity(0x5a),
            EffectIntent::AlphaRuntimeV1,
            object(json!({"request": "reserved"})),
        ),
    );
    let non_manual_mode = ResolvedExecutionMode {
        mode: ExecutionMode::Reviewed,
        source: ExecutionModeSource::Cli,
    };
    assert!(matches!(
        ledger.degrade_reserved_append(
            non_manual_mode,
            &created.owner_lease,
            &lease,
            ArtifactDegradationReason::ArtifactStoreUnavailable,
        ),
        Err(LedgerError::ManualArtifactDegradationRequiresManual { .. })
    ));
    assert_eq!(
        ledger.events_for_run(created.header.run_id).unwrap().len(),
        1,
        "non-manual degradation must not mutate its source reservation"
    );
    let forged_lease = AppendLease {
        event_id: EventId::new(),
        ..lease.clone()
    };
    assert!(matches!(
        ledger.degrade_reserved_append(
            mode(),
            &created.owner_lease,
            &forged_lease,
            ArtifactDegradationReason::ArtifactStoreUnavailable,
        ),
        Err(LedgerError::StaleAppendLease { .. })
    ));
    assert_eq!(
        ledger.events_for_run(created.header.run_id).unwrap().len(),
        1,
        "a forged lease must not emit a degradation audit"
    );

    let receipt = ledger
        .degrade_reserved_append(
            mode(),
            &created.owner_lease,
            &lease,
            ArtifactDegradationReason::ArtifactStoreUnavailable,
        )
        .expect("owned unpublished reservation can terminalize as manual degradation");
    assert_eq!(
        receipt.event_kind,
        SessionRunEventKind::ManualAppendDegradedAudit
    );
    assert!(receipt.outbox_ids.is_empty());
    assert!(receipt.sealed_artifact_ref.is_none());
    assert_eq!(
        receipt.artifact_degradation_reason,
        Some(ArtifactDegradationReason::ArtifactStoreUnavailable)
    );

    assert!(matches!(
        ledger.commit_reserved_append(mode(), &created.owner_lease, &lease, None),
        Err(LedgerError::StaleAppendLease { .. })
    ));
    let events = ledger.events_for_run(created.header.run_id).unwrap();
    assert_eq!(events.len(), 2);
    assert_ne!(events[1].append_key.as_ref(), Some(&append_key));
    assert_eq!(events[1].effect_intent, EffectIntent::None);
}

#[test]
fn local_one_shot_protocol_derivations_match_fixed_vectors() {
    let event_id: EventId = "00112233-4455-6677-8899-aabbccddeeff".parse().unwrap();
    let token = LeaseToken::from_blob((0_u8..16).collect()).unwrap();
    let idempotency_key = derive_nonresumable_idempotency_key(event_id).unwrap();
    let claim_binding = derive_nonresumable_claim_binding(&token).unwrap();
    let result_append_key =
        derive_nonresumable_result_append_key(event_id, &claim_binding).unwrap();
    assert_eq!(
        idempotency_key.as_str(),
        "fc9eb88c8aee68218431f24c09d66952196cf705e55ab2084c2b4bbcef460485"
    );
    assert_eq!(
        claim_binding.as_str(),
        "efc460569c17d90deb4aa7deeaecb4afe3a5a228976a0761178b4c2f16cd0ad5"
    );
    assert_eq!(
        result_append_key.as_str(),
        "178669bb58012d11d2a2d0098f737b3e0085970f287986bd99391aa6a8dc7195"
    );
}

#[tokio::test]
async fn nonresumable_effect_executes_once_and_requires_claim_bound_result() {
    let (_directory, _factory, ledger, _clock) = fixture(3);
    let created = create_owned_run(&ledger);
    let intent = commit_effect(
        &ledger,
        &created.owner_lease,
        append_request(
            created.header.run_id,
            "local-one-shot-intent",
            EffectIntent::LocalOneShotV1,
            0x91,
        ),
        artifact_ref(0x91),
    );
    assert!(intent.outbox_ids.is_empty());

    let certified = CertifiedEffectIntent::from_receipt_for_test(&intent).unwrap();
    let effect_lease = ledger
        .begin_nonresumable_effect(mode(), &created.owner_lease, &certified)
        .unwrap();
    let attempt =
        RunLedger::observed_nonresumable_attempt(effect_lease, "returned").unwrap();
    ledger.persist_nonresumable_result(mode(), &attempt).unwrap();

    let retry = ledger
        .begin_nonresumable_effect(mode(), &created.owner_lease, &certified)
        .expect_err("an executing one-shot must never be claimed a second time");
    assert!(matches!(
        retry,
        LedgerError::NonResumableEffectNotArmed {
            state: "executing",
            ..
        }
    ));

    let bound = RunLedger::bind_nonresumable_result_payload(&attempt).unwrap();
    let forged_result = commit_effect(
        &ledger,
        &created.owner_lease,
        AppendRequest::new(
            created.header.run_id,
            AppendKey::new("forged-preclaim-result").unwrap(),
            identity(0x93),
            EffectIntent::None,
            bound.clone(),
        ),
        artifact_ref(0x93),
    );
    assert!(matches!(
        ledger
            .complete_nonresumable_effect(mode(), &attempt, forged_result.event_id)
            .expect_err("wrong append key must not satisfy the one-shot claim"),
        LedgerError::NonResumableEffectResultMismatch { .. }
    ));
    let result_receipt = commit_effect(
        &ledger,
        &created.owner_lease,
        AppendRequest::new(
            created.header.run_id,
            attempt.result_append_key().clone(),
            identity(0x92),
            EffectIntent::None,
            bound,
        ),
        artifact_ref(0x92),
    );
    let completed = ledger
        .complete_nonresumable_effect(mode(), &attempt, result_receipt.event_id)
        .unwrap();
    assert_eq!(completed.state, NonResumableEffectState::Completed);
    assert_eq!(completed.result_event_id, Some(result_receipt.event_id));
    assert_eq!(
        ledger
            .complete_nonresumable_effect(mode(), &attempt, result_receipt.event_id)
            .unwrap(),
        completed
    );

    let status = ledger
        .finalize_run_exit(
            mode(),
            &created.owner_lease,
            RunExit::new(TerminalReason::Completed, object(json!({"done": true}))),
        )
        .unwrap();
    assert_eq!(status.final_outcome, Some(RunOutcome::Success));
    assert!(ledger
        .outbox_for_run(created.header.run_id)
        .unwrap()
        .is_empty());
    ledger.verify_run_chain(created.header.run_id).unwrap();
}

#[tokio::test]
async fn one_shot_completion_rejects_payload_mutated_after_binding() {
    let (_directory, _factory, ledger, clock) = fixture(3);
    let created = create_owned_run(&ledger);
    let intent = commit_effect(
        &ledger,
        &created.owner_lease,
        append_request(
            created.header.run_id,
            "mutated-result-one-shot",
            EffectIntent::LocalOneShotV1,
            0x94,
        ),
        artifact_ref(0x94),
    );
    let attempt = begin_observed_one_shot(&ledger, &created.owner_lease, &intent);
    let bound = RunLedger::bind_nonresumable_result_payload(&attempt).unwrap();
    let mut changed_value = bound.as_value().unwrap();
    changed_value["payload"] = json!({"substituted": true});
    let changed = CanonicalJson::from_value(&changed_value).unwrap();
    let forged_result = commit_effect(
        &ledger,
        &created.owner_lease,
        AppendRequest::new(
            created.header.run_id,
            attempt.result_append_key().clone(),
            identity(0x95),
            EffectIntent::None,
            changed,
        ),
        artifact_ref(0x95),
    );
    assert!(matches!(
        ledger
            .complete_nonresumable_effect(mode(), &attempt, forged_result.event_id)
            .expect_err("post-execution result substitution must fail closed"),
        LedgerError::NonResumableEffectResultMismatch { .. }
    ));
    let effect = ledger
        .nonresumable_effect(intent.event_id)
        .unwrap()
        .unwrap();
    assert_eq!(effect.state, NonResumableEffectState::Executing);
    assert!(effect.result_payload_digest.is_some());
    clock.advance(30_001);
    let status = match ledger.recover_stale_run(created.header.run_id).unwrap() {
        StaleRunRecovery::Recovered(status) => status,
        other => panic!("expected stale recovery, got {other:?}"),
    };
    assert_eq!(
        status.exit_terminal_reason,
        Some(TerminalReason::EffectNonResumableUnknown)
    );
    assert_eq!(
        ledger
            .nonresumable_effect(intent.event_id)
            .unwrap()
            .unwrap()
            .state,
        NonResumableEffectState::UnknownNonResumable
    );
    ledger.verify_run_chain(created.header.run_id).unwrap();
}

#[tokio::test]
async fn run_chain_rejects_status_tamper_that_masks_abandoned_one_shot() {
    let (_directory, factory, ledger, _clock) = fixture(3);
    let created = create_owned_run(&ledger);
    let intent = commit_effect(
        &ledger,
        &created.owner_lease,
        append_request(
            created.header.run_id,
            "status-binding-one-shot",
            EffectIntent::LocalOneShotV1,
            0x96,
        ),
        artifact_ref(0x96),
    );
    let attempt = begin_observed_one_shot(&ledger, &created.owner_lease, &intent);
    let bound = RunLedger::bind_nonresumable_result_payload(&attempt).unwrap();
    let result = commit_effect(
        &ledger,
        &created.owner_lease,
        AppendRequest::new(
            created.header.run_id,
            attempt.result_append_key().clone(),
            identity(0x97),
            EffectIntent::None,
            bound,
        ),
        artifact_ref(0x97),
    );
    ledger
        .complete_nonresumable_effect(mode(), &attempt, result.event_id)
        .unwrap();
    ledger
        .finalize_run_exit(
            mode(),
            &created.owner_lease,
            RunExit::new(TerminalReason::Completed, object(json!({"done": true}))),
        )
        .unwrap();
    ledger.verify_run_chain(created.header.run_id).unwrap();

    let connection = factory.open().unwrap();
    let exit_event_id = ledger
        .run_status(created.header.run_id)
        .unwrap()
        .exit_event_id
        .unwrap();
    connection
        .execute(
            "UPDATE session_runs SET exit_event_id = NULL WHERE run_id = ?1",
            [created.header.run_id.to_string()],
        )
        .unwrap();
    assert!(ledger.verify_run_chain(created.header.run_id).is_err());
    connection
        .execute(
            "UPDATE session_runs SET exit_event_id = ?1 WHERE run_id = ?2",
            [exit_event_id.to_string(), created.header.run_id.to_string()],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE session_run_nonresumable_effects
             SET state = 'abandoned', claim_token = NULL, claimed_at_ms = NULL,
                 result_payload_digest = NULL, result_event_id = NULL
             WHERE event_id = ?1",
            [intent.event_id.to_string()],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE session_runs
             SET exit_terminal_reason = 'execution_failure',
                 candidate_outcome = 'failure', final_outcome = 'failure'
             WHERE run_id = ?1",
            [created.header.run_id.to_string()],
        )
        .unwrap();
    assert!(matches!(
        ledger
            .verify_run_chain(created.header.run_id)
            .expect_err("hashed run-exit event must bind the authoritative status"),
        LedgerError::IntegrityViolation(_)
    ));
}

#[test]
fn current_owner_heartbeat_remains_live_without_recovery_or_lease_theft() {
    let (_directory, _factory, ledger, _clock) = fixture(3);
    let created = create_owned_run(&ledger);
    let heartbeated = ledger
        .heartbeat_run_owner(mode(), &created.owner_lease)
        .expect("current owner heartbeat");
    assert!(matches!(
        ledger.recover_stale_run(created.header.run_id).unwrap(),
        StaleRunRecovery::Live
    ));
    let events = ledger.events_for_run(created.header.run_id).unwrap();
    assert_eq!(events.len(), 1, "live recovery must append no exit or audit");
    assert!(events
        .iter()
        .all(|event| event.kind == SessionRunEventKind::RunStart));
    assert!(ledger.outbox_for_run(created.header.run_id).unwrap().is_empty());
    let continued = ledger
        .heartbeat_run_owner(mode(), &heartbeated)
        .expect("live recovery must not steal the original owner lease");
    assert_eq!(continued.owner_id, created.owner_lease.owner_id);
    let status = ledger
        .finalize_run_exit(
            mode(),
            &continued,
            RunExit::new(TerminalReason::Completed, object(json!({"finished": true}))),
        )
        .expect("original owner can still finish after live recovery check");
    assert_eq!(status.exit_terminal_reason, Some(TerminalReason::Completed));
    ledger.verify_run_chain(created.header.run_id).unwrap();
}

#[tokio::test]
async fn stale_executing_one_shot_becomes_unknown_without_replay() {
    let (_directory, _factory, ledger, clock) = fixture(3);
    let created = create_owned_run(&ledger);
    let intent = commit_effect(
        &ledger,
        &created.owner_lease,
        append_request(
            created.header.run_id,
            "crashing-local-one-shot",
            EffectIntent::LocalOneShotV1,
            0xa1,
        ),
        artifact_ref(0xa1),
    );
    let certified = CertifiedEffectIntent::from_receipt_for_test(&intent).unwrap();
    let _effect_lease = ledger
        .begin_nonresumable_effect(mode(), &created.owner_lease, &certified)
        .unwrap();
    clock.advance(30_001);

    let status = match ledger.recover_stale_run(created.header.run_id).unwrap() {
        StaleRunRecovery::Recovered(status) => status,
        other => panic!("expected stale recovery, got {other:?}"),
    };
    assert_eq!(
        status.exit_terminal_reason,
        Some(TerminalReason::EffectNonResumableUnknown)
    );
    assert_eq!(status.final_outcome, Some(RunOutcome::Failure));
    let exit = ledger
        .events_for_run(created.header.run_id)
        .unwrap()
        .into_iter()
        .find(|event| event.kind == SessionRunEventKind::RunExit)
        .expect("stale recovery exit");
    let binding = exit.public_payload.as_value().unwrap();
    assert_eq!(
        binding["terminal_reason"],
        TerminalReason::EffectNonResumableUnknown.as_str()
    );
    assert_eq!(
        binding["payload"]["causal_terminal_reason"],
        TerminalReason::ProcessDeath.as_str()
    );
    assert_eq!(
        ledger
            .nonresumable_effect(intent.event_id)
            .unwrap()
            .unwrap()
            .state,
        NonResumableEffectState::UnknownNonResumable
    );
    ledger.verify_run_chain(created.header.run_id).unwrap();
}

#[test]
fn manual_unknown_one_shot_does_not_claim_process_death() {
    let (_directory, _factory, ledger, _clock) = fixture(3);
    let created = create_owned_run(&ledger);
    let intent = commit_effect(
        &ledger,
        &created.owner_lease,
        append_request(
            created.header.run_id,
            "manual-unknown-local-one-shot",
            EffectIntent::LocalOneShotV1,
            0xa2,
        ),
        artifact_ref(0xa2),
    );
    let certified = CertifiedEffectIntent::from_receipt_for_test(&intent).unwrap();
    let _effect_lease = ledger
        .begin_nonresumable_effect(mode(), &created.owner_lease, &certified)
        .unwrap();
    let status = ledger
        .finalize_run_exit(
            mode(),
            &created.owner_lease,
            RunExit::new(
                TerminalReason::ExecutionFailure,
                object(json!({"manual_failure": true})),
            ),
        )
        .expect("manual exit terminalizes an executing one-shot as unknown");
    assert_eq!(
        status.exit_terminal_reason,
        Some(TerminalReason::EffectNonResumableUnknown)
    );
    let exit = ledger
        .events_for_run(created.header.run_id)
        .unwrap()
        .into_iter()
        .find(|event| event.kind == SessionRunEventKind::RunExit)
        .expect("manual exit");
    let binding = exit.public_payload.as_value().unwrap();
    assert_eq!(
        binding["terminal_reason"],
        TerminalReason::EffectNonResumableUnknown.as_str()
    );
    assert!(
        binding["payload"]
            .get("causal_terminal_reason")
            .is_none(),
        "manual unknown must not claim a stale-owner process death"
    );
    ledger.verify_run_chain(created.header.run_id).unwrap();
}

#[tokio::test]
async fn stale_recovery_recognizes_durable_one_shot_result_before_completion_link() {
    let (_directory, _factory, ledger, clock) = fixture(3);
    let created = create_owned_run(&ledger);
    let intent = commit_effect(
        &ledger,
        &created.owner_lease,
        append_request(
            created.header.run_id,
            "recoverable-local-one-shot",
            EffectIntent::LocalOneShotV1,
            0xaa,
        ),
        artifact_ref(0xaa),
    );
    let attempt = begin_observed_one_shot(&ledger, &created.owner_lease, &intent);
    let payload = RunLedger::bind_nonresumable_result_payload(&attempt).unwrap();
    let result_receipt = commit_effect(
        &ledger,
        &created.owner_lease,
        AppendRequest::new(
            created.header.run_id,
            attempt.result_append_key().clone(),
            identity(0xab),
            EffectIntent::None,
            payload,
        ),
        artifact_ref(0xab),
    );
    assert_eq!(
        ledger
            .nonresumable_effect(intent.event_id)
            .unwrap()
            .unwrap()
            .result_event_id,
        None
    );
    clock.advance(30_001);

    let status = match ledger.recover_stale_run(created.header.run_id).unwrap() {
        StaleRunRecovery::Recovered(status) => status,
        other => panic!("expected stale recovery, got {other:?}"),
    };
    assert_eq!(
        status.exit_terminal_reason,
        Some(TerminalReason::ProcessDeath)
    );
    let effect = ledger
        .nonresumable_effect(intent.event_id)
        .unwrap()
        .unwrap();
    assert_eq!(effect.state, NonResumableEffectState::Completed);
    assert_eq!(effect.result_event_id, Some(result_receipt.event_id));
    ledger.verify_run_chain(created.header.run_id).unwrap();
}

#[test]
fn successful_exit_rejects_unclaimed_one_shot_and_reserved_unknown_reason() {
    let (_directory, _factory, ledger, _clock) = fixture(3);
    let created = create_owned_run(&ledger);
    let intent = commit_effect(
        &ledger,
        &created.owner_lease,
        append_request(
            created.header.run_id,
            "unclaimed-local-one-shot",
            EffectIntent::LocalOneShotV1,
            0xb1,
        ),
        artifact_ref(0xb1),
    );
    let error = ledger
        .finalize_run_exit(
            mode(),
            &created.owner_lease,
            RunExit::new(TerminalReason::Completed, object(json!({"done": true}))),
        )
        .expect_err("success must not abandon an intended local effect");
    assert!(matches!(
        error,
        LedgerError::UnclaimedNonResumableEffects { count: 1, .. }
    ));
    assert_eq!(
        ledger
            .nonresumable_effect(intent.event_id)
            .unwrap()
            .unwrap()
            .state,
        NonResumableEffectState::Armed
    );

    let error = ledger
        .finalize_run_exit(
            mode(),
            &created.owner_lease,
            RunExit::new(
                TerminalReason::EffectNonResumableUnknown,
                object(json!({"forged": true})),
            ),
        )
        .expect_err("unknown terminal reason is derived, not caller asserted");
    assert!(matches!(
        error,
        LedgerError::EffectNonResumableUnknownRequiresEffect { .. }
    ));

    let status = ledger
        .finalize_run_exit(
            mode(),
            &created.owner_lease,
            RunExit::new(
                TerminalReason::ExecutionFailure,
                object(json!({"failed_before_effect": true})),
            ),
        )
        .unwrap();
    assert_eq!(status.final_outcome, Some(RunOutcome::Failure));
    assert_eq!(
        ledger
            .nonresumable_effect(intent.event_id)
            .unwrap()
            .unwrap()
            .state,
        NonResumableEffectState::Abandoned
    );
    ledger.verify_run_chain(created.header.run_id).unwrap();
}
