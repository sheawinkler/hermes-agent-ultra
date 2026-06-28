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
    #[serde(default = "default_objective_lifecycle_status")]
    pub lifecycle_status: String,
    #[serde(default)]
    pub status_reason: String,
    #[serde(default = "default_objective_behavior_mode")]
    pub behavior_mode: String,
    #[serde(default = "default_objective_behavior_directives")]
    pub behavior_directives: Vec<String>,
    #[serde(default)]
    pub success_criteria: Vec<String>,
    pub utility: UtilityFunctionSpec,
    pub horizons: Vec<HorizonPlan>,
    pub promotion_gate: EvidencePromotionGate,
    pub confidence: f64,
    pub trading_sensitive: bool,
    #[serde(default)]
    pub counterfactual_journal: Vec<CounterfactualEntry>,
    #[serde(default)]
    pub waiting_on_pid: Option<u32>,
    #[serde(default)]
    pub waiting_on_session: Option<String>,
    #[serde(default)]
    pub waiting_until_unix_ms: i64,
    #[serde(default)]
    pub waiting_reason: String,
    #[serde(default)]
    pub waiting_since: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectiveWaitTarget {
    Pid(u32),
    Session(String),
    Time { until_unix_ms: i64 },
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
#[serde(default)]
pub struct ContextLatticePolicy {
    pub preflight_required: bool,
    pub auto_context_pack_on_mission_start: bool,
    pub degradation_aware_planning: bool,
    pub checkpoint_write_policy: Vec<String>,
    pub readback_verification_required: bool,
    pub shared_topic_taxonomy: Vec<String>,
    pub conflict_resolution_mode: String,
    pub include_grounding_required: bool,
    pub include_retrieval_debug_for_execution: bool,
    pub broaden_scope_on_zero_hits: bool,
    pub scoped_recency_pass_before_finalize: bool,
    pub contradiction_check_across_layers: bool,
    pub numeric_fact_verbatim_copy: bool,
    pub objective_analytics_writeback_required: bool,
    pub preferred_retrieval_mode: String,
    pub deep_retry_budget_secs: Vec<u64>,
    pub regular_retry_budget_secs: Vec<u64>,
    pub summary_sink_order: Vec<String>,
    pub required_project_scoping: bool,
    pub checkpoint_payload_requires_project_file_topic: bool,
}

impl Default for ContextLatticePolicy {
    fn default() -> Self {
        Self {
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
            include_grounding_required: true,
            include_retrieval_debug_for_execution: true,
            broaden_scope_on_zero_hits: true,
            scoped_recency_pass_before_finalize: true,
            contradiction_check_across_layers: true,
            numeric_fact_verbatim_copy: true,
            objective_analytics_writeback_required: true,
            preferred_retrieval_mode: "deep".to_string(),
            deep_retry_budget_secs: vec![120, 180, 240],
            regular_retry_budget_secs: vec![120, 180],
            summary_sink_order: vec![
                "contextlattice".to_string(),
                "github".to_string(),
                "local".to_string(),
            ],
            required_project_scoping: true,
            checkpoint_payload_requires_project_file_topic: true,
        }
    }
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
    pub preflight_line: String,
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

pub fn set_contextlattice_policy_mode(mode: &str) -> Result<ContextLatticePolicy, AgentError> {
    let mut policy = load_contextlattice_policy()?;
    match mode.trim().to_ascii_lowercase().as_str() {
        "max" | "strict" => {
            policy.preflight_required = true;
            policy.auto_context_pack_on_mission_start = true;
            policy.degradation_aware_planning = true;
            policy.readback_verification_required = true;
            policy.include_grounding_required = true;
            policy.include_retrieval_debug_for_execution = true;
            policy.broaden_scope_on_zero_hits = true;
            policy.scoped_recency_pass_before_finalize = true;
            policy.contradiction_check_across_layers = true;
            policy.numeric_fact_verbatim_copy = true;
            policy.objective_analytics_writeback_required = true;
            policy.required_project_scoping = true;
            policy.checkpoint_payload_requires_project_file_topic = true;
            policy.preferred_retrieval_mode = "deep".to_string();
            policy.deep_retry_budget_secs = vec![120, 180, 240];
            policy.regular_retry_budget_secs = vec![120, 180];
        }
        "balanced" => {
            policy.preflight_required = true;
            policy.auto_context_pack_on_mission_start = true;
            policy.degradation_aware_planning = true;
            policy.readback_verification_required = true;
            policy.include_grounding_required = true;
            policy.include_retrieval_debug_for_execution = true;
            policy.broaden_scope_on_zero_hits = true;
            policy.scoped_recency_pass_before_finalize = true;
            policy.contradiction_check_across_layers = true;
            policy.numeric_fact_verbatim_copy = true;
            policy.objective_analytics_writeback_required = true;
            policy.required_project_scoping = true;
            policy.checkpoint_payload_requires_project_file_topic = true;
            policy.preferred_retrieval_mode = "balanced".to_string();
            policy.deep_retry_budget_secs = vec![90, 120, 180];
            policy.regular_retry_budget_secs = vec![90, 120];
        }
        "speed" | "fast" => {
            policy.preflight_required = true;
            policy.auto_context_pack_on_mission_start = true;
            policy.degradation_aware_planning = true;
            policy.readback_verification_required = true;
            policy.include_grounding_required = true;
            policy.include_retrieval_debug_for_execution = false;
            policy.broaden_scope_on_zero_hits = true;
            policy.scoped_recency_pass_before_finalize = true;
            policy.contradiction_check_across_layers = true;
            policy.numeric_fact_verbatim_copy = true;
            policy.objective_analytics_writeback_required = true;
            policy.required_project_scoping = true;
            policy.checkpoint_payload_requires_project_file_topic = true;
            policy.preferred_retrieval_mode = "fast".to_string();
            policy.deep_retry_budget_secs = vec![60, 90, 120];
            policy.regular_retry_budget_secs = vec![60, 90];
        }
        _ => {
            return Err(AgentError::Config(
                "unknown contextlattice policy mode; expected one of: max|strict|balanced|fast|speed"
                    .to_string(),
            ));
        }
    }
    set_contextlattice_policy(policy)
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
    let base_url = std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL")
        .or_else(|_| std::env::var("MEMMCP_ORCHESTRATOR_URL"))
        .unwrap_or_else(|_| "http://127.0.0.1:8075".to_string())
        .trim_end_matches('/')
        .to_string();
    let health_line = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(client) => match client.get(format!("{base_url}/health")).send().await {
            Ok(resp) if resp.status().is_success() => {
                format!("contextlattice: healthy ({base_url})")
            }
            Ok(resp) => format!("contextlattice: unhealthy (status {})", resp.status()),
            Err(err) => format!("contextlattice: unreachable ({})", err),
        },
        Err(err) => format!("contextlattice: client_error ({})", err),
    };

    let preflight_line = format!(
        "contextlattice preflight: Rust-native memory write endpoint {base_url}/memory/write"
    );

    ContextLatticeStatus {
        health_line,
        preflight_line,
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
        "objective_id: {}\nlifecycle_status: {}\nbehavior_mode: {}\nwait_barrier: {}\nconfidence: {:.2}\nutility_terms: {}\nhard_constraints: {}\nhorizons: {}",
        contract.id,
        canonical_objective_lifecycle_status(&contract.lifecycle_status),
        canonical_objective_behavior_mode(&contract.behavior_mode),
        summarize_objective_wait_barrier(contract),
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
    out.push_str(&format!("- {}\n", ctx_status.preflight_line));
    out.push_str(&format!(
        "- policy: preflight={} context_pack_on_start={} degradation_aware={} readback_required={}\n",
        ctx_policy.preflight_required,
        ctx_policy.auto_context_pack_on_mission_start,
        ctx_policy.degradation_aware_planning,
        ctx_policy.readback_verification_required
    ));
    out.push_str(&format!(
        "- retrieval: mode={} grounding_required={} retrieval_debug={} broaden_scope={} recency_pass={}\n",
        ctx_policy.preferred_retrieval_mode,
        ctx_policy.include_grounding_required,
        ctx_policy.include_retrieval_debug_for_execution,
        ctx_policy.broaden_scope_on_zero_hits,
        ctx_policy.scoped_recency_pass_before_finalize
    ));
    out.push_str(&format!(
        "- integrity: contradiction_check={} numeric_verbatim={} project_scoping={} checkpoint_payload_contract={}\n",
        ctx_policy.contradiction_check_across_layers,
        ctx_policy.numeric_fact_verbatim_copy,
        ctx_policy.required_project_scoping,
        ctx_policy.checkpoint_payload_requires_project_file_topic
    ));
    out.push_str(&format!(
        "- retries: deep={:?} regular={:?} sinks={}\n\n",
        ctx_policy.deep_retry_budget_secs,
        ctx_policy.regular_retry_budget_secs,
        ctx_policy.summary_sink_order.join(",")
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

include!("alpha_runtime/trading.rs");

#[cfg(test)]
mod tests;
