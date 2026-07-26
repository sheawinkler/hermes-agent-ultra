use super::*;
use hermes_agent::run_ledger::{
    ActionId, BranchId, CanonicalJson, DigestAbsence, ModelId, ObjectiveId, ProviderId,
    ProvenanceDigest, RunExit, RunId, RunOwnerId, RunProvenance, RunStart, SessionId,
    Sha256Digest, TaskId, TenantId, TerminalReason,
};
use serde_json::json;
use std::path::PathBuf;
use tempfile::tempdir;

fn completed_run_for_runtime(run_runtime: &hermes_agent::RunRuntime, suffix: &str) -> RunId {
    let run_id = RunId::new();
    let absent = || ProvenanceDigest::absent(DigestAbsence::NotApplicable);
    let provenance = RunProvenance::new(
        SessionId::new(format!("cli-replay-session-{suffix}")).expect("session ID"),
        None,
        BranchId::new("main").expect("branch ID"),
        TenantId::new("local").expect("tenant ID"),
        ActionId::new("cli-replay-test").expect("action ID"),
        ObjectiveId::new(format!("cli-replay-objective-{suffix}")).expect("objective ID"),
        TaskId::new(format!("cli-replay-task-{suffix}")).expect("task ID"),
        ModelId::new("cli-replay-model").expect("model ID"),
        ProviderId::new("cli-replay-provider").expect("provider ID"),
        Sha256Digest::new("11".repeat(32)).expect("config digest"),
        absent(),
        absent(),
        absent(),
        absent(),
        absent(),
        absent(),
        absent(),
    );
    let mode = hermes_core::ResolvedExecutionMode::default();
    let created = run_runtime
        .ledger()
        .create_run_and_start(
            mode,
            RunOwnerId::new(format!("cli-replay-owner-{suffix}")).expect("owner ID"),
            RunStart::new(
                run_id,
                provenance,
                CanonicalJson::from_value(&json!({"test": "run-start"}))
                    .expect("canonical run-start payload"),
            ),
        )
        .expect("create terminal test run");
    run_runtime
        .ledger()
        .finalize_run_exit(
            mode,
            &created.owner_lease,
            RunExit::new(
                TerminalReason::Completed,
                CanonicalJson::from_value(&json!({"test": "run-exit"}))
                    .expect("canonical run-exit payload"),
            ),
        )
        .expect("complete terminal test run");
    run_id
}

fn completed_run(app: &App, suffix: &str) -> RunId {
    completed_run_for_runtime(app.run_runtime.as_ref(), suffix)
}

fn completed_metadata_only_effect_export(
    app: &App,
    suffix: &str,
) -> hermes_agent::run_export::RunExportReceipt {
    let mode = hermes_core::ResolvedExecutionMode::default();
    let receipt = app
        .run_runtime
        .enqueue_alpha_runtime_v1(
            mode,
            hermes_agent::AlphaRuntimeV1Request::new(
                format!("cli-replay-{suffix}"),
                "manual",
                format!("private replay artifact payload {suffix}"),
            )
            .expect("valid AlphaRuntime request"),
        )
        .expect("enqueue sealed artifact run");
    let dispatched = app
        .run_runtime
        .dispatch_alpha_runtime_v1(mode, 1)
        .expect("ack durable artifact run");
    assert_eq!(dispatched.acknowledged, 1);
    app.run_runtime
        .publish_run_export(receipt.run_id)
        .expect("publish artifact-bearing export")
}

#[test]
fn run_export_selectors_are_explicit_and_reject_tail_syntax() {
    assert!(matches!(
        parse_run_export_selection("latest"),
        Ok(RunExportSelection::Latest)
    ));
    assert!(matches!(
        parse_run_export_selection("all"),
        Ok(RunExportSelection::All)
    ));
    let run_id = RunId::new();
    assert!(matches!(
        parse_run_export_selection(&run_id.to_string()),
        Ok(RunExportSelection::Exact(parsed)) if parsed == run_id
    ));
    assert!(parse_run_export_selection("100").is_err());
    assert!(parse_run_export_selection("/tmp/tail.json").is_err());
}

#[tokio::test]
async fn raw_trace_export_publishes_complete_exact_run_selector() {
    let _lock = env_test_lock();
    let home = tempdir().expect("tempdir");
    let global_home = home.path().join("global-home");
    let _home = TempHomeGuard::new(&global_home);
    let mut app = build_test_app_with_stream(home.path()).await;
    let run_id = completed_run(&app, "exact");
    let run_id_text = run_id.to_string();

    handle_raw_command(&mut app, &["trace", "export", &run_id_text])
        .expect("export exact run selector");

    let output = latest_ui_assistant_text(&app);
    assert!(output.contains("Authoritative complete run export publication"));
    assert!(output.contains("selector="));
    assert!(output.contains(&run_id_text));
    assert!(output.contains("scope=exact_accessible_local_run"));
    assert!(output.contains("newly_published=true"));
    let export_path = output
        .lines()
        .find_map(|line| line.strip_prefix("path="))
        .map(PathBuf::from)
        .expect("published export path");
    assert!(export_path.starts_with(
        app.state_root
            .join("run-record-v1")
            .join("exports")
    ));
    let verified = hermes_agent::run_export::verify_run_export(&export_path)
        .expect("published export is verifier-valid");
    assert_eq!(verified.run_id, run_id);
    assert!(verified.evidence_complete());
    assert_eq!(verified.legacy_delivery_result_count, 0);
}

#[tokio::test]
async fn replay_diagnostic_paths_are_isolated_to_app_state_root() {
    let _lock = env_test_lock();
    let home = tempdir().expect("tempdir");
    let global_home = home.path().join("global-home");
    let _home = TempHomeGuard::new(&global_home);
    let mut app = build_test_app_with_stream(home.path()).await;

    handle_raw_command(&mut app, &["trace", "status"]).expect("trace status");

    let output = latest_ui_assistant_text(&app);
    let expected_legacy_dir = app.state_root.join("logs").join("replay");
    let global_legacy_dir = global_home.join("logs").join("replay");
    assert!(output.contains(&expected_legacy_dir.display().to_string()));
    assert!(output.contains(
        &app
            .state_root
            .join("run-record-v1")
            .join("exports")
            .display()
            .to_string()
    ));
    assert!(!output.contains(&global_legacy_dir.display().to_string()));
}

#[tokio::test]
async fn raw_trace_verify_and_replay_refuse_legacy_jsonl_without_integrity_claim() {
    let _lock = env_test_lock();
    let home = tempdir().expect("tempdir");
    let _home = TempHomeGuard::new(home.path());
    let mut app = build_test_app_with_stream(home.path()).await;
    let legacy_path = app
        .state_root
        .join("run-record-v1")
        .join("exports")
        .join("legacy.jsonl");
    std::fs::write(
        &legacy_path,
        r#"{"seq":1,"event":"legacy","prev_hash":"0000000000000000","event_hash":"1111111111111111"}
"#,
    )
    .expect("write legacy JSONL");
    let legacy_text = legacy_path.display().to_string();

    handle_raw_command(&mut app, &["trace", "verify", &legacy_text])
        .expect("verify command handles legacy rejection");

    let output = latest_ui_assistant_text(&app);
    assert!(output.contains("Legacy v0 JSONL replay is refused"));
    assert!(output.contains("integrity_verified=false"));
    assert!(output.contains("legacy_v0_unverified_non_executable=true"));
    assert!(!output.contains("proof"));

    handle_raw_command(&mut app, &["trace", "replay", &legacy_text])
        .expect("replay command handles legacy refusal");
    let replay_output = latest_ui_assistant_text(&app);
    assert!(replay_output.contains("Legacy v0 JSONL replay is refused"));
    assert!(replay_output.contains("integrity_verified=false"));
    assert!(replay_output.contains("legacy_v0_unverified_non_executable=true"));
    assert!(replay_output.contains("effects_reconstructed_not_executed=false"));
    assert!(replay_output.contains("exact=false"));
    assert!(!replay_output.contains("proof"));
}

#[tokio::test]
async fn studio_diff_refuses_tampered_canonical_export() {
    let _lock = env_test_lock();
    let home = tempdir().expect("tempdir");
    let _home = TempHomeGuard::new(home.path());
    let mut app = build_test_app_with_stream(home.path()).await;
    let run_a = completed_run(&app, "diff-a");
    let run_b = completed_run(&app, "diff-b");
    let export_a = app
        .run_runtime
        .publish_run_export(run_a)
        .expect("publish first export");
    let export_b = app
        .run_runtime
        .publish_run_export(run_b)
        .expect("publish second export");
    let mut tampered = std::fs::read(&export_b.path).expect("read second export");
    tampered.pop();
    std::fs::write(&export_b.path, tampered).expect("tamper second export");
    let a_text = export_a.path.display().to_string();
    let b_text = export_b.path.display().to_string();

    handle_studio_command(&mut app, &["replay", "diff", &a_text, &b_text])
        .await
        .expect("diff command handles rejected input");

    let output = latest_ui_assistant_text(&app);
    assert!(output.contains("Replay diff refused"));
    assert!(output.contains("proof=false"));
    assert!(!output.contains("Authoritative run export diff"));
}

#[tokio::test]
async fn studio_diff_reports_verified_run_ids_digests_and_counts() {
    let _lock = env_test_lock();
    let home = tempdir().expect("tempdir");
    let _home = TempHomeGuard::new(home.path());
    let mut app = build_test_app_with_stream(home.path()).await;
    let run_a = completed_run(&app, "diff-report-a");
    let run_b = completed_run(&app, "diff-report-b");
    let export_a = app
        .run_runtime
        .publish_run_export(run_a)
        .expect("publish first export");
    let export_b = app
        .run_runtime
        .publish_run_export(run_b)
        .expect("publish second export");
    let a_text = export_a.path.display().to_string();
    let b_text = export_b.path.display().to_string();

    handle_studio_command(&mut app, &["replay", "diff", &a_text, &b_text])
        .await
        .expect("diff verifier-valid exports");

    let output = latest_ui_assistant_text(&app);
    assert!(output.contains("Authoritative run export diff"));
    assert!(output.contains("structural_verification=verified"));
    assert!(output.contains("proof=true"));
    assert!(output.contains(&format!("run_id={run_a}")));
    assert!(output.contains(&format!("run_id={run_b}")));
    assert!(output.contains(&format!("digest={}", export_a.digest)));
    assert!(output.contains(&format!("digest={}", export_b.digest)));
    assert!(output.contains("event_count=2"));
    assert!(output.contains("legacy_delivery_result_count=0"));
}

#[tokio::test]
async fn studio_verify_accepts_only_canonical_authoritative_export_path() {
    let _lock = env_test_lock();
    let home = tempdir().expect("tempdir");
    let _home = TempHomeGuard::new(home.path());
    let mut app = build_test_app_with_stream(home.path()).await;
    let run_id = completed_run(&app, "studio-verify");
    let export = app
        .run_runtime
        .publish_run_export(run_id)
        .expect("publish canonical export");
    let canonical_text = export.path.display().to_string();

    handle_studio_command(&mut app, &["replay", "verify", &canonical_text])
        .await
        .expect("verify canonical export");
    let verified_output = latest_ui_assistant_text(&app);
    assert!(verified_output.contains("structural_verification=true"));
    assert!(verified_output.contains("proof=true"));

    let copied_path = home.path().join("outside.run.json");
    std::fs::copy(&export.path, &copied_path).expect("copy valid export outside canonical directory");
    let copied_text = copied_path.display().to_string();
    handle_studio_command(&mut app, &["replay", "verify", &copied_text])
        .await
        .expect("reject noncanonical export path");

    let rejected_output = latest_ui_assistant_text(&app);
    assert!(rejected_output.contains("Authoritative run export verification rejected"));
    assert!(rejected_output.contains("proof=false"));
    assert!(!rejected_output.contains("proof=true"));
}

#[test]
fn replay_plan_remains_immutable_after_export_replacement_without_provider_or_tool_construction() {
    let state = tempdir().expect("temporary replay state");
    let runtime = hermes_agent::RunRuntime::open_for_ephemeral_testing(state.path())
        .expect("open isolated run runtime");
    let run_id = completed_run_for_runtime(&runtime, "immutable-plan");
    let export = runtime
        .publish_run_export(run_id)
        .expect("publish complete export");

    let plan = read_verified_run_replay_plan(&export.path).expect("read and verify replay plan");
    let expected_digest = plan.summary().digest.clone();
    std::fs::write(&export.path, b"replaced after plan verification")
        .expect("replace export after verified plan creation");

    let report =
        hermes_agent::deterministic_replay::reconstruct_verified_run(&plan, None)
            .expect("reconstruct immutable verified plan");
    assert_eq!(report.run_id(), run_id);
    assert_eq!(report.export_digest(), expected_digest);
    assert_eq!(report.event_count(), 2);

    let rendered = render_verified_run_reconstruction(&export.path, &plan, &report);
    assert!(rendered.contains("integrity_verified=true"));
    assert!(rendered.contains("effects_reconstructed_not_executed=true"));
    assert!(!rendered.contains("proof"));
    assert!(!rendered.contains("signature"));
    assert!(!rendered.contains("effects_executed=true"));
}

#[tokio::test]
async fn raw_trace_replay_reconstructs_metadata_only_export_without_exact_claim() {
    let _lock = env_test_lock();
    let home = tempdir().expect("tempdir");
    let _home = TempHomeGuard::new(home.path());
    let mut app = build_test_app_with_stream(home.path()).await;
    let export = completed_metadata_only_effect_export(&app, "metadata-only");
    let export_text = export.path.display().to_string();

    handle_raw_command(&mut app, &["trace", "replay", &export_text])
        .expect("replay handles a verified export");

    let output = latest_ui_assistant_text(&app);
    assert!(output.contains("Authoritative verified run reconstruction"));
    assert!(output.contains("integrity_verified=true"));
    assert!(output.contains("evidence_complete=true"));
    assert!(output.contains("effects_reconstructed_not_executed=true"));
    assert!(output.contains("exact=false"));
    assert!(output.contains("reconstruction_fidelity=metadata_only"));
    assert!(!output.contains("proof"));
    assert!(!output.contains("signature"));
    assert!(!output.contains("effects_executed=true"));
}

#[tokio::test]
async fn raw_trace_replay_fails_closed_when_required_artifact_is_missing() {
    let _lock = env_test_lock();
    let home = tempdir().expect("tempdir");
    let _home = TempHomeGuard::new(home.path());
    let mut app = build_test_app_with_stream(home.path()).await;
    let export = completed_metadata_only_effect_export(&app, "missing-artifact");
    let plan = read_verified_run_replay_plan(&export.path).expect("verify artifact-bearing plan");
    assert!(
        !plan.artifact_references().is_empty(),
        "fixture must bind an exact artifact"
    );
    std::fs::remove_dir_all(app.state_root.join("run-record-v1").join("artifacts"))
        .expect("remove required artifact store after export verification");
    let export_text = export.path.display().to_string();

    handle_raw_command(&mut app, &["trace", "replay", &export_text])
        .expect("replay handles missing artifact failure");

    let output = latest_ui_assistant_text(&app);
    assert!(output.contains("Authoritative run reconstruction refused"));
    assert!(output.contains("integrity_verified=true"));
    assert!(output.contains("effects_reconstructed_not_executed=false"));
    assert!(output.contains("exact=false"));
    assert!(!output.contains("effects_reconstructed_not_executed=true"));
}
