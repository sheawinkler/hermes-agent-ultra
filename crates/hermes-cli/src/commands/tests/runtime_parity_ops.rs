use super::*;

#[test]
fn p1_trigger_triage_escalates_high_severity_events() {
    let _guard = env_test_lock();
    std::env::set_var("HERMES_TRIGGER_TRIAGE_MODE", "strict");
    let assessment = evaluate_trigger_triage(
        "webhook",
        "critical outage with secret key leak and panic in runtime",
    );
    assert_eq!(assessment.decision, TriggerTriageDecision::Escalate);
    assert!(assessment.requires_approval);
    assert!(assessment.severity >= 7);
    std::env::remove_var("HERMES_TRIGGER_TRIAGE_MODE");
}

#[test]
fn p2_trigger_triage_feedback_persists_bias_and_influences_scoring() {
    let _guard = env_test_lock();
    let tmp = tempdir().expect("tempdir");
    let _home_guard = TempHomeGuard::new(tmp.path());
    let baseline = evaluate_trigger_triage("webhook", "timeout error while polling");
    let feedback_assessment = evaluate_trigger_triage("webhook", "critical outage and panic");
    append_triage_learning_feedback(
        "webhook",
        "critical outage and panic",
        "critical",
        &feedback_assessment,
    )
    .expect("append triage feedback");
    let (bias, _) = triage_learning_bias("webhook", "timeout error while polling");
    assert!(bias > 0);
    let after = evaluate_trigger_triage("webhook", "timeout error while polling");
    assert!(after.severity >= baseline.severity);
    assert!(
        trigger_triage_learning_state_path().exists(),
        "triage learning state file should be persisted"
    );
}

#[tokio::test]
async fn p2_subconscious_profile_dry_run_blocks_high_risk_tasks() {
    let _guard = env_test_lock();
    let tmp = tempdir().expect("tempdir");
    let _home_guard = TempHomeGuard::new(tmp.path());
    let mut app = build_test_app_with_stream(tmp.path()).await;
    let now = chrono::Utc::now().to_rfc3339();
    let state = SubconsciousQueueState {
        tasks: vec![SubconsciousTask {
            id: "sc-risky".to_string(),
            source: "test".to_string(),
            prompt: "rotate key and deploy to prod".to_string(),
            score: 4.2,
            risk: "high".to_string(),
            requires_approval: false,
            status: "pending".to_string(),
            job_id: None,
            created_at: now.clone(),
            updated_at: now,
        }],
    };
    save_subconscious_state(&state).expect("save subconscious state");

    handle_slash_command(
        &mut app,
        "/subconscious",
        &["run", "1", "--dry-run", "profile=strict"],
    )
    .await
    .expect("subconscious dry-run");
    let out = latest_ui_assistant_text(&app);
    assert!(out.contains("Dry-run subconscious run profile=strict"));
    assert!(out.contains("blocked=1"));
}

#[tokio::test]
async fn p2_walkthrough_insights_persists_events() {
    let _guard = env_test_lock();
    let tmp = tempdir().expect("tempdir");
    let _home_guard = TempHomeGuard::new(tmp.path());
    let mut app = build_test_app_with_stream(tmp.path()).await;

    handle_slash_command(&mut app, "/walkthrough", &["start", "quick"])
        .await
        .expect("walkthrough start");
    handle_slash_command(&mut app, "/walkthrough", &["done", "boot-gate"])
        .await
        .expect("walkthrough done");
    handle_slash_command(&mut app, "/walkthrough", &["insights"])
        .await
        .expect("walkthrough insights");
    let out = latest_ui_assistant_text(&app);
    assert!(out.contains("Walkthrough insights"));
    assert!(out.contains("resume_hint:"));
    assert!(walkthrough_events_path().exists());
}

#[tokio::test]
async fn p2_integrations_snapshot_and_repair_commands_work() {
    let _guard = env_test_lock();
    let tmp = tempdir().expect("tempdir");
    let _home_guard = TempHomeGuard::new(tmp.path());
    let mut app = build_test_app_with_stream(tmp.path()).await;

    handle_slash_command(&mut app, "/integrations", &["snapshot"])
        .await
        .expect("integrations snapshot");
    let snapshot_out = latest_ui_assistant_text(&app);
    assert!(snapshot_out.contains("Integration snapshot exported"));

    handle_slash_command(&mut app, "/integrations", &["repair"])
        .await
        .expect("integrations repair");
    let repair_out = latest_ui_assistant_text(&app);
    assert!(repair_out.contains("Integrations repair plan"));
}

#[tokio::test]
async fn p2_compress_rules_autotune_apply_updates_runtime_env() {
    let _guard = env_test_lock();
    let tmp = tempdir().expect("tempdir");
    let _home_guard = TempHomeGuard::new(tmp.path());
    let mut app = build_test_app_with_stream(tmp.path()).await;
    std::env::remove_var("HERMES_TUI_MAX_TOOL_OUTPUT_TOTAL_CHARS");

    handle_slash_command(
        &mut app,
        "/compress",
        &["rules", "autotune", "apply", "user"],
    )
    .await
    .expect("compress autotune apply");
    let out = latest_ui_assistant_text(&app);
    assert!(out.contains("Autotune applied"));
    assert!(
        std::env::var("HERMES_TUI_MAX_TOOL_OUTPUT_TOTAL_CHARS")
            .ok()
            .is_some(),
        "autotune should write runtime compression env"
    );
}

#[test]
fn p2_oauth_runtime_gate_manifest_override_is_honored() {
    let _guard = env_test_lock();
    let tmp = tempdir().expect("tempdir");
    let manifest = tmp.path().join("oauth-manifest.json");
    std::fs::write(
        &manifest,
        r#"{
  "default_min_version": "99.0.0",
  "required_oauth_provider_ids": ["nous"],
  "provider_min_versions": { "nous": "99.0.0" }
}"#,
    )
    .expect("write manifest");
    std::env::set_var("HERMES_OAUTH_GATE_MANIFEST_PATH", &manifest);
    let (ok, detail) = oauth_runtime_gate_for_provider("nous").expect("oauth gate");
    assert!(!ok);
    assert!(detail.contains("required>=99.0.0"));
    assert!(detail.contains("oauth-manifest.json"));
    std::env::remove_var("HERMES_OAUTH_GATE_MANIFEST_PATH");
}

#[test]
fn test_debug_alias_maps_to_debug_dump() {
    assert_eq!(canonical_command("/debug"), "/debug-dump");
}

#[test]
fn test_upstream_compat_aliases_are_mapped() {
    assert_eq!(canonical_command("/topic"), "/title");
    assert_eq!(canonical_command("/reload-skills"), "/reload-skills");
    assert_eq!(canonical_command("/reload_skills"), "/reload-skills");
    assert_eq!(canonical_command("/swarms"), "/swarm");
    assert_eq!(canonical_command("/summary"), "/recap");
    assert_eq!(canonical_command("/whoami"), "/profile");
    assert_eq!(canonical_command("/v"), "/version");
    assert_eq!(canonical_command("/billing"), "/billing");
    assert_eq!(canonical_command("/credits"), "/usage");
    assert_eq!(canonical_command("/suggest"), "/suggestions");
    assert_eq!(canonical_command("/footer"), "/statusbar");
    assert_eq!(canonical_command("/indicator"), "/statusbar");
    assert_eq!(canonical_command("/tasks"), "/kanban");
    assert_eq!(canonical_command("/kanban"), "/kanban");
    assert_eq!(canonical_command("/busy"), "/status");
    assert_eq!(canonical_command("/bg"), "/background");
    assert_eq!(canonical_command("/curator"), "/skills");
    assert_eq!(canonical_command("/tt"), "/timetravel");
    assert_eq!(canonical_command("/rb"), "/runbook");
}

#[test]
fn p3_swarm_commands_registered_and_completable() {
    assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/swarm"));
    assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/swarms"));
    assert!(autocomplete("/swa").contains(&"/swarm"));
}

#[tokio::test]
async fn p3_swarm_status_plan_run_cancel_surface_is_handled() {
    let _guard = env_test_lock();
    let tmp = tempdir().expect("tempdir");
    let _home_guard = TempHomeGuard::new(tmp.path());
    let mut app = build_test_app_with_stream(tmp.path()).await;

    handle_slash_command(&mut app, "/swarm", &["status"])
        .await
        .expect("swarm status");
    let status = latest_ui_assistant_text(&app);
    assert!(status.contains("Swarm runtime"));

    handle_slash_command(&mut app, "/swarm", &["plan", "graph"])
        .await
        .expect("swarm plan");
    let plan = latest_ui_assistant_text(&app);
    assert!(plan.contains("Swarm execution plan"));
    assert!(plan.contains("\"mode\": \"graph\""));

    handle_slash_command(&mut app, "/swarm", &["on"])
        .await
        .expect("swarm on");
    handle_slash_command(&mut app, "/swarm", &["run", "4", "sequential"])
        .await
        .expect("swarm run");
    assert!(app.quorum_armed_once, "swarm run should arm quorum fanout");
    let run_msg = latest_ui_assistant_text(&app);
    assert!(run_msg.contains("Swarm run armed."));
    assert!(run_msg.contains("mode=sequential"));

    handle_slash_command(&mut app, "/swarm", &["cancel"])
        .await
        .expect("swarm cancel");
    assert!(!app.quorum_armed_once, "cancel should disarm run");
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
        Some("50")
    );
}

#[test]
fn test_recap_and_context_commands_are_registered() {
    assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/recap"));
    assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/context"));
    assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/auth"));
    assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/telemetry"));
    assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/runbook"));
    let recap = autocomplete("/rec");
    assert!(recap.contains(&"/recap"));
    let context = autocomplete("/cont");
    assert!(context.contains(&"/context"));
    let auth = autocomplete("/au");
    assert!(auth.contains(&"/auth"));
    let telemetry = autocomplete("/tele");
    assert!(telemetry.contains(&"/telemetry"));
    let runbook = autocomplete("/runb");
    assert!(runbook.contains(&"/runbook"));
}

#[test]
fn test_memory_command_is_registered_completable_and_cataloged() {
    assert!(SLASH_COMMANDS.iter().any(|(name, _)| *name == "/memory"));
    let results = autocomplete("/mem");
    assert!(results.contains(&"/memory"));
    let catalog = render_command_catalog(Some("memory"));
    assert!(catalog.contains("/memory"));
    assert!(catalog.contains("Show memory backend status"));
}

#[test]
fn test_render_memory_backend_status_reports_file_backend() {
    let tmp = tempdir().expect("tempdir");
    let memories = tmp.path().join("memories");
    std::fs::create_dir_all(&memories).expect("create memories dir");
    std::fs::write(memories.join("MEMORY.md"), "# Memory\nfact\n").expect("write memory");
    std::fs::write(memories.join("USER.md"), "# User\npreference\n").expect("write user");

    let status = render_memory_backend_status(tmp.path());
    assert!(status.contains("Memory provider: files (MEMORY.md + USER.md)"));
    assert!(status.contains("MEMORY.md"));
    assert!(status.contains("USER.md"));
}

#[test]
fn test_render_mcp_runtime_status_includes_json_only_servers() {
    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("mcp_servers.json");
    std::fs::write(
        &path,
        r#"{"contextlattice":{"url":"http://127.0.0.1:8075/mcp","enabled":true,"supports_parallel_tool_calls":true}}"#,
    )
    .expect("write mcp json");
    let cfg = crate::mcp_config::load_mcp_config(&path).expect("load mcp config");

    let status = render_mcp_runtime_status(&[], Some(&cfg), &path);
    assert!(status.contains("MCP runtime status"));
    assert!(status.contains("contextlattice"));
    assert!(status.contains("source:mcp_servers.json"));
    assert!(status.contains("json_only=[contextlattice]"));
}

#[test]
fn test_render_mcp_runtime_status_reports_drift_between_yaml_and_json() {
    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("mcp_servers.json");
    let cfg = crate::mcp_config::parse_mcp_config_json(
        r#"{"json-only":{"url":"https://example.com/mcp"}}"#,
    )
    .expect("parse mcp config");
    let yaml = vec![hermes_config::McpServerEntry {
        name: "yaml-only".to_string(),
        command: Some("local-mcp".to_string()),
        url: None,
        supports_parallel_tool_calls: false,
        keepalive_interval: None,
    }];

    let status = render_mcp_runtime_status(&yaml, Some(&cfg), &path);
    assert!(status.contains("yaml-only"));
    assert!(status.contains("json-only"));
    assert!(status.contains("config_only=[yaml-only]"));
    assert!(status.contains("json_only=[json-only]"));
}

#[tokio::test]
async fn guard_provider_model_selection_soft_accepts_unlisted_codex_models() {
    let _guard = env_test_lock();
    std::env::set_var("HERMES_MODEL_CATALOG_GUARD", "1");
    let (guarded, note) = guard_provider_model_selection_for_config(
        "openai-codex:gpt-9-codex-preview",
        &GatewayConfig::default(),
    )
    .await
    .expect("codex soft-accept");
    assert_eq!(guarded, "openai-codex:gpt-9-codex-preview");
    assert!(note
        .as_deref()
        .unwrap_or_default()
        .contains("soft-accepted"));
    std::env::remove_var("HERMES_MODEL_CATALOG_GUARD");
}

#[test]
fn alpha_loop_defaults_are_written_and_loadable() {
    let _guard = env_test_lock();
    let tmp = tempdir().expect("tempdir");
    std::env::set_var("HERMES_HOME", tmp.path());
    let path = crate::alpha_runtime::write_default_alpha_loops(true).expect("write defaults");
    assert!(path.exists());
    let loops = crate::alpha_runtime::load_alpha_loops().expect("load defaults");
    assert_eq!(loops.len(), 3);
    assert!(loops.iter().any(|l| l.id == "primary-objective-loop"));
    assert!(loops.iter().all(|l| !l.trading_sensitive));
    std::env::remove_var("HERMES_HOME");
}

#[test]
fn test_autocomplete_includes_evolve() {
    let results = autocomplete("/evo");
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
  "command": "/ops eval run"
}
  ]
}"#,
    )
    .expect("write report");
    let lines = self_evolution_recommendations(&path);
    assert_eq!(lines.len(), 1);
    assert!(lines[0].contains("EVAL_REGRESSION"));
    assert!(lines[0].contains("/ops eval run"));
}

#[test]
fn native_session_eval_harness_writes_compatible_report() {
    let repo = tempdir().expect("repo");
    let home = tempdir().expect("home");
    let sessions_dir = home.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("sessions dir");
    std::fs::write(
        sessions_dir.join("rich-session.json"),
        r#"{
  "messages": [
{"role":"user","content":"please inspect /objective status"},
{"role":"assistant","content":"I will use tool_call evidence and apply_patch. [objective_patch] exists_now=true"},
{"role":"user","content":"verify"},
{"role":"assistant","content":"verified_exists=true"}
  ]
}"#,
    )
    .expect("write session");

    let (report, path) = run_session_eval_harness_native(repo.path(), &sessions_dir, 25, None)
        .expect("run native session eval");
    assert!(path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .starts_with("session-eval-harness-"));
    assert!(report["ok"].as_bool().expect("ok bool"));
    assert_eq!(report["summary"]["sessions_analyzed"], 1);
    assert_eq!(report["summary"]["tool_activity_sessions"], 1);
    assert_eq!(report["summary"]["objective_activity_sessions"], 1);
    assert_eq!(report["summary"]["patch_evidence_sessions"], 1);
    assert_eq!(report["summary"]["user_turns"], 2);
    assert_eq!(report["summary"]["assistant_turns"], 2);
    let on_disk = read_json_file(&path).expect("report on disk");
    assert_eq!(on_disk["summary"]["sessions_analyzed"], 1);
}

#[test]
fn native_eval_trend_gate_matches_python_contract() {
    let repo = tempdir().expect("repo");
    let evals = repo.path().join("evals");
    std::fs::create_dir_all(&evals).expect("evals dir");
    let baseline = evals.join("baseline.json");
    let current = evals.join("current.json");
    std::fs::write(
        &baseline,
        r#"{"metrics":{"total":2,"pass_at_1":0.90,"total_duration":{"secs":20,"nanos":0},"total_cost_usd":1.0}}"#,
    )
    .expect("write baseline");
    std::fs::write(
        &current,
        r#"{"metrics":{"total":2,"pass_at_1":0.88,"total_duration":{"secs":22,"nanos":0},"total_cost_usd":1.1}}"#,
    )
    .expect("write current");

    let (report, path) = run_eval_trend_gate_native(
        repo.path(),
        Some(&current),
        Some(&baseline),
        None,
        EvalTrendGateOptions::default(),
    )
    .expect("run trend gate");
    assert!(report["ok"].as_bool().expect("ok bool"));
    assert_eq!(
        report["current_path"].as_str(),
        Some(current.to_string_lossy().as_ref())
    );
    assert_eq!(
        report["baseline_path"].as_str(),
        Some(baseline.to_string_lossy().as_ref())
    );
    assert_eq!(report["checks"][0]["name"], "pass_at_1_drop");
    assert!(report["checks"][0]["ok"].as_bool().expect("check ok bool"));
    assert!(path.exists());
}

#[test]
fn native_eval_trend_gate_allows_missing_baseline_when_requested() {
    let repo = tempdir().expect("repo");
    let (report, path) = run_eval_trend_gate_native(
        repo.path(),
        None,
        None,
        None,
        EvalTrendGateOptions {
            allow_missing_baseline: true,
            ..Default::default()
        },
    )
    .expect("run missing-input gate");
    assert!(report["ok"].as_bool().expect("ok bool"));
    assert_eq!(report["reason"], "missing_eval_inputs");
    assert!(path.exists());
}

#[tokio::test]
async fn ops_eval_run_uses_native_report_not_python_script() {
    let _guard = env_test_lock();
    let repo = tempdir().expect("repo");
    let home = tempdir().expect("home");
    let _home_guard = TempHomeGuard::new(home.path());
    let previous_repo_root = std::env::var("HERMES_REPO_ROOT").ok();
    std::env::set_var("HERMES_REPO_ROOT", repo.path());
    let sessions_dir = home.path().join("sessions");
    std::fs::create_dir_all(&sessions_dir).expect("sessions dir");
    std::fs::write(
        sessions_dir.join("rich-session.json"),
        r#"{"messages":[
  {"role":"user","content":"run /objective status"},
  {"role":"assistant","content":"tool_call result with apply_patch exists_now=true"},
  {"role":"user","content":"ok"},
  {"role":"assistant","content":"done"}
]}"#,
    )
    .expect("write session");
    let mut app = build_test_app_with_stream(home.path()).await;

    handle_ops_eval_command(&mut app, &["run"])
        .await
        .expect("handle ops eval run");
    let out = latest_ui_assistant_text(&app);
    assert!(out.contains("\"sessions_analyzed\": 1"));
    assert!(out.contains("session-eval-harness-"));
    assert!(!out.contains("python3 scripts/run-session-eval-harness.py"));

    match previous_repo_root {
        Some(value) => std::env::set_var("HERMES_REPO_ROOT", value),
        None => std::env::remove_var("HERMES_REPO_ROOT"),
    }
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
fn native_performance_autopilot_recommends_throughput_for_slow_hotpath() {
    let hotpath = serde_json::json!({
        "ok": true,
        "stdout_tail": "tool_policy_hot_path_ns_per_eval=13000\n",
        "stderr_tail": "",
        "exit_code": 0,
    });
    let pass = serde_json::json!({
        "ok": true,
        "stdout_tail": "{}",
        "stderr_tail": "",
        "exit_code": 0,
    });
    let context = serde_json::json!({
        "ok": true,
        "stdout_tail": r#"{"health":{"ok":true},"warnings":[],"context_pack":{"retrieval":{"source_counts":{"qdrant":2},"fallback_counts":{"python_hot_path_total":0}}},"status":{"queue":{"pendingTotal":0}}}"#,
        "stderr_tail": "",
        "exit_code": 0,
    });

    let recs = build_performance_autopilot_recommendations(&hotpath, &pass, &pass, &context);
    assert!(recs
        .iter()
        .any(|rec| rec.get("id").and_then(|v| v.as_str()) == Some("HOTPATH_SLOW")));
    let adaptive =
        compute_performance_autopilot_indexes(&hotpath, &pass, &pass, &context, &recs);
    assert_eq!(adaptive["profile_recommendation"], "throughput");
    assert!(adaptive["adaptive_index"].as_f64().unwrap_or(0.0) > 80.0);
}

#[test]
fn native_performance_autopilot_env_writer_uses_safe_actions() {
    let tmp = tempdir().expect("tempdir");
    let path = tmp.path().join("autopilot.env");
    let report = serde_json::json!({
        "generated_at": "2026-06-04T00:00:00Z",
        "profile_recommendation": "throughput",
        "recommendations": [
            {"id":"HOTPATH_SLOW","severity":"P1","title":"slow","recommendation":"tune"}
        ],
        "adaptive_actions": [
            {"key":"HERMES_PERF_AUTOPILOT_PROFILE","value":"throughput","reason":"profile"},
            {"key":"HERMES_MODEL_CATALOG_GUARD","value":"1","reason":"guard"}
        ]
    });

    write_performance_autopilot_env(&path, &report).expect("write env");
    let raw = std::fs::read_to_string(&path).expect("read env");
    assert!(raw.contains("HERMES_TOOL_POLICY_PRESET=standard"));
    assert!(raw.contains("HERMES_MODEL_CATALOG_GUARD=1"));
    assert!(raw.contains("HERMES_PERF_AUTOPILOT_PROFILE=throughput"));
    let kvs = parse_env_file_kv(&path);
    assert!(kvs
        .iter()
        .any(|(k, v)| k == "HERMES_MODEL_CATALOG_GUARD" && v == "1"));
}

#[tokio::test]
async fn native_slo_auto_rollback_runs_rollback_on_violation() {
    let repo = tempdir().expect("repo");
    let rollback_marker = repo.path().join("rollback.marker");
    let rollback_cmd = format!("printf rolled-back > {}", rollback_marker.display());
    let (report, path) =
        run_slo_auto_rollback_native(repo.path(), "false", &rollback_cmd, false, None)
            .await
            .expect("run slo");
    assert!(!report["ok"].as_bool().expect("ok bool"));
    assert!(report["violated"].as_bool().expect("violated bool"));
    assert!(report["rollback"]["ok"].as_bool().expect("rollback ok bool"));
    assert!(path.exists());
    assert_eq!(
        std::fs::read_to_string(&rollback_marker).expect("read marker"),
        "rolled-back"
    );
}

#[test]
fn native_self_evolution_recommendations_use_runtime_commands() {
    let sections = serde_json::json!({
        "golden_parity": {"ok": true},
        "eval_trend": {"ok": false},
        "elite_sync": {"ok": false}
    });
    let recs = build_self_evolution_recommendations_native("ship rust surfaces", &sections);
    assert!(recs
        .iter()
        .any(|rec| rec.get("id").and_then(|v| v.as_str()) == Some("EVAL_REGRESSION")));
    assert!(recs
        .iter()
        .any(|rec| rec.get("command").and_then(|v| v.as_str()) == Some("/ops gate elite")));
    assert!(recs.iter().all(|rec| !rec
        .get("command")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .contains("python3")));
}

#[test]
fn native_parity_sections_read_release_and_backlog_gates() {
    let repo = tempdir().expect("repo");
    let parity_dir = repo.path().join("docs/parity");
    std::fs::create_dir_all(&parity_dir).expect("parity dir");
    std::fs::write(
        parity_dir.join("global-parity-proof.json"),
        r#"{"release_gate":{"pass":true}}"#,
    )
    .expect("write proof");
    std::fs::write(
        parity_dir.join("shared-diff-backlog.json"),
        r#"{"summary":{"pending_classification":0,"pending_review":0}}"#,
    )
    .expect("write backlog");

    assert!(parity_release_gate_section(repo.path())["ok"]
        .as_bool()
        .expect("release gate ok bool"));
    assert!(shared_backlog_gate_section(repo.path())["ok"]
        .as_bool()
        .expect("backlog gate ok bool"));
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
    let results = autocomplete("/auto");
    assert!(results.contains(&"/autopilot"));
}

#[test]
fn canonical_command_maps_pilot_alias() {
    assert_eq!(canonical_command("/pilot"), "/autopilot");
}

#[test]
fn test_autocomplete_includes_raw_controls() {
    let results = autocomplete("/ra");
    assert!(results.contains(&"/raw"));
}

#[test]
fn test_autocomplete_ops_control_plane() {
    let results = autocomplete("/op");
    assert!(results.contains(&"/ops"));
}

#[test]
fn test_autocomplete_fuzzy_prefers_close_matches() {
    let results = autocomplete("/mdl");
    assert!(!results.is_empty());
    assert_eq!(results[0], "/model");
}

#[test]
fn test_autocomplete_matches_description_terms() {
    let results = autocomplete("/quota");
    assert!(results.contains(&"/gquota"));
}

#[test]
fn test_autocomplete_exact() {
    let results = autocomplete("/help");
    assert!(!results.is_empty());
    assert_eq!(results[0], "/help");
}

#[test]
fn test_autocomplete_no_match() {
    let results = autocomplete("/xyz");
    assert!(results.is_empty());
}

#[test]
fn test_help_for_known_command() {
    assert!(help_for("/help").is_some());
    assert!(help_for("/model").is_some());
}

#[test]
fn test_help_for_unknown_command() {
    assert!(help_for("/unknown").is_none());
}

#[test]
fn test_command_result_equality() {
    assert_eq!(CommandResult::Handled, CommandResult::Handled);
    assert_ne!(CommandResult::Handled, CommandResult::Quit);
}

#[tokio::test]
async fn test_mcp_sentrux_setup_syncs_json_and_yaml() {
    let tmp = tempdir().expect("tempdir");
    let config_dir = tmp.path().join("hermes-home");
    std::fs::create_dir_all(&config_dir).expect("create config dir");

    upsert_sentrux_mcp_profile(&config_dir).expect("sentrux setup helper");

    let mcp_json = config_dir.join("mcp_servers.json");
    assert!(mcp_json.exists(), "mcp_servers.json should be created");
    let json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&mcp_json).expect("read mcp_servers.json"),
    )
    .expect("parse mcp json");
    let sentrux = json
        .get(SENTRUX_MCP_SERVER_NAME)
        .expect("sentrux entry should exist");
    assert_eq!(
        sentrux.get("command").and_then(|v| v.as_str()),
        Some(SENTRUX_MCP_COMMAND)
    );
    assert_eq!(
        sentrux
            .get("args")
            .and_then(|v| v.as_array())
            .and_then(|arr| arr.first())
            .and_then(|v| v.as_str()),
        Some(SENTRUX_MCP_ARG)
    );
    assert!(
        sentrux
            .get("supports_parallel_tool_calls")
            .and_then(|v| v.as_bool())
            .expect("sentrux parallel flag")
    );

    let cfg = hermes_config::load_user_config_file(&config_dir.join("config.yaml"))
        .expect("load config.yaml");
    assert!(
        cfg.mcp_servers
            .iter()
            .any(|entry| entry.name == SENTRUX_MCP_SERVER_NAME
                && entry.command.as_deref() == Some("sentrux --mcp")
                && entry.supports_parallel_tool_calls),
        "config.yaml mcp_servers should include sentrux command"
    );
}

#[tokio::test]
async fn test_mcp_sentrux_remove_syncs_json_and_yaml() {
    let tmp = tempdir().expect("tempdir");
    let config_dir = tmp.path().join("hermes-home");
    std::fs::create_dir_all(&config_dir).expect("create config dir");

    upsert_sentrux_mcp_profile(&config_dir).expect("sentrux setup helper");
    remove_sentrux_mcp_profile(&config_dir).expect("sentrux remove helper");

    let json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(config_dir.join("mcp_servers.json")).expect("read mcp json"),
    )
    .expect("parse mcp json");
    assert!(
        json.get(SENTRUX_MCP_SERVER_NAME).is_none(),
        "mcp_servers.json should remove sentrux"
    );

    let cfg = hermes_config::load_user_config_file(&config_dir.join("config.yaml"))
        .expect("load config.yaml");
    assert!(
        cfg.mcp_servers
            .iter()
            .all(|entry| entry.name != SENTRUX_MCP_SERVER_NAME),
        "config.yaml mcp_servers should remove sentrux"
    );
}

#[tokio::test]
async fn test_mcp_unreal_engine_setup_syncs_json_and_yaml() {
    let tmp = tempdir().expect("tempdir");
    let config_dir = tmp.path().join("hermes-home");
    std::fs::create_dir_all(&config_dir).expect("create config dir");

    upsert_unreal_mcp_profile(&config_dir).expect("unreal setup helper");

    let mcp_json = config_dir.join("mcp_servers.json");
    assert!(mcp_json.exists(), "mcp_servers.json should be created");
    let json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(&mcp_json).expect("read mcp_servers.json"),
    )
    .expect("parse mcp json");
    let unreal = json
        .get(UNREAL_MCP_SERVER_NAME)
        .expect("unreal entry should exist");
    assert_eq!(
        unreal.get("url").and_then(|v| v.as_str()),
        Some(UNREAL_MCP_URL)
    );
    assert!(
        !unreal
            .get("supports_parallel_tool_calls")
            .and_then(|v| v.as_bool())
            .expect("unreal parallel flag")
    );
    assert_eq!(
        unreal.get("keepalive_interval").and_then(|v| v.as_u64()),
        Some(10)
    );

    let cfg = hermes_config::load_user_config_file(&config_dir.join("config.yaml"))
        .expect("load config.yaml");
    assert!(
        cfg.mcp_servers
            .iter()
            .any(|entry| entry.name == UNREAL_MCP_SERVER_NAME
                && entry.url.as_deref() == Some(UNREAL_MCP_URL)
                && !entry.supports_parallel_tool_calls
                && entry.keepalive_interval == Some(10)),
        "config.yaml mcp_servers should include the Unreal HTTP profile"
    );

    let (json_present, yaml_present) = unreal_mcp_status(&config_dir);
    assert!(json_present);
    assert!(yaml_present);
}

#[tokio::test]
async fn test_mcp_unreal_engine_remove_syncs_json_and_yaml() {
    let tmp = tempdir().expect("tempdir");
    let config_dir = tmp.path().join("hermes-home");
    std::fs::create_dir_all(&config_dir).expect("create config dir");

    upsert_unreal_mcp_profile(&config_dir).expect("unreal setup helper");
    remove_unreal_mcp_profile(&config_dir).expect("unreal remove helper");

    let json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(config_dir.join("mcp_servers.json")).expect("read mcp json"),
    )
    .expect("parse mcp json");
    assert!(
        json.get(UNREAL_MCP_SERVER_NAME).is_none(),
        "mcp_servers.json should remove unreal-engine"
    );

    let cfg = hermes_config::load_user_config_file(&config_dir.join("config.yaml"))
        .expect("load config.yaml");
    assert!(
        cfg.mcp_servers
            .iter()
            .all(|entry| entry.name != UNREAL_MCP_SERVER_NAME),
        "config.yaml mcp_servers should remove unreal-engine"
    );
}

#[test]
fn test_default_skill_tap_present_in_merged_list() {
    let merged = merged_skill_taps(&[]);
    assert!(merged
        .iter()
        .any(|tap| tap == "https://github.com/MiniMax-AI/cli::skill"));
}

#[test]
fn test_autoresearch_default_skill_tap_present_in_merged_list() {
    let merged = merged_skill_taps(&[]);
    assert!(merged
        .iter()
        .any(|tap| tap == "https://github.com/github/awesome-copilot::skills"));
}

#[test]
fn test_nous_official_default_skill_taps_present_in_merged_list() {
    let merged = merged_skill_taps(&[]);
    assert!(merged
        .iter()
        .any(|tap| tap == "https://github.com/NousResearch/hermes-agent::skills"));
    assert!(merged
        .iter()
        .any(|tap| tap == "https://github.com/NousResearch/hermes-agent::optional-skills"));
}

#[test]
fn test_official_skill_path_candidates_cover_skills_and_optional() {
    let candidates = official_skill_path_candidates("creative/comfyui");
    assert_eq!(
        candidates,
        vec![
            "skills/creative/comfyui".to_string(),
            "optional-skills/creative/comfyui".to_string(),
        ]
    );

    let rooted = official_skill_path_candidates("optional-skills/security/1password");
    assert_eq!(
        rooted,
        vec!["optional-skills/security/1password".to_string()]
    );
}

#[test]
fn test_mattpocock_default_skill_tap_present_in_merged_list() {
    let merged = merged_skill_taps(&[]);
    assert!(merged
        .iter()
        .any(|tap| tap == "https://github.com/mattpocock/skills::skills"));
}

#[test]
fn test_merged_skill_taps_deduplicates_default() {
    let merged = merged_skill_taps(&["https://github.com/MiniMax-AI/cli::skill".to_string()]);
    assert_eq!(
        merged
            .iter()
            .filter(|tap| tap.as_str() == "https://github.com/MiniMax-AI/cli::skill")
            .count(),
        1
    );
}

#[test]
fn parse_skill_tap_spec_parses_github_url_with_override() {
    let parsed =
        parse_skill_tap_spec("https://github.com/openai/skills::skills").expect("tap parse");
    assert_eq!(parsed.repo, "openai/skills");
    assert_eq!(parsed.path, "skills");
}

#[test]
fn parse_skill_tap_spec_parses_tree_url() {
    let parsed = parse_skill_tap_spec("https://github.com/anthropics/skills/tree/main/skills")
        .expect("tap parse");
    assert_eq!(parsed.repo, "anthropics/skills");
    assert_eq!(parsed.path, "skills");
}
