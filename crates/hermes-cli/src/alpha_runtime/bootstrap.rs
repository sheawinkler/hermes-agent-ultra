pub fn default_alpha_loops() -> Vec<LoopDefinition> {
    vec![
        LoopDefinition {
            id: "primary-objective-loop".to_string(),
            title: "Primary Objective Loop".to_string(),
            objective: "Continuously drive the operator primary objective with measurable gates"
                .to_string(),
            cadence: "continuous".to_string(),
            target: "objective:primary".to_string(),
            enabled: true,
            trading_sensitive: false,
            steps: vec![
                "preflight".to_string(),
                "context-pack".to_string(),
                "analyze".to_string(),
                "patch".to_string(),
                "verify".to_string(),
                "checkpoint".to_string(),
            ],
            alert_channels: vec!["tui".to_string()],
        },
        LoopDefinition {
            id: "secondary-monitor-loop".to_string(),
            title: "Secondary Monitor Loop".to_string(),
            objective: "Continuously monitor a secondary production workflow for regressions"
                .to_string(),
            cadence: "1m".to_string(),
            target: "repo:secondary".to_string(),
            enabled: true,
            trading_sensitive: false,
            steps: vec![
                "collect-metrics".to_string(),
                "compare-slo".to_string(),
                "alert-if-drift".to_string(),
            ],
            alert_channels: vec!["tui".to_string()],
        },
        LoopDefinition {
            id: "research-improvement-loop".to_string(),
            title: "Research + Improvement Loop".to_string(),
            objective: "Run continuous research and implementation recommendations".to_string(),
            cadence: "5m".to_string(),
            target: "workflow:research".to_string(),
            enabled: true,
            trading_sensitive: false,
            steps: vec![
                "scan-upstream".to_string(),
                "classify-diff".to_string(),
                "propose-patches".to_string(),
            ],
            alert_channels: vec!["tui".to_string()],
        },
    ]
}

pub fn write_default_alpha_loops(force: bool) -> Result<PathBuf, AgentError> {
    ensure_alpha_dir()?;
    let path = loops_path();
    if path.exists() && !force {
        return Ok(path);
    }
    write_json_file(&path, &default_alpha_loops())?;
    Ok(path)
}

pub fn load_alpha_loops() -> Result<Vec<LoopDefinition>, AgentError> {
    let path = write_default_alpha_loops(false)?;
    read_json_file::<Vec<LoopDefinition>>(&path)
}

pub fn ensure_alpha_runtime_bootstrap(force: bool) -> Result<Vec<PathBuf>, AgentError> {
    ensure_alpha_dir()?;
    let mut written = Vec::new();

    let loops = write_default_alpha_loops(force)?;
    written.push(loops);

    let subagent_path = subagent_registry_path();
    if force || !subagent_path.exists() {
        write_json_file(&subagent_path, &default_subagent_registry())?;
        written.push(subagent_path);
    }

    let policy_path = contextlattice_policy_path();
    if force || !policy_path.exists() {
        write_json_file(&policy_path, &default_contextlattice_policy())?;
        written.push(policy_path);
    }

    let queue_path = loop_queue_path();
    if force || !queue_path.exists() {
        std::fs::write(&queue_path, "")
            .map_err(|e| AgentError::Io(format!("write {} failed: {}", queue_path.display(), e)))?;
        written.push(queue_path);
    }

    let runtime_path = loop_runtime_path();
    if force || !runtime_path.exists() {
        write_json_file(
            &runtime_path,
            &LoopRuntimeState {
                updated_at: now_rfc3339(),
                loops: vec![],
                queue_pending: 0,
                queue_replayable: 0,
                orphaned_events: 0,
            },
        )?;
        written.push(runtime_path);
    }

    let profile_path = objective_profile_path();
    if force || !profile_path.exists() {
        write_json_file(&profile_path, &default_objective_profile())?;
        written.push(profile_path);
    }

    let sim_policy_path = objective_simulation_policy_path();
    if force || !sim_policy_path.exists() {
        write_json_file(&sim_policy_path, &default_objective_simulation_policy())?;
        written.push(sim_policy_path);
    }

    let ensemble_policy_path = objective_ensemble_policy_path();
    if force || !ensemble_policy_path.exists() {
        write_json_file(&ensemble_policy_path, &default_objective_ensemble_policy())?;
        written.push(ensemble_policy_path);
    }

    let learning_ledger_path = objective_learning_ledger_path();
    if force || !learning_ledger_path.exists() {
        write_json_file(&learning_ledger_path, &default_objective_learning_ledger())?;
        written.push(learning_ledger_path);
    }

    let dag_path = objective_dag_path();
    if force || !dag_path.exists() {
        write_json_file(
            &dag_path,
            &ObjectiveDag {
                updated_at: now_rfc3339(),
                objective_id: "none".to_string(),
                nodes: vec![],
                auto_resume_checkpoint: "none".to_string(),
            },
        )?;
        written.push(dag_path);
    }

    let claim_policy = claim_verifier_policy_path();
    if force || !claim_policy.exists() {
        write_json_file(&claim_policy, &default_claim_verifier_policy())?;
        written.push(claim_policy);
    }

    let quorum = quorum_policy_path();
    if force || !quorum.exists() {
        write_json_file(&quorum, &default_quorum_policy())?;
        written.push(quorum);
    }

    let eval_trend = objective_eval_trend_path();
    if force || !eval_trend.exists() {
        write_json_file(&eval_trend, &default_objective_eval_trend())?;
        written.push(eval_trend);
    }

    Ok(written)
}

