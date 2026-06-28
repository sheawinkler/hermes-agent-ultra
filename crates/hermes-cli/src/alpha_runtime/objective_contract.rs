fn objective_id(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    format!("obj-{}", &hex::encode(digest)[..12])
}

fn extract_hard_constraints(objective: &str) -> Vec<ObjectiveConstraint> {
    let mut out = Vec::new();
    let lowered = objective.to_ascii_lowercase();
    for needle in [
        "must", "never", "without", "do not", "<=", ">=", "max ", "min ", "strictly",
    ] {
        if lowered.contains(needle) {
            out.push(ObjectiveConstraint {
                expression: needle.to_string(),
                hard: true,
            });
        }
    }
    if out.is_empty() {
        out.push(ObjectiveConstraint {
            expression: "preserve correctness".to_string(),
            hard: true,
        });
    }
    out
}

fn extract_utility_terms(objective: &str) -> Vec<UtilityTerm> {
    let mut seen = HashSet::new();
    let mut terms = Vec::new();
    for token in objective.split(|c: char| !c.is_ascii_alphanumeric() && c != '_' && c != '-') {
        let trimmed = token.trim().to_ascii_lowercase();
        if trimmed.len() < 4 {
            continue;
        }
        if !seen.insert(trimmed.clone()) {
            continue;
        }
        let weight = if [
            "profit",
            "latency",
            "reliability",
            "safety",
            "parity",
            "accuracy",
        ]
        .contains(&trimmed.as_str())
        {
            1.25
        } else {
            1.0
        };
        terms.push(UtilityTerm {
            name: trimmed,
            weight,
        });
        if terms.len() >= 10 {
            break;
        }
    }
    if terms.is_empty() {
        terms.push(UtilityTerm {
            name: "correctness".to_string(),
            weight: 1.0,
        });
    }
    terms
}

fn build_horizons(objective: &str) -> Vec<HorizonPlan> {
    let objective = objective.trim();
    vec![
        HorizonPlan {
            horizon: "intra".to_string(),
            goals: vec![
                "collect evidence from live artifacts".to_string(),
                "ship one verified improvement".to_string(),
            ],
        },
        HorizonPlan {
            horizon: "day".to_string(),
            goals: vec![
                format!("stabilize objective track for: {}", objective),
                "run regression and policy gates".to_string(),
            ],
        },
        HorizonPlan {
            horizon: "week".to_string(),
            goals: vec![
                "maintain parity and improve capability depth".to_string(),
                "review drift and refresh loop DSL".to_string(),
            ],
        },
    ]
}

fn calibrate_confidence(objective: &str) -> f64 {
    let lowered = objective.to_ascii_lowercase();
    let mut confidence: f64 = 0.55;
    for token in [
        "verify",
        "test",
        "measurable",
        "gate",
        "evidence",
        "objective",
    ] {
        if lowered.contains(token) {
            confidence += 0.05;
        }
    }
    confidence.clamp(0.40, 0.95)
}

pub fn load_objective_contract() -> Result<Option<ObjectiveContract>, AgentError> {
    ensure_alpha_dir()?;
    let path = objective_contract_path();
    if !path.exists() {
        return Ok(None);
    }
    read_json_file::<ObjectiveContract>(&path).map(Some)
}

pub fn clear_objective_contract() -> Result<(), AgentError> {
    ensure_alpha_dir()?;
    let path = objective_contract_path();
    if path.exists() {
        std::fs::remove_file(&path)
            .map_err(|e| AgentError::Io(format!("remove {} failed: {}", path.display(), e)))?;
    }
    Ok(())
}

pub fn upsert_objective_contract(
    objective_text: &str,
    trading_sensitive: bool,
) -> Result<ObjectiveContract, AgentError> {
    ensure_alpha_dir()?;
    let existing = load_objective_contract()?;
    let created_at = existing
        .as_ref()
        .map(|v| v.created_at.clone())
        .unwrap_or_else(now_rfc3339);
    let existing_objective = existing
        .as_ref()
        .map(|v| v.objective_text.trim().to_string())
        .unwrap_or_default();
    let existing_status = existing
        .as_ref()
        .map(|v| canonical_objective_lifecycle_status(&v.lifecycle_status))
        .unwrap_or_else(default_objective_lifecycle_status);
    let lifecycle_status = if existing_objective.eq_ignore_ascii_case(objective_text.trim()) {
        existing_status
    } else {
        "active".to_string()
    };
    let status_reason = existing
        .as_ref()
        .map(|v| v.status_reason.trim().to_string())
        .unwrap_or_default();
    let inferred_behavior_mode = if objective_prefers_mission_mode(objective_text) {
        "mission".to_string()
    } else {
        default_objective_behavior_mode()
    };
    let mut behavior_mode = existing
        .as_ref()
        .map(|v| canonical_objective_behavior_mode(&v.behavior_mode))
        .unwrap_or(inferred_behavior_mode);
    if !existing_objective.eq_ignore_ascii_case(objective_text.trim())
        && behavior_mode == "balanced"
        && objective_prefers_mission_mode(objective_text)
    {
        behavior_mode = "mission".to_string();
    }
    let behavior_directives = existing
        .as_ref()
        .map(|v| {
            if v.behavior_directives.is_empty() {
                objective_behavior_directives_for_mode(&behavior_mode)
            } else {
                v.behavior_directives.clone()
            }
        })
        .unwrap_or_else(|| objective_behavior_directives_for_mode(&behavior_mode));
    let success_criteria = existing
        .as_ref()
        .map(|v| v.success_criteria.clone())
        .unwrap_or_else(|| {
            vec![
                "verified patch list with concrete file paths".to_string(),
                "objective analytics state captured with explicit metrics".to_string(),
                "contradictions either resolved or explicitly marked unproven".to_string(),
            ]
        });
    let counterfactual_journal = existing
        .as_ref()
        .map(|v| v.counterfactual_journal.clone())
        .unwrap_or_default();
    let preserve_existing_wait = existing_objective.eq_ignore_ascii_case(objective_text.trim());
    let contract = ObjectiveContract {
        id: objective_id(objective_text),
        created_at,
        updated_at: now_rfc3339(),
        objective_text: objective_text.trim().to_string(),
        lifecycle_status,
        status_reason,
        behavior_mode,
        behavior_directives,
        success_criteria,
        utility: UtilityFunctionSpec {
            objective: "maximize objective utility under hard constraints".to_string(),
            terms: extract_utility_terms(objective_text),
            hard_constraints: extract_hard_constraints(objective_text),
        },
        horizons: build_horizons(objective_text),
        promotion_gate: EvidencePromotionGate {
            min_patch_items: 2,
            min_unique_files: 5,
            min_unique_commands: 3,
            require_objective_state: true,
        },
        confidence: calibrate_confidence(objective_text),
        trading_sensitive,
        counterfactual_journal,
        waiting_on_pid: existing
            .as_ref()
            .filter(|_| preserve_existing_wait)
            .and_then(|v| v.waiting_on_pid),
        waiting_on_session: existing
            .as_ref()
            .filter(|_| preserve_existing_wait)
            .and_then(|v| v.waiting_on_session.clone())
            .filter(|v| !v.trim().is_empty()),
        waiting_until_unix_ms: existing
            .as_ref()
            .filter(|_| preserve_existing_wait)
            .map(|v| v.waiting_until_unix_ms)
            .unwrap_or_default(),
        waiting_reason: existing
            .as_ref()
            .filter(|_| preserve_existing_wait)
            .map(|v| v.waiting_reason.trim().to_string())
            .unwrap_or_default(),
        waiting_since: existing
            .as_ref()
            .filter(|_| preserve_existing_wait)
            .map(|v| v.waiting_since.trim().to_string())
            .unwrap_or_default(),
    };
    write_json_file(&objective_contract_path(), &contract)?;
    Ok(contract)
}

pub fn append_counterfactual(
    scenario: &str,
    expected_delta: &str,
) -> Result<ObjectiveContract, AgentError> {
    let mut contract = load_objective_contract()?.ok_or_else(|| {
        AgentError::Config(
            "objective contract is not initialized; run `/objective <text>` first".to_string(),
        )
    })?;
    contract.counterfactual_journal.push(CounterfactualEntry {
        created_at: now_rfc3339(),
        scenario: scenario.trim().to_string(),
        expected_delta: expected_delta.trim().to_string(),
    });
    contract.updated_at = now_rfc3339();
    write_json_file(&objective_contract_path(), &contract)?;
    Ok(contract)
}

pub fn set_objective_contract_lifecycle_status(
    status: &str,
    reason: Option<&str>,
) -> Result<ObjectiveContract, AgentError> {
    let mut contract = load_objective_contract()?.ok_or_else(|| {
        AgentError::Config(
            "objective contract is not initialized; run `/objective <text>` first".to_string(),
        )
    })?;
    contract.lifecycle_status = canonical_objective_lifecycle_status(status);
    clear_objective_wait_fields(&mut contract);
    if let Some(reason) = reason {
        let trimmed = reason.trim();
        if !trimmed.is_empty() {
            contract.status_reason = trimmed.to_string();
        }
    }
    if contract.status_reason.trim().is_empty() {
        contract.status_reason = "operator update".to_string();
    }
    contract.updated_at = now_rfc3339();
    write_json_file(&objective_contract_path(), &contract)?;
    Ok(contract)
}

pub fn set_objective_contract_wait_pid(
    pid: u32,
    reason: Option<&str>,
) -> Result<ObjectiveContract, AgentError> {
    if pid == 0 {
        return Err(AgentError::Config(
            "objective wait pid must be positive".to_string(),
        ));
    }
    let mut contract = load_objective_contract()?.ok_or_else(|| {
        AgentError::Config(
            "objective contract is not initialized; run `/objective <text>` first".to_string(),
        )
    })?;
    if !objective_lifecycle_is_active(&contract.lifecycle_status) {
        return Err(AgentError::Config(
            "objective wait requires an active objective".to_string(),
        ));
    }
    clear_objective_wait_fields(&mut contract);
    contract.waiting_on_pid = Some(pid);
    contract.waiting_reason = reason.unwrap_or("").trim().to_string();
    contract.waiting_since = now_rfc3339();
    contract.updated_at = now_rfc3339();
    write_json_file(&objective_contract_path(), &contract)?;
    Ok(contract)
}

pub fn set_objective_contract_wait_session(
    session_id: &str,
    reason: Option<&str>,
) -> Result<ObjectiveContract, AgentError> {
    let session_id = session_id.trim();
    if session_id.is_empty() {
        return Err(AgentError::Config(
            "objective wait session id cannot be empty".to_string(),
        ));
    }
    let mut contract = load_objective_contract()?.ok_or_else(|| {
        AgentError::Config(
            "objective contract is not initialized; run `/objective <text>` first".to_string(),
        )
    })?;
    if !objective_lifecycle_is_active(&contract.lifecycle_status) {
        return Err(AgentError::Config(
            "objective wait requires an active objective".to_string(),
        ));
    }
    clear_objective_wait_fields(&mut contract);
    contract.waiting_on_session = Some(session_id.to_string());
    contract.waiting_reason = reason.unwrap_or("").trim().to_string();
    contract.waiting_since = now_rfc3339();
    contract.updated_at = now_rfc3339();
    write_json_file(&objective_contract_path(), &contract)?;
    Ok(contract)
}

pub fn set_objective_contract_wait_seconds(
    seconds: u64,
    reason: Option<&str>,
) -> Result<ObjectiveContract, AgentError> {
    if seconds == 0 {
        return Err(AgentError::Config(
            "objective wait seconds must be positive".to_string(),
        ));
    }
    let mut contract = load_objective_contract()?.ok_or_else(|| {
        AgentError::Config(
            "objective contract is not initialized; run `/objective <text>` first".to_string(),
        )
    })?;
    if !objective_lifecycle_is_active(&contract.lifecycle_status) {
        return Err(AgentError::Config(
            "objective wait requires an active objective".to_string(),
        ));
    }
    let delta_ms = i64::try_from(seconds.saturating_mul(1000)).unwrap_or(i64::MAX);
    clear_objective_wait_fields(&mut contract);
    contract.waiting_until_unix_ms = objective_now_unix_ms().saturating_add(delta_ms);
    contract.waiting_reason = reason.unwrap_or("").trim().to_string();
    contract.waiting_since = now_rfc3339();
    contract.updated_at = now_rfc3339();
    write_json_file(&objective_contract_path(), &contract)?;
    Ok(contract)
}

pub fn clear_objective_contract_wait_barrier() -> Result<ObjectiveContract, AgentError> {
    let mut contract = load_objective_contract()?.ok_or_else(|| {
        AgentError::Config(
            "objective contract is not initialized; run `/objective <text>` first".to_string(),
        )
    })?;
    clear_objective_wait_fields(&mut contract);
    contract.updated_at = now_rfc3339();
    write_json_file(&objective_contract_path(), &contract)?;
    Ok(contract)
}

pub fn objective_wait_target(contract: &ObjectiveContract) -> Option<ObjectiveWaitTarget> {
    if let Some(pid) = contract.waiting_on_pid.filter(|pid| *pid > 0) {
        return Some(ObjectiveWaitTarget::Pid(pid));
    }
    if let Some(session_id) = contract
        .waiting_on_session
        .as_deref()
        .map(str::trim)
        .filter(|v| !v.is_empty())
    {
        return Some(ObjectiveWaitTarget::Session(session_id.to_string()));
    }
    if contract.waiting_until_unix_ms > 0 {
        return Some(ObjectiveWaitTarget::Time {
            until_unix_ms: contract.waiting_until_unix_ms,
        });
    }
    None
}

pub fn objective_wait_remaining_seconds(contract: &ObjectiveContract) -> Option<i64> {
    if contract.waiting_until_unix_ms <= 0 {
        return None;
    }
    Some(
        contract
            .waiting_until_unix_ms
            .saturating_sub(objective_now_unix_ms())
            .saturating_add(999)
            / 1000,
    )
}

pub fn summarize_objective_wait_barrier(contract: &ObjectiveContract) -> String {
    let reason = contract.waiting_reason.trim();
    let suffix = if reason.is_empty() {
        String::new()
    } else {
        format!(" reason={reason}")
    };
    match objective_wait_target(contract) {
        Some(ObjectiveWaitTarget::Pid(pid)) => format!("pid={pid}{suffix}"),
        Some(ObjectiveWaitTarget::Session(session_id)) => {
            format!("session_id={session_id}{suffix}")
        }
        Some(ObjectiveWaitTarget::Time { until_unix_ms }) => {
            let remaining = objective_wait_remaining_seconds(contract).unwrap_or_default();
            format!("until_unix_ms={until_unix_ms} remaining_seconds={remaining}{suffix}")
        }
        None => "none".to_string(),
    }
}

pub fn set_objective_contract_behavior_mode(mode: &str) -> Result<ObjectiveContract, AgentError> {
    let mut contract = load_objective_contract()?.ok_or_else(|| {
        AgentError::Config(
            "objective contract is not initialized; run `/objective <text>` first".to_string(),
        )
    })?;
    let canonical = canonical_objective_behavior_mode(mode);
    contract.behavior_mode = canonical.clone();
    contract.behavior_directives = objective_behavior_directives_for_mode(&canonical);
    contract.updated_at = now_rfc3339();
    write_json_file(&objective_contract_path(), &contract)?;
    Ok(contract)
}

