use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use chrono::{DateTime, Utc};
use hermes_core::AgentError;
use hermes_intelligence::model_metadata::{get_model_context_length, get_model_info};
use hermes_intelligence::models_dev::default_client;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::model_switch::{curated_provider_slugs, provider_model_ids};

const ALPHA_STATE_DIR: &str = "alpha";
const OBJECTIVE_CONTRACT_FILE: &str = "objective_contract.json";
const OBJECTIVE_PROFILE_FILE: &str = "objective_profile.json";
const OBJECTIVE_SIMULATION_POLICY_FILE: &str = "objective_simulation_policy.json";
const OBJECTIVE_ENSEMBLE_POLICY_FILE: &str = "objective_ensemble_policy.json";
const OBJECTIVE_LEARNING_LEDGER_FILE: &str = "objective_learning_ledger.json";
const OBJECTIVE_DAG_FILE: &str = "objective_dag.json";
const CLAIM_VERIFIER_POLICY_FILE: &str = "claim_verifier_policy.json";
const QUORUM_POLICY_FILE: &str = "quorum_policy.json";
const OBJECTIVE_EVAL_TREND_FILE: &str = "objective_eval_trend.json";
const SUBAGENT_REGISTRY_FILE: &str = "subagents.json";
const CONTEXTLATTICE_POLICY_FILE: &str = "contextlattice_policy.json";
const LOOPS_FILE: &str = "loops.json";
const LOOP_QUEUE_FILE: &str = "loop_queue.jsonl";
const LOOP_RUNTIME_FILE: &str = "loop_runtime.json";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObjectiveConstraint {
    pub expression: String,
    pub hard: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UtilityTerm {
    pub name: String,
    pub weight: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct UtilityFunctionSpec {
    pub objective: String,
    pub terms: Vec<UtilityTerm>,
    pub hard_constraints: Vec<ObjectiveConstraint>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HorizonPlan {
    pub horizon: String,
    pub goals: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvidencePromotionGate {
    pub min_patch_items: usize,
    pub min_unique_files: usize,
    pub min_unique_commands: usize,
    pub require_objective_state: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CounterfactualEntry {
    pub created_at: String,
    pub scenario: String,
    pub expected_delta: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObjectiveContract {
    pub id: String,
    pub created_at: String,
    pub updated_at: String,
    pub objective_text: String,
    pub utility: UtilityFunctionSpec,
    pub horizons: Vec<HorizonPlan>,
    pub promotion_gate: EvidencePromotionGate,
    pub confidence: f64,
    pub trading_sensitive: bool,
    #[serde(default)]
    pub counterfactual_journal: Vec<CounterfactualEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectiveProfile {
    pub profile_id: String,
    pub updated_at: String,
    pub operator_hint: String,
    pub default_shell: String,
    pub memory_backend: String,
    pub specialization_note: String,
    #[serde(default)]
    pub preferred_repos: Vec<String>,
    #[serde(default)]
    pub preferred_languages: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObjectiveSimulationPolicy {
    pub mode: String,
    pub require_shadow_pass: bool,
    pub min_shadow_samples: usize,
    pub require_replay_validation: bool,
    pub max_live_capital_fraction: f64,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectiveEnsemblePolicy {
    pub mode: String,
    pub arbitration: String,
    pub min_voters: usize,
    pub require_disagreement_explainer: bool,
    pub allow_fast_path_single_model: bool,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectiveLearningLedgerEntry {
    pub recorded_at: String,
    pub objective_id: String,
    pub objective_state: String,
    pub decision: String,
    #[serde(default)]
    pub evidence_files: Vec<String>,
    #[serde(default)]
    pub evidence_commands: Vec<String>,
    pub notes: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectiveLearningLedger {
    pub updated_at: String,
    #[serde(default)]
    pub entries: Vec<ObjectiveLearningLedgerEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectiveDagNode {
    pub id: String,
    pub title: String,
    pub status: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub rollback: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ObjectiveDag {
    pub updated_at: String,
    pub objective_id: String,
    #[serde(default)]
    pub nodes: Vec<ObjectiveDagNode>,
    pub auto_resume_checkpoint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ClaimVerifierPolicy {
    pub enabled: bool,
    pub required: bool,
    pub max_retries: u32,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct QuorumPolicy {
    pub enabled: bool,
    pub voters: usize,
    #[serde(default)]
    pub models: Vec<String>,
    pub mode: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObjectiveEvalSample {
    pub recorded_at: String,
    pub objective_id: String,
    pub objective_state: String,
    pub score: f64,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ObjectiveEvalTrend {
    pub updated_at: String,
    #[serde(default)]
    pub samples: Vec<ObjectiveEvalSample>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubagentBudgetPolicy {
    pub max_turns: u32,
    pub max_tool_calls: u32,
    pub max_tokens: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubagentRoleProfile {
    pub role: String,
    pub purpose: String,
    pub skill_affinity: Vec<String>,
    pub escalation_target: String,
    pub budget: SubagentBudgetPolicy,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SubagentRegistry {
    pub updated_at: String,
    pub deterministic_lineage: bool,
    pub contradiction_detection: bool,
    pub durable_checkpoints: bool,
    pub profiles: Vec<SubagentRoleProfile>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ContextLatticePolicy {
    pub preflight_required: bool,
    pub auto_context_pack_on_mission_start: bool,
    pub degradation_aware_planning: bool,
    pub checkpoint_write_policy: Vec<String>,
    pub readback_verification_required: bool,
    pub shared_topic_taxonomy: Vec<String>,
    pub conflict_resolution_mode: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoopDefinition {
    pub id: String,
    pub title: String,
    pub objective: String,
    pub cadence: String,
    pub target: String,
    pub enabled: bool,
    pub trading_sensitive: bool,
    #[serde(default)]
    pub steps: Vec<String>,
    #[serde(default)]
    pub alert_channels: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoopQueueEvent {
    pub id: String,
    pub created_at: String,
    pub loop_id: String,
    pub event_type: String,
    pub status: String,
    pub payload: String,
    pub fingerprint: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LoopRuntimeEntry {
    pub id: String,
    pub last_status: String,
    pub last_started_at: Option<String>,
    pub last_finished_at: Option<String>,
    pub success_count: u64,
    pub failure_count: u64,
    pub health_score: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct LoopRuntimeState {
    pub updated_at: String,
    pub loops: Vec<LoopRuntimeEntry>,
    pub queue_pending: usize,
    pub queue_replayable: usize,
    pub orphaned_events: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextLatticeStatus {
    pub health_line: String,
    pub preflight_script_line: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ProviderRouteCandidate {
    pub provider: String,
    pub model: String,
    pub score: f64,
    pub supports_tools: bool,
    pub supports_reasoning: bool,
    pub context_window: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthTokenStatus {
    pub provider: String,
    pub expires_at: String,
    pub status: String,
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

pub fn alpha_state_dir() -> PathBuf {
    hermes_config::hermes_home().join(ALPHA_STATE_DIR)
}

fn objective_contract_path() -> PathBuf {
    alpha_state_dir().join(OBJECTIVE_CONTRACT_FILE)
}

fn objective_profile_path() -> PathBuf {
    alpha_state_dir().join(OBJECTIVE_PROFILE_FILE)
}

fn objective_simulation_policy_path() -> PathBuf {
    alpha_state_dir().join(OBJECTIVE_SIMULATION_POLICY_FILE)
}

fn objective_ensemble_policy_path() -> PathBuf {
    alpha_state_dir().join(OBJECTIVE_ENSEMBLE_POLICY_FILE)
}

fn objective_learning_ledger_path() -> PathBuf {
    alpha_state_dir().join(OBJECTIVE_LEARNING_LEDGER_FILE)
}

fn objective_dag_path() -> PathBuf {
    alpha_state_dir().join(OBJECTIVE_DAG_FILE)
}

fn claim_verifier_policy_path() -> PathBuf {
    alpha_state_dir().join(CLAIM_VERIFIER_POLICY_FILE)
}

fn quorum_policy_path() -> PathBuf {
    alpha_state_dir().join(QUORUM_POLICY_FILE)
}

fn objective_eval_trend_path() -> PathBuf {
    alpha_state_dir().join(OBJECTIVE_EVAL_TREND_FILE)
}

fn subagent_registry_path() -> PathBuf {
    alpha_state_dir().join(SUBAGENT_REGISTRY_FILE)
}

fn contextlattice_policy_path() -> PathBuf {
    alpha_state_dir().join(CONTEXTLATTICE_POLICY_FILE)
}

fn loops_path() -> PathBuf {
    alpha_state_dir().join(LOOPS_FILE)
}

fn loop_queue_path() -> PathBuf {
    alpha_state_dir().join(LOOP_QUEUE_FILE)
}

fn loop_runtime_path() -> PathBuf {
    alpha_state_dir().join(LOOP_RUNTIME_FILE)
}

fn ensure_alpha_dir() -> Result<(), AgentError> {
    let dir = alpha_state_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| AgentError::Io(format!("failed to create {}: {}", dir.display(), e)))
}

fn write_json_file<T: Serialize>(path: &Path, value: &T) -> Result<(), AgentError> {
    let serialized = serde_json::to_string_pretty(value)
        .map_err(|e| AgentError::Config(format!("serialize {} failed: {}", path.display(), e)))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("failed to create {}: {}", parent.display(), e)))?;
    }
    std::fs::write(path, serialized)
        .map_err(|e| AgentError::Io(format!("write {} failed: {}", path.display(), e)))
}

fn read_json_file<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, AgentError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {} failed: {}", path.display(), e)))?;
    serde_json::from_str::<T>(&raw)
        .map_err(|e| AgentError::Config(format!("parse {} failed: {}", path.display(), e)))
}

fn default_contextlattice_policy() -> ContextLatticePolicy {
    ContextLatticePolicy {
        preflight_required: true,
        auto_context_pack_on_mission_start: true,
        degradation_aware_planning: true,
        checkpoint_write_policy: vec![
            "plan_started".to_string(),
            "implementation_checkpoint".to_string(),
            "verification_complete".to_string(),
            "final_readback_verified".to_string(),
        ],
        readback_verification_required: true,
        shared_topic_taxonomy: vec![
            "runbooks/alpha".to_string(),
            "runbooks/alpha/objective".to_string(),
            "runbooks/alpha/loops".to_string(),
            "runbooks/alpha/provider".to_string(),
        ],
        conflict_resolution_mode: "source_weight_then_recency".to_string(),
    }
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
        mode: "optional-deep".to_string(),
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
                    max_turns: 24,
                    max_tool_calls: 72,
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
                    max_turns: 48,
                    max_tool_calls: 120,
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
                    max_turns: 20,
                    max_tool_calls: 50,
                    max_tokens: 180_000,
                },
            },
        ],
    }
}

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
    let counterfactual_journal = existing
        .as_ref()
        .map(|v| v.counterfactual_journal.clone())
        .unwrap_or_default();
    let contract = ObjectiveContract {
        id: objective_id(objective_text),
        created_at,
        updated_at: now_rfc3339(),
        objective_text: objective_text.trim().to_string(),
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
        policy.voters = v.clamp(2, 5);
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
    read_json_file(&subagent_registry_path())
}

pub fn load_contextlattice_policy() -> Result<ContextLatticePolicy, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    read_json_file(&contextlattice_policy_path())
}

pub fn enqueue_loop_event(
    loop_id: &str,
    event_type: &str,
    payload: &str,
) -> Result<LoopQueueEvent, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    let source = format!(
        "{}|{}|{}|{}",
        loop_id.trim(),
        event_type.trim(),
        payload.trim(),
        now_rfc3339()
    );
    let digest = Sha256::digest(source.as_bytes());
    let id = format!("evt-{}", &hex::encode(digest)[..12]);
    let fingerprint = hex::encode(Sha256::digest(
        format!(
            "{}|{}|{}",
            loop_id.trim(),
            event_type.trim(),
            payload.trim()
        )
        .as_bytes(),
    ));
    let event = LoopQueueEvent {
        id,
        created_at: now_rfc3339(),
        loop_id: loop_id.trim().to_string(),
        event_type: event_type.trim().to_string(),
        status: "queued".to_string(),
        payload: payload.trim().to_string(),
        fingerprint,
    };
    let line = serde_json::to_string(&event)
        .map_err(|e| AgentError::Config(format!("serialize queue event failed: {}", e)))?;
    use std::io::Write as _;
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(loop_queue_path())
        .map_err(|e| AgentError::Io(format!("open queue file failed: {}", e)))?;
    writeln!(file, "{}", line)
        .map_err(|e| AgentError::Io(format!("append queue failed: {}", e)))?;
    Ok(event)
}

fn load_queue_events() -> Result<Vec<LoopQueueEvent>, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    let path = loop_queue_path();
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| AgentError::Io(format!("read {} failed: {}", path.display(), e)))?;
    let mut events = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Ok(ev) = serde_json::from_str::<LoopQueueEvent>(line) {
            events.push(ev);
        }
    }
    Ok(events)
}

fn write_queue_events(events: &[LoopQueueEvent]) -> Result<(), AgentError> {
    let serialized = events
        .iter()
        .map(serde_json::to_string)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| AgentError::Config(format!("serialize queue events failed: {}", e)))?
        .join("\n");
    let body = if serialized.is_empty() {
        String::new()
    } else {
        format!("{}\n", serialized)
    };
    std::fs::write(loop_queue_path(), body)
        .map_err(|e| AgentError::Io(format!("write queue failed: {}", e)))
}

pub fn recover_orphan_loop_events(max_age_secs: i64) -> Result<usize, AgentError> {
    let mut events = load_queue_events()?;
    let now = Utc::now();
    let mut updated = 0usize;
    for ev in &mut events {
        if ev.status != "running" {
            continue;
        }
        if let Ok(ts) = DateTime::parse_from_rfc3339(&ev.created_at) {
            if (now - ts.with_timezone(&Utc)).num_seconds() > max_age_secs {
                ev.status = "orphaned".to_string();
                updated = updated.saturating_add(1);
            }
        }
    }
    if updated > 0 {
        write_queue_events(&events)?;
    }
    Ok(updated)
}

pub fn replay_loop_queue(limit: usize) -> Result<usize, AgentError> {
    let mut events = load_queue_events()?;
    let mut seen = HashSet::new();
    let mut replayed = 0usize;
    for ev in &mut events {
        if replayed >= limit {
            break;
        }
        if ev.status != "queued" && ev.status != "orphaned" {
            continue;
        }
        if !seen.insert(ev.fingerprint.clone()) {
            ev.status = "deduped".to_string();
            continue;
        }
        ev.status = "replayed".to_string();
        replayed = replayed.saturating_add(1);
    }
    write_queue_events(&events)?;
    Ok(replayed)
}

pub fn refresh_loop_runtime_state(
    loops: &[LoopDefinition],
    background_counts: (usize, usize, usize, usize),
) -> Result<LoopRuntimeState, AgentError> {
    let (queued, running, completed, failed) = background_counts;
    let events = load_queue_events().unwrap_or_default();
    let queue_pending = events.iter().filter(|ev| ev.status == "queued").count();
    let queue_replayable = events
        .iter()
        .filter(|ev| ev.status == "queued" || ev.status == "orphaned")
        .count();
    let orphaned_events = events.iter().filter(|ev| ev.status == "orphaned").count();

    let mut loop_entries = Vec::with_capacity(loops.len());
    for lp in loops {
        let total = completed.saturating_add(failed).max(1);
        let health_score = ((completed as f64) / (total as f64)).clamp(0.0, 1.0);
        let status = if failed > completed {
            "degraded"
        } else if running > 0 {
            "running"
        } else if queued > 0 {
            "queued"
        } else {
            "healthy"
        };
        loop_entries.push(LoopRuntimeEntry {
            id: lp.id.clone(),
            last_status: status.to_string(),
            last_started_at: None,
            last_finished_at: None,
            success_count: completed as u64,
            failure_count: failed as u64,
            health_score,
        });
    }

    let state = LoopRuntimeState {
        updated_at: now_rfc3339(),
        loops: loop_entries,
        queue_pending,
        queue_replayable,
        orphaned_events,
    };
    write_json_file(&loop_runtime_path(), &state)?;
    Ok(state)
}

pub async fn contextlattice_status() -> ContextLatticeStatus {
    let health_line = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(client) => match client.get("http://127.0.0.1:8075/health").send().await {
            Ok(resp) if resp.status().is_success() => {
                "contextlattice: healthy (127.0.0.1:8075)".to_string()
            }
            Ok(resp) => format!("contextlattice: unhealthy (status {})", resp.status()),
            Err(err) => format!("contextlattice: unreachable ({})", err),
        },
        Err(err) => format!("contextlattice: client_error ({})", err),
    };

    let script_path =
        PathBuf::from("/Users/sheawinkler/Documents/Projects/scripts/agent_orchestration.py");
    let preflight_script_line = if script_path.exists() {
        format!(
            "contextlattice preflight: available ({})",
            script_path.display()
        )
    } else {
        "contextlattice preflight: missing scripts/agent_orchestration.py".to_string()
    };

    ContextLatticeStatus {
        health_line,
        preflight_script_line,
    }
}

pub async fn provider_router_snapshot(limit: usize) -> Vec<ProviderRouteCandidate> {
    let mut rows = Vec::new();
    let client = default_client();
    client.fetch(false).await;
    let providers = curated_provider_slugs();

    for provider in providers {
        let models = provider_model_ids(provider).await;
        for model_id in models.into_iter().take(3) {
            let provider_model = format!("{}:{}", provider, model_id);
            let info = get_model_info(&provider_model).or_else(|| get_model_info(&model_id));
            let supports_tools = info.as_ref().map(|i| i.supports_tools).unwrap_or(true);
            let supports_reasoning = info.as_ref().map(|i| i.supports_reasoning).unwrap_or(false);
            let context_window = get_model_context_length(&provider_model);
            let mut score = 0.0f64;
            if supports_tools {
                score += 1.0;
            }
            if supports_reasoning {
                score += 1.0;
            }
            if context_window >= 128_000 {
                score += 1.0;
            }
            if provider.eq_ignore_ascii_case("nous") {
                score += 0.25;
            }
            rows.push(ProviderRouteCandidate {
                provider: provider.to_string(),
                model: model_id,
                score,
                supports_tools,
                supports_reasoning,
                context_window,
            });
        }
    }

    rows.sort_by(|a, b| b.score.total_cmp(&a.score));
    rows.truncate(limit.max(1));
    rows
}

pub fn recommend_reasoning_level_from_text(text: &str) -> &'static str {
    let lowered = text.to_ascii_lowercase();
    if [
        "security",
        "production",
        "money",
        "trading",
        "risk",
        "parity",
        "release",
    ]
    .iter()
    .any(|needle| lowered.contains(needle))
    {
        "xhigh"
    } else if ["debug", "implement", "architecture", "investigate"]
        .iter()
        .any(|needle| lowered.contains(needle))
    {
        "high"
    } else if ["summarize", "quick", "short", "list"]
        .iter()
        .any(|needle| lowered.contains(needle))
    {
        "low"
    } else {
        "medium"
    }
}

pub fn oauth_session_sentinel() -> Vec<OAuthTokenStatus> {
    let mut out = Vec::new();
    let path = hermes_config::hermes_home()
        .join("auth")
        .join("tokens.json");
    let Ok(raw) = std::fs::read_to_string(path) else {
        return out;
    };
    let Ok(value) = serde_json::from_str::<Value>(&raw) else {
        return out;
    };

    let mut push_status = |provider: String, expires_at: String| {
        let status = DateTime::parse_from_rfc3339(&expires_at)
            .map(|ts| {
                let secs = (ts.with_timezone(&Utc) - Utc::now()).num_seconds();
                if secs < 0 {
                    "expired"
                } else if secs < 3600 {
                    "expires<1h"
                } else if secs < 86_400 {
                    "expires<24h"
                } else {
                    "ok"
                }
            })
            .unwrap_or("unknown");
        out.push(OAuthTokenStatus {
            provider,
            expires_at,
            status: status.to_string(),
        });
    };

    if let Some(obj) = value.as_object() {
        if let Some(creds) = obj.get("credentials").and_then(|v| v.as_array()) {
            for cred in creds {
                let provider = cred
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let expires = cred
                    .get("expires_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !expires.is_empty() {
                    push_status(provider, expires);
                }
            }
        } else {
            for (provider, entry) in obj {
                let expires = entry
                    .get("expires_at")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                if !expires.is_empty() {
                    push_status(provider.to_string(), expires);
                }
            }
        }
    }

    out.sort_by(|a, b| a.provider.cmp(&b.provider));
    out
}

pub fn summarize_objective_contract(contract: &ObjectiveContract) -> String {
    let terms = contract
        .utility
        .terms
        .iter()
        .map(|t| format!("{}:{:.2}", t.name, t.weight))
        .collect::<Vec<_>>()
        .join(", ");
    let constraints = contract
        .utility
        .hard_constraints
        .iter()
        .map(|c| c.expression.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "objective_id: {}\nconfidence: {:.2}\nutility_terms: {}\nhard_constraints: {}\nhorizons: {}",
        contract.id,
        contract.confidence,
        terms,
        constraints,
        contract
            .horizons
            .iter()
            .map(|h| h.horizon.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    )
}

pub async fn render_mission_board(
    current_model: &str,
    session_objective: Option<&str>,
    background_counts: (usize, usize, usize, usize),
) -> Result<String, AgentError> {
    ensure_alpha_runtime_bootstrap(false)?;
    let loops = load_alpha_loops()?;
    let runtime = refresh_loop_runtime_state(&loops, background_counts)?;
    let objective = load_objective_contract()?;
    let subagents = load_subagent_registry()?;
    let ctx_policy = load_contextlattice_policy()?;
    let ctx_status = contextlattice_status().await;
    let provider_rows = provider_router_snapshot(6).await;
    let oauth_rows = oauth_session_sentinel();

    let mut out = String::new();
    out.push_str("Mission Control\n\n");
    out.push_str(&format!(
        "session_objective: {}\n",
        session_objective.unwrap_or("(none; use /objective <text>)")
    ));
    out.push_str(&format!("model: {}\n", current_model));
    out.push_str(&format!(
        "reasoning_policy_recommendation: {}\n\n",
        recommend_reasoning_level_from_text(session_objective.unwrap_or(current_model),)
    ));

    out.push_str("ContextLattice\n");
    out.push_str(&format!("- {}\n", ctx_status.health_line));
    out.push_str(&format!("- {}\n", ctx_status.preflight_script_line));
    out.push_str(&format!(
        "- policy: preflight={} context_pack_on_start={} degradation_aware={} readback_required={}\n\n",
        ctx_policy.preflight_required,
        ctx_policy.auto_context_pack_on_mission_start,
        ctx_policy.degradation_aware_planning,
        ctx_policy.readback_verification_required
    ));

    out.push_str("Objective Contract\n");
    if let Some(contract) = objective {
        out.push_str(&format!("- updated_at: {}\n", contract.updated_at));
        out.push_str(&format!(
            "- {}\n",
            summarize_objective_contract(&contract).replace('\n', " | ")
        ));
    } else {
        out.push_str("- no persisted objective contract yet\n");
    }
    out.push('\n');

    out.push_str("Subagent Runtime\n");
    out.push_str(&format!(
        "- deterministic_lineage={} durable_checkpoints={} contradiction_detection={}\n",
        subagents.deterministic_lineage,
        subagents.durable_checkpoints,
        subagents.contradiction_detection
    ));
    out.push_str(&format!(
        "- profiles={} (skill-affinity registry active)\n\n",
        subagents.profiles.len()
    ));

    out.push_str("Loop Runtime\n");
    out.push_str(&format!(
        "- loops={} queue_pending={} replayable={} orphaned={}\n",
        runtime.loops.len(),
        runtime.queue_pending,
        runtime.queue_replayable,
        runtime.orphaned_events
    ));
    for row in runtime.loops.iter().take(8) {
        out.push_str(&format!(
            "  - {} status={} health={:.2} success={} failure={}\n",
            row.id, row.last_status, row.health_score, row.success_count, row.failure_count
        ));
    }
    out.push('\n');

    out.push_str("Provider Intelligence (top candidates)\n");
    for row in provider_rows {
        out.push_str(&format!(
            "- {}:{} score={:.2} tools={} reasoning={} ctx={}\n",
            row.provider,
            row.model,
            row.score,
            row.supports_tools,
            row.supports_reasoning,
            row.context_window
        ));
    }
    out.push('\n');

    out.push_str("OAuth Sentinel\n");
    if oauth_rows.is_empty() {
        out.push_str("- no token expiry metadata detected\n");
    } else {
        for row in oauth_rows {
            out.push_str(&format!(
                "- provider={} expires_at={} status={}\n",
                row.provider, row.expires_at, row.status
            ));
        }
    }
    Ok(out)
}

pub fn utility_terms_from_contract() -> Result<HashMap<String, f64>, AgentError> {
    let Some(contract) = load_objective_contract()? else {
        return Ok(HashMap::new());
    };
    Ok(contract
        .utility
        .terms
        .iter()
        .map(|term| (term.name.clone(), term.weight))
        .collect())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TradingProjectSpec {
    pub id: String,
    pub path: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TradingRuntimeConfig {
    pub updated_at: String,
    pub target_wallet_sol: f64,
    pub starting_wallet_sol: f64,
    pub projects: Vec<TradingProjectSpec>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TradingProjectReport {
    pub id: String,
    pub path: String,
    pub exists: bool,
    pub run_context_files: usize,
    pub latest_wallet_sol: f64,
    pub latest_pnl_sol: f64,
    pub drawdown_pct: f64,
    pub volatility_score: f64,
    pub slippage_bps: f64,
    pub impact_bps: f64,
    pub fee_drag_sol: f64,
    pub funding_drag_sol: f64,
    pub latency_p50_ms: f64,
    pub latency_p95_ms: f64,
    pub reject_rate: f64,
    pub anomaly_score: f64,
    pub incident_class: String,
    pub regime: String,
    pub objective_state: String,
    pub patch_recommendations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct StrategyHypothesis {
    pub id: String,
    pub statement: String,
    pub novelty_score: f64,
    pub expected_gain_sol: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ExperimentSpec {
    pub id: String,
    pub hypothesis_id: String,
    pub metric: String,
    pub control: String,
    pub treatment: String,
    pub pass_criterion: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CapitalAllocationRow {
    pub project_id: String,
    pub target_weight: f64,
    pub target_capital_sol: f64,
    pub max_loss_budget_sol: f64,
    pub throttle_factor: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PortfolioRiskGovernor {
    pub mode: String,
    pub halt_new_entries: bool,
    pub max_portfolio_drawdown_pct: f64,
    pub max_project_drawdown_pct: f64,
    pub max_ruin_probability: f64,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct CanaryPromotionStep {
    pub stage: String,
    pub passed: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RepoDriftSentinel {
    pub project_id: String,
    pub git_head: String,
    pub baseline_head: String,
    pub dirty_files: usize,
    pub changed_since_baseline: bool,
    pub drift_state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RunContextAudit {
    pub project_id: String,
    pub files_scanned: usize,
    pub required_metrics_present: Vec<String>,
    pub missing_metrics: Vec<String>,
    pub passed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct EnvProvenanceGate {
    pub project_id: String,
    pub inspected_files: Vec<String>,
    pub conflicting_keys: Vec<String>,
    pub passed: bool,
    pub decision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ReplayCanaryResult {
    pub project_id: String,
    pub sample_size: usize,
    pub pass_rate: f64,
    pub decision: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct RemediationRunbookAction {
    pub project_id: String,
    pub priority: String,
    pub title: String,
    pub command: String,
    pub rationale: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct ResearchSourceIngestion {
    pub project_id: String,
    pub source: String,
    pub path: String,
    pub found: bool,
    pub items: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct TradingAlphaReport {
    pub generated_at: String,
    pub projects: Vec<TradingProjectReport>,
    pub wallet_progress_pct: f64,
    pub ruin_probability: f64,
    pub volatility_sizing_factor: f64,
    pub strategy_weights: HashMap<String, f64>,
    pub pnl_decomposition: HashMap<String, f64>,
    pub canary_recommendation: String,
    pub postmortem: String,
    pub hypotheses: Vec<StrategyHypothesis>,
    pub experiments: Vec<ExperimentSpec>,
    pub backtest_matrix: Vec<String>,
    pub walkforward_checks: Vec<String>,
    pub meta_ranking: Vec<String>,
    pub promotion_candidate: String,
    pub capital_allocator: Vec<CapitalAllocationRow>,
    pub risk_governor: PortfolioRiskGovernor,
    pub canary_pipeline: Vec<CanaryPromotionStep>,
    pub repo_drift: Vec<RepoDriftSentinel>,
    pub run_context_audits: Vec<RunContextAudit>,
    pub env_provenance: Vec<EnvProvenanceGate>,
    pub replay_canary: Vec<ReplayCanaryResult>,
    pub remediation_runbook: Vec<RemediationRunbookAction>,
    pub research_sources: Vec<ResearchSourceIngestion>,
}

fn trading_state_dir() -> PathBuf {
    alpha_state_dir().join("trading")
}

fn trading_config_path() -> PathBuf {
    trading_state_dir().join("runtime_config.json")
}

fn trading_last_report_path() -> PathBuf {
    trading_state_dir().join("last_report.json")
}

fn default_trading_projects() -> Vec<TradingProjectSpec> {
    let mut projects = vec![
        TradingProjectSpec {
            id: "algotraderv2_rust".to_string(),
            path: "~/Documents/Projects/algotraderv2_rust".to_string(),
            enabled: true,
        },
        TradingProjectSpec {
            id: "fastapi-sidecar".to_string(),
            path: "~/Documents/Projects/fastapi-sidecar".to_string(),
            enabled: true,
        },
        TradingProjectSpec {
            id: "kraken-trader".to_string(),
            path: "~/Documents/Projects/kraken-trader".to_string(),
            enabled: true,
        },
    ];

    if let Ok(raw) = std::env::var("HERMES_ALPHA_TRADING_PROJECTS") {
        let custom = raw
            .split(':')
            .map(str::trim)
            .filter(|p| !p.is_empty())
            .enumerate()
            .map(|(idx, path)| TradingProjectSpec {
                id: format!("project-{}", idx + 1),
                path: path.to_string(),
                enabled: true,
            })
            .collect::<Vec<_>>();
        if !custom.is_empty() {
            projects = custom;
        }
    }
    projects
}

fn default_trading_runtime_config() -> TradingRuntimeConfig {
    TradingRuntimeConfig {
        updated_at: now_rfc3339(),
        target_wallet_sol: 1000.0,
        starting_wallet_sol: 0.2,
        projects: default_trading_projects(),
    }
}

fn expand_home(path: &str) -> String {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            let mut p = PathBuf::from(home);
            p.push(rest);
            return p.to_string_lossy().to_string();
        }
    }
    path.to_string()
}

pub fn ensure_trading_runtime_bootstrap(force: bool) -> Result<Vec<PathBuf>, AgentError> {
    ensure_alpha_dir()?;
    let dir = trading_state_dir();
    std::fs::create_dir_all(&dir)
        .map_err(|e| AgentError::Io(format!("create {} failed: {}", dir.display(), e)))?;
    let mut written = Vec::new();

    let cfg = trading_config_path();
    if force || !cfg.exists() {
        write_json_file(&cfg, &default_trading_runtime_config())?;
        written.push(cfg);
    }

    let report = trading_last_report_path();
    if force || !report.exists() {
        write_json_file(
            &report,
            &TradingAlphaReport {
                generated_at: now_rfc3339(),
                ..TradingAlphaReport::default()
            },
        )?;
        written.push(report);
    }
    Ok(written)
}

pub fn load_trading_runtime_config() -> Result<TradingRuntimeConfig, AgentError> {
    ensure_trading_runtime_bootstrap(false)?;
    read_json_file(&trading_config_path())
}

fn discover_recent_json_files(root: &Path, limit: usize) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let logs = root.join("logs").join("run_context");
    if !logs.exists() {
        return out;
    }
    if let Ok(entries) = std::fs::read_dir(&logs) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) == Some("json") {
                out.push(path);
            }
        }
    }
    out.sort_by(|a, b| b.cmp(a));
    out.truncate(limit.max(1));
    out
}

fn find_numeric_hint(value: &Value, keys: &[&str]) -> Option<f64> {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                let key = k.to_ascii_lowercase();
                if keys.iter().any(|needle| key.contains(needle)) {
                    if let Some(n) = v.as_f64() {
                        return Some(n);
                    }
                    if let Some(n) = v.as_i64() {
                        return Some(n as f64);
                    }
                    if let Some(n) = v.as_u64() {
                        return Some(n as f64);
                    }
                }
                if let Some(found) = find_numeric_hint(v, keys) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(arr) => arr.iter().find_map(|v| find_numeric_hint(v, keys)),
        _ => None,
    }
}

fn compute_regime(volatility: f64, pnl: f64, reject_rate: f64) -> String {
    if reject_rate > 0.10 {
        "execution-stressed".to_string()
    } else if volatility > 1.2 {
        if pnl >= 0.0 {
            "high-vol-trending".to_string()
        } else {
            "high-vol-adverse".to_string()
        }
    } else if volatility < 0.35 {
        "low-vol-range".to_string()
    } else {
        "neutral".to_string()
    }
}

fn compute_incident_class(reject_rate: f64, latency_p95: f64, anomaly_score: f64) -> String {
    if anomaly_score > 0.75 {
        "critical-anomaly".to_string()
    } else if reject_rate > 0.08 {
        "order-reject-spike".to_string()
    } else if latency_p95 > 1800.0 {
        "latency-degradation".to_string()
    } else {
        "none".to_string()
    }
}

fn analyze_project(spec: &TradingProjectSpec) -> TradingProjectReport {
    let path = PathBuf::from(expand_home(&spec.path));
    let exists = path.exists();
    if !exists {
        return TradingProjectReport {
            id: spec.id.clone(),
            path: path.display().to_string(),
            exists: false,
            objective_state: "unproven".to_string(),
            incident_class: "missing-project-path".to_string(),
            patch_recommendations: vec![
                "verify project path and mount".to_string(),
                "re-run /mission trading refresh".to_string(),
            ],
            ..TradingProjectReport::default()
        };
    }

    let files = discover_recent_json_files(&path, 20);
    let mut wallet_values = Vec::new();
    let mut pnl_values = Vec::new();
    let mut latency_values = Vec::new();
    let mut reject_values = Vec::new();
    let mut slip_values = Vec::new();
    let mut fee_values = Vec::new();
    let mut funding_values = Vec::new();
    let mut impact_values = Vec::new();

    for file in &files {
        if let Ok(raw) = std::fs::read_to_string(file) {
            if let Ok(v) = serde_json::from_str::<Value>(&raw) {
                if let Some(n) = find_numeric_hint(
                    &v,
                    &[
                        "wallet_sol",
                        "wallet_balance_sol",
                        "sol_balance",
                        "sol_wallet",
                    ],
                ) {
                    wallet_values.push(n);
                }
                if let Some(n) = find_numeric_hint(&v, &["pnl_sol", "profit_sol", "net_sol"]) {
                    pnl_values.push(n);
                }
                if let Some(n) =
                    find_numeric_hint(&v, &["latency_ms", "latency_p95_ms", "latency_p50_ms"])
                {
                    latency_values.push(n);
                }
                if let Some(n) = find_numeric_hint(&v, &["reject_rate", "rejects_pct", "reject"]) {
                    reject_values.push(n);
                }
                if let Some(n) = find_numeric_hint(&v, &["slippage_bps", "slip_bps"]) {
                    slip_values.push(n);
                }
                if let Some(n) = find_numeric_hint(&v, &["impact_bps", "price_impact_bps"]) {
                    impact_values.push(n);
                }
                if let Some(n) = find_numeric_hint(&v, &["fee_sol", "fees_sol", "fee_drag_sol"]) {
                    fee_values.push(n);
                }
                if let Some(n) = find_numeric_hint(&v, &["funding_sol", "funding_drag_sol"]) {
                    funding_values.push(n);
                }
            }
        }
    }

    let latest_wallet = wallet_values.first().copied().unwrap_or(0.0);
    let latest_pnl = pnl_values.first().copied().unwrap_or(0.0);
    let peak_wallet = wallet_values.iter().copied().fold(latest_wallet, f64::max);
    let drawdown_pct = if peak_wallet > 0.0 {
        ((peak_wallet - latest_wallet) / peak_wallet).clamp(0.0, 1.0)
    } else {
        0.0
    };

    let mut volatility_score = 0.0f64;
    if pnl_values.len() >= 2 {
        let mean = pnl_values.iter().sum::<f64>() / (pnl_values.len() as f64);
        let var = pnl_values
            .iter()
            .map(|v| {
                let d = *v - mean;
                d * d
            })
            .sum::<f64>()
            / (pnl_values.len() as f64);
        volatility_score = var.sqrt().abs();
    }

    let latency_p95 = latency_values
        .iter()
        .copied()
        .fold(0.0f64, f64::max)
        .max(0.0);
    let mut latency_sorted = latency_values.clone();
    latency_sorted.sort_by(|a, b| a.total_cmp(b));
    let latency_p50 = if latency_sorted.is_empty() {
        0.0
    } else {
        latency_sorted[latency_sorted.len() / 2]
    };
    let reject_rate = reject_values
        .first()
        .copied()
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let slippage_bps = slip_values.first().copied().unwrap_or(0.0).abs();
    let impact_bps = impact_values.first().copied().unwrap_or(0.0).abs();
    let fee_drag = fee_values.first().copied().unwrap_or(0.0).abs();
    let funding_drag = funding_values.first().copied().unwrap_or(0.0).abs();

    let anomaly_score = (drawdown_pct * 0.45
        + reject_rate * 0.25
        + (slippage_bps / 100.0).min(0.2)
        + (latency_p95 / 4000.0).min(0.1))
    .clamp(0.0, 1.0);

    let regime = compute_regime(volatility_score, latest_pnl, reject_rate);
    let incident = compute_incident_class(reject_rate, latency_p95, anomaly_score);
    let objective_state = if latest_pnl > 0.0 && drawdown_pct < 0.2 {
        "advancing".to_string()
    } else if latest_pnl < 0.0 && drawdown_pct > 0.3 {
        "regressing".to_string()
    } else {
        "flat".to_string()
    };

    let mut recommendations = Vec::new();
    if slippage_bps > 50.0 {
        recommendations.push("tighten max slippage + add venue spread gate".to_string());
    }
    if reject_rate > 0.06 {
        recommendations.push("lower order aggressiveness and add retry backoff".to_string());
    }
    if drawdown_pct > 0.25 {
        recommendations.push("activate drawdown circuit breaker and reduce size".to_string());
    }
    if recommendations.is_empty() {
        recommendations.push("maintain current controls; continue telemetry burn-in".to_string());
    }

    TradingProjectReport {
        id: spec.id.clone(),
        path: path.display().to_string(),
        exists: true,
        run_context_files: files.len(),
        latest_wallet_sol: latest_wallet,
        latest_pnl_sol: latest_pnl,
        drawdown_pct,
        volatility_score,
        slippage_bps,
        impact_bps,
        fee_drag_sol: fee_drag,
        funding_drag_sol: funding_drag,
        latency_p50_ms: latency_p50,
        latency_p95_ms: latency_p95,
        reject_rate,
        anomaly_score,
        incident_class: incident,
        regime,
        objective_state,
        patch_recommendations: recommendations,
    }
}

fn derive_hypotheses(report: &TradingAlphaReport) -> Vec<StrategyHypothesis> {
    let mut out = Vec::new();
    for project in &report.projects {
        if project.slippage_bps > 30.0 {
            out.push(StrategyHypothesis {
                id: format!("hyp-{}-slippage", project.id),
                statement: format!(
                    "{}: routing/quote freshness is degrading; tighter slippage controls should improve expectancy",
                    project.id
                ),
                novelty_score: (project.slippage_bps / 200.0).clamp(0.05, 1.0),
                expected_gain_sol: (project.slippage_bps / 1000.0).clamp(0.001, 0.25),
            });
        }
        if project.reject_rate > 0.04 {
            out.push(StrategyHypothesis {
                id: format!("hyp-{}-rejects", project.id),
                statement: format!(
                    "{}: reject spikes imply stale sizing/latency assumptions; adaptive order cadence may recover PnL",
                    project.id
                ),
                novelty_score: (project.reject_rate * 8.0).clamp(0.05, 1.0),
                expected_gain_sol: (project.reject_rate * 0.6).clamp(0.001, 0.25),
            });
        }
        if project.drawdown_pct > 0.2 {
            out.push(StrategyHypothesis {
                id: format!("hyp-{}-drawdown", project.id),
                statement: format!(
                    "{}: drawdown profile suggests risk governor should step down exposure faster",
                    project.id
                ),
                novelty_score: project.drawdown_pct.clamp(0.05, 1.0),
                expected_gain_sol: (project.drawdown_pct * 0.4).clamp(0.001, 0.30),
            });
        }
    }
    out.sort_by(|a, b| b.novelty_score.total_cmp(&a.novelty_score));
    out.dedup_by(|a, b| a.statement == b.statement);
    out
}

fn compile_experiment_specs(hypotheses: &[StrategyHypothesis]) -> Vec<ExperimentSpec> {
    hypotheses
        .iter()
        .enumerate()
        .map(|(idx, h)| ExperimentSpec {
            id: format!("exp-{}", idx + 1),
            hypothesis_id: h.id.clone(),
            metric: "net_pnl_after_costs_sol".to_string(),
            control: "current_config".to_string(),
            treatment: format!("treatment_from_{}", h.id),
            pass_criterion: format!(
                "delta_sol > {:.4}",
                (h.expected_gain_sol * 0.35).max(0.0005)
            ),
        })
        .collect()
}

fn build_backtest_matrix(specs: &[ExperimentSpec]) -> Vec<String> {
    specs
        .iter()
        .map(|s| {
            format!(
                "{} | metric={} | control={} | treatment={} | pass={}",
                s.id, s.metric, s.control, s.treatment, s.pass_criterion
            )
        })
        .collect()
}

fn derive_walkforward_checks(projects: &[TradingProjectReport]) -> Vec<String> {
    let mut checks = vec![
        "walk-forward folds: train=30d validate=7d test=7d".to_string(),
        "leakage gate: forbid label/source overlap across folds".to_string(),
        "leakage gate: enforce timestamp monotonicity and no future joins".to_string(),
    ];
    for p in projects {
        checks.push(format!(
            "{}: run_context_audit files={} objective_state={}",
            p.id, p.run_context_files, p.objective_state
        ));
    }
    checks
}

fn rank_meta_strategies(projects: &[TradingProjectReport]) -> Vec<String> {
    let mut rows: Vec<(String, f64)> = projects
        .iter()
        .map(|p| {
            let score = p.latest_pnl_sol
                - (p.drawdown_pct * 0.6)
                - ((p.slippage_bps + p.impact_bps) / 10_000.0)
                - (p.reject_rate * 0.4);
            (p.id.clone(), score)
        })
        .collect();
    rows.sort_by(|a, b| b.1.total_cmp(&a.1));
    rows.into_iter()
        .map(|(id, score)| format!("{} score={:.6}", id, score))
        .collect()
}

fn compute_strategy_weights(projects: &[TradingProjectReport]) -> HashMap<String, f64> {
    let mut weights = HashMap::new();
    let mut raw = Vec::new();
    for p in projects {
        let score = (p.latest_pnl_sol + 0.25).max(0.01)
            * (1.0 - p.drawdown_pct).max(0.1)
            * (1.0 - p.reject_rate).max(0.1);
        raw.push((p.id.clone(), score));
    }
    let total = raw.iter().map(|(_, v)| *v).sum::<f64>().max(1e-9);
    for (id, score) in raw {
        weights.insert(id, score / total);
    }
    weights
}

fn choose_promotion_candidate(meta: &[String]) -> String {
    meta.first()
        .and_then(|line| line.split_whitespace().next())
        .unwrap_or("none")
        .to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
struct TradingDriftBaseline {
    pub updated_at: String,
    pub heads: HashMap<String, String>,
}

fn trading_drift_baseline_path() -> PathBuf {
    trading_state_dir().join("drift_baseline.json")
}

fn load_trading_drift_baseline() -> TradingDriftBaseline {
    let path = trading_drift_baseline_path();
    if !path.exists() {
        return TradingDriftBaseline::default();
    }
    read_json_file::<TradingDriftBaseline>(&path).unwrap_or_default()
}

fn write_trading_drift_baseline(baseline: &TradingDriftBaseline) {
    let _ = write_json_file(&trading_drift_baseline_path(), baseline);
}

fn run_command_capture(cwd: &Path, bin: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(bin)
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if text.is_empty() {
        None
    } else {
        Some(text)
    }
}

fn collect_repo_drift(
    spec: &TradingProjectSpec,
    baseline: &TradingDriftBaseline,
) -> RepoDriftSentinel {
    let root = PathBuf::from(expand_home(&spec.path));
    if !root.exists() {
        return RepoDriftSentinel {
            project_id: spec.id.clone(),
            drift_state: "missing-project".to_string(),
            ..RepoDriftSentinel::default()
        };
    }

    let git_head =
        run_command_capture(&root, "git", &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    let dirty_output =
        run_command_capture(&root, "git", &["status", "--short"]).unwrap_or_default();
    let dirty_files = dirty_output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    let baseline_head = baseline.heads.get(&spec.id).cloned().unwrap_or_default();
    let changed_since_baseline =
        !baseline_head.is_empty() && !git_head.is_empty() && git_head != baseline_head;
    let drift_state = if git_head.is_empty() {
        "not-a-git-repo".to_string()
    } else if dirty_files > 0 {
        "dirty-working-tree".to_string()
    } else if changed_since_baseline {
        "head-changed".to_string()
    } else {
        "stable".to_string()
    };
    RepoDriftSentinel {
        project_id: spec.id.clone(),
        git_head,
        baseline_head,
        dirty_files,
        changed_since_baseline,
        drift_state,
    }
}

fn collect_run_context_audit(project: &TradingProjectReport) -> RunContextAudit {
    let required = vec![
        "wallet_sol".to_string(),
        "pnl_sol".to_string(),
        "reject_rate".to_string(),
        "slippage_bps".to_string(),
        "latency_p95_ms".to_string(),
    ];
    let mut present = Vec::new();
    if project.latest_wallet_sol != 0.0 {
        present.push("wallet_sol".to_string());
    }
    if project.latest_pnl_sol != 0.0 {
        present.push("pnl_sol".to_string());
    }
    if project.reject_rate != 0.0 {
        present.push("reject_rate".to_string());
    }
    if project.slippage_bps != 0.0 {
        present.push("slippage_bps".to_string());
    }
    if project.latency_p95_ms != 0.0 {
        present.push("latency_p95_ms".to_string());
    }
    let missing = required
        .iter()
        .filter(|key| !present.iter().any(|p| p == *key))
        .cloned()
        .collect::<Vec<_>>();
    let passed = project.run_context_files > 0 && missing.len() <= 2;
    RunContextAudit {
        project_id: project.id.clone(),
        files_scanned: project.run_context_files,
        required_metrics_present: present,
        missing_metrics: missing,
        passed,
    }
}

fn parse_env_kv(raw: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((k, v)) = line.split_once('=') {
            out.insert(k.trim().to_string(), v.trim().to_string());
        }
    }
    out
}

fn collect_env_provenance(spec: &TradingProjectSpec) -> EnvProvenanceGate {
    let root = PathBuf::from(expand_home(&spec.path));
    if !root.exists() {
        return EnvProvenanceGate {
            project_id: spec.id.clone(),
            passed: false,
            decision: "project-missing".to_string(),
            ..EnvProvenanceGate::default()
        };
    }
    let candidates = vec![
        root.join(".env"),
        root.join("logs").join("knob_tuner").join("overrides.env"),
        root.join("logs")
            .join("nightly_tuner")
            .join("overrides.env"),
    ];
    let critical = vec![
        "RISK_CIRCUIT_MAX_CONSEC_LOSING_CLOSES",
        "REAL_ALGOTRADER_WS_BUY_GATE_ENABLED",
        "PRE_TRADE_MAX_SLIPPAGE_BPS",
        "PRE_TRADE_MIN_LIQUIDITY_USD",
        "REAL_ALGOTRADER_FAMILY_ENABLE",
    ];
    let mut seen: HashMap<String, String> = HashMap::new();
    let mut conflicts = Vec::new();
    let mut inspected = Vec::new();
    for file in candidates {
        if !file.exists() {
            continue;
        }
        inspected.push(file.display().to_string());
        let Ok(raw) = std::fs::read_to_string(&file) else {
            continue;
        };
        let kv = parse_env_kv(&raw);
        for key in &critical {
            if let Some(value) = kv.get(*key) {
                if let Some(prev) = seen.get(*key) {
                    if prev != value {
                        conflicts.push((*key).to_string());
                    }
                } else {
                    seen.insert((*key).to_string(), value.clone());
                }
            }
        }
    }
    conflicts.sort();
    conflicts.dedup();
    let passed = conflicts.is_empty();
    EnvProvenanceGate {
        project_id: spec.id.clone(),
        inspected_files: inspected,
        conflicting_keys: conflicts.clone(),
        passed,
        decision: if passed {
            "provenance-clean".to_string()
        } else {
            format!("conflicts: {}", conflicts.join(", "))
        },
    }
}

fn compute_risk_governor(
    projects: &[TradingProjectReport],
    ruin_probability: f64,
    worst_drawdown: f64,
) -> PortfolioRiskGovernor {
    let max_project_drawdown = projects
        .iter()
        .map(|p| p.drawdown_pct)
        .fold(0.0f64, f64::max);
    if ruin_probability >= 0.6 || worst_drawdown >= 0.45 {
        return PortfolioRiskGovernor {
            mode: "hard-stop".to_string(),
            halt_new_entries: true,
            max_portfolio_drawdown_pct: 0.12,
            max_project_drawdown_pct: 0.08,
            max_ruin_probability: 0.25,
            reason: "ruin_probability or drawdown exceeded hard safety envelope".to_string(),
        };
    }
    if ruin_probability >= 0.35 || max_project_drawdown >= 0.25 {
        return PortfolioRiskGovernor {
            mode: "de-risk".to_string(),
            halt_new_entries: false,
            max_portfolio_drawdown_pct: 0.18,
            max_project_drawdown_pct: 0.12,
            max_ruin_probability: 0.35,
            reason: "risk elevated; reduce exposure and tighten gates".to_string(),
        };
    }
    PortfolioRiskGovernor {
        mode: "normal".to_string(),
        halt_new_entries: false,
        max_portfolio_drawdown_pct: 0.25,
        max_project_drawdown_pct: 0.18,
        max_ruin_probability: 0.45,
        reason: "risk envelope healthy".to_string(),
    }
}

fn compute_capital_allocator(
    projects: &[TradingProjectReport],
    strategy_weights: &HashMap<String, f64>,
    current_wallet: f64,
    governor: &PortfolioRiskGovernor,
) -> Vec<CapitalAllocationRow> {
    let mode_factor = match governor.mode.as_str() {
        "hard-stop" => 0.10,
        "de-risk" => 0.55,
        _ => 1.0,
    };
    projects
        .iter()
        .map(|p| {
            let target_weight = *strategy_weights.get(&p.id).unwrap_or(&0.0);
            let throttle_factor = (1.0 - p.drawdown_pct).clamp(0.1, 1.0) * mode_factor;
            let target_capital_sol = (current_wallet * target_weight * throttle_factor).max(0.0);
            let max_loss_budget_sol =
                (target_capital_sol * governor.max_project_drawdown_pct).max(0.0);
            CapitalAllocationRow {
                project_id: p.id.clone(),
                target_weight,
                target_capital_sol,
                max_loss_budget_sol,
                throttle_factor,
            }
        })
        .collect()
}

fn compute_canary_pipeline(
    report: &TradingAlphaReport,
    governor: &PortfolioRiskGovernor,
) -> Vec<CanaryPromotionStep> {
    let stage1 = report
        .run_context_audits
        .iter()
        .all(|audit| audit.passed && audit.files_scanned > 0);
    let stage2 = report.ruin_probability <= governor.max_ruin_probability;
    let stage3 = report
        .replay_canary
        .iter()
        .all(|row| row.pass_rate >= 0.60 && row.sample_size > 0);
    vec![
        CanaryPromotionStep {
            stage: "telemetry-audit".to_string(),
            passed: stage1,
            detail: "run_context invariants and coverage checks".to_string(),
        },
        CanaryPromotionStep {
            stage: "risk-envelope".to_string(),
            passed: stage2,
            detail: format!(
                "ruin_probability {:.4} <= max {:.4}",
                report.ruin_probability, governor.max_ruin_probability
            ),
        },
        CanaryPromotionStep {
            stage: "replay-canary".to_string(),
            passed: stage3,
            detail: "fresh telemetry replay pass-rate gate".to_string(),
        },
    ]
}

fn compute_replay_canary(projects: &[TradingProjectReport]) -> Vec<ReplayCanaryResult> {
    projects
        .iter()
        .map(|p| {
            let sample = p.run_context_files.max(1);
            let quality = (1.0 - p.reject_rate).clamp(0.0, 1.0)
                * (1.0 - (p.slippage_bps / 150.0).clamp(0.0, 1.0))
                * (1.0 - p.drawdown_pct.clamp(0.0, 1.0));
            let decision = if quality >= 0.60 {
                "pass".to_string()
            } else {
                "fail".to_string()
            };
            ReplayCanaryResult {
                project_id: p.id.clone(),
                sample_size: sample,
                pass_rate: quality,
                decision,
            }
        })
        .collect()
}

fn build_remediation_runbook(
    projects: &[TradingProjectReport],
    governor: &PortfolioRiskGovernor,
) -> Vec<RemediationRunbookAction> {
    let mut out = Vec::new();
    for p in projects {
        let mut pushed = false;
        for rec in p.patch_recommendations.iter().take(2) {
            out.push(RemediationRunbookAction {
                project_id: p.id.clone(),
                priority: if p.objective_state == "regressing" {
                    "p0".to_string()
                } else {
                    "p1".to_string()
                },
                title: rec.clone(),
                command: format!(
                    "cd {} && rg -n \"slippage|reject|drawdown|risk\" src scripts",
                    p.path
                ),
                rationale: format!("incident={} regime={}", p.incident_class, p.regime),
            });
            pushed = true;
        }
        if !pushed {
            out.push(RemediationRunbookAction {
                project_id: p.id.clone(),
                priority: "p2".to_string(),
                title: "continue telemetry burn-in".to_string(),
                command: format!("cd {} && ls logs/run_context | tail -n 20", p.path),
                rationale: "no urgent remediation signals found".to_string(),
            });
        }
    }
    if governor.halt_new_entries {
        out.push(RemediationRunbookAction {
            project_id: "portfolio".to_string(),
            priority: "p0".to_string(),
            title: "halt new entries and switch to shadow-only".to_string(),
            command: "set RISK_MODE=shadow_only and disable live entries until risk recovers"
                .to_string(),
            rationale: governor.reason.clone(),
        });
    }
    out
}

fn count_files(dir: &Path, max_depth: usize) -> usize {
    if max_depth == 0 || !dir.exists() {
        return 0;
    }
    let mut total = 0usize;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                total = total.saturating_add(1);
            } else if path.is_dir() {
                total = total.saturating_add(count_files(&path, max_depth.saturating_sub(1)));
            }
        }
    }
    total
}

fn ingest_research_sources(spec: &TradingProjectSpec) -> Vec<ResearchSourceIngestion> {
    let root = PathBuf::from(expand_home(&spec.path));
    let sources = vec![
        ("run_context", root.join("logs").join("run_context"), 2usize),
        ("docs", root.join("docs"), 2usize),
        ("scripts", root.join("scripts"), 2usize),
        ("notebooks", root.join("notebooks"), 2usize),
        ("backtests", root.join("backtests"), 2usize),
    ];
    sources
        .into_iter()
        .map(|(name, path, depth)| ResearchSourceIngestion {
            project_id: spec.id.clone(),
            source: name.to_string(),
            path: path.display().to_string(),
            found: path.exists(),
            items: if path.exists() {
                count_files(&path, depth)
            } else {
                0
            },
        })
        .collect()
}

fn compute_postmortem(projects: &[TradingProjectReport]) -> String {
    let mut lines = Vec::new();
    lines.push("Postmortem packet".to_string());
    for p in projects {
        lines.push(format!(
            "- {} state={} incident={} drawdown={:.2}% pnl={:.6} sol",
            p.id,
            p.objective_state,
            p.incident_class,
            p.drawdown_pct * 100.0,
            p.latest_pnl_sol
        ));
        for r in p.patch_recommendations.iter().take(2) {
            lines.push(format!("  remediation: {}", r));
        }
    }
    lines.join("\n")
}

pub fn refresh_trading_alpha_report() -> Result<TradingAlphaReport, AgentError> {
    ensure_trading_runtime_bootstrap(false)?;
    let cfg = load_trading_runtime_config()?;
    let projects = cfg
        .projects
        .iter()
        .filter(|p| p.enabled)
        .map(analyze_project)
        .collect::<Vec<_>>();
    let active_specs = cfg
        .projects
        .iter()
        .filter(|p| p.enabled)
        .cloned()
        .collect::<Vec<_>>();

    let current_wallet = projects
        .iter()
        .map(|p| p.latest_wallet_sol)
        .fold(0.0f64, f64::max);
    let progress = if cfg.target_wallet_sol > cfg.starting_wallet_sol {
        ((current_wallet - cfg.starting_wallet_sol)
            / (cfg.target_wallet_sol - cfg.starting_wallet_sol))
            .clamp(0.0, 1.0)
    } else {
        0.0
    };
    let worst_drawdown = projects
        .iter()
        .map(|p| p.drawdown_pct)
        .fold(0.0f64, f64::max);
    let ruin_probability = (worst_drawdown * 0.85 + (1.0 - progress) * 0.15).clamp(0.0, 1.0);
    let avg_vol = if projects.is_empty() {
        0.0
    } else {
        projects.iter().map(|p| p.volatility_score).sum::<f64>() / (projects.len() as f64)
    };
    let volatility_sizing_factor = (1.0 / (1.0 + avg_vol)).clamp(0.15, 1.25);

    let strategy_weights = compute_strategy_weights(&projects);
    let signal = projects
        .iter()
        .map(|p| p.latest_pnl_sol.max(0.0))
        .sum::<f64>();
    let execution_cost = projects
        .iter()
        .map(|p| (p.slippage_bps + p.impact_bps) / 10_000.0)
        .sum::<f64>();
    let fee_cost = projects
        .iter()
        .map(|p| p.fee_drag_sol + p.funding_drag_sol)
        .sum::<f64>();
    let mut pnl_decomposition = HashMap::new();
    pnl_decomposition.insert("signal".to_string(), signal);
    pnl_decomposition.insert("execution_cost".to_string(), -execution_cost);
    pnl_decomposition.insert("fee_cost".to_string(), -fee_cost);

    let canary_recommendation = if ruin_probability > 0.45 {
        "rollback-to-shadow".to_string()
    } else if progress > 0.55 && worst_drawdown < 0.15 {
        "promote-canary".to_string()
    } else {
        "hold-canary".to_string()
    };

    let hypotheses = derive_hypotheses(&TradingAlphaReport {
        projects: projects.clone(),
        ..TradingAlphaReport::default()
    });
    let experiments = compile_experiment_specs(&hypotheses);
    let backtest_matrix = build_backtest_matrix(&experiments);
    let walkforward_checks = derive_walkforward_checks(&projects);
    let meta_ranking = rank_meta_strategies(&projects);
    let promotion_candidate = choose_promotion_candidate(&meta_ranking);
    let postmortem = compute_postmortem(&projects);
    let risk_governor = compute_risk_governor(&projects, ruin_probability, worst_drawdown);
    let capital_allocator =
        compute_capital_allocator(&projects, &strategy_weights, current_wallet, &risk_governor);
    let replay_canary = compute_replay_canary(&projects);
    let run_context_audits = projects
        .iter()
        .map(collect_run_context_audit)
        .collect::<Vec<_>>();
    let env_provenance = active_specs
        .iter()
        .map(collect_env_provenance)
        .collect::<Vec<_>>();
    let remediation_runbook = build_remediation_runbook(&projects, &risk_governor);
    let research_sources = active_specs
        .iter()
        .flat_map(ingest_research_sources)
        .collect::<Vec<_>>();

    let baseline = load_trading_drift_baseline();
    let repo_drift = active_specs
        .iter()
        .map(|spec| collect_repo_drift(spec, &baseline))
        .collect::<Vec<_>>();

    let mut next_baseline = baseline.clone();
    for drift in &repo_drift {
        if !drift.git_head.is_empty() {
            next_baseline
                .heads
                .insert(drift.project_id.clone(), drift.git_head.clone());
        }
    }
    next_baseline.updated_at = now_rfc3339();

    let mut report = TradingAlphaReport {
        generated_at: now_rfc3339(),
        projects,
        wallet_progress_pct: progress,
        ruin_probability,
        volatility_sizing_factor,
        strategy_weights,
        pnl_decomposition,
        canary_recommendation,
        postmortem,
        hypotheses,
        experiments,
        backtest_matrix,
        walkforward_checks,
        meta_ranking,
        promotion_candidate,
        capital_allocator,
        risk_governor,
        canary_pipeline: Vec::new(),
        repo_drift,
        run_context_audits,
        env_provenance,
        replay_canary,
        remediation_runbook,
        research_sources,
    };
    report.canary_pipeline = compute_canary_pipeline(&report, &report.risk_governor);

    write_trading_drift_baseline(&next_baseline);
    write_json_file(&trading_last_report_path(), &report)?;
    Ok(report)
}

pub fn load_last_trading_alpha_report() -> Result<TradingAlphaReport, AgentError> {
    ensure_trading_runtime_bootstrap(false)?;
    read_json_file(&trading_last_report_path())
}

pub fn render_trading_alpha_board(report: &TradingAlphaReport) -> String {
    let mut out = String::new();
    out.push_str("Trading Private Mission Board\n");
    out.push_str("----------------------------\n");
    out.push_str(&format!("generated_at: {}\n", report.generated_at));
    out.push_str(&format!(
        "wallet_progress_pct: {:.2}%\nruin_probability: {:.4}\nvolatility_sizing_factor: {:.4}\ncanary_recommendation: {}\npromotion_candidate: {}\n\n",
        report.wallet_progress_pct * 100.0,
        report.ruin_probability,
        report.volatility_sizing_factor,
        report.canary_recommendation,
        report.promotion_candidate
    ));

    out.push_str("Project telemetry\n");
    for p in &report.projects {
        out.push_str(&format!(
            "- {} exists={} run_context_files={} wallet={:.6} pnl={:.6} drawdown={:.2}% reject_rate={:.2}% regime={} incident={} objective_state={}\n",
            p.id,
            p.exists,
            p.run_context_files,
            p.latest_wallet_sol,
            p.latest_pnl_sol,
            p.drawdown_pct * 100.0,
            p.reject_rate * 100.0,
            p.regime,
            p.incident_class,
            p.objective_state
        ));
    }
    out.push('\n');

    out.push_str("Strategy weights\n");
    let mut weights = report
        .strategy_weights
        .iter()
        .map(|(k, v)| (k.clone(), *v))
        .collect::<Vec<_>>();
    weights.sort_by(|a, b| b.1.total_cmp(&a.1));
    for (id, w) in weights {
        out.push_str(&format!("- {}: {:.4}\n", id, w));
    }
    out.push('\n');

    out.push_str("Capital allocator\n");
    for row in &report.capital_allocator {
        out.push_str(&format!(
            "- {} weight={:.4} capital_sol={:.6} max_loss_sol={:.6} throttle={:.3}\n",
            row.project_id,
            row.target_weight,
            row.target_capital_sol,
            row.max_loss_budget_sol,
            row.throttle_factor
        ));
    }
    out.push('\n');

    out.push_str("Risk governor\n");
    out.push_str(&format!(
        "- mode={} halt_new_entries={} max_portfolio_drawdown={:.2}% max_project_drawdown={:.2}% max_ruin_probability={:.4}\n",
        report.risk_governor.mode,
        report.risk_governor.halt_new_entries,
        report.risk_governor.max_portfolio_drawdown_pct * 100.0,
        report.risk_governor.max_project_drawdown_pct * 100.0,
        report.risk_governor.max_ruin_probability
    ));
    out.push_str(&format!("  reason: {}\n\n", report.risk_governor.reason));

    out.push_str("Canary promotion pipeline\n");
    for step in &report.canary_pipeline {
        out.push_str(&format!(
            "- {} passed={} detail={}\n",
            step.stage, step.passed, step.detail
        ));
    }
    out.push('\n');

    out.push_str("Autoresearch\n");
    out.push_str(&format!(
        "- hypotheses={} experiments={} matrix_rows={}\n",
        report.hypotheses.len(),
        report.experiments.len(),
        report.backtest_matrix.len()
    ));
    for h in report.hypotheses.iter().take(4) {
        out.push_str(&format!(
            "  - {} novelty={:.3} expected_gain_sol={:.4}\n",
            h.id, h.novelty_score, h.expected_gain_sol
        ));
    }
    out.push('\n');

    out.push_str("Walk-forward + leakage defense\n");
    for line in report.walkforward_checks.iter().take(6) {
        out.push_str(&format!("- {}\n", line));
    }
    out.push('\n');
    out.push_str("Meta ranking\n");
    for line in report.meta_ranking.iter().take(6) {
        out.push_str(&format!("- {}\n", line));
    }
    out.push('\n');

    out.push_str("Repo drift sentinel\n");
    for row in &report.repo_drift {
        out.push_str(&format!(
            "- {} state={} head={} baseline={} dirty_files={} changed_since_baseline={}\n",
            row.project_id,
            row.drift_state,
            row.git_head,
            row.baseline_head,
            row.dirty_files,
            row.changed_since_baseline
        ));
    }
    out.push('\n');

    out.push_str("Run context audits\n");
    for audit in &report.run_context_audits {
        out.push_str(&format!(
            "- {} passed={} files_scanned={} missing={}\n",
            audit.project_id,
            audit.passed,
            audit.files_scanned,
            if audit.missing_metrics.is_empty() {
                "none".to_string()
            } else {
                audit.missing_metrics.join(",")
            }
        ));
    }
    out.push('\n');

    out.push_str("Env provenance gates\n");
    for gate in &report.env_provenance {
        out.push_str(&format!(
            "- {} passed={} inspected_files={} decision={}\n",
            gate.project_id,
            gate.passed,
            gate.inspected_files.len(),
            gate.decision
        ));
    }
    out.push('\n');

    out.push_str("Replay canary\n");
    for row in &report.replay_canary {
        out.push_str(&format!(
            "- {} sample_size={} pass_rate={:.3} decision={}\n",
            row.project_id, row.sample_size, row.pass_rate, row.decision
        ));
    }
    out.push('\n');

    out.push_str("Remediation runbook\n");
    for action in report.remediation_runbook.iter().take(12) {
        out.push_str(&format!(
            "- [{}] {} :: {} | {}\n",
            action.priority, action.project_id, action.title, action.command
        ));
    }
    out.push('\n');

    out.push_str("Research source ingestion\n");
    for src in report.research_sources.iter().take(24) {
        out.push_str(&format!(
            "- {}:{} found={} items={} path={}\n",
            src.project_id, src.source, src.found, src.items, src.path
        ));
    }
    out.push('\n');

    out.push_str(&report.postmortem);
    out
}

#[cfg(test)]
mod tests {
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
            let updated = append_counterfactual("if we defer tests", "risk rises").expect("append");
            assert_eq!(updated.counterfactual_journal.len(), 1);
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
    fn objective_dag_claim_quorum_and_eval_surfaces_roundtrip() {
        with_test_hermes_home(|| {
            ensure_alpha_runtime_bootstrap(true).expect("bootstrap");
            upsert_objective_contract("improve objective with verified rollout", false)
                .expect("obj");

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
}
