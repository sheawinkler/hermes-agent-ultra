pub fn load_objective_profile() -> Result<ObjectiveProfile, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    read_json_file(&objective_profile_path())
}

pub fn set_objective_profile(profile: ObjectiveProfile) -> Result<ObjectiveProfile, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    let mut updated = profile;
    updated.updated_at = now_rfc3339();
    write_json_file(&objective_profile_path(), &updated)?;
    Ok(updated)
}

pub fn reset_objective_profile_generalized() -> Result<ObjectiveProfile, AgentError> {
    set_objective_profile(default_objective_profile())
}

pub fn load_objective_simulation_policy() -> Result<ObjectiveSimulationPolicy, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    read_json_file(&objective_simulation_policy_path())
}

pub fn set_objective_simulation_mode(mode: &str) -> Result<ObjectiveSimulationPolicy, AgentError> {
    let policy = simulation_policy_for_mode(mode);
    ensure_alpha_runtime_bootstrap(false)?;
    write_json_file(&objective_simulation_policy_path(), &policy)?;
    Ok(policy)
}

pub fn load_objective_ensemble_policy() -> Result<ObjectiveEnsemblePolicy, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    read_json_file(&objective_ensemble_policy_path())
}

pub fn set_objective_ensemble_mode(mode: &str) -> Result<ObjectiveEnsemblePolicy, AgentError> {
    let policy = ensemble_policy_for_mode(mode);
    ensure_alpha_runtime_bootstrap(false)?;
    write_json_file(&objective_ensemble_policy_path(), &policy)?;
    Ok(policy)
}

pub fn load_objective_learning_ledger() -> Result<ObjectiveLearningLedger, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    read_json_file(&objective_learning_ledger_path())
}

pub fn append_objective_learning_entry(
    mut entry: ObjectiveLearningLedgerEntry,
) -> Result<ObjectiveLearningLedger, AgentError> {
    let mut ledger = load_objective_learning_ledger()?;
    if entry.recorded_at.trim().is_empty() {
        entry.recorded_at = now_rfc3339();
    }
    ledger.entries.push(entry);
    if ledger.entries.len() > 512 {
        let drain = ledger.entries.len().saturating_sub(512);
        ledger.entries.drain(0..drain);
    }
    ledger.updated_at = now_rfc3339();
    write_json_file(&objective_learning_ledger_path(), &ledger)?;
    Ok(ledger)
}

pub fn clear_objective_learning_ledger() -> Result<ObjectiveLearningLedger, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    let ledger = default_objective_learning_ledger();
    write_json_file(&objective_learning_ledger_path(), &ledger)?;
    Ok(ledger)
}

pub fn build_objective_dag_from_contract() -> Result<ObjectiveDag, AgentError> {
    let contract = load_objective_contract()?.ok_or_else(|| {
        AgentError::Config(
            "objective contract is not initialized; run `/objective <text>` first".to_string(),
        )
    })?;
    let mut nodes = Vec::new();
    nodes.push(ObjectiveDagNode {
        id: "discover".to_string(),
        title: "Discover facts and gather constraints".to_string(),
        status: "pending".to_string(),
        depends_on: vec![],
        rollback: "narrow_scope_and_reprobe".to_string(),
    });
    nodes.push(ObjectiveDagNode {
        id: "design".to_string(),
        title: "Design targeted patch strategy".to_string(),
        status: "pending".to_string(),
        depends_on: vec!["discover".to_string()],
        rollback: "re-open alternatives".to_string(),
    });
    nodes.push(ObjectiveDagNode {
        id: "implement".to_string(),
        title: "Implement smallest reversible change-set".to_string(),
        status: "pending".to_string(),
        depends_on: vec!["design".to_string()],
        rollback: "git_revert_candidate".to_string(),
    });
    nodes.push(ObjectiveDagNode {
        id: "verify".to_string(),
        title: "Verify with objective-linked tests".to_string(),
        status: "pending".to_string(),
        depends_on: vec!["implement".to_string()],
        rollback: "re-open_implementation".to_string(),
    });
    if contract.trading_sensitive {
        nodes.push(ObjectiveDagNode {
            id: "shadow".to_string(),
            title: "Shadow/simulator gate before promotion".to_string(),
            status: "pending".to_string(),
            depends_on: vec!["verify".to_string()],
            rollback: "reduce_exposure_and_rerun".to_string(),
        });
    }
    let dag = ObjectiveDag {
        updated_at: now_rfc3339(),
        objective_id: contract.id.clone(),
        nodes,
        auto_resume_checkpoint: "discover".to_string(),
    };
    write_json_file(&objective_dag_path(), &dag)?;
    Ok(dag)
}

pub fn load_objective_dag() -> Result<ObjectiveDag, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    read_json_file(&objective_dag_path())
}

pub fn clear_objective_dag() -> Result<ObjectiveDag, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    let dag = ObjectiveDag {
        updated_at: now_rfc3339(),
        objective_id: "none".to_string(),
        nodes: vec![],
        auto_resume_checkpoint: "none".to_string(),
    };
    write_json_file(&objective_dag_path(), &dag)?;
    Ok(dag)
}

pub fn load_claim_verifier_policy() -> Result<ClaimVerifierPolicy, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    read_json_file(&claim_verifier_policy_path())
}

pub fn set_claim_verifier_enabled(enabled: bool) -> Result<ClaimVerifierPolicy, AgentError> {
    let mut policy = load_claim_verifier_policy()?;
    policy.enabled = enabled;
    policy.updated_at = now_rfc3339();
    write_json_file(&claim_verifier_policy_path(), &policy)?;
    Ok(policy)
}

pub fn load_quorum_policy() -> Result<QuorumPolicy, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    read_json_file(&quorum_policy_path())
}

pub fn set_quorum_policy(
    enabled: bool,
    voters: Option<usize>,
    models: Option<Vec<String>>,
) -> Result<QuorumPolicy, AgentError> {
    let mut policy = load_quorum_policy()?;
    policy.enabled = enabled;
    if let Some(v) = voters {
        policy.voters = v.clamp(2, 8);
    }
    if let Some(m) = models {
        policy.models = m;
    }
    policy.updated_at = now_rfc3339();
    write_json_file(&quorum_policy_path(), &policy)?;
    Ok(policy)
}

pub fn load_objective_eval_trend() -> Result<ObjectiveEvalTrend, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    read_json_file(&objective_eval_trend_path())
}

pub fn append_objective_eval_sample(
    objective_id: &str,
    objective_state: &str,
    note: &str,
) -> Result<ObjectiveEvalTrend, AgentError> {
    let mut trend = load_objective_eval_trend()?;
    trend.samples.push(ObjectiveEvalSample {
        recorded_at: now_rfc3339(),
        objective_id: objective_id.trim().to_string(),
        objective_state: objective_state.trim().to_string(),
        score: score_for_objective_state(objective_state),
        note: note.trim().to_string(),
    });
    if trend.samples.len() > 512 {
        let drain = trend.samples.len().saturating_sub(512);
        trend.samples.drain(0..drain);
    }
    trend.updated_at = now_rfc3339();
    write_json_file(&objective_eval_trend_path(), &trend)?;
    Ok(trend)
}

pub fn load_subagent_registry() -> Result<SubagentRegistry, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    let path = subagent_registry_path();
    let mut registry = read_json_file::<SubagentRegistry>(&path)?;
    let mut changed = false;
    for profile in registry.profiles.iter_mut() {
        let role = profile.role.trim().to_ascii_lowercase();
        let (min_turns, min_tool_calls, min_tokens) = match role.as_str() {
            "research" => (64u32, 180u32, 250_000u32),
            "coder" => (96u32, 320u32, 350_000u32),
            "release-manager" => (48u32, 180u32, 180_000u32),
            _ => (48u32, 120u32, 180_000u32),
        };
        if profile.budget.max_turns < min_turns {
            profile.budget.max_turns = min_turns;
            changed = true;
        }
        if profile.budget.max_tool_calls < min_tool_calls {
            profile.budget.max_tool_calls = min_tool_calls;
            changed = true;
        }
        if profile.budget.max_tokens < min_tokens {
            profile.budget.max_tokens = min_tokens;
            changed = true;
        }
    }
    if changed {
        registry.updated_at = now_rfc3339();
        write_json_file(&path, &registry)?;
    }
    Ok(registry)
}

pub fn load_contextlattice_policy() -> Result<ContextLatticePolicy, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    read_json_file(&contextlattice_policy_path())
}

pub fn set_contextlattice_policy(
    policy: ContextLatticePolicy,
) -> Result<ContextLatticePolicy, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    let mut updated = policy;
    if updated.checkpoint_write_policy.is_empty() {
        updated.checkpoint_write_policy = ContextLatticePolicy::default().checkpoint_write_policy;
    }
    if updated.shared_topic_taxonomy.is_empty() {
        updated.shared_topic_taxonomy = ContextLatticePolicy::default().shared_topic_taxonomy;
    }
    if updated.deep_retry_budget_secs.is_empty() {
        updated.deep_retry_budget_secs = ContextLatticePolicy::default().deep_retry_budget_secs;
    }
    if updated.regular_retry_budget_secs.is_empty() {
        updated.regular_retry_budget_secs =
            ContextLatticePolicy::default().regular_retry_budget_secs;
    }
    if updated.summary_sink_order.is_empty() {
        updated.summary_sink_order = ContextLatticePolicy::default().summary_sink_order;
    }
    if updated.preferred_retrieval_mode.trim().is_empty() {
        updated.preferred_retrieval_mode = ContextLatticePolicy::default().preferred_retrieval_mode;
    } else {
        let normalized = updated.preferred_retrieval_mode.trim().to_ascii_lowercase();
        updated.preferred_retrieval_mode = match normalized.as_str() {
            "fast" | "balanced" | "deep" => normalized,
            _ => "deep".to_string(),
        };
    }
    write_json_file(&contextlattice_policy_path(), &updated)?;
    Ok(updated)
}

