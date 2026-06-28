use super::*;
use crate::test_env_lock;
use std::ffi::OsString;
use std::path::Path;
use std::sync::MutexGuard;
use tempfile::tempdir;

struct ScopedHermesHome {
    previous: Option<OsString>,
}

impl ScopedHermesHome {
    fn set(path: &Path) -> Self {
        let previous = std::env::var_os("HERMES_HOME");
        std::env::set_var("HERMES_HOME", path);
        Self { previous }
    }
}

impl Drop for ScopedHermesHome {
    fn drop(&mut self) {
        match self.previous.take() {
            Some(value) => std::env::set_var("HERMES_HOME", value),
            None => std::env::remove_var("HERMES_HOME"),
        }
    }
}

fn hermes_home_lock() -> MutexGuard<'static, ()> {
    test_env_lock::lock()
}

fn with_test_hermes_home<T>(f: impl FnOnce() -> T) -> T {
    let _lock = hermes_home_lock();
    let tmp = tempdir().expect("tempdir");
    let _home = ScopedHermesHome::set(tmp.path());
    f()
}

#[test]
fn objective_contract_roundtrip_and_counterfactual() {
    with_test_hermes_home(|| {
        let contract = upsert_objective_contract(
            "maximize reliability while keeping latency low and never skipping tests",
            false,
        )
        .expect("upsert");
        assert!(!contract.utility.terms.is_empty());
        assert!(!contract.utility.hard_constraints.is_empty());
        assert_eq!(contract.horizons.len(), 3);
        assert_eq!(
            canonical_objective_lifecycle_status(&contract.lifecycle_status),
            "active"
        );
        assert_eq!(
            canonical_objective_behavior_mode(&contract.behavior_mode),
            "balanced"
        );
        assert!(!contract.behavior_directives.is_empty());
        let updated = append_counterfactual("if we defer tests", "risk rises").expect("append");
        assert_eq!(updated.counterfactual_journal.len(), 1);
    });
}

#[test]
fn objective_lifecycle_and_behavior_updates_are_persisted() {
    with_test_hermes_home(|| {
        let initial =
            upsert_objective_contract("stabilize runtime telemetry", false).expect("objective set");
        assert_eq!(
            canonical_objective_lifecycle_status(&initial.lifecycle_status),
            "active"
        );

        let paused = set_objective_contract_lifecycle_status("pause", Some("waiting on oauth"))
            .expect("pause");
        assert_eq!(
            canonical_objective_lifecycle_status(&paused.lifecycle_status),
            "paused"
        );
        assert_eq!(paused.status_reason, "waiting on oauth");

        let strict = set_objective_contract_behavior_mode("strict").expect("behavior strict");
        assert_eq!(
            canonical_objective_behavior_mode(&strict.behavior_mode),
            "strict"
        );
        assert!(!strict.behavior_directives.is_empty());

        let mission =
            set_objective_contract_behavior_mode("sigma").expect("behavior sigma->mission");
        assert_eq!(
            canonical_objective_behavior_mode(&mission.behavior_mode),
            "mission"
        );
        assert!(mission
            .behavior_directives
            .iter()
            .any(|line| line.contains("closed-loop objective cycles")));

        let same = upsert_objective_contract("stabilize runtime telemetry", false)
            .expect("upsert same objective");
        assert_eq!(
            canonical_objective_lifecycle_status(&same.lifecycle_status),
            "paused"
        );

        let replaced =
            upsert_objective_contract("stabilize runtime telemetry plus recovery gates", false)
                .expect("upsert new objective");
        assert_eq!(
            canonical_objective_lifecycle_status(&replaced.lifecycle_status),
            "active"
        );
    });
}

#[test]
fn objective_wait_barrier_roundtrip_and_lifecycle_clear() {
    with_test_hermes_home(|| {
        upsert_objective_contract("stabilize runtime telemetry", false).expect("objective set");

        let waiting =
            set_objective_contract_wait_pid(4242, Some("build still running")).expect("wait");
        assert_eq!(waiting.waiting_on_pid, Some(4242));
        assert_eq!(waiting.waiting_reason, "build still running");
        assert!(matches!(
            objective_wait_target(&waiting),
            Some(ObjectiveWaitTarget::Pid(4242))
        ));
        let reloaded = load_objective_contract()
            .expect("load objective")
            .expect("objective present");
        assert_eq!(reloaded.waiting_on_pid, Some(4242));

        let session_wait =
            set_objective_contract_wait_session("proc_abc", Some("watcher")).expect("wait");
        assert_eq!(session_wait.waiting_on_session.as_deref(), Some("proc_abc"));
        assert!(matches!(
            objective_wait_target(&session_wait),
            Some(ObjectiveWaitTarget::Session(ref session_id)) if session_id == "proc_abc"
        ));

        let seconds_wait =
            set_objective_contract_wait_seconds(30, Some("backoff")).expect("wait seconds");
        assert!(seconds_wait.waiting_until_unix_ms > objective_now_unix_ms());
        assert!(objective_wait_remaining_seconds(&seconds_wait).unwrap_or_default() > 0);

        let paused =
            set_objective_contract_lifecycle_status("pause", Some("manual hold")).expect("pause");
        assert!(objective_wait_target(&paused).is_none());
        assert_eq!(summarize_objective_wait_barrier(&paused), "none");
    });
}

#[test]
fn objective_legacy_contract_loads_without_wait_fields() {
    with_test_hermes_home(|| {
        ensure_alpha_dir().expect("alpha dir");
        let path = objective_contract_path();
        std::fs::write(
            &path,
            serde_json::json!({
                "id": "obj-legacy",
                "created_at": "2026-01-01T00:00:00Z",
                "updated_at": "2026-01-01T00:00:00Z",
                "objective_text": "legacy objective",
                "lifecycle_status": "active",
                "status_reason": "",
                "behavior_mode": "balanced",
                "behavior_directives": [],
                "success_criteria": [],
                "utility": {"objective": "legacy", "terms": [], "hard_constraints": []},
                "horizons": [],
                "promotion_gate": {
                    "min_patch_items": 1,
                    "min_unique_files": 1,
                    "min_unique_commands": 1,
                    "require_objective_state": true
                },
                "confidence": 0.5,
                "trading_sensitive": false,
                "counterfactual_journal": []
            })
            .to_string(),
        )
        .expect("write legacy");
        let loaded = load_objective_contract()
            .expect("load objective")
            .expect("objective present");
        assert!(objective_wait_target(&loaded).is_none());
        assert_eq!(loaded.waiting_until_unix_ms, 0);
        assert!(loaded.waiting_reason.is_empty());
    });
}

#[test]
fn objective_upsert_can_infer_mission_mode_for_perpetual_objectives() {
    with_test_hermes_home(|| {
        let contract = upsert_objective_contract(
            "run this assignment in perpetuity and continuously improve output quality",
            false,
        )
        .expect("upsert mission objective");
        assert_eq!(
            canonical_objective_behavior_mode(&contract.behavior_mode),
            "mission"
        );
    });
}

#[test]
fn bootstrap_writes_runtime_files() {
    with_test_hermes_home(|| {
        let written = ensure_alpha_runtime_bootstrap(true).expect("bootstrap");
        assert!(!written.is_empty());
        assert!(alpha_state_dir().join(LOOPS_FILE).exists());
        assert!(alpha_state_dir().join(SUBAGENT_REGISTRY_FILE).exists());
        assert!(alpha_state_dir().join(CONTEXTLATTICE_POLICY_FILE).exists());
    });
}

#[test]
fn load_subagent_registry_normalizes_legacy_low_budgets() {
    with_test_hermes_home(|| {
        ensure_alpha_runtime_bootstrap(true).expect("bootstrap");
        let path = alpha_state_dir().join(SUBAGENT_REGISTRY_FILE);
        let mut registry: SubagentRegistry = read_json_file(&path).expect("read registry");
        for profile in registry.profiles.iter_mut() {
            profile.budget.max_turns = 1;
            profile.budget.max_tool_calls = 1;
            profile.budget.max_tokens = 1;
        }
        write_json_file(&path, &registry).expect("write legacy budgets");

        let normalized = load_subagent_registry().expect("load normalized registry");
        for profile in normalized.profiles {
            assert!(profile.budget.max_turns >= 48);
            assert!(profile.budget.max_tool_calls >= 120);
            assert!(profile.budget.max_tokens >= 180_000);
        }
    });
}

#[test]
fn queue_replay_is_deduplicated() {
    with_test_hermes_home(|| {
        ensure_alpha_runtime_bootstrap(true).expect("bootstrap");
        enqueue_loop_event("loop-a", "tick", "same-payload").expect("event1");
        enqueue_loop_event("loop-a", "tick", "same-payload").expect("event2");
        let replayed = replay_loop_queue(10).expect("replay");
        assert_eq!(replayed, 1);
    });
}

#[test]
fn reasoning_policy_recommends_xhigh_for_risky_terms() {
    let level =
        recommend_reasoning_level_from_text("release-critical security objective for money");
    assert_eq!(level, "xhigh");
}

#[test]
fn trading_runtime_bootstrap_and_report_refresh_work() {
    with_test_hermes_home(|| {
        ensure_trading_runtime_bootstrap(true).expect("bootstrap trading");
        let cfg = load_trading_runtime_config().expect("load trading config");
        assert!(!cfg.projects.is_empty());
        let report = refresh_trading_alpha_report().expect("refresh report");
        assert!(!report.generated_at.is_empty());
        let loaded = load_last_trading_alpha_report().expect("load report");
        assert_eq!(loaded.generated_at, report.generated_at);
    });
}

#[test]
fn trading_board_render_contains_core_sections() {
    let report = TradingAlphaReport {
        generated_at: "2026-05-06T00:00:00Z".to_string(),
        projects: vec![TradingProjectReport {
            id: "proj-a".to_string(),
            exists: true,
            objective_state: "flat".to_string(),
            incident_class: "none".to_string(),
            ..TradingProjectReport::default()
        }],
        wallet_progress_pct: 0.1,
        ruin_probability: 0.2,
        volatility_sizing_factor: 0.8,
        strategy_weights: HashMap::from([("proj-a".to_string(), 1.0)]),
        canary_recommendation: "hold-canary".to_string(),
        postmortem: "Postmortem packet".to_string(),
        promotion_candidate: "proj-a".to_string(),
        risk_governor: PortfolioRiskGovernor {
            mode: "normal".to_string(),
            ..PortfolioRiskGovernor::default()
        },
        ..TradingAlphaReport::default()
    };
    let rendered = render_trading_alpha_board(&report);
    assert!(rendered.contains("Trading Private Mission Board"));
    assert!(rendered.contains("Strategy weights"));
    assert!(rendered.contains("Capital allocator"));
    assert!(rendered.contains("Risk governor"));
    assert!(rendered.contains("Repo drift sentinel"));
    assert!(rendered.contains("Autoresearch"));
}

#[test]
fn env_provenance_detects_conflicts() {
    let tmp = tempdir().expect("tempdir");
    let root = tmp.path().join("proj");
    std::fs::create_dir_all(root.join("logs").join("knob_tuner")).expect("dirs");
    std::fs::create_dir_all(root.join("logs").join("nightly_tuner")).expect("dirs");
    std::fs::write(
        root.join(".env"),
        "REAL_ALGOTRADER_WS_BUY_GATE_ENABLED=true\nPRE_TRADE_MAX_SLIPPAGE_BPS=40\n",
    )
    .expect("write");
    std::fs::write(
        root.join("logs").join("knob_tuner").join("overrides.env"),
        "REAL_ALGOTRADER_WS_BUY_GATE_ENABLED=false\n",
    )
    .expect("write");
    let spec = TradingProjectSpec {
        id: "proj-a".to_string(),
        path: root.display().to_string(),
        enabled: true,
    };
    let gate = collect_env_provenance(&spec);
    assert!(!gate.passed);
    assert!(gate
        .conflicting_keys
        .iter()
        .any(|k| k == "REAL_ALGOTRADER_WS_BUY_GATE_ENABLED"));
}

#[test]
fn risk_governor_hard_stop_triggers_on_high_ruin() {
    let governor = compute_risk_governor(&[], 0.75, 0.10);
    assert_eq!(governor.mode, "hard-stop");
    assert!(governor.halt_new_entries);
}

#[test]
fn repo_drift_marks_missing_project() {
    let spec = TradingProjectSpec {
        id: "missing".to_string(),
        path: "/tmp/definitely-missing-hermes-ultra-alpha-project".to_string(),
        enabled: true,
    };
    let drift = collect_repo_drift(&spec, &TradingDriftBaseline::default());
    assert_eq!(drift.drift_state, "missing-project");
}

#[test]
fn objective_profile_and_policy_planes_roundtrip() {
    with_test_hermes_home(|| {
        ensure_alpha_runtime_bootstrap(true).expect("bootstrap");

        let profile = objective_profile_specialized_for("sheawinkler");
        let profile = set_objective_profile(profile).expect("set profile");
        assert_eq!(profile.profile_id, "sheawinkler");
        assert_eq!(profile.default_shell, "zsh");
        let loaded_profile = load_objective_profile().expect("load profile");
        assert_eq!(loaded_profile.profile_id, "sheawinkler");

        let sim = set_objective_simulation_mode("strict").expect("strict sim");
        assert_eq!(sim.mode, "strict");
        assert!(sim.require_shadow_pass);
        assert!(sim.require_replay_validation);
        let ensemble = set_objective_ensemble_mode("debate").expect("debate ensemble");
        assert_eq!(ensemble.mode, "debate");
        assert!(ensemble.require_disagreement_explainer);
        assert!(!ensemble.allow_fast_path_single_model);

        let ledger = append_objective_learning_entry(ObjectiveLearningLedgerEntry {
            recorded_at: String::new(),
            objective_id: "obj-demo".to_string(),
            objective_state: "advancing".to_string(),
            decision: "promote".to_string(),
            evidence_files: vec!["src/lib.rs".to_string()],
            evidence_commands: vec!["cargo test".to_string()],
            notes: "test-entry".to_string(),
        })
        .expect("append ledger");
        assert_eq!(ledger.entries.len(), 1);
        assert_eq!(ledger.entries[0].objective_id, "obj-demo");

        let generalized = reset_objective_profile_generalized().expect("reset profile");
        assert_eq!(generalized.profile_id, "repo-general");
    });
}

#[test]
fn contextlattice_policy_modes_roundtrip() {
    with_test_hermes_home(|| {
        ensure_alpha_runtime_bootstrap(true).expect("bootstrap");

        let max = set_contextlattice_policy_mode("max").expect("max mode");
        assert_eq!(max.preferred_retrieval_mode, "deep");
        assert!(max.include_retrieval_debug_for_execution);
        assert!(max.preflight_required);
        assert_eq!(max.deep_retry_budget_secs, vec![120, 180, 240]);

        let fast = set_contextlattice_policy_mode("fast").expect("fast mode");
        assert_eq!(fast.preferred_retrieval_mode, "fast");
        assert!(!fast.include_retrieval_debug_for_execution);
        assert_eq!(fast.regular_retry_budget_secs, vec![60, 90]);

        let loaded = load_contextlattice_policy().expect("load context policy");
        assert_eq!(loaded.preferred_retrieval_mode, "fast");
        assert!(loaded.required_project_scoping);
        assert!(loaded.checkpoint_payload_requires_project_file_topic);
    });
}

#[test]
fn objective_dag_claim_quorum_and_eval_surfaces_roundtrip() {
    with_test_hermes_home(|| {
        ensure_alpha_runtime_bootstrap(true).expect("bootstrap");
        upsert_objective_contract("improve objective with verified rollout", false).expect("obj");

        let dag = build_objective_dag_from_contract().expect("build dag");
        assert_eq!(dag.objective_id.starts_with("obj-"), true);
        assert!(dag.nodes.len() >= 4);
        let loaded_dag = load_objective_dag().expect("load dag");
        assert_eq!(loaded_dag.nodes.len(), dag.nodes.len());

        let claim = set_claim_verifier_enabled(false).expect("claim off");
        assert!(!claim.enabled);
        let claim = set_claim_verifier_enabled(true).expect("claim on");
        assert!(claim.enabled);

        let quorum = set_quorum_policy(
            true,
            Some(3),
            Some(vec!["nous:nousresearch/hermes-4-70b".to_string()]),
        )
        .expect("quorum");
        assert!(quorum.enabled);
        assert_eq!(quorum.voters, 3);
        assert_eq!(quorum.models.len(), 1);

        let trend = append_objective_eval_sample("obj-demo", "advancing", "test sample")
            .expect("append eval");
        assert_eq!(trend.samples.len(), 1);
        assert!(trend.samples[0].score > 0.9);

        let cleared = clear_objective_dag().expect("clear dag");
        assert!(cleared.nodes.is_empty());
    });
}
