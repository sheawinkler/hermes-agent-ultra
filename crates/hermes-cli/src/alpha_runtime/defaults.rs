fn default_contextlattice_policy() -> ContextLatticePolicy {
    ContextLatticePolicy::default()
}

fn default_objective_profile() -> ObjectiveProfile {
    ObjectiveProfile {
        profile_id: "repo-general".to_string(),
        updated_at: now_rfc3339(),
        operator_hint: "operator".to_string(),
        default_shell: "auto".to_string(),
        memory_backend: "contextlattice-preferred".to_string(),
        specialization_note:
            "Generalized repository profile: portable defaults with evidence-first execution."
                .to_string(),
        preferred_repos: vec![],
        preferred_languages: vec!["rust".to_string(), "python".to_string(), "go".to_string()],
    }
}

fn default_objective_lifecycle_status() -> String {
    "active".to_string()
}

fn default_objective_behavior_mode() -> String {
    "balanced".to_string()
}

fn objective_behavior_directives_for_mode(mode: &str) -> Vec<String> {
    match canonical_objective_behavior_mode(mode).as_str() {
        "mission" => vec![
            "run closed-loop objective cycles: evidence -> action -> verification -> next loop"
                .to_string(),
            "avoid status-only updates; each loop must execute at least one concrete action"
                .to_string(),
            "persist measurable deltas and objective analytics on every major turn".to_string(),
            "treat objective as continuously improvable; prefer iterative upgrades over one-shot answers"
                .to_string(),
            "escalate only on hard boundaries; otherwise keep autonomous progress".to_string(),
        ],
        "strict" => vec![
            "retrieve context before inference".to_string(),
            "verify facts from direct artifacts before claiming state".to_string(),
            "mark unresolved claims as unproven".to_string(),
            "run contradiction checks across code/process/runtime layers".to_string(),
        ],
        "autonomous" => vec![
            "proactively continue objective loops until blocked".to_string(),
            "prefer smallest reversible patches with immediate verification".to_string(),
            "only ask operator when a hard decision boundary is reached".to_string(),
            "always end loops with concrete next actions".to_string(),
        ],
        "minimal" => vec![
            "keep responses concise and action-first".to_string(),
            "avoid speculative detours".to_string(),
            "report blockers in one line plus next action".to_string(),
        ],
        _ => vec![
            "decompose objective into measurable checkpoints".to_string(),
            "prefer evidence-backed decisions over inference".to_string(),
            "verify changes before claiming completion".to_string(),
            "escalate contradictions instead of guessing".to_string(),
        ],
    }
}

fn default_objective_behavior_directives() -> Vec<String> {
    objective_behavior_directives_for_mode(&default_objective_behavior_mode())
}

pub fn objective_now_unix_ms() -> i64 {
    Utc::now().timestamp_millis()
}

fn clear_objective_wait_fields(contract: &mut ObjectiveContract) {
    contract.waiting_on_pid = None;
    contract.waiting_on_session = None;
    contract.waiting_until_unix_ms = 0;
    contract.waiting_reason.clear();
    contract.waiting_since.clear();
}

pub fn canonical_objective_lifecycle_status(status: &str) -> String {
    match status.trim().to_ascii_lowercase().as_str() {
        "active" | "pursuing" | "in_progress" | "running" => "active".to_string(),
        "paused" | "pause" => "paused".to_string(),
        "budget_limited" | "budget-limited" | "budgetlimited" | "limited" => {
            "budget_limited".to_string()
        }
        "complete" | "completed" | "achieved" | "done" | "success" => "complete".to_string(),
        "unmet" | "failed" | "blocked_terminal" => "unmet".to_string(),
        _ => "active".to_string(),
    }
}

pub fn objective_lifecycle_is_active(status: &str) -> bool {
    canonical_objective_lifecycle_status(status) == "active"
}

pub fn canonical_objective_behavior_mode(mode: &str) -> String {
    match mode.trim().to_ascii_lowercase().as_str() {
        "mission" | "sigma" | "god-tier" | "god_tier" | "godtier" | "perpetual" | "continuous" => {
            "mission".to_string()
        }
        "strict" | "evidence" | "evidence-first" => "strict".to_string(),
        "autonomous" | "proactive" | "loop" | "agentic" => "autonomous".to_string(),
        "minimal" | "concise" | "lean" => "minimal".to_string(),
        _ => "balanced".to_string(),
    }
}

fn objective_prefers_mission_mode(objective: &str) -> bool {
    let lowered = objective.to_ascii_lowercase();
    [
        "perpetuity",
        "perpetual",
        "always improve",
        "continuous improvement",
        "sigma",
        "god tier",
        "mission-driven",
        "mission driven",
        "exponentiate",
        "compound",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
}

pub fn objective_profile_specialized_for(operator_hint: &str) -> ObjectiveProfile {
    let normalized = operator_hint.trim().to_ascii_lowercase();
    if normalized == "sheawinkler" {
        return ObjectiveProfile {
            profile_id: "sheawinkler".to_string(),
            updated_at: now_rfc3339(),
            operator_hint: "sheawinkler".to_string(),
            default_shell: "zsh".to_string(),
            memory_backend: "contextlattice-primary".to_string(),
            specialization_note: "Specialized operator profile: ContextLattice-first, zsh-first, objective verification with deterministic evidence gates.".to_string(),
            preferred_repos: vec![
                "~/Documents/Projects/hermes-agent-ultra".to_string(),
                "~/Documents/Projects/algotraderv2_rust".to_string(),
                "~/Documents/Projects/fastapi-sidecar".to_string(),
            ],
            preferred_languages: vec![
                "rust".to_string(),
                "python".to_string(),
                "go".to_string(),
                "typescript".to_string(),
            ],
        };
    }
    ObjectiveProfile {
        profile_id: "operator-custom".to_string(),
        updated_at: now_rfc3339(),
        operator_hint: operator_hint.trim().to_string(),
        default_shell: "auto".to_string(),
        memory_backend: "contextlattice-preferred".to_string(),
        specialization_note: "Specialized operator profile generated from runtime command."
            .to_string(),
        preferred_repos: vec![],
        preferred_languages: vec!["rust".to_string(), "python".to_string(), "go".to_string()],
    }
}

fn default_objective_simulation_policy() -> ObjectiveSimulationPolicy {
    ObjectiveSimulationPolicy {
        mode: "balanced".to_string(),
        require_shadow_pass: true,
        min_shadow_samples: 5,
        require_replay_validation: true,
        max_live_capital_fraction: 0.25,
        updated_at: now_rfc3339(),
    }
}

fn simulation_policy_for_mode(mode: &str) -> ObjectiveSimulationPolicy {
    match mode.trim().to_ascii_lowercase().as_str() {
        "strict" => ObjectiveSimulationPolicy {
            mode: "strict".to_string(),
            require_shadow_pass: true,
            min_shadow_samples: 12,
            require_replay_validation: true,
            max_live_capital_fraction: 0.08,
            updated_at: now_rfc3339(),
        },
        "aggressive" => ObjectiveSimulationPolicy {
            mode: "aggressive".to_string(),
            require_shadow_pass: false,
            min_shadow_samples: 0,
            require_replay_validation: false,
            max_live_capital_fraction: 0.40,
            updated_at: now_rfc3339(),
        },
        _ => default_objective_simulation_policy(),
    }
}

fn default_objective_ensemble_policy() -> ObjectiveEnsemblePolicy {
    ObjectiveEnsemblePolicy {
        mode: "committee".to_string(),
        arbitration: "weighted-confidence".to_string(),
        min_voters: 2,
        require_disagreement_explainer: true,
        allow_fast_path_single_model: true,
        updated_at: now_rfc3339(),
    }
}

fn ensemble_policy_for_mode(mode: &str) -> ObjectiveEnsemblePolicy {
    match mode.trim().to_ascii_lowercase().as_str() {
        "single" => ObjectiveEnsemblePolicy {
            mode: "single".to_string(),
            arbitration: "primary-model".to_string(),
            min_voters: 1,
            require_disagreement_explainer: false,
            allow_fast_path_single_model: true,
            updated_at: now_rfc3339(),
        },
        "debate" => ObjectiveEnsemblePolicy {
            mode: "debate".to_string(),
            arbitration: "disagreement-resolution".to_string(),
            min_voters: 3,
            require_disagreement_explainer: true,
            allow_fast_path_single_model: false,
            updated_at: now_rfc3339(),
        },
        _ => default_objective_ensemble_policy(),
    }
}

fn default_objective_learning_ledger() -> ObjectiveLearningLedger {
    ObjectiveLearningLedger {
        updated_at: now_rfc3339(),
        entries: vec![],
    }
}

fn default_claim_verifier_policy() -> ClaimVerifierPolicy {
    ClaimVerifierPolicy {
        enabled: true,
        required: true,
        max_retries: 1,
        updated_at: now_rfc3339(),
    }
}

fn default_quorum_policy() -> QuorumPolicy {
    QuorumPolicy {
        enabled: false,
        voters: 3,
        models: vec![],
        mode: "adaptive-unbounded".to_string(),
        updated_at: now_rfc3339(),
    }
}

fn default_objective_eval_trend() -> ObjectiveEvalTrend {
    ObjectiveEvalTrend {
        updated_at: now_rfc3339(),
        samples: vec![],
    }
}

fn score_for_objective_state(state: &str) -> f64 {
    match state.trim().to_ascii_lowercase().as_str() {
        "advancing" => 1.0,
        "flat" => 0.5,
        "regressing" => 0.0,
        "unproven" => 0.25,
        "active" | "pursuing" => 0.6,
        "paused" => 0.45,
        "budget_limited" | "budget-limited" => 0.2,
        "complete" | "achieved" => 1.0,
        "unmet" => 0.0,
        _ => 0.4,
    }
}

fn default_subagent_registry() -> SubagentRegistry {
    SubagentRegistry {
        updated_at: now_rfc3339(),
        deterministic_lineage: true,
        contradiction_detection: true,
        durable_checkpoints: true,
        profiles: vec![
            SubagentRoleProfile {
                role: "research".to_string(),
                purpose: "read-only exploration and source synthesis".to_string(),
                skill_affinity: vec![
                    "research".to_string(),
                    "contextlattice-search".to_string(),
                    "repo-context".to_string(),
                ],
                escalation_target: "coder".to_string(),
                budget: SubagentBudgetPolicy {
                    max_turns: 64,
                    max_tool_calls: 180,
                    max_tokens: 250_000,
                },
            },
            SubagentRoleProfile {
                role: "coder".to_string(),
                purpose: "implementation and test execution".to_string(),
                skill_affinity: vec![
                    "rust".to_string(),
                    "testing".to_string(),
                    "build-system".to_string(),
                ],
                escalation_target: "release-manager".to_string(),
                budget: SubagentBudgetPolicy {
                    max_turns: 96,
                    max_tool_calls: 320,
                    max_tokens: 350_000,
                },
            },
            SubagentRoleProfile {
                role: "release-manager".to_string(),
                purpose: "gate checks, rollback policy, release readiness".to_string(),
                skill_affinity: vec![
                    "ci".to_string(),
                    "security".to_string(),
                    "release".to_string(),
                ],
                escalation_target: "operator".to_string(),
                budget: SubagentBudgetPolicy {
                    max_turns: 48,
                    max_tool_calls: 180,
                    max_tokens: 180_000,
                },
            },
        ],
    }
}

