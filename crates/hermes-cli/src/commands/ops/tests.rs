use super::budget::{
    RepoReviewBudgetProfile, RepoReviewBudgetRuntime, apply_repo_review_budget_profile,
};
use super::reports::{
    performance_autopilot_recommendations, self_evolution_recommendations,
    summarize_performance_autopilot_report, summarize_self_evolution_report,
};
use super::shell::parse_env_file_kv;
use super::task_depth::{TaskDepthProfile, apply_task_depth_profile};
use crate::test_env_lock;
use tempfile::tempdir;

fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
    test_env_lock::lock()
}

#[test]
fn repo_review_budget_profile_application_sets_expected_env() {
    let _guard = env_test_lock();
    apply_repo_review_budget_profile(RepoReviewBudgetProfile::Aggressive);
    let runtime = RepoReviewBudgetRuntime::from_env();
    assert_eq!(runtime.profile, RepoReviewBudgetProfile::Aggressive);
    assert_eq!(runtime.repeat_threshold, 1);
    assert_eq!(runtime.low_signal_threshold, 1);
    assert_eq!(runtime.keep_repeat, 1);
    assert_eq!(runtime.keep_low_signal, 1);
    assert!(runtime.min_signal_score >= 0.34);

    apply_repo_review_budget_profile(RepoReviewBudgetProfile::Balanced);
    let runtime_balanced = RepoReviewBudgetRuntime::from_env();
    assert_eq!(runtime_balanced.profile, RepoReviewBudgetProfile::Balanced);
    assert_eq!(runtime_balanced.repeat_threshold, 2);
    assert_eq!(runtime_balanced.low_signal_threshold, 2);
}

#[test]
fn task_depth_profile_application_sets_expected_env() {
    let _guard = env_test_lock();
    apply_task_depth_profile(TaskDepthProfile::Max);
    assert_eq!(
        std::env::var("HERMES_TASK_DEPTH_PROFILE").ok().as_deref(),
        Some("max")
    );
    assert_eq!(
        std::env::var("HERMES_MAX_ITERATIONS").ok().as_deref(),
        Some("250")
    );
    assert_eq!(
        std::env::var("HERMES_REPO_REVIEW_BUDGET_PROFILE")
            .ok()
            .as_deref(),
        Some("off")
    );

    apply_task_depth_profile(TaskDepthProfile::Balanced);
    assert_eq!(
        std::env::var("HERMES_TASK_DEPTH_PROFILE").ok().as_deref(),
        Some("balanced")
    );
    assert_eq!(
        std::env::var("HERMES_MAX_ITERATIONS").ok().as_deref(),
        Some("250")
    );
}

#[test]
fn test_autocomplete_includes_evolve() {
    let results = super::super::autocomplete("/evo");
    assert!(results.contains(&"/evolve"));
}

#[test]
fn summarize_self_evolution_report_formats_fields() {
    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("self-evolution-loop-test.json");
    std::fs::write(
        &path,
        r#"{
  "ok": false,
  "generated_at": "2026-05-02T00:00:00Z",
  "summary": { "intelligence_index": 66.67 },
  "recommendations": [{"id":"PARITY_DRIFT"}]
}"#,
    )
    .expect("write report");
    let line = summarize_self_evolution_report(&path, "self_evolution").expect("summary");
    assert!(line.contains("self_evolution=fail"));
    assert!(line.contains("idx=66.67"));
    assert!(line.contains("recs=1"));
}

#[test]
fn self_evolution_recommendations_extracts_lines() {
    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("self-evolution-loop-test.json");
    std::fs::write(
        &path,
        r#"{
  "recommendations": [
    {
      "id": "EVAL_REGRESSION",
      "severity": "P0",
      "title": "Recover eval trend before promotion",
      "command": "python3 scripts/run-eval-trend-gate.py --json"
    }
  ]
}"#,
    )
    .expect("write report");
    let lines = self_evolution_recommendations(&path);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("EVAL_REGRESSION"));
    assert!(lines[0].contains("python3 scripts/run-eval-trend-gate.py --json"));
}

#[test]
fn runtime_evolve_status_empty_ledger() {
    let tmp = tempdir().expect("tempdir");
    let cfg = hermes_agent::AgentConfig::default();
    let text = hermes_agent::evolution_ledger::format_evolve_status(tmp.path(), &cfg);
    assert!(text.contains("Evolution status"));
    assert!(text.contains("last review: none yet"));
}

#[test]
fn summarize_performance_autopilot_report_formats_fields() {
    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("performance-autopilot-test.json");
    std::fs::write(
        &path,
        r#"{
  "ok": true,
  "generated_at": "2026-05-08T00:00:00Z",
  "recommendations": [
    {"id":"PERF_STABLE", "severity":"P3", "title":"stable", "recommendation":"none"}
  ]
}"#,
    )
    .expect("write report");
    let line = summarize_performance_autopilot_report(&path, "autopilot").expect("summary");
    assert!(line.contains("autopilot=pass"));
    assert!(line.contains("recs=1"));
    assert!(line.contains("severe=0"));
}

#[test]
fn performance_autopilot_recommendations_extract_lines() {
    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("performance-autopilot-test.json");
    std::fs::write(
        &path,
        r#"{
  "recommendations": [
    {
      "id":"HOTPATH_SLOW",
      "severity":"P1",
      "title":"Tool policy hot-path latency above target",
      "recommendation":"Keep HERMES_TOOL_POLICY_PRESET=standard"
    }
  ]
}"#,
    )
    .expect("write report");
    let recs = performance_autopilot_recommendations(&path);
    assert_eq!(recs.len(), 1);
    assert!(recs[0].contains("HOTPATH_SLOW"));
    assert!(recs[0].contains("recommendation"));
}

#[test]
fn parse_env_file_kv_ignores_comments() {
    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("autopilot.env");
    std::fs::write(
        &path,
        "# comment\nHERMES_TOOL_POLICY_PRESET=standard\n \nINVALID_LINE\nHERMES_REPLAY_ENABLED=1\n",
    )
    .expect("write env");
    let kvs = parse_env_file_kv(&path);
    assert_eq!(kvs.len(), 2);
    assert_eq!(kvs[0].0, "HERMES_TOOL_POLICY_PRESET");
    assert_eq!(kvs[1].0, "HERMES_REPLAY_ENABLED");
}

#[test]
fn test_autocomplete_includes_autopilot() {
    let results = super::super::autocomplete("/auto");
    assert!(results.contains(&"/autopilot"));
}
