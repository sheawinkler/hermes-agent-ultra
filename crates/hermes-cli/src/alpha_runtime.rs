use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn objective_contract_roundtrip_and_counterfactual() {
        let tmp = tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());
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
        std::env::remove_var("HERMES_HOME");
    }

    #[test]
    fn bootstrap_writes_runtime_files() {
        let tmp = tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());
        let written = ensure_alpha_runtime_bootstrap(true).expect("bootstrap");
        assert!(!written.is_empty());
        assert!(alpha_state_dir().join(LOOPS_FILE).exists());
        assert!(alpha_state_dir().join(SUBAGENT_REGISTRY_FILE).exists());
        assert!(alpha_state_dir().join(CONTEXTLATTICE_POLICY_FILE).exists());
        std::env::remove_var("HERMES_HOME");
    }

    #[test]
    fn queue_replay_is_deduplicated() {
        let tmp = tempdir().expect("tempdir");
        std::env::set_var("HERMES_HOME", tmp.path());
        ensure_alpha_runtime_bootstrap(true).expect("bootstrap");
        enqueue_loop_event("loop-a", "tick", "same-payload").expect("event1");
        enqueue_loop_event("loop-a", "tick", "same-payload").expect("event2");
        let replayed = replay_loop_queue(10).expect("replay");
        assert_eq!(replayed, 1);
        std::env::remove_var("HERMES_HOME");
    }

    #[test]
    fn reasoning_policy_recommends_xhigh_for_risky_terms() {
        let level =
            recommend_reasoning_level_from_text("release-critical security objective for money");
        assert_eq!(level, "xhigh");
    }
}
