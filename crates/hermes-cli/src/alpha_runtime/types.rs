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

