use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use hermes_core::execution_mode::{ExecutionMode, ExecutionModeSource, ResolvedExecutionMode};
use serde_json::{json, Value};
use tempfile::TempDir;

use crate::run_ledger::{
    ActionId, AppendIdentity, AppendKey, AppendLease, AppendReceipt, AppendRequest,
    AppendReservation, BranchId, CanonicalJson, CertifiedOutboxDestinationHandler, ClaimNext,
    DeliveryCompletion, DeliveryEvidence, DeliveryFailureReason, Destination, DigestAbsence,
    DurableDeliveryReceipt, DurableOutboxDestinationHandler, EffectIntent, LedgerClock,
    LedgerConnectionFactory, LedgerError, LedgerIdentityKeyId, ModelId, ObjectiveId, OutboxLease,
    OutboxRouteRegistry, PrivateFingerprint, ProvenanceDigest, ProviderId, ReplayAppendContract,
    ReplayArtifactFidelity, ReplayOperationKind, ReplayOperationPhase, RunExit, RunId, RunLedger,
    RunLedgerConfig, RunOwnerId, RunOwnerLease, RunProvenance, RunStart, SealedArtifactRef,
    SessionId, Sha256Digest, TaskId, TenantId, TerminalReason,
};
use crate::run_runtime::CertifiedEffectIntent;
use crate::sealed_artifact;
use crate::deterministic_replay::{
    reconstruct_verified_run, verify_branch_replay_plan, DeterministicReplayError,
    ExactArtifactReadError, ExactArtifactReader, ReplayArtifactReference, ReplayBranchBoundary,
    VerifiedReplayArtifact, VerifiedRunReplayPlan,
};

use super::{
    canonical_value_bytes, classify_run_export, export_path, export_run,
    export_run_interrupting_before_rename, sha256_hex, verify_run_export,
    verify_run_export_bytes, verify_run_export_replay_plan, verify_run_export_replay_plan_bytes,
    RunExportClassification, RunExportError, RUN_EXPORT_DIGEST_ALGORITHM,
    RUN_EXPORT_SCHEMA_VERSION,
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
        _idempotency_key: &crate::run_ledger::DownstreamIdempotencyKey,
        _public_payload: &CanonicalJson,
    ) -> Result<DurableDeliveryReceipt, DeliveryFailureReason> {
        DurableDeliveryReceipt::new("export-test-receipt")
            .map_err(|_| DeliveryFailureReason::HandlerRejected)
    }
}

fn mode() -> ResolvedExecutionMode {
    ResolvedExecutionMode {
        mode: ExecutionMode::Manual,
        source: ExecutionModeSource::BuiltInDefault,
    }
}

fn object(value: Value) -> CanonicalJson {
    CanonicalJson::from_value(&value).expect("canonical JSON")
}

fn digest(byte: u8) -> Sha256Digest {
    Sha256Digest::new(hex::encode([byte; 32])).expect("digest")
}

fn provenance() -> RunProvenance {
    RunProvenance::new(
        SessionId::new("export-session").expect("session"),
        None,
        BranchId::new("export-branch").expect("branch"),
        TenantId::new("export-tenant").expect("tenant"),
        ActionId::new("export-action").expect("action"),
        ObjectiveId::new("export-objective").expect("objective"),
        TaskId::new("export-task").expect("task"),
        ModelId::new("export-model").expect("model"),
        ProviderId::new("export-provider").expect("provider"),
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
        object(json!({"subject":"public-subject","tenant":"public-tenant"})),
        PrivateFingerprint::hmac_sha256_v1(
            LedgerIdentityKeyId::new("export-ledger-key-v1").expect("key id"),
            1,
            [byte; 32],
        )
        .expect("private fingerprint"),
    )
}

fn artifact_ref(byte: u8) -> SealedArtifactRef {
    SealedArtifactRef::try_from(sealed_artifact::SealedArtifactRef {
        envelope_digest: hex::encode([byte; 32]),
        key_id: "export-artifact-key-v1".to_string(),
        schema_version: sealed_artifact::SEALED_ARTIFACT_SCHEMA_VERSION,
        encryption_algorithm: sealed_artifact::ARTIFACT_ENCRYPTION_ALGORITHM.to_string(),
        canonicalization_algorithm: sealed_artifact::ARTIFACT_CANONICALIZATION.to_string(),
        canonicalization_version: sealed_artifact::ARTIFACT_CANONICALIZATION_VERSION,
        redactor_version: "export-redactor-v1".to_string(),
    })
    .expect("artifact reference")
}

fn delivery_evidence(
    lease: &OutboxLease,
    completion: &DeliveryCompletion,
    byte: u8,
) -> DeliveryEvidence {
    DeliveryEvidence::new(
        AppendIdentity::new(
            CanonicalJson::from_value(&crate::run_ledger::delivery_result_evidence_identity(
                lease, completion,
            ))
            .expect("delivery evidence identity"),
            PrivateFingerprint::hmac_sha256_v1(
                LedgerIdentityKeyId::new("export-delivery-evidence-key-v1").expect("evidence key"),
                1,
                [byte; 32],
            )
            .expect("delivery evidence fingerprint"),
        ),
        artifact_ref(byte),
    )
}

fn fixture() -> (TempDir, RunLedger) {
    let directory = tempfile::tempdir().expect("ledger directory");
    let factory = LedgerConnectionFactory::new(directory.path().join("sessions.db"));
    factory
        .migrate_and_cut_over_v0_to_v1()
        .expect("migrate ledger");
    let clock = Arc::new(TestClock::new(1_700_000_000_000));
    let ledger = RunLedger::open_with_clock(
        factory,
        RunLedgerConfig {
            append_lease_duration: Duration::from_secs(10),
            outbox_lease_duration: Duration::from_secs(10),
            owner_lease_duration: Duration::from_secs(10),
            max_delivery_attempts: 3,
        },
        clock,
    )
    .expect("open ledger");
    (directory, ledger)
}

fn create_run(ledger: &RunLedger) -> (RunId, RunOwnerLease) {
    let run_id = RunId::new();
    let created = ledger
        .create_run_and_start(
            mode(),
            RunOwnerId::new("run-export-owner").expect("owner"),
            RunStart::new(run_id, provenance(), object(json!({"start":"ok"}))),
        )
        .expect("create run");
    assert_eq!(created.header.run_id, run_id);
    (run_id, created.owner_lease)
}

/// A RunStart may carry its exact sealed artifact identity without being an
/// append: it is the only non-append producer of this identity shape.
fn create_exact_start_run(ledger: &RunLedger) -> (RunId, RunOwnerLease) {
    let run_id = RunId::new();
    let created = ledger
        .create_run_and_start_with_sealed_artifact(
            mode(),
            RunOwnerId::new("exact-start-export-owner").expect("owner"),
            RunStart::new(run_id, provenance(), object(json!({"start":"exact"}))),
            identity(6),
            artifact_ref(6),
        )
        .expect("create exact-start run");
    assert_eq!(created.header.run_id, run_id);
    (run_id, created.owner_lease)
}

fn append_request(
    run_id: RunId,
    key: &str,
    intent: EffectIntent,
    fingerprint: u8,
) -> AppendRequest {
    AppendRequest::new(
        run_id,
        AppendKey::new(key).expect("append key"),
        identity(fingerprint),
        intent,
        object(json!({"request":"public"})),
    )
}

fn reserve(ledger: &RunLedger, owner_lease: &RunOwnerLease, request: AppendRequest) -> AppendLease {
    match ledger
        .reserve_append(mode(), owner_lease, request)
        .expect("reserve append")
    {
        AppendReservation::Reserved(lease) => lease,
        other => panic!("unexpected reservation: {other:?}"),
    }
}

fn commit_none(ledger: &RunLedger, owner_lease: &RunOwnerLease, request: AppendRequest) {
    let lease = reserve(ledger, owner_lease, request);
    ledger
        .commit_reserved_append(mode(), owner_lease, &lease, None)
        .expect("commit non-effect append");
}

fn commit_effect(
    ledger: &RunLedger,
    owner_lease: &RunOwnerLease,
    request: AppendRequest,
) -> AppendReceipt {
    commit_effect_with_artifact(ledger, owner_lease, request, 9)
}

fn commit_effect_with_artifact(
    ledger: &RunLedger,
    owner_lease: &RunOwnerLease,
    request: AppendRequest,
    artifact_byte: u8,
) -> AppendReceipt {
    let lease = reserve(ledger, owner_lease, request);
    let reference = ledger
        .publish_sealed_ref(mode(), owner_lease, &lease, artifact_ref(artifact_byte))
        .expect("publish sealed artifact reference");
    ledger
        .commit_reserved_append(mode(), owner_lease, &lease, Some(&reference))
        .expect("commit effect append")
}

fn routes() -> OutboxRouteRegistry {
    let mut routes = OutboxRouteRegistry::default();
    routes
        .register_certified(
            CertifiedOutboxDestinationHandler::alpha_runtime_v1(Arc::new(DedupeHandler))
                .expect("certified route"),
        )
        .expect("register route");
    routes
}

fn complete_multi_event_run(ledger: &RunLedger) -> RunId {
    let (run_id, owner_lease) = create_run(ledger);
    commit_none(
        ledger,
        &owner_lease,
        append_request(run_id, "export-none", EffectIntent::None, 1),
    );
    commit_effect(
        ledger,
        &owner_lease,
        append_request(run_id, "export-effect", EffectIntent::AlphaRuntimeV1, 2),
    );
    let lease = match ledger.claim_next(mode(), &routes()).expect("claim outbox") {
        ClaimNext::Claimed(lease) => lease,
        other => panic!("expected claimed outbox: {other:?}"),
    };
    let completion = DeliveryCompletion::Acknowledged(
        DurableDeliveryReceipt::new("export-test-receipt").expect("receipt"),
    );
    let evidence = delivery_evidence(&lease, &completion, 8);
    ledger
        .complete_delivery_with_evidence(mode(), &lease, completion, evidence)
        .expect("ack delivery with sealed evidence");
    ledger
        .finalize_run_exit(
            mode(),
            &owner_lease,
            RunExit::new(TerminalReason::Completed, object(json!({"exit":"ok"}))),
        )
        .expect("finalize run");
    run_id
}

fn export_completed_run() -> (TempDir, RunLedger, RunId, PathBuf, Vec<u8>) {
    let (directory, ledger) = fixture();
    let run_id = complete_multi_event_run(&ledger);
    let export_dir = directory.path().join("exports");
    let receipt = export_run(&ledger, &export_dir, run_id).expect("export complete run");
    let bytes = std::fs::read(&receipt.path).expect("export bytes");
    (directory, ledger, run_id, receipt.path, bytes)
}

fn recanonicalize_with_digest(value: &mut Value) {
    value
        .as_object_mut()
        .expect("envelope object")
        .remove("digest");
    let unsigned = canonical_value_bytes(value).expect("canonical unsigned envelope");
    value.as_object_mut().expect("envelope object").insert(
        "digest".to_string(),
        json!({"algorithm":RUN_EXPORT_DIGEST_ALGORITHM,"value":sha256_hex(&unsigned)}),
    );
}

fn canonical_document(value: &Value) -> Vec<u8> {
    canonical_value_bytes(value).expect("canonical document")
}

fn parsed_export(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).expect("parse export")
}

#[test]
fn exports_complete_terminal_multi_event_run() {
    let (_directory, _ledger, run_id, path, bytes) = export_completed_run();
    let verified = verify_run_export(&path).expect("verify published export");
    assert_eq!(verified.run_id, run_id);
    assert_eq!(verified.event_count, 5);
    assert_eq!(verified.outbox_count, 1);

    let document = parsed_export(&bytes);
    assert_eq!(document["complete"], Value::Bool(true));
    assert_eq!(document["total_event_count"], Value::from(5));
    assert_eq!(document["events"].as_array().expect("events").len(), 5);
    assert_eq!(
        document["manifests"].as_array().expect("manifests").len(),
        5
    );
    assert_eq!(document["outbox"].as_array().expect("outbox").len(), 1);
    assert_eq!(
        document["header"]["owner_id"],
        Value::String("run-export-owner".to_string())
    );
}

#[test]
fn exports_terminal_non_tool_run_with_exact_start_identity() {
    let (directory, ledger) = fixture();
    let (run_id, owner_lease) = create_exact_start_run(&ledger);
    ledger
        .finalize_run_exit(
            mode(),
            &owner_lease,
            RunExit::new(TerminalReason::Completed, object(json!({"exit":"ok"}))),
        )
        .expect("finalize exact-start run");

    let export_dir = directory.path().join("exports");
    let receipt = export_run(&ledger, &export_dir, run_id).expect("export exact-start run");
    let bytes = std::fs::read(&receipt.path).expect("read exact-start export");
    let document = parsed_export(&bytes);
    let start = &document["events"][0];
    assert_eq!(start["kind"], "run_start");
    assert_eq!(start["append_key"], Value::Null);
    assert!(start["public_identity"].is_object());
    assert!(start["private_fingerprint"].is_object());
    assert!(start["sealed_artifact_ref"].is_object());
    verify_run_export(&receipt.path).expect("verify exact-start export");
    let mut partial_identity = document;
    partial_identity["events"][0]
        .as_object_mut()
        .expect("run-start event object")
        .remove("private_fingerprint");
    recanonicalize_with_digest(&mut partial_identity);
    assert!(
        verify_run_export_bytes(&canonical_document(&partial_identity)).is_err(),
        "a RunStart exact binding must retain its complete identity/reference trio"
    );
}

#[test]
fn v3_export_binds_exact_replay_status_and_keeps_v2_v1_readable() {
    let (directory, ledger) = fixture();
    let (run_id, owner_lease) = create_exact_start_run(&ledger);
    ledger
        .finalize_run_exit(
            mode(),
            &owner_lease,
            RunExit::new(TerminalReason::Completed, object(json!({"exit":"exact"}))),
        )
        .expect("finalize exact replay fixture");
    let receipt = export_run(&ledger, directory.path().join("exact-replay-export"), run_id)
        .expect("export exact replay fixture");
    let bytes = std::fs::read(receipt.path).expect("read exact replay export");
    let document = parsed_export(&bytes);
    assert_eq!(
        document["schema_version"],
        Value::from(RUN_EXPORT_SCHEMA_VERSION)
    );
    assert!(document["replay_obligations"].is_array());
    assert_eq!(document["status"]["exact_replay"]["state"], "available");
    assert_eq!(document["status"]["exact_replay"]["reason"], Value::Null);
    assert_eq!(
        document["status"]["exact_replay"]["exact_replay"],
        Value::Bool(true)
    );

    let mut tampered = document.clone();
    tampered["status"]["exact_replay"]["state"] = Value::String("retired".to_string());
    tampered["status"]["exact_replay"]["reason"] = Value::String("retention_deleted".to_string());
    tampered["status"]["exact_replay"]["exact_replay"] = Value::Bool(false);
    recanonicalize_with_digest(&mut tampered);
    assert!(verify_run_export_bytes(&canonical_document(&tampered)).is_err());

    let mut v2 = document.clone();
    v2["schema_version"] = Value::from(2);
    v2.as_object_mut()
        .expect("v2 envelope object")
        .remove("replay_obligations");
    recanonicalize_with_digest(&mut v2);
    verify_run_export_bytes(&canonical_document(&v2)).expect("v2 export remains readable");
    assert!(matches!(
        verify_run_export_replay_plan_bytes(&canonical_document(&v2)),
        Err(RunExportError::IncompleteReplayEvidence(_))
    ));

    let mut v1 = v2;
    v1["schema_version"] = Value::from(1);
    v1["status"]
        .as_object_mut()
        .expect("v1 status object")
        .remove("exact_replay");
    recanonicalize_with_digest(&mut v1);
    verify_run_export_bytes(&canonical_document(&v1)).expect("v1 export remains readable");
    assert!(matches!(
        verify_run_export_replay_plan_bytes(&canonical_document(&v1)),
        Err(RunExportError::IncompleteReplayEvidence(_))
    ));
}

#[test]
fn rejects_nonterminal_run() {
    let (directory, ledger) = fixture();
    let (run_id, _owner_lease) = create_run(&ledger);
    let error = export_run(&ledger, directory.path().join("exports"), run_id)
        .expect_err("open run cannot be exported");
    assert!(matches!(error, RunExportError::NonTerminalRun { .. }));
}

#[test]
fn publication_is_atomic_and_idempotent() {
    let (directory, ledger) = fixture();
    let run_id = complete_multi_event_run(&ledger);
    let directory = directory.path().join("exports");
    let first = export_run(&ledger, &directory, run_id).expect("first export");
    let first_bytes = std::fs::read(&first.path).expect("first bytes");
    let second = export_run(&ledger, &directory, run_id).expect("idempotent export");
    assert!(first.published);
    assert!(!second.published);
    assert_eq!(first.path, second.path);
    assert_eq!(first.digest, second.digest);
    assert_eq!(
        std::fs::read(second.path).expect("second bytes"),
        first_bytes
    );
}

#[test]
fn rejects_conflicting_existing_export() {
    let (directory, ledger) = fixture();
    let run_id = complete_multi_event_run(&ledger);
    let export_dir = directory.path().join("exports");
    std::fs::create_dir(&export_dir).expect("create export directory");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&export_dir, std::fs::Permissions::from_mode(0o700))
            .expect("private directory");
    }
    let path = export_path(&export_dir, run_id);
    std::fs::write(&path, b"different immutable content").expect("conflicting file");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("private file");
    }
    let error = export_run(&ledger, &export_dir, run_id).expect_err("conflict must fail closed");
    assert!(matches!(error, RunExportError::PublicationConflict { .. }));
}

#[test]
fn interruption_before_rename_leaves_no_partial_export() {
    let (directory, ledger) = fixture();
    let run_id = complete_multi_event_run(&ledger);
    let export_dir = directory.path().join("exports");
    let path = export_path(&export_dir, run_id);
    let error = export_run_interrupting_before_rename(&ledger, &export_dir, run_id)
        .expect_err("injected interruption");
    assert!(matches!(error, RunExportError::TestInterruptedBeforeRename));
    assert!(
        !path.exists(),
        "interrupted publication exposed a final file"
    );
    assert!(
        std::fs::read_dir(&export_dir)
            .expect("export directory")
            .next()
            .is_none(),
        "interrupted publication left a temporary file"
    );
    let recovered = export_run(&ledger, &export_dir, run_id).expect("retry export");
    assert!(recovered.published);
    assert_eq!(verify_run_export(&recovered.path).unwrap().run_id, run_id);
}

#[test]
fn rejects_last_row_truncation() {
    let (_directory, _ledger, _run_id, _path, bytes) = export_completed_run();
    let truncated = &bytes[..bytes.len() - 1];
    assert!(verify_run_export_bytes(truncated).is_err());
}

#[test]
fn rejects_middle_event_deletion() {
    let (_directory, _ledger, _run_id, _path, bytes) = export_completed_run();
    let mut document = parsed_export(&bytes);
    document["events"].as_array_mut().expect("events").remove(2);
    recanonicalize_with_digest(&mut document);
    assert!(verify_run_export_bytes(&canonical_document(&document)).is_err());
}

#[test]
fn rejects_event_reordering() {
    let (_directory, _ledger, _run_id, _path, bytes) = export_completed_run();
    let mut document = parsed_export(&bytes);
    document["events"]
        .as_array_mut()
        .expect("events")
        .swap(1, 2);
    recanonicalize_with_digest(&mut document);
    assert!(verify_run_export_bytes(&canonical_document(&document)).is_err());
}

#[test]
fn rejects_event_payload_tamper() {
    let (_directory, _ledger, _run_id, _path, bytes) = export_completed_run();
    let mut document = parsed_export(&bytes);
    document["events"][1]["public_payload"] = json!({"request":"tampered"});
    recanonicalize_with_digest(&mut document);
    assert!(verify_run_export_bytes(&canonical_document(&document)).is_err());
}

#[test]
fn rejects_rehashed_header_and_terminal_status_tamper() {
    let (_directory, _ledger, _run_id, _path, bytes) = export_completed_run();

    let mut started_at = parsed_export(&bytes);
    started_at["header"]["started_at_ms"] = Value::from(
        started_at["header"]["started_at_ms"]
            .as_i64()
            .expect("started_at_ms")
            + 1,
    );
    recanonicalize_with_digest(&mut started_at);
    assert!(verify_run_export_bytes(&canonical_document(&started_at)).is_err());

    let mut owner = parsed_export(&bytes);
    owner["header"]["owner_id"] = Value::String("tampered-owner".to_string());
    recanonicalize_with_digest(&mut owner);
    assert!(verify_run_export_bytes(&canonical_document(&owner)).is_err());

    let mut mode = parsed_export(&bytes);
    mode["header"]["execution_mode"]["mode"] = Value::String("reviewed".to_string());
    recanonicalize_with_digest(&mut mode);
    assert!(verify_run_export_bytes(&canonical_document(&mode)).is_err());

    let mut terminal = parsed_export(&bytes);
    terminal["status"]["exit_terminal_reason"] = Value::String("execution_failure".to_string());
    terminal["status"]["candidate_outcome"] = Value::String("failure".to_string());
    terminal["status"]["final_outcome"] = Value::String("failure".to_string());
    recanonicalize_with_digest(&mut terminal);
    assert!(verify_run_export_bytes(&canonical_document(&terminal)).is_err());
}

#[test]
fn rejects_event_hash_tamper() {
    let (_directory, _ledger, _run_id, _path, bytes) = export_completed_run();
    let mut document = parsed_export(&bytes);
    document["events"][1]["event_hash"] = Value::String("0".repeat(64));
    recanonicalize_with_digest(&mut document);
    assert!(verify_run_export_bytes(&canonical_document(&document)).is_err());
}

#[test]
fn rejects_unknown_schema_and_canonicalization_versions() {
    let (_directory, _ledger, _run_id, _path, bytes) = export_completed_run();

    let mut unknown_schema = parsed_export(&bytes);
    unknown_schema["schema_version"] = Value::from(RUN_EXPORT_SCHEMA_VERSION + 1);
    recanonicalize_with_digest(&mut unknown_schema);
    assert!(matches!(
        verify_run_export_bytes(&canonical_document(&unknown_schema)),
        Err(RunExportError::UnsupportedSchema { .. })
    ));

    let mut unknown_canonicalization = parsed_export(&bytes);
    unknown_canonicalization["canonicalization_version"] = Value::from(2);
    recanonicalize_with_digest(&mut unknown_canonicalization);
    assert!(matches!(
        verify_run_export_bytes(&canonical_document(&unknown_canonicalization)),
        Err(RunExportError::UnsupportedCanonicalization { .. })
    ));
}

#[test]
fn classifies_legacy_v0_jsonl_without_v1_verification() {
    let legacy = concat!(
        "{\"event_hash\":\"0123456789abcdef\",\"prev_hash\":\"fedcba9876543210\",\"seq\":1}\n",
        "{\"event_hash\":\"1111111111111111\",\"prev_hash\":\"0123456789abcdef\",\"seq\":2}\n"
    );
    let classification = classify_run_export(legacy.as_bytes()).expect("classify legacy JSONL");
    assert!(matches!(
        classification,
        RunExportClassification::LegacyV0Jsonl {
            record_count: 2,
            ..
        }
    ));
    assert!(matches!(
        verify_run_export_bytes(legacy.as_bytes()),
        Err(RunExportError::LegacyV0NotVerifiable)
    ));
}

#[tokio::test]
async fn completed_one_shot_export_verifies_and_rejects_projection_tamper() {
    let (directory, ledger) = fixture();
    let (run_id, owner_lease) = create_run(&ledger);
    let intent = commit_effect(
        &ledger,
        &owner_lease,
        append_request(
            run_id,
            "export-local-one-shot",
            EffectIntent::LocalOneShotV1,
            0xc1,
        ),
    );
    let certified = CertifiedEffectIntent::from_receipt_for_test(&intent).unwrap();
    let effect_lease = ledger
        .begin_nonresumable_effect(mode(), &owner_lease, &certified)
        .unwrap();
    let attempt = RunLedger::observed_nonresumable_attempt(effect_lease, "returned").unwrap();
    ledger
        .persist_nonresumable_result(mode(), &attempt)
        .unwrap();
    let bound = RunLedger::bind_nonresumable_result_payload(&attempt).unwrap();
    let result_lease = reserve(
        &ledger,
        &owner_lease,
        AppendRequest::new(
            run_id,
            attempt.result_append_key().clone(),
            identity(0xc2),
            EffectIntent::None,
            bound,
        ),
    );
    let result_reference = ledger
        .publish_sealed_ref(mode(), &owner_lease, &result_lease, artifact_ref(0xc2))
        .unwrap();
    let result_receipt = ledger
        .commit_reserved_append(mode(), &owner_lease, &result_lease, Some(&result_reference))
        .unwrap();
    ledger
        .complete_nonresumable_effect(mode(), &attempt, result_receipt.event_id)
        .unwrap();
    ledger
        .finalize_run_exit(
            mode(),
            &owner_lease,
            RunExit::new(TerminalReason::Completed, object(json!({"exit":"ok"}))),
        )
        .unwrap();

    let receipt = export_run(&ledger, directory.path().join("one-shot-export"), run_id).unwrap();
    let bytes = std::fs::read(receipt.path).unwrap();
    verify_run_export_bytes(&bytes).expect("completed one-shot export must verify");
    let document = parsed_export(&bytes);
    assert_eq!(
        document["nonresumable_effects"]
            .as_array()
            .expect("nonresumable effect array")
            .len(),
        1
    );

    let mut exit_pointer = document.clone();
    let start_event_id = exit_pointer["events"][0]["event_id"].clone();
    exit_pointer["status"]["exit_event_id"] = start_event_id;
    recanonicalize_with_digest(&mut exit_pointer);
    assert!(verify_run_export_bytes(&canonical_document(&exit_pointer)).is_err());

    let mut state_only = document.clone();
    state_only["nonresumable_effects"][0]["state"] = Value::String("abandoned".to_string());
    recanonicalize_with_digest(&mut state_only);
    assert!(verify_run_export_bytes(&canonical_document(&state_only)).is_err());

    let mut claim_binding = document.clone();
    claim_binding["nonresumable_effects"][0]["claim_binding"] = Value::String("0".repeat(64));
    recanonicalize_with_digest(&mut claim_binding);
    assert!(verify_run_export_bytes(&canonical_document(&claim_binding)).is_err());

    let mut result_event = document.clone();
    let source_event_id = result_event["nonresumable_effects"][0]["event_id"].clone();
    result_event["nonresumable_effects"][0]["result_event_id"] = source_event_id;
    recanonicalize_with_digest(&mut result_event);
    assert!(verify_run_export_bytes(&canonical_document(&result_event)).is_err());

    let mut coherent_abandonment = document;
    coherent_abandonment["nonresumable_effects"][0]["state"] =
        Value::String("abandoned".to_string());
    coherent_abandonment["nonresumable_effects"][0]["claim_binding"] = Value::Null;
    coherent_abandonment["nonresumable_effects"][0]["claimed_at_ms"] = Value::Null;
    coherent_abandonment["nonresumable_effects"][0]["result_payload_digest"] = Value::Null;
    coherent_abandonment["nonresumable_effects"][0]["result_event_id"] = Value::Null;
    recanonicalize_with_digest(&mut coherent_abandonment);
    assert!(
        verify_run_export_bytes(&canonical_document(&coherent_abandonment)).is_err(),
        "a re-digested successful export cannot abandon a one-shot"
    );
}

#[test]
fn replay_plan_reconstructs_verified_bytes_after_export_path_is_rewritten() {
    let (directory, ledger) = fixture();
    let (run_id, owner_lease) = create_run(&ledger);
    ledger
        .finalize_run_exit(
            mode(),
            &owner_lease,
            RunExit::new(TerminalReason::Completed, object(json!({"exit":"replay"}))),
        )
        .expect("finalize replay fixture");

    let receipt = export_run(&ledger, directory.path().join("replay-export"), run_id)
        .expect("export replay fixture");
    let verified_bytes = std::fs::read(&receipt.path).expect("read verified fixture");
    let plan = verify_run_export_replay_plan(&receipt.path).expect("build replay plan once");
    let bytes_plan =
        verify_run_export_replay_plan_bytes(&verified_bytes).expect("verify the same bytes");
    assert_eq!(plan.canonical_bytes(), verified_bytes.as_slice());
    assert_eq!(plan.summary(), bytes_plan.summary());
    assert_eq!(plan.events().len(), usize::try_from(plan.summary().event_count).unwrap());

    struct PanicIfArtifactReaderIsUsed;

    impl ExactArtifactReader for PanicIfArtifactReaderIsUsed {
        fn read_exact(
            &self,
            _plan: &VerifiedRunReplayPlan,
            _artifact: &ReplayArtifactReference,
        ) -> Result<VerifiedReplayArtifact, ExactArtifactReadError> {
            panic!("provider, tool, or artifact-read path must not run for an artifact-free plan")
        }
    }

    std::fs::write(&receipt.path, b"{\"replacement\":\"not verified\"}")
        .expect("rewrite export after plan creation");
    let report = reconstruct_verified_run(&plan, Some(&PanicIfArtifactReaderIsUsed))
        .expect("replay consumes only the original verified bytes");

    assert_eq!(report.run_id(), run_id);
    assert_eq!(report.export_digest(), plan.summary().digest.as_str());
    assert_eq!(report.event_count(), plan.summary().event_count);
    assert_eq!(
        report.event_count(),
        u64::try_from(plan.events().len()).expect("event count")
    );
    assert_eq!(report.artifact_count(), 0);
    assert_eq!(report.terminal_reason(), TerminalReason::Completed);
    assert_eq!(report.final_outcome(), crate::run_ledger::RunOutcome::Success);
}

#[test]
fn replay_plan_rejects_reordered_and_tampered_events_even_when_redigested() {
    let (_directory, _ledger, _run_id, _path, bytes) = export_completed_run();
    let document = parsed_export(&bytes);

    let mut reordered = document.clone();
    reordered["events"]
        .as_array_mut()
        .expect("event array")
        .swap(1, 2);
    recanonicalize_with_digest(&mut reordered);
    assert!(matches!(
        verify_run_export_replay_plan_bytes(&canonical_document(&reordered)),
        Err(RunExportError::InvalidExport(_))
    ));

    let mut tampered = document;
    tampered["events"][1]["public_payload"] = json!({"tampered":true});
    recanonicalize_with_digest(&mut tampered);
    assert!(matches!(
        verify_run_export_replay_plan_bytes(&canonical_document(&tampered)),
        Err(RunExportError::InvalidExport(_))
    ));
}

#[test]
fn replay_plan_fails_closed_when_private_artifact_is_missing() {
    let (directory, ledger) = fixture();
    let (run_id, owner_lease) = create_exact_start_run(&ledger);
    ledger
        .finalize_run_exit(
            mode(),
            &owner_lease,
            RunExit::new(TerminalReason::Completed, object(json!({"exit":"exact"}))),
        )
        .expect("finalize exact replay fixture");
    let receipt = export_run(&ledger, directory.path().join("exact-replay-export"), run_id)
        .expect("export exact replay fixture");
    let plan = verify_run_export_replay_plan(&receipt.path).expect("verify exact replay plan");
    assert_eq!(plan.artifact_references().len(), 1);

    struct MissingPrivateArtifact;

    impl ExactArtifactReader for MissingPrivateArtifact {
        fn read_exact(
            &self,
            _plan: &VerifiedRunReplayPlan,
            _artifact: &ReplayArtifactReference,
        ) -> Result<VerifiedReplayArtifact, ExactArtifactReadError> {
            Err(ExactArtifactReadError::ReferenceUnavailable)
        }
    }

    assert!(matches!(
        reconstruct_verified_run(&plan, Some(&MissingPrivateArtifact)),
        Err(DeterministicReplayError::ArtifactRead { .. })
    ));
}
