//! Rust-native autonomy cockpit inspired by external harness product surfaces.
//!
//! This module intentionally builds on Hermes primitives instead of creating a
//! separate agent runtime: task boards are local JSON state, loop/resource/memory
//! decisions are deterministic, and dashboard/channel consumers can subscribe to
//! the structured event envelopes emitted here.

use std::collections::{BTreeMap, BTreeSet, HashMap, VecDeque};
use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use hermes_core::{tool_schema, JsonSchema, ToolError, ToolHandler, ToolSchema};

use crate::tools::ultra_autonomy_evals::{
    evaluate_outcome_loop_rehearsal, evaluate_recall_quality, OutcomeLoopRehearsalInput,
    RecallQualityInput,
};

const TOOL_NAME: &str = "ultra_autonomy";
const LOOP_IDENTICAL_THRESHOLD: usize = 4;
const LOOP_SIMILAR_FAILURE_THRESHOLD: usize = 6;
const LOOP_TEXT_REPEAT_THRESHOLD: usize = 4;
const LOOP_NO_ACTION_THRESHOLD: usize = 6;
const LOOP_ABSOLUTE_MAX: usize = 75;
const LOOP_FAILED_ABSOLUTE_MAX: usize = 20;
const DEFAULT_RAM_PER_AGENT_MB: u64 = 2048;
const DEFAULT_MIN_FREE_RAM_MB: u64 = 1024;
const DEFAULT_PER_AGENT_TOKEN_RESERVE: u64 = 32_000;

#[derive(Debug, Clone)]
pub struct UltraAutonomyState {
    root: PathBuf,
}

impl UltraAutonomyState {
    pub fn new(root: PathBuf) -> Self {
        let state = Self { root };
        state.ensure_dirs();
        state
    }

    fn ensure_dirs(&self) {
        let _ = fs::create_dir_all(self.boards_dir());
        let _ = fs::create_dir_all(self.loops_dir());
    }

    fn boards_dir(&self) -> PathBuf {
        self.root.join("boards")
    }

    fn loops_dir(&self) -> PathBuf {
        self.root.join("loops")
    }

    fn board_path(&self, id: &str) -> PathBuf {
        self.boards_dir().join(format!("{}.json", safe_id(id)))
    }

    fn loop_path(&self, session: &str) -> PathBuf {
        self.loops_dir().join(format!("{}.json", safe_id(session)))
    }
}

#[derive(Clone)]
pub struct UltraAutonomyHandler {
    state: Arc<UltraAutonomyState>,
}

impl UltraAutonomyHandler {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            state: Arc::new(UltraAutonomyState::new(data_dir.join("ultra_autonomy"))),
        }
    }
}

#[async_trait]
impl ToolHandler for UltraAutonomyHandler {
    async fn execute(&self, params: Value) -> Result<String, ToolError> {
        self.state.ensure_dirs();
        let action = params
            .get("action")
            .and_then(Value::as_str)
            .unwrap_or("status")
            .trim()
            .to_ascii_lowercase();
        let payload = match action.as_str() {
            "status" => autonomy_status_snapshot(&self.state),
            "loop_record" => record_loop_event(&self.state, &params)?,
            "loop_evaluate" => evaluate_loop_action(&self.state, &params)?,
            "board_create" => create_board(&self.state, &params)?,
            "board_add_card" => add_board_card(&self.state, &params)?,
            "board_update" => update_board_card(&self.state, &params)?,
            "board_plan" => board_plan_action(&self.state, &params)?,
            "objective_bridge" => objective_bridge_action(&self.state, &params)?,
            "resource_plan" => resource_plan_action(&params),
            "memory_lifecycle" => memory_lifecycle_action(&params),
            "memory_resolve" => memory_resolve_action(&params)?,
            "outcome_rehearsal" => outcome_rehearsal_action(&params)?,
            "recall_quality" => recall_quality_action(&params)?,
            "service_plan" => service_plan_action(),
            "channel_surface" => channel_surface_action(),
            "events" => event_catalog_action(),
            "help" => help_action(),
            other => {
                return Err(ToolError::InvalidParams(format!(
                    "unknown ultra_autonomy action: {other}"
                )))
            }
        };
        serde_json::to_string_pretty(&payload)
            .map_err(|err| ToolError::ExecutionFailed(format!("render autonomy payload: {err}")))
    }

    fn schema(&self) -> ToolSchema {
        let mut props = IndexMap::new();
        props.insert(
            "action".into(),
            json!({
                "type": "string",
                "enum": [
                    "status", "loop_record", "loop_evaluate", "board_create",
                    "board_add_card", "board_update", "board_plan", "objective_bridge",
                    "resource_plan", "memory_lifecycle", "memory_resolve",
                    "outcome_rehearsal", "recall_quality", "service_plan",
                    "channel_surface", "events", "help"
                ],
                "description": "Autonomy cockpit action. Defaults to status."
            }),
        );
        props.insert("board_id".into(), str_schema("Board id."));
        props.insert("card_id".into(), str_schema("Card id."));
        props.insert("session".into(), str_schema("Loop/session id."));
        props.insert("title".into(), str_schema("Board or card title."));
        props.insert("description".into(), str_schema("Card description."));
        props.insert(
            "objective".into(),
            str_schema("Objective text for objective_bridge."),
        );
        props.insert("context".into(), str_schema("Board context."));
        props.insert(
            "events".into(),
            json!({"type":"array", "description":"Loop events for loop_evaluate."}),
        );
        props.insert(
            "providers".into(),
            json!({"type":"array", "description":"Memory provider signals for memory_lifecycle."}),
        );
        props.insert(
            "plan_steps".into(),
            json!({"type":"array", "description":"Outcome rehearsal plan steps."}),
        );
        props.insert(
            "tool_calls".into(),
            json!({"type":"array", "description":"Outcome rehearsal tool-call evidence."}),
        );
        props.insert(
            "verification".into(),
            json!({"type":"array", "description":"Deterministic verification evidence."}),
        );
        props.insert(
            "checkpoints".into(),
            json!({"type":"array", "description":"Durable checkpoint, PR, commit, release, or ContextLattice evidence."}),
        );
        props.insert(
            "recall_items".into(),
            json!({"type":"array", "description":"Recall/synthesis pack items for recall_quality."}),
        );
        props.insert(
            "outcome".into(),
            json!({"type":"object", "description":"Task outcome evidence for recall_quality."}),
        );
        props.insert("cpu_cores".into(), int_schema("Available CPU cores."));
        props.insert("free_ram_mb".into(), int_schema("Free RAM in MB."));
        props.insert("total_ram_mb".into(), int_schema("Total RAM in MB."));
        props.insert(
            "token_budget_remaining".into(),
            int_schema("Remaining token budget."),
        );
        tool_schema(
            TOOL_NAME,
            "Operate the Hermes Ultra autonomy cockpit: loop guard, task boards, resource-governed subagents, memory lifecycle, channel/service surfaces, and dashboard event envelopes.",
            JsonSchema::object(props, vec![]),
        )
    }
}

fn str_schema(description: &str) -> Value {
    json!({"type":"string", "description":description})
}

fn int_schema(description: &str) -> Value {
    json!({"type":"integer", "description":description})
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ToolLoopEvent {
    pub tool: String,
    #[serde(default)]
    pub params_hash: String,
    #[serde(default)]
    pub failed: bool,
    #[serde(default)]
    pub no_action: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timestamp: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LoopVerdict {
    Productive,
    Suspicious,
    Stuck,
    HardAbort,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LoopEvaluation {
    pub verdict: LoopVerdict,
    pub reason: String,
    pub total_calls: usize,
    pub failed_calls: usize,
    pub consecutive_no_action_steps: usize,
    pub identical_repeat_count: usize,
    pub similar_failure_count: usize,
    pub repeated_text_count: usize,
    pub recommendation: String,
}

pub fn evaluate_loop_events(events: &[ToolLoopEvent]) -> LoopEvaluation {
    let total_calls = events.len();
    let failed_calls = events.iter().filter(|event| event.failed).count();
    let consecutive_no_action_steps = events
        .iter()
        .rev()
        .take_while(|event| event.no_action)
        .count();

    let mut identical_counts: HashMap<(&str, &str), usize> = HashMap::new();
    let mut failure_counts: HashMap<&str, usize> = HashMap::new();
    let mut text_counts: HashMap<String, usize> = HashMap::new();
    for event in events {
        *identical_counts
            .entry((event.tool.as_str(), event.params_hash.as_str()))
            .or_insert(0) += 1;
        if event.failed {
            *failure_counts.entry(event.tool.as_str()).or_insert(0) += 1;
        }
        if let Some(text) = event.step_text.as_deref().map(normalize_loop_text) {
            if !text.is_empty() {
                *text_counts.entry(text).or_insert(0) += 1;
            }
        }
    }

    let identical_repeat_count = identical_counts.values().copied().max().unwrap_or(0);
    let similar_failure_count = failure_counts.values().copied().max().unwrap_or(0);
    let repeated_text_count = text_counts.values().copied().max().unwrap_or(0);

    let (verdict, reason, recommendation) = if total_calls >= LOOP_ABSOLUTE_MAX {
        (
            LoopVerdict::HardAbort,
            "absolute tool-call ceiling reached".to_string(),
            "abort the turn, summarize evidence, and ask the user before retrying".to_string(),
        )
    } else if failed_calls >= LOOP_FAILED_ABSOLUTE_MAX {
        (
            LoopVerdict::HardAbort,
            "failed tool-call ceiling reached".to_string(),
            "stop repeating failing tool calls and request a narrower recovery plan".to_string(),
        )
    } else if identical_repeat_count >= LOOP_IDENTICAL_THRESHOLD {
        (
            LoopVerdict::Stuck,
            "same tool and parameters repeated past threshold".to_string(),
            "change strategy or ask for approval to continue".to_string(),
        )
    } else if similar_failure_count >= LOOP_SIMILAR_FAILURE_THRESHOLD {
        (
            LoopVerdict::Stuck,
            "same tool family is failing repeatedly".to_string(),
            "switch tool, inspect root cause, or surface blocker".to_string(),
        )
    } else if repeated_text_count >= LOOP_TEXT_REPEAT_THRESHOLD {
        (
            LoopVerdict::Suspicious,
            "model output text is repeating".to_string(),
            "compress state and force a concrete next action".to_string(),
        )
    } else if consecutive_no_action_steps >= LOOP_NO_ACTION_THRESHOLD {
        (
            LoopVerdict::Suspicious,
            "too many no-action steps".to_string(),
            "force tool-backed progress or ask a blocking question".to_string(),
        )
    } else {
        (
            LoopVerdict::Productive,
            "no loop threshold crossed".to_string(),
            "continue within turn budget".to_string(),
        )
    };

    LoopEvaluation {
        verdict,
        reason,
        total_calls,
        failed_calls,
        consecutive_no_action_steps,
        identical_repeat_count,
        similar_failure_count,
        repeated_text_count,
        recommendation,
    }
}

fn normalize_loop_text(text: &str) -> String {
    text.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_ascii_lowercase()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutonomyBoard {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub context: String,
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    #[serde(default)]
    pub instructions: Vec<String>,
    #[serde(default)]
    pub cards: Vec<AutonomyCard>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutonomyCard {
    pub id: String,
    pub title: String,
    #[serde(default)]
    pub description: String,
    pub status: CardStatus,
    pub priority: CardPriority,
    #[serde(default)]
    pub labels: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
    #[serde(default)]
    pub token_used: u64,
    #[serde(default)]
    pub comments: Vec<CardComment>,
    #[serde(default)]
    pub attachments: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CardComment {
    pub author: String,
    pub body: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CardStatus {
    Todo,
    Doing,
    Done,
    Blocked,
    Question,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum CardPriority {
    Low,
    Normal,
    High,
    Critical,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BoardExecutionPlan {
    pub board_id: String,
    pub ready: Vec<String>,
    pub blocked: Vec<BlockedCard>,
    pub done: Vec<String>,
    pub execution_order: Vec<String>,
    pub token_budget_remaining: Option<u64>,
    pub events: Vec<AutonomyEvent>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BlockedCard {
    pub card_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AutonomyEvent {
    pub event: String,
    pub payload: Value,
}

pub fn plan_board_execution(board: &AutonomyBoard) -> BoardExecutionPlan {
    let done_set: BTreeSet<String> = board
        .cards
        .iter()
        .filter(|card| card.status == CardStatus::Done)
        .map(|card| card.id.clone())
        .collect();
    let known: BTreeSet<String> = board.cards.iter().map(|card| card.id.clone()).collect();
    let mut ready_cards = Vec::new();
    let mut blocked = Vec::new();
    let mut done = Vec::new();

    for card in &board.cards {
        match card.status {
            CardStatus::Done => done.push(card.id.clone()),
            CardStatus::Blocked => blocked.push(BlockedCard {
                card_id: card.id.clone(),
                reason: "card status is blocked".to_string(),
            }),
            CardStatus::Question => blocked.push(BlockedCard {
                card_id: card.id.clone(),
                reason: "card is waiting on user feedback".to_string(),
            }),
            CardStatus::Doing | CardStatus::Todo => {
                let missing = card
                    .dependencies
                    .iter()
                    .filter(|dep| !done_set.contains(*dep))
                    .cloned()
                    .collect::<Vec<_>>();
                let unknown = card
                    .dependencies
                    .iter()
                    .filter(|dep| !known.contains(*dep))
                    .cloned()
                    .collect::<Vec<_>>();
                if !unknown.is_empty() {
                    blocked.push(BlockedCard {
                        card_id: card.id.clone(),
                        reason: format!("unknown dependencies: {}", unknown.join(", ")),
                    });
                } else if !missing.is_empty() {
                    blocked.push(BlockedCard {
                        card_id: card.id.clone(),
                        reason: format!("waiting on dependencies: {}", missing.join(", ")),
                    });
                } else if card
                    .token_budget
                    .map(|budget| card.token_used >= budget)
                    .unwrap_or(false)
                {
                    blocked.push(BlockedCard {
                        card_id: card.id.clone(),
                        reason: "card token budget exhausted".to_string(),
                    });
                } else {
                    ready_cards.push(card.clone());
                }
            }
        }
    }

    ready_cards.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.id.cmp(&b.id))
            .then_with(|| a.title.cmp(&b.title))
    });
    let ready = ready_cards
        .iter()
        .map(|card| card.id.clone())
        .collect::<Vec<_>>();
    let mut execution_order =
        topological_ready_order(board, &ready).unwrap_or_else(|| ready.clone());
    execution_order.retain(|id| ready.contains(id));

    let token_budget_remaining = board
        .cards
        .iter()
        .filter_map(|card| {
            card.token_budget
                .map(|budget| budget.saturating_sub(card.token_used))
        })
        .reduce(|a, b| a.saturating_add(b));
    let events = vec![
        AutonomyEvent {
            event: "board.plan.updated".to_string(),
            payload: json!({"board_id": board.id, "ready_count": ready.len(), "blocked_count": blocked.len()}),
        },
        AutonomyEvent {
            event: "dashboard.sse.autonomy".to_string(),
            payload: json!({"board_id": board.id, "execution_order": execution_order}),
        },
    ];

    BoardExecutionPlan {
        board_id: board.id.clone(),
        ready,
        blocked,
        done,
        execution_order,
        token_budget_remaining,
        events,
    }
}

fn topological_ready_order(board: &AutonomyBoard, ready: &[String]) -> Option<Vec<String>> {
    let ready_set: BTreeSet<String> = ready.iter().cloned().collect();
    let mut indegree: HashMap<String, usize> = HashMap::new();
    let mut edges: HashMap<String, Vec<String>> = HashMap::new();
    for card in &board.cards {
        if !ready_set.contains(&card.id) {
            continue;
        }
        indegree.entry(card.id.clone()).or_insert(0);
        for dep in &card.dependencies {
            if ready_set.contains(dep) {
                edges.entry(dep.clone()).or_default().push(card.id.clone());
                *indegree.entry(card.id.clone()).or_insert(0) += 1;
            }
        }
    }
    let mut queue = indegree
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(id, _)| id.clone())
        .collect::<VecDeque<_>>();
    let mut out = Vec::new();
    while let Some(id) = queue.pop_front() {
        out.push(id.clone());
        if let Some(children) = edges.get(&id) {
            for child in children {
                if let Some(degree) = indegree.get_mut(child) {
                    *degree = degree.saturating_sub(1);
                    if *degree == 0 {
                        queue.push_back(child.clone());
                    }
                }
            }
        }
    }
    (out.len() == indegree.len()).then_some(out)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourceGovernorInput {
    pub cpu_cores: usize,
    pub free_ram_mb: Option<u64>,
    pub total_ram_mb: Option<u64>,
    pub ram_per_agent_mb: u64,
    pub min_free_ram_mb: u64,
    pub token_budget_remaining: Option<u64>,
    pub per_agent_token_reserve: u64,
    pub user_override: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceGovernorPlan {
    pub max_concurrent: usize,
    pub cpu_limit: usize,
    pub ram_limit: Option<usize>,
    pub token_limit: Option<usize>,
    pub limiting_factor: String,
    pub reasons: Vec<String>,
}

pub fn resource_admission_plan(input: ResourceGovernorInput) -> ResourceGovernorPlan {
    let cpu_limit = input.cpu_cores.saturating_sub(1).max(1);
    let ram_limit = match (input.free_ram_mb, input.total_ram_mb) {
        (Some(free), Some(total)) => {
            let free_based = free
                .saturating_sub(input.min_free_ram_mb)
                .checked_div(input.ram_per_agent_mb.max(1))
                .unwrap_or(0)
                .max(1) as usize;
            let system_based = total
                .checked_div(2)
                .unwrap_or(total)
                .checked_div(input.ram_per_agent_mb.max(1))
                .unwrap_or(0)
                .max(1) as usize;
            Some(free_based.min(system_based))
        }
        _ => None,
    };
    let token_limit = input
        .token_budget_remaining
        .map(|tokens| (tokens / input.per_agent_token_reserve.max(1)).max(1) as usize);

    let mut candidates = vec![("cpu".to_string(), cpu_limit)];
    if let Some(limit) = ram_limit {
        candidates.push(("ram".to_string(), limit));
    }
    if let Some(limit) = token_limit {
        candidates.push(("token".to_string(), limit));
    }
    if let Some(limit) = input.user_override {
        candidates.push(("user_override".to_string(), limit.max(1)));
    }
    let (limiting_factor, max_concurrent) = candidates
        .into_iter()
        .min_by_key(|(_, limit)| *limit)
        .unwrap_or_else(|| ("cpu".to_string(), 1));
    let mut reasons = vec![format!("cpu_limit={cpu_limit}")];
    if let Some(limit) = ram_limit {
        reasons.push(format!("ram_limit={limit}"));
    } else {
        reasons.push("ram_limit=unproven_no_ram_sample".to_string());
    }
    if let Some(limit) = token_limit {
        reasons.push(format!("token_limit={limit}"));
    }
    if let Some(limit) = input.user_override {
        reasons.push(format!("user_override={}", limit.max(1)));
    }
    ResourceGovernorPlan {
        max_concurrent,
        cpu_limit,
        ram_limit,
        token_limit,
        limiting_factor,
        reasons,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryProviderSignal {
    pub provider: String,
    pub available: bool,
    #[serde(default)]
    pub score: f64,
    #[serde(default)]
    pub confidence: f64,
    #[serde(default)]
    pub last_seen_days: u32,
    #[serde(default)]
    pub evidence_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryLifecycleSnapshot {
    pub hot: Vec<String>,
    pub warm: Vec<String>,
    pub archive: Vec<String>,
    pub contextlattice_boost: f64,
    pub recall_budget_chars: usize,
    pub consolidation_policy: String,
    pub conflict_policy: String,
    pub provenance_required: bool,
}

pub fn memory_lifecycle_snapshot(providers: &[MemoryProviderSignal]) -> MemoryLifecycleSnapshot {
    let mut hot = Vec::new();
    let mut warm = Vec::new();
    let mut archive = Vec::new();
    for provider in providers {
        let name = provider.provider.trim().to_ascii_lowercase();
        if !provider.available {
            archive.push(name);
        } else if name == "contextlattice"
            || provider.score >= 1.20
            || provider.confidence >= 0.82
            || provider.last_seen_days <= 7
        {
            hot.push(name);
        } else if provider.last_seen_days <= 30 || provider.evidence_count >= 2 {
            warm.push(name);
        } else {
            archive.push(name);
        }
    }
    hot.sort();
    hot.dedup();
    warm.sort();
    warm.dedup();
    archive.sort();
    archive.dedup();
    MemoryLifecycleSnapshot {
        hot,
        warm,
        archive,
        contextlattice_boost: 1.25,
        recall_budget_chars: 1200,
        consolidation_policy:
            "ContextLattice-first: checkpoint/runbook facts stay hot; dormant external-provider facts decay to warm/archive."
                .to_string(),
        conflict_policy:
            "Prefer higher confidence; tie-break by recency; retain lower-confidence contradicted facts as provenance notes."
                .to_string(),
        provenance_required: true,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryCandidate {
    pub content: String,
    pub confidence: f64,
    pub importance: f64,
    pub durability_days: u32,
    pub last_seen_days: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryResolution {
    pub action: String,
    pub reason: String,
    pub retained_content: String,
    pub provenance_note: String,
}

pub fn resolve_memory_candidate(
    existing: Option<MemoryCandidate>,
    candidate: MemoryCandidate,
    overlap: f64,
    conflict: bool,
) -> MemoryResolution {
    let Some(existing) = existing else {
        return MemoryResolution {
            action: "create".to_string(),
            reason: "no existing memory matched".to_string(),
            retained_content: candidate.content,
            provenance_note: "new typed memory candidate".to_string(),
        };
    };
    if overlap >= 0.74 && !conflict {
        return MemoryResolution {
            action: "reinforce".to_string(),
            reason: format!("overlap {overlap:.2} exceeded merge threshold"),
            retained_content: existing.content,
            provenance_note: "candidate strengthens existing memory evidence".to_string(),
        };
    }
    if conflict {
        let candidate_wins = candidate.confidence > existing.confidence
            || ((candidate.confidence - existing.confidence).abs() < f64::EPSILON
                && candidate.last_seen_days < existing.last_seen_days);
        if candidate_wins {
            return MemoryResolution {
                action: "supersede".to_string(),
                reason: "candidate has stronger confidence or newer equal-confidence evidence"
                    .to_string(),
                retained_content: candidate.content,
                provenance_note: format!(
                    "superseded lower-confidence memory: {}",
                    existing.content
                ),
            };
        }
        return MemoryResolution {
            action: "retain_existing".to_string(),
            reason: "existing memory has stronger confidence or fresher equal-confidence evidence"
                .to_string(),
            retained_content: existing.content,
            provenance_note: format!("rejected conflicting candidate: {}", candidate.content),
        };
    }
    MemoryResolution {
        action: "create_related".to_string(),
        reason: "candidate is related but below merge threshold and not contradictory".to_string(),
        retained_content: candidate.content,
        provenance_note: "related memory should remain separately searchable".to_string(),
    }
}

fn autonomy_status_snapshot(state: &UltraAutonomyState) -> Value {
    let board_count = fs::read_dir(state.boards_dir())
        .map(|entries| {
            entries
                .flatten()
                .filter(|entry| entry.path().is_file())
                .count()
        })
        .unwrap_or(0);
    json!({
        "status": "ok",
        "rust_runtime_surface": true,
        "implemented_items": [
            "agent_loop_repetition_governor",
            "json_backed_task_board_execution",
            "dashboard_sse_event_envelopes",
            "resource_governed_subagent_admission",
            "approval_laundering_regression_tests",
            "contextlattice_first_memory_lifecycle",
            "memory_conflict_reinforcement_resolution",
            "deterministic_outcome_loop_rehearsal",
            "contextlattice_recall_quality_outcome_eval",
            "one_command_service_plan",
            "channel_skill_permission_status_surface",
            "objective_to_board_bridge"
        ],
        "boards": board_count,
        "data_root": state.root,
        "event_topics": event_catalog(),
    })
}

fn record_loop_event(state: &UltraAutonomyState, params: &Value) -> Result<Value, ToolError> {
    let session = optional_str(params, "session").unwrap_or("default");
    let path = state.loop_path(session);
    let mut events = if path.is_file() {
        serde_json::from_str::<Vec<ToolLoopEvent>>(&fs::read_to_string(&path).map_err(io_err)?)
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let tool = required_str(params, "tool")?.to_string();
    let params_hash = params
        .get("params_hash")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| params.get("tool_params").map(stable_value_fingerprint))
        .unwrap_or_default();
    events.push(ToolLoopEvent {
        tool,
        params_hash,
        failed: bool_param(params, "failed", false),
        no_action: bool_param(params, "no_action", false),
        step_text: optional_str(params, "step_text").map(str::to_string),
        timestamp: Some(Utc::now()),
    });
    if events.len() > LOOP_ABSOLUTE_MAX {
        let drain = events.len() - LOOP_ABSOLUTE_MAX;
        events.drain(0..drain);
    }
    fs::write(&path, serde_json::to_vec_pretty(&events).map_err(json_err)?).map_err(io_err)?;
    let evaluation = evaluate_loop_events(&events);
    Ok(json!({"session": session, "events": events.len(), "evaluation": evaluation}))
}

fn evaluate_loop_action(state: &UltraAutonomyState, params: &Value) -> Result<Value, ToolError> {
    let events = if let Some(raw_events) = params.get("events") {
        serde_json::from_value::<Vec<ToolLoopEvent>>(raw_events.clone())
            .map_err(|err| ToolError::InvalidParams(format!("events: {err}")))?
    } else {
        let session = optional_str(params, "session").unwrap_or("default");
        let path = state.loop_path(session);
        if path.is_file() {
            serde_json::from_str::<Vec<ToolLoopEvent>>(&fs::read_to_string(path).map_err(io_err)?)
                .map_err(|err| ToolError::ExecutionFailed(format!("read loop events: {err}")))?
        } else {
            Vec::new()
        }
    };
    Ok(json!({"evaluation": evaluate_loop_events(&events), "events": events.len()}))
}

fn create_board(state: &UltraAutonomyState, params: &Value) -> Result<Value, ToolError> {
    let id = optional_str(params, "board_id")
        .map(str::to_string)
        .unwrap_or_else(|| format!("board-{}", Utc::now().timestamp_millis()));
    let title = optional_str(params, "title").unwrap_or("Hermes autonomy board");
    let now = Utc::now();
    let board = AutonomyBoard {
        id: id.clone(),
        title: title.to_string(),
        context: optional_str(params, "context")
            .unwrap_or_default()
            .to_string(),
        variables: BTreeMap::new(),
        instructions: vec![
            "Process ready cards in dependency order.".to_string(),
            "Move cards to question when user feedback is required.".to_string(),
            "Record verification evidence as comments before done.".to_string(),
        ],
        cards: Vec::new(),
        created_at: now,
        updated_at: now,
    };
    save_board(state, &board)?;
    Ok(json!({"board": board, "events": board_events(&id, "board.created")}))
}

fn add_board_card(state: &UltraAutonomyState, params: &Value) -> Result<Value, ToolError> {
    let board_id = required_str(params, "board_id")?;
    let mut board = load_board(state, board_id)?;
    let card_id = optional_str(params, "card_id")
        .map(str::to_string)
        .unwrap_or_else(|| format!("card-{}", board.cards.len() + 1));
    if board.cards.iter().any(|card| card.id == card_id) {
        return Err(ToolError::InvalidParams(format!(
            "card already exists: {card_id}"
        )));
    }
    let card = AutonomyCard {
        id: card_id.clone(),
        title: required_str(params, "title")?.to_string(),
        description: optional_str(params, "description")
            .unwrap_or_default()
            .to_string(),
        status: parse_card_status(optional_str(params, "status").unwrap_or("todo"))?,
        priority: parse_card_priority(optional_str(params, "priority").unwrap_or("normal"))?,
        labels: string_array(params.get("labels")),
        dependencies: string_array(params.get("dependencies")),
        token_budget: params.get("token_budget").and_then(Value::as_u64),
        token_used: params
            .get("token_used")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        comments: Vec::new(),
        attachments: string_array(params.get("attachments")),
    };
    board.cards.push(card);
    board.updated_at = Utc::now();
    save_board(state, &board)?;
    let plan = plan_board_execution(&board);
    Ok(
        json!({"board_id": board.id, "card_id": card_id, "plan": plan, "events": board_events(&board.id, "card.created")}),
    )
}

fn update_board_card(state: &UltraAutonomyState, params: &Value) -> Result<Value, ToolError> {
    let board_id = required_str(params, "board_id")?;
    let card_id = required_str(params, "card_id")?;
    let mut board = load_board(state, board_id)?;
    let Some(card) = board.cards.iter_mut().find(|card| card.id == card_id) else {
        return Err(ToolError::NotFound(format!("card not found: {card_id}")));
    };
    if let Some(status) = optional_str(params, "status") {
        card.status = parse_card_status(status)?;
    }
    if let Some(priority) = optional_str(params, "priority") {
        card.priority = parse_card_priority(priority)?;
    }
    if let Some(token_used) = params.get("token_used").and_then(Value::as_u64) {
        card.token_used = token_used;
    }
    if let Some(comment) = optional_str(params, "comment").filter(|comment| !comment.is_empty()) {
        card.comments.push(CardComment {
            author: optional_str(params, "author")
                .unwrap_or("hermes")
                .to_string(),
            body: comment.to_string(),
            created_at: Utc::now(),
        });
    }
    board.updated_at = Utc::now();
    save_board(state, &board)?;
    let plan = plan_board_execution(&board);
    Ok(
        json!({"board_id": board.id, "card_id": card_id, "plan": plan, "events": board_events(&board.id, "card.updated")}),
    )
}

fn board_plan_action(state: &UltraAutonomyState, params: &Value) -> Result<Value, ToolError> {
    let board_id = required_str(params, "board_id")?;
    let board = load_board(state, board_id)?;
    Ok(json!({"board": board, "plan": plan_board_execution(&board)}))
}

fn objective_bridge_action(state: &UltraAutonomyState, params: &Value) -> Result<Value, ToolError> {
    let objective = required_str(params, "objective")?;
    let board_id = optional_str(params, "board_id")
        .map(str::to_string)
        .unwrap_or_else(|| format!("objective-{}", Utc::now().timestamp_millis()));
    let now = Utc::now();
    let titles = [
        "Confirm objective and constraints",
        "Fetch ContextLattice scoped pack",
        "Create implementation map",
        "Implement loop governor slice",
        "Implement task board execution slice",
        "Implement resource governor slice",
        "Implement memory lifecycle slice",
        "Implement service and channel UX slice",
        "Run deterministic verification",
        "Checkpoint and report evidence",
    ];
    let cards = titles
        .iter()
        .enumerate()
        .map(|(idx, title)| AutonomyCard {
            id: format!("step-{}", idx + 1),
            title: (*title).to_string(),
            description: objective.to_string(),
            status: CardStatus::Todo,
            priority: if idx < 2 {
                CardPriority::Critical
            } else {
                CardPriority::High
            },
            labels: vec!["objective".to_string(), "verification".to_string()],
            dependencies: if idx == 0 {
                Vec::new()
            } else {
                vec![format!("step-{idx}")]
            },
            token_budget: Some(40_000),
            token_used: 0,
            comments: Vec::new(),
            attachments: Vec::new(),
        })
        .collect::<Vec<_>>();
    let board = AutonomyBoard {
        id: board_id.clone(),
        title: "Objective execution board".to_string(),
        context: objective.to_string(),
        variables: BTreeMap::new(),
        instructions: vec![
            "Every done card needs evidence in comments or linked artifacts.".to_string(),
            "Blocked/question cards must surface the exact missing input.".to_string(),
        ],
        cards,
        created_at: now,
        updated_at: now,
    };
    save_board(state, &board)?;
    Ok(
        json!({"board": board, "plan": plan_board_execution(&board), "events": board_events(&board_id, "objective.board.created")}),
    )
}

fn resource_plan_action(params: &Value) -> Value {
    let cpu_cores = params
        .get("cpu_cores")
        .and_then(Value::as_u64)
        .map(|v| v as usize)
        .unwrap_or_else(default_cpu_cores);
    let input = ResourceGovernorInput {
        cpu_cores,
        free_ram_mb: params.get("free_ram_mb").and_then(Value::as_u64),
        total_ram_mb: params.get("total_ram_mb").and_then(Value::as_u64),
        ram_per_agent_mb: params
            .get("ram_per_agent_mb")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_RAM_PER_AGENT_MB),
        min_free_ram_mb: params
            .get("min_free_ram_mb")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_MIN_FREE_RAM_MB),
        token_budget_remaining: params.get("token_budget_remaining").and_then(Value::as_u64),
        per_agent_token_reserve: params
            .get("per_agent_token_reserve")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_PER_AGENT_TOKEN_RESERVE),
        user_override: params
            .get("user_override")
            .and_then(Value::as_u64)
            .map(|v| v as usize),
    };
    json!({"input": input, "plan": resource_admission_plan(input)})
}

fn memory_lifecycle_action(params: &Value) -> Value {
    let providers = params
        .get("providers")
        .cloned()
        .and_then(|value| serde_json::from_value::<Vec<MemoryProviderSignal>>(value).ok())
        .unwrap_or_else(default_memory_providers);
    json!({"providers": providers, "lifecycle": memory_lifecycle_snapshot(&providers)})
}

fn memory_resolve_action(params: &Value) -> Result<Value, ToolError> {
    let candidate_value = params
        .get("candidate")
        .cloned()
        .unwrap_or_else(|| json!({"content": required_str(params, "content").unwrap_or(""), "confidence": 0.75, "importance": 0.5, "durability_days": 30, "last_seen_days": 0}));
    let candidate = serde_json::from_value::<MemoryCandidate>(candidate_value)
        .map_err(|err| ToolError::InvalidParams(format!("candidate: {err}")))?;
    let existing = params
        .get("existing")
        .cloned()
        .map(serde_json::from_value::<MemoryCandidate>)
        .transpose()
        .map_err(|err| ToolError::InvalidParams(format!("existing: {err}")))?;
    let overlap = params.get("overlap").and_then(Value::as_f64).unwrap_or(0.0);
    let conflict = bool_param(params, "conflict", false);
    Ok(json!({"resolution": resolve_memory_candidate(existing, candidate, overlap, conflict)}))
}

fn outcome_rehearsal_action(params: &Value) -> Result<Value, ToolError> {
    let input = serde_json::from_value::<OutcomeLoopRehearsalInput>(params.clone())
        .map_err(|err| ToolError::InvalidParams(format!("outcome_rehearsal: {err}")))?;
    Ok(json!({"report": evaluate_outcome_loop_rehearsal(input)}))
}

fn recall_quality_action(params: &Value) -> Result<Value, ToolError> {
    let input = serde_json::from_value::<RecallQualityInput>(params.clone())
        .map_err(|err| ToolError::InvalidParams(format!("recall_quality: {err}")))?;
    Ok(json!({"report": evaluate_recall_quality(input)}))
}

fn service_plan_action() -> Value {
    json!({
        "status": "ok",
        "one_command": "hermes-ultra up",
        "equivalent_steps": [
            "hermes-ultra gateway install",
            "hermes-ultra gateway start",
            "hermes-ultra gateway status"
        ],
        "logs": "hermes-ultra logs --follow",
        "contract": "ensure service exists, start it when supported, then print gateway status",
        "platform_notes": {
            "macos": "uses user LaunchAgent; no sudo required",
            "linux_windows": "falls back to gateway status until service install support exists"
        }
    })
}

fn channel_surface_action() -> Value {
    json!({
        "status": "ok",
        "channels": [
            {"name":"cli", "surfaces":["/harness autonomy", "/tools", "/skills", "/memory", "/objective"]},
            {"name":"dashboard", "surfaces":["harness cockpit", "autonomy SSE event envelopes", "board plan JSON", "memory lifecycle JSON"]},
            {"name":"gateway", "surfaces":["approval buttons", "status commands", "skill reload", "objective notices"]},
            {"name":"telegram", "surfaces":["approval buttons", "topic/session routing", "skill reload", "status/yolo/model commands"]}
        ],
        "permission_modes": ["ask", "session", "always", "yolo_recoverable_only"],
        "admin_model": "gateway allowlists and platform access policies remain authoritative"
    })
}

fn event_catalog_action() -> Value {
    json!({"events": event_catalog()})
}

fn help_action() -> Value {
    json!({
        "tool": TOOL_NAME,
        "actions": [
            "status", "loop_record", "loop_evaluate", "board_create", "board_add_card",
            "board_update", "board_plan", "objective_bridge", "resource_plan",
            "memory_lifecycle", "memory_resolve", "outcome_rehearsal", "recall_quality",
            "service_plan", "channel_surface", "events", "help"
        ]
    })
}

fn event_catalog() -> Vec<Value> {
    vec![
        json!({"event":"board.created", "consumer":"dashboard/gateway", "purpose":"new autonomy board available"}),
        json!({"event":"card.created", "consumer":"dashboard/gateway", "purpose":"new card queued"}),
        json!({"event":"card.updated", "consumer":"dashboard/gateway", "purpose":"card status/comment/token changed"}),
        json!({"event":"board.plan.updated", "consumer":"dashboard SSE", "purpose":"ready/blocked execution plan changed"}),
        json!({"event":"objective.board.created", "consumer":"ContextLattice checkpoint", "purpose":"objective materialized into durable cards"}),
        json!({"event":"memory.lifecycle.updated", "consumer":"harness cockpit", "purpose":"hot/warm/archive memory projection changed"}),
        json!({"event":"eval.outcome_rehearsal.scored", "consumer":"harness cockpit", "purpose":"plan/tool/verification/checkpoint/recovery gates scored"}),
        json!({"event":"eval.recall_quality.scored", "consumer":"ContextLattice synthesis pack", "purpose":"recall tied to implementation and verification outcomes"}),
        json!({"event":"subagent.resource.plan", "consumer":"delegate_task", "purpose":"admission control changed"}),
    ]
}

fn board_events(board_id: &str, event: &str) -> Vec<AutonomyEvent> {
    vec![AutonomyEvent {
        event: event.to_string(),
        payload: json!({"board_id": board_id, "ts": Utc::now().to_rfc3339()}),
    }]
}

fn load_board(state: &UltraAutonomyState, board_id: &str) -> Result<AutonomyBoard, ToolError> {
    let path = state.board_path(board_id);
    let raw = fs::read_to_string(&path)
        .map_err(|err| ToolError::NotFound(format!("read board {}: {err}", path.display())))?;
    serde_json::from_str(&raw)
        .map_err(|err| ToolError::ExecutionFailed(format!("parse board {}: {err}", path.display())))
}

fn save_board(state: &UltraAutonomyState, board: &AutonomyBoard) -> Result<(), ToolError> {
    fs::write(
        state.board_path(&board.id),
        serde_json::to_vec_pretty(board).map_err(json_err)?,
    )
    .map_err(io_err)
}

fn parse_card_status(value: &str) -> Result<CardStatus, ToolError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "todo" => Ok(CardStatus::Todo),
        "doing" | "in_progress" => Ok(CardStatus::Doing),
        "done" | "complete" | "completed" => Ok(CardStatus::Done),
        "blocked" => Ok(CardStatus::Blocked),
        "question" | "waiting" | "needs_feedback" => Ok(CardStatus::Question),
        other => Err(ToolError::InvalidParams(format!(
            "unknown card status: {other}"
        ))),
    }
}

fn parse_card_priority(value: &str) -> Result<CardPriority, ToolError> {
    match value.trim().to_ascii_lowercase().as_str() {
        "low" => Ok(CardPriority::Low),
        "normal" | "medium" => Ok(CardPriority::Normal),
        "high" => Ok(CardPriority::High),
        "critical" | "p0" => Ok(CardPriority::Critical),
        other => Err(ToolError::InvalidParams(format!(
            "unknown card priority: {other}"
        ))),
    }
}

fn default_memory_providers() -> Vec<MemoryProviderSignal> {
    vec![
        MemoryProviderSignal {
            provider: "builtin".to_string(),
            available: true,
            score: 1.20,
            confidence: 0.80,
            last_seen_days: 0,
            evidence_count: 1,
        },
        MemoryProviderSignal {
            provider: "contextlattice".to_string(),
            available: true,
            score: 1.25,
            confidence: 0.90,
            last_seen_days: 0,
            evidence_count: 3,
        },
        MemoryProviderSignal {
            provider: "supermemory".to_string(),
            available: true,
            score: 1.15,
            confidence: 0.70,
            last_seen_days: 14,
            evidence_count: 2,
        },
    ]
}

fn default_cpu_cores() -> usize {
    std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1)
}

fn stable_value_fingerprint(value: &Value) -> String {
    match value {
        Value::Null => "null".to_string(),
        Value::Bool(v) => v.to_string(),
        Value::Number(v) => v.to_string(),
        Value::String(v) => v.clone(),
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(stable_value_fingerprint)
                .collect::<Vec<_>>()
                .join(",")
        ),
        Value::Object(map) => {
            let mut keys = map.keys().collect::<Vec<_>>();
            keys.sort();
            keys.into_iter()
                .map(|key| format!("{key}:{}", stable_value_fingerprint(&map[key])))
                .collect::<Vec<_>>()
                .join("|")
        }
    }
}

fn required_str<'a>(params: &'a Value, key: &str) -> Result<&'a str, ToolError> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ToolError::InvalidParams(format!("missing required string param: {key}")))
}

fn optional_str<'a>(params: &'a Value, key: &str) -> Option<&'a str> {
    params
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn bool_param(params: &Value, key: &str, default: bool) -> bool {
    params.get(key).and_then(Value::as_bool).unwrap_or(default)
}

fn string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(Value::as_str)
                .map(str::trim)
                .filter(|item| !item.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn safe_id(id: &str) -> String {
    let out = id
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if out.is_empty() {
        "default".to_string()
    } else {
        out
    }
}

fn io_err(err: std::io::Error) -> ToolError {
    ToolError::ExecutionFailed(err.to_string())
}

fn json_err(err: serde_json::Error) -> ToolError {
    ToolError::ExecutionFailed(err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_governor_flags_identical_repetition() {
        let events = (0..LOOP_IDENTICAL_THRESHOLD)
            .map(|_| ToolLoopEvent {
                tool: "terminal".to_string(),
                params_hash: "same".to_string(),
                failed: false,
                no_action: false,
                step_text: None,
                timestamp: None,
            })
            .collect::<Vec<_>>();
        let evaluation = evaluate_loop_events(&events);
        assert_eq!(evaluation.verdict, LoopVerdict::Stuck);
        assert_eq!(evaluation.identical_repeat_count, LOOP_IDENTICAL_THRESHOLD);
    }

    #[test]
    fn board_plan_respects_dependencies_feedback_and_budget() {
        let now = Utc::now();
        let board = AutonomyBoard {
            id: "b".to_string(),
            title: "board".to_string(),
            context: String::new(),
            variables: BTreeMap::new(),
            instructions: Vec::new(),
            cards: vec![
                AutonomyCard {
                    id: "a".to_string(),
                    title: "a".to_string(),
                    description: String::new(),
                    status: CardStatus::Done,
                    priority: CardPriority::Normal,
                    labels: Vec::new(),
                    dependencies: Vec::new(),
                    token_budget: None,
                    token_used: 0,
                    comments: Vec::new(),
                    attachments: Vec::new(),
                },
                AutonomyCard {
                    id: "b".to_string(),
                    title: "b".to_string(),
                    description: String::new(),
                    status: CardStatus::Todo,
                    priority: CardPriority::Critical,
                    labels: Vec::new(),
                    dependencies: vec!["a".to_string()],
                    token_budget: Some(10),
                    token_used: 2,
                    comments: Vec::new(),
                    attachments: Vec::new(),
                },
                AutonomyCard {
                    id: "c".to_string(),
                    title: "c".to_string(),
                    description: String::new(),
                    status: CardStatus::Question,
                    priority: CardPriority::Critical,
                    labels: Vec::new(),
                    dependencies: Vec::new(),
                    token_budget: None,
                    token_used: 0,
                    comments: Vec::new(),
                    attachments: Vec::new(),
                },
            ],
            created_at: now,
            updated_at: now,
        };
        let plan = plan_board_execution(&board);
        assert_eq!(plan.ready, vec!["b"]);
        assert_eq!(plan.done, vec!["a"]);
        assert_eq!(plan.blocked[0].card_id, "c");
        assert_eq!(plan.token_budget_remaining, Some(8));
    }

    #[test]
    fn resource_governor_uses_tightest_limit() {
        let plan = resource_admission_plan(ResourceGovernorInput {
            cpu_cores: 8,
            free_ram_mb: Some(5_000),
            total_ram_mb: Some(16_000),
            ram_per_agent_mb: 2_000,
            min_free_ram_mb: 1_000,
            token_budget_remaining: Some(64_000),
            per_agent_token_reserve: 32_000,
            user_override: None,
        });
        assert_eq!(plan.max_concurrent, 2);
        assert_eq!(plan.limiting_factor, "ram");
    }

    #[test]
    fn memory_lifecycle_keeps_contextlattice_hot() {
        let snapshot = memory_lifecycle_snapshot(&[MemoryProviderSignal {
            provider: "contextlattice".to_string(),
            available: true,
            score: 0.1,
            confidence: 0.1,
            last_seen_days: 90,
            evidence_count: 0,
        }]);
        assert_eq!(snapshot.hot, vec!["contextlattice"]);
        assert!(snapshot
            .consolidation_policy
            .contains("ContextLattice-first"));
    }

    #[test]
    fn memory_resolution_supersedes_on_stronger_conflict() {
        let existing = MemoryCandidate {
            content: "old".to_string(),
            confidence: 0.7,
            importance: 0.5,
            durability_days: 90,
            last_seen_days: 3,
        };
        let candidate = MemoryCandidate {
            content: "new".to_string(),
            confidence: 0.9,
            importance: 0.5,
            durability_days: 90,
            last_seen_days: 0,
        };
        let resolution = resolve_memory_candidate(Some(existing), candidate, 0.2, true);
        assert_eq!(resolution.action, "supersede");
        assert_eq!(resolution.retained_content, "new");
    }

    #[test]
    fn handler_persists_objective_board() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let state = UltraAutonomyState::new(tmp.path().join("autonomy"));
        let payload = objective_bridge_action(
            &state,
            &json!({"board_id":"obj", "objective":"close all autonomy gaps"}),
        )
        .expect("bridge");
        assert_eq!(payload["board"]["id"], "obj");
        assert!(state.board_path("obj").is_file());
        let plan = board_plan_action(&state, &json!({"board_id":"obj"})).expect("plan");
        assert_eq!(plan["plan"]["ready"].as_array().unwrap().len(), 1);
    }
}
