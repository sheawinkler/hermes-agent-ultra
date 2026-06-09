//! Curator state persistence, scheduling, and LLM review orchestration.
//!
//! The curator background engine stores its runtime state in a JSON file
//! (`skills_dir/.curator_state`). This module provides load/save helpers with
//! atomic-write semantics and graceful fallback on corrupt/missing state.
//!
//! The LLM review pass is orchestrated via [`run_curator_review`] which applies
//! deterministic auto-transitions first, then optionally invokes an LLM runner
//! callback provided by the caller (to avoid circular dependencies on
//! `hermes-agent`).

use chrono::{DateTime, Utc};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::future::Future;

use crate::curator_prompt::CURATOR_REVIEW_PROMPT;
use crate::usage::{
    agent_created_report, load_usage, set_state, STATE_ACTIVE, STATE_ARCHIVED, STATE_STALE,
};

/// Curator persistent state (stored at `skills_dir/.curator_state`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CuratorState {
    #[serde(default)]
    pub last_run_at: Option<String>,
    #[serde(default)]
    pub last_run_duration_seconds: Option<f64>,
    #[serde(default)]
    pub last_run_summary: Option<String>,
    #[serde(default)]
    pub last_run_summary_shown_at: Option<String>,
    #[serde(default)]
    pub last_report_path: Option<String>,
    #[serde(default)]
    pub paused: bool,
    #[serde(default)]
    pub run_count: u64,
}

const STATE_FILE: &str = ".curator_state";

/// Load curator state from `skills_dir/.curator_state`.
///
/// Returns [`CuratorState::default()`] when the file is missing or contains
/// invalid JSON (mirrors Python fault-tolerance).
pub fn load_curator_state(skills_dir: &Path) -> CuratorState {
    let path = skills_dir.join(STATE_FILE);
    let Ok(raw) = fs::read_to_string(&path) else {
        tracing::debug!("curator state file not found, using defaults");
        return CuratorState::default();
    };
    match serde_json::from_str::<CuratorState>(&raw) {
        Ok(state) => state,
        Err(e) => {
            tracing::warn!("failed to parse curator state, resetting to default: {e}");
            CuratorState::default()
        }
    }
}

/// Atomically persist curator state to `skills_dir/.curator_state`.
///
/// Writes to a temporary file first then renames over the target to prevent
/// corruption on crash. On Windows the target is removed before rename if it
/// already exists (Windows `rename` semantics differ from POSIX).
pub fn save_curator_state(skills_dir: &Path, state: &CuratorState) -> Result<(), std::io::Error> {
    fs::create_dir_all(skills_dir)?;
    let path = skills_dir.join(STATE_FILE);
    let tmp = skills_dir.join(format!(".curator_state_{}.tmp", std::process::id()));

    let body = serde_json::to_string_pretty(state).map_err(|e| {
        std::io::Error::new(std::io::ErrorKind::InvalidData, e)
    })?;

    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(body.as_bytes())?;
        file.flush()?;
    }

    // On Windows, fs::rename fails if the destination exists.
    #[cfg(target_os = "windows")]
    {
        if path.exists() {
            let _ = fs::remove_file(&path);
        }
    }

    fs::rename(&tmp, &path)?;
    tracing::debug!("curator state saved to {}", path.display());
    Ok(())
}

/// Set the `paused` flag in curator state (load → mutate → save).
pub fn set_paused(skills_dir: &Path, paused: bool) -> Result<(), std::io::Error> {
    let mut state = load_curator_state(skills_dir);
    state.paused = paused;
    save_curator_state(skills_dir, &state)
}

/// Check whether the curator is currently paused.
pub fn is_paused(skills_dir: &Path) -> bool {
    load_curator_state(skills_dir).paused
}

// ---------------------------------------------------------------------------
// CuratorConfig (local mirror to avoid depending on hermes-config)
// ---------------------------------------------------------------------------

/// Curator engine configuration.
///
/// This is a local mirror of `hermes_config::CuratorConfig` so that
/// `hermes-skills` does not need a direct dependency on `hermes-config`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CuratorConfig {
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default = "default_interval_hours")]
    pub interval_hours: u64,
    #[serde(default = "default_min_idle_hours")]
    pub min_idle_hours: u64,
    #[serde(default = "default_stale_after_days")]
    pub stale_after_days: u64,
    #[serde(default = "default_archive_after_days")]
    pub archive_after_days: u64,
}

fn default_enabled() -> bool {
    true
}
fn default_interval_hours() -> u64 {
    168
}
fn default_min_idle_hours() -> u64 {
    2
}
fn default_stale_after_days() -> u64 {
    30
}
fn default_archive_after_days() -> u64 {
    90
}

impl Default for CuratorConfig {
    fn default() -> Self {
        Self {
            enabled: default_enabled(),
            interval_hours: default_interval_hours(),
            min_idle_hours: default_min_idle_hours(),
            stale_after_days: default_stale_after_days(),
            archive_after_days: default_archive_after_days(),
        }
    }
}

// ---------------------------------------------------------------------------
// TransitionResult
// ---------------------------------------------------------------------------

/// 自动状态迁移的执行结果
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TransitionResult {
    pub checked: u64,
    pub marked_stale: u64,
    pub archived: u64,
    pub reactivated: u64,
}

// ---------------------------------------------------------------------------
// Automatic transitions
// ---------------------------------------------------------------------------

/// 应用确定性状态迁移规则。
/// 不涉及 LLM，纯规则引擎。
///
/// 规则：
/// - 跳过 pinned 技能
/// - archive 阈值: now - archive_after_days
/// - stale 阈值: now - stale_after_days
/// - 活跃锚点: last_used_at / last_viewed_at / last_patched_at 中最大值
/// - 转换: active→stale, stale→active(reactivation), *→archived
pub fn apply_automatic_transitions(
    skills_dir: &Path,
    config: &CuratorConfig,
) -> TransitionResult {
    let now = Utc::now();
    let stale_cutoff = now - chrono::Duration::seconds((config.stale_after_days * 86400) as i64);
    let archive_cutoff = now - chrono::Duration::seconds((config.archive_after_days * 86400) as i64);

    let usage = load_usage(skills_dir);
    let mut result = TransitionResult::default();

    for (name, record) in &usage {
        // Skip pinned skills
        if record.pinned {
            continue;
        }

        // Determine activity anchor (max of last_used_at, last_viewed_at, last_patched_at)
        let anchor_str = [
            record.last_used_at.as_deref(),
            record.last_viewed_at.as_deref(),
            record.last_patched_at.as_deref(),
        ]
        .into_iter()
        .flatten()
        .max();

        let Some(anchor_str) = anchor_str else {
            // No activity records — cannot determine staleness, skip
            continue;
        };

        let anchor: DateTime<Utc> = match anchor_str.parse() {
            Ok(dt) => dt,
            Err(e) => {
                tracing::warn!(
                    "curator: failed to parse anchor timestamp for skill '{}': {}",
                    name, e
                );
                continue;
            }
        };

        result.checked += 1;

        // Archive rule (highest priority)
        if anchor <= archive_cutoff && record.state != STATE_ARCHIVED {
            if let Err(e) = set_state(skills_dir, name, STATE_ARCHIVED) {
                tracing::warn!("curator: failed to archive skill '{}': {}", name, e);
            } else {
                tracing::debug!("curator: archived skill '{}'", name);
                result.archived += 1;
            }
            continue;
        }

        // Stale rule
        if anchor <= stale_cutoff && record.state == STATE_ACTIVE {
            if let Err(e) = set_state(skills_dir, name, STATE_STALE) {
                tracing::warn!("curator: failed to mark skill '{}' stale: {}", name, e);
            } else {
                tracing::debug!("curator: marked skill '{}' as stale", name);
                result.marked_stale += 1;
            }
            continue;
        }

        // Reactivation rule
        if anchor > stale_cutoff && record.state == STATE_STALE {
            if let Err(e) = set_state(skills_dir, name, STATE_ACTIVE) {
                tracing::warn!("curator: failed to reactivate skill '{}': {}", name, e);
            } else {
                tracing::debug!("curator: reactivated skill '{}'", name);
                result.reactivated += 1;
            }
        }
    }

    result
}

// ---------------------------------------------------------------------------
// Scheduling gate
// ---------------------------------------------------------------------------

/// 判断 curator 是否应该执行。
/// 检查：enabled、not paused、last_run_at 超过 interval。
/// 首次运行时种子化 last_run_at 为当前时间（延迟一个 interval 再真正执行）。
pub fn should_run_now(skills_dir: &Path, config: &CuratorConfig) -> bool {
    if !config.enabled {
        return false;
    }

    let mut state = load_curator_state(skills_dir);

    if state.paused {
        return false;
    }

    let now = Utc::now();

    // First run: seed last_run_at and defer execution
    let Some(ref last_run_str) = state.last_run_at else {
        state.last_run_at = Some(now.to_rfc3339());
        if let Err(e) = save_curator_state(skills_dir, &state) {
            tracing::warn!("curator: failed to seed last_run_at: {}", e);
        }
        return false;
    };

    let last_run: DateTime<Utc> = match last_run_str.parse() {
        Ok(dt) => dt,
        Err(e) => {
            tracing::warn!("curator: failed to parse last_run_at: {}", e);
            // Treat as first run
            state.last_run_at = Some(now.to_rfc3339());
            let _ = save_curator_state(skills_dir, &state);
            return false;
        }
    };

    let interval = chrono::Duration::seconds((config.interval_hours * 3600) as i64);
    now - last_run >= interval
}

/// 组合调度门控：idle 时间足够 + should_run_now。
/// 由 session 主循环在空闲时调用。
pub fn maybe_run_curator(skills_dir: &Path, config: &CuratorConfig, idle_seconds: u64) -> bool {
    if idle_seconds < config.min_idle_hours * 3600 {
        return false;
    }
    should_run_now(skills_dir, config)
}

// ---------------------------------------------------------------------------
// Classification / reconciliation structs
// ---------------------------------------------------------------------------

/// 结构化摘要中的一条 consolidation 记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConsolidationEntry {
    pub from: String,
    pub into: String,
    pub reason: String,
    pub source: String, // "model" | "heuristic" | "fallback"
}

/// 结构化摘要中的一条 pruning 记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PruningEntry {
    pub name: String,
    pub reason: String,
    pub source: String, // "model" | "heuristic" | "fallback"
}

/// LLM 结构化摘要解析结果
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StructuredSummary {
    pub consolidations: Vec<ConsolidationEntry>,
    pub prunings: Vec<PruningEntry>,
}

/// absorbed_into 声明（从 tool calls 中提取）
#[derive(Debug, Clone)]
pub struct AbsorbedDeclaration {
    pub into: String,
    pub declared: bool,
}

/// 分类结果
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClassificationResult {
    pub consolidated: Vec<ConsolidationEntry>,
    pub pruned: Vec<PruningEntry>,
}

/// 完整的 curator 运行报告
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CuratorRunReport {
    pub started_at: String,
    pub duration_seconds: f64,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub dry_run: bool,
    pub auto_transitions: TransitionResult,
    pub counts: CuratorRunCounts,
    pub consolidated: Vec<ConsolidationEntry>,
    pub pruned: Vec<PruningEntry>,
    pub tool_calls: Vec<ToolCallRecord>,
    pub llm_error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CuratorRunCounts {
    pub before: u64,
    pub after: u64,
    pub delta: i64,
    pub archived_this_run: u64,
    pub consolidated_this_run: u64,
    pub pruned_this_run: u64,
    pub state_transitions: u64,
    pub tool_calls_total: u64,
}

// ---------------------------------------------------------------------------
// CuratorError
// ---------------------------------------------------------------------------

/// Errors that can occur during a curator run.
#[derive(Debug, thiserror::Error)]
pub enum CuratorError {
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Curator is disabled")]
    Disabled,
    #[error("Curator is paused")]
    Paused,
    #[error("LLM review failed: {0}")]
    LlmError(String),
    #[error("Timeout after {0} seconds")]
    Timeout(u64),
}

// ---------------------------------------------------------------------------
// LLM Review types
// ---------------------------------------------------------------------------

/// 记录 curator LLM session 中的单个工具调用
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRecord {
    pub name: String,
    pub arguments: String,
}

/// Curator LLM review 的完整结果
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CuratorReviewResult {
    pub final_response: String,
    pub summary: String,
    pub model: String,
    pub provider: String,
    pub tool_calls: Vec<ToolCallRecord>,
    pub error: Option<String>,
    pub duration_seconds: f64,
}

/// 一次 curator run 的完整记录
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CuratorRunRecord {
    pub started_at: String,
    pub duration_seconds: f64,
    pub dry_run: bool,
    pub auto_transitions: TransitionResult,
    pub llm_review: Option<CuratorReviewResult>,
    pub model: Option<String>,
    pub provider: Option<String>,
}

// ---------------------------------------------------------------------------
// build_curator_prompt
// ---------------------------------------------------------------------------

/// 构建完整的 curator prompt，包含技能候选列表。
///
/// 加载 skills_dir 下所有 agent-created 技能的状态信息，
/// 格式化为表格附加在 [`CURATOR_REVIEW_PROMPT`] 后面。
pub fn build_curator_prompt(skills_dir: &Path) -> String {
    let rows = agent_created_report(skills_dir);

    let mut table = String::from("## Current skill inventory\n\n");
    table.push_str("| Name | State | Pinned | Activity | Last Active |\n");
    table.push_str("|------|-------|--------|----------|-------------|\n");

    let now = Utc::now();
    for row in &rows {
        let pinned_str = if row.pinned { "yes" } else { "no" };
        let last_active = match &row.last_activity_at {
            Some(ts) => format_relative_time(ts, now),
            None => "never".to_string(),
        };
        table.push_str(&format!(
            "| {} | {} | {} | {} | {} |\n",
            row.name, row.state, pinned_str, row.activity_count, last_active,
        ));
    }

    format!("{}\n\n{}", CURATOR_REVIEW_PROMPT, table)
}

/// Format a timestamp as a relative duration (e.g. "2d ago", "5h ago").
fn format_relative_time(ts: &str, now: DateTime<Utc>) -> String {
    let Ok(dt) = ts.parse::<DateTime<Utc>>() else {
        return ts.to_string();
    };
    let delta = now.signed_duration_since(dt);
    let secs = delta.num_seconds();
    if secs < 0 {
        return "now".to_string();
    }
    if secs < 3600 {
        return format!("{}m ago", secs / 60);
    }
    if secs < 86400 {
        return format!("{}h ago", secs / 3600);
    }
    let days = secs / 86400;
    if days < 30 {
        return format!("{}d ago", days);
    }
    format!("{}d ago", days)
}

// ---------------------------------------------------------------------------
// run_curator_review (orchestrator)
// ---------------------------------------------------------------------------

/// 执行完整的 curator review 流程（auto transitions + LLM pass）。
///
/// `llm_runner` 参数由调用方提供，负责实际 spawn AIAgent。
/// 当 `llm_runner` 为 `None` 时仅执行确定性自动状态迁移。
///
/// 返回 [`CuratorRunRecord`] 记录本次运行的完整结果。
pub async fn run_curator_review<F, Fut>(
    skills_dir: &Path,
    config: &CuratorConfig,
    dry_run: bool,
    llm_runner: Option<F>,
) -> Result<CuratorRunRecord, CuratorError>
where
    F: FnOnce(String) -> Fut + Send,
    Fut: Future<Output = Result<CuratorReviewResult, CuratorError>> + Send,
{
    if !config.enabled {
        return Err(CuratorError::Disabled);
    }

    let state = load_curator_state(skills_dir);
    if state.paused {
        return Err(CuratorError::Paused);
    }

    let started_at = Utc::now();

    // Phase 1: deterministic auto-transitions
    let auto_transitions = if dry_run {
        TransitionResult::default()
    } else {
        apply_automatic_transitions(skills_dir, config)
    };

    // Phase 2: LLM review (optional)
    let llm_review = if let Some(runner) = llm_runner {
        let prompt = build_curator_prompt(skills_dir);
        match runner(prompt).await {
            Ok(result) => Some(result),
            Err(e) => {
                tracing::warn!("curator LLM review failed: {}", e);
                return Err(e);
            }
        }
    } else {
        None
    };

    let duration_seconds = (Utc::now() - started_at).num_milliseconds() as f64 / 1000.0;

    let model = llm_review.as_ref().map(|r| r.model.clone());
    let provider = llm_review.as_ref().map(|r| r.provider.clone());

    // Update curator state
    if !dry_run {
        let mut curator_state = load_curator_state(skills_dir);
        curator_state.last_run_at = Some(started_at.to_rfc3339());
        curator_state.last_run_duration_seconds = Some(duration_seconds);
        if let Some(ref review) = llm_review {
            curator_state.last_run_summary = Some(review.summary.clone());
        }
        curator_state.run_count += 1;
        if let Err(e) = save_curator_state(skills_dir, &curator_state) {
            tracing::warn!("curator: failed to save post-run state: {}", e);
        }
    }

    Ok(CuratorRunRecord {
        started_at: started_at.to_rfc3339(),
        duration_seconds,
        dry_run,
        auto_transitions,
        llm_review,
        model,
        provider,
    })
}

// ---------------------------------------------------------------------------
// parse_structured_summary
// ---------------------------------------------------------------------------

/// 从 LLM 最终响应中解析 YAML 格式的结构化摘要。
///
/// 寻找 ```yaml 或 ```yml code block，解析其中的 consolidations/prunings 列表。
/// 容错：解析失败返回空 StructuredSummary。
pub fn parse_structured_summary(llm_final: &str) -> StructuredSummary {
    // Use regex to extract ```yaml or ```yml code block
    let re = match Regex::new(r"(?s)```ya?ml\s*\n(.*?)\n```") {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("curator: failed to compile yaml regex: {}", e);
            return StructuredSummary::default();
        }
    };

    let Some(caps) = re.captures(llm_final) else {
        tracing::debug!("curator: no yaml code block found in LLM response");
        return StructuredSummary::default();
    };

    let yaml_content = &caps[1];

    // Parse YAML with serde_yaml
    let yaml_value: serde_yaml::Value = match serde_yaml::from_str(yaml_content) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!("curator: failed to parse yaml block: {}", e);
            return StructuredSummary::default();
        }
    };

    let mut result = StructuredSummary::default();

    // Parse consolidations
    if let Some(consolidations) = yaml_value.get("consolidations").and_then(|v| v.as_sequence()) {
        for item in consolidations {
            let from = item
                .get("from")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let into = item
                .get("into")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let reason = item
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if !from.is_empty() {
                result.consolidations.push(ConsolidationEntry {
                    from,
                    into,
                    reason,
                    source: "model".to_string(),
                });
            }
        }
    }

    // Parse prunings
    if let Some(prunings) = yaml_value.get("prunings").and_then(|v| v.as_sequence()) {
        for item in prunings {
            let name = item
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            let reason = item
                .get("reason")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_string();
            if !name.is_empty() {
                result.prunings.push(PruningEntry {
                    name,
                    reason,
                    source: "model".to_string(),
                });
            }
        }
    }

    tracing::debug!(
        "curator: parsed structured summary: {} consolidations, {} prunings",
        result.consolidations.len(),
        result.prunings.len()
    );
    result
}

// ---------------------------------------------------------------------------
// extract_absorbed_into_declarations
// ---------------------------------------------------------------------------

/// 从 tool calls 中提取 model 声明的 absorbed_into 信息。
///
/// 遍历 tool calls，找到 skill_manage action=delete 的调用，
/// 提取其 absorbed_into 参数作为权威信号。
pub fn extract_absorbed_into_declarations(
    tool_calls: &[ToolCallRecord],
) -> HashMap<String, AbsorbedDeclaration> {
    let mut declarations = HashMap::new();

    for tc in tool_calls {
        if tc.name != "skill_manage" {
            continue;
        }

        let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments) else {
            continue;
        };

        let action = args.get("action").and_then(|v| v.as_str()).unwrap_or_default();
        if action != "delete" {
            continue;
        }

        let Some(name) = args.get("name").and_then(|v| v.as_str()) else {
            continue;
        };

        let into = args
            .get("absorbed_into")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();

        declarations.insert(
            name.to_string(),
            AbsorbedDeclaration {
                into,
                declared: true,
            },
        );
    }

    tracing::debug!(
        "curator: extracted {} absorbed_into declarations",
        declarations.len()
    );
    declarations
}

// ---------------------------------------------------------------------------
// classify_removed_skills
// ---------------------------------------------------------------------------

/// 启发式分类：通过审查 tool call 参数判断被移除技能是被 consolidate 还是 prune。
///
/// 检查 tool calls 的参数中是否引用了被移除技能的名称。
/// 如果某个被移除技能的名称出现在另一个技能的创建/修改中，
/// 则认为它被 consolidated；否则认为它被 pruned。
pub fn classify_removed_skills(
    removed: &[String],
    after_names: &[String],
    tool_calls: &[ToolCallRecord],
) -> ClassificationResult {
    let mut result = ClassificationResult::default();

    for skill_name in removed {
        let mut found_in: Option<String> = None;

        // Check each tool call's arguments for references to this skill
        for tc in tool_calls {
            // Skip delete calls for this skill itself
            if tc.name == "skill_manage"
                && let Ok(args) = serde_json::from_str::<serde_json::Value>(&tc.arguments)
            {
                let action = args.get("action").and_then(|v| v.as_str()).unwrap_or_default();
                let tc_name = args.get("name").and_then(|v| v.as_str()).unwrap_or_default();
                if action == "delete" && tc_name == skill_name {
                    continue;
                }
            }

            // Check if arguments reference the removed skill
            let args_str = &tc.arguments;

            // Check as path component (e.g. "/skill_name/" or "skill_name/")
            let path_pattern = format!("/{}/", skill_name);
            let path_pattern_start = format!("{}/", skill_name);
            let has_path_ref =
                args_str.contains(&path_pattern) || args_str.starts_with(&path_pattern_start);

            // Check as word in content (simple substring for the skill name)
            let has_content_ref = if !has_path_ref {
                // Check if skill name appears in content fields
                args_str.contains(skill_name.as_str())
            } else {
                true
            };

            if has_content_ref || has_path_ref {
                // Try to determine the target skill from this tool call
                if let Ok(args) = serde_json::from_str::<serde_json::Value>(args_str) {
                    let target = args
                        .get("name")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                        .or_else(|| {
                            // Try to extract from file_path
                            args.get("file_path")
                                .and_then(|v| v.as_str())
                                .and_then(|p| {
                                    p.split('/')
                                        .find(|seg| {
                                            !seg.is_empty()
                                                && seg != skill_name
                                                && after_names.contains(&seg.to_string())
                                        })
                                        .map(|s| s.to_string())
                                })
                        });

                    if let Some(t) = target
                        && t != *skill_name
                        && after_names.contains(&t)
                    {
                        found_in = Some(t);
                        break;
                    }
                }

                // Even if we can't determine target, mark as consolidated
                if found_in.is_none() {
                    found_in = Some(String::new());
                    break;
                }
            }
        }

        match found_in {
            Some(target) => {
                result.consolidated.push(ConsolidationEntry {
                    from: skill_name.clone(),
                    into: target,
                    reason: String::new(),
                    source: "heuristic".to_string(),
                });
            }
            None => {
                result.pruned.push(PruningEntry {
                    name: skill_name.clone(),
                    reason: String::new(),
                    source: "heuristic".to_string(),
                });
            }
        }
    }

    result
}

// ---------------------------------------------------------------------------
// reconcile_classification
// ---------------------------------------------------------------------------

/// 调和分类结果，按优先级合并多个来源的信号。
///
/// 优先级（第一个匹配者获胜）：
/// 1. Model 声明的 absorbed_into（来自 delete tool call）— 权威
/// 2. Model YAML 中声明的 consolidation（目标存在）
/// 3. Model YAML 中声明的 consolidation（目标缺失 → 降级为 heuristic）
/// 4. Heuristic 分类结果
/// 5. Model YAML 中的 pruning
/// 6. 无证据 fallback → pruned
pub fn reconcile_classification(
    removed: &[String],
    heuristic: &ClassificationResult,
    model_summary: &StructuredSummary,
    absorbed_declarations: &HashMap<String, AbsorbedDeclaration>,
    after_names: &[String],
) -> ClassificationResult {
    let mut result = ClassificationResult::default();

    for skill_name in removed {
        // Priority 1: absorbed_into declarations (authoritative)
        if let Some(decl) = absorbed_declarations.get(skill_name) {
            result.consolidated.push(ConsolidationEntry {
                from: skill_name.clone(),
                into: decl.into.clone(),
                reason: "model declared absorbed_into at delete time".to_string(),
                source: "model".to_string(),
            });
            continue;
        }

        // Priority 2 & 3: Model YAML consolidation
        if let Some(model_cons) = model_summary
            .consolidations
            .iter()
            .find(|c| c.from == *skill_name)
        {
            if after_names.contains(&model_cons.into) {
                // Priority 2: target exists
                result.consolidated.push(ConsolidationEntry {
                    from: skill_name.clone(),
                    into: model_cons.into.clone(),
                    reason: model_cons.reason.clone(),
                    source: "model".to_string(),
                });
            } else {
                // Priority 3: target missing → check heuristic
                if let Some(h_cons) = heuristic
                    .consolidated
                    .iter()
                    .find(|c| c.from == *skill_name)
                {
                    result.consolidated.push(ConsolidationEntry {
                        from: skill_name.clone(),
                        into: h_cons.into.clone(),
                        reason: model_cons.reason.clone(),
                        source: "heuristic".to_string(),
                    });
                } else {
                    // Heuristic says pruned, use heuristic
                    result.pruned.push(PruningEntry {
                        name: skill_name.clone(),
                        reason: model_cons.reason.clone(),
                        source: "heuristic".to_string(),
                    });
                }
            }
            continue;
        }

        // Priority 4: Heuristic classification
        if let Some(h_cons) = heuristic
            .consolidated
            .iter()
            .find(|c| c.from == *skill_name)
        {
            result.consolidated.push(ConsolidationEntry {
                from: skill_name.clone(),
                into: h_cons.into.clone(),
                reason: h_cons.reason.clone(),
                source: "heuristic".to_string(),
            });
            continue;
        }

        // Priority 5: Model YAML pruning
        if let Some(model_prune) = model_summary.prunings.iter().find(|p| p.name == *skill_name) {
            result.pruned.push(PruningEntry {
                name: skill_name.clone(),
                reason: model_prune.reason.clone(),
                source: "model".to_string(),
            });
            continue;
        }

        // Priority 6: No evidence fallback → pruned
        result.pruned.push(PruningEntry {
            name: skill_name.clone(),
            reason: "no evidence of consolidation or model declaration".to_string(),
            source: "fallback".to_string(),
        });
    }

    result
}

// ---------------------------------------------------------------------------
// write_curator_report
// ---------------------------------------------------------------------------

/// 将 curator 运行报告写入日志目录。
///
/// 创建 `base_dir/curator/{YYYYMMDD-HHMMSS}/` 目录，
/// 写入 run.json（机器可读）和 REPORT.md（人类可读）。
/// 返回报告目录路径。
///
/// `base_dir` 应为 `~/.hermes/logs`（由调用方传入）。
pub fn write_curator_report(
    report: &CuratorRunReport,
    base_dir: &Path,
) -> Result<PathBuf, CuratorError> {
    let now = Utc::now();
    let ts_dir = now.format("%Y%m%d-%H%M%S").to_string();
    let curator_dir = base_dir.join("curator");

    // Find a unique directory name
    let mut dir = curator_dir.join(&ts_dir);
    if dir.exists() {
        let mut suffix = 1u32;
        loop {
            dir = curator_dir.join(format!("{}-{}", ts_dir, suffix));
            if !dir.exists() {
                break;
            }
            suffix += 1;
            if suffix > 100 {
                return Err(CuratorError::Io(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    "too many report directories with same timestamp",
                )));
            }
        }
    }

    fs::create_dir_all(&dir)?;

    // Write run.json
    let json_content = serde_json::to_string_pretty(report).map_err(|e| {
        CuratorError::Io(std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    })?;
    let json_path = dir.join("run.json");
    fs::write(&json_path, json_content)?;

    // Write REPORT.md
    let md_content = build_report_markdown(report);
    let md_path = dir.join("REPORT.md");
    fs::write(&md_path, md_content)?;

    tracing::info!("curator: report written to {}", dir.display());
    Ok(dir)
}

/// Build the markdown content for the curator run report.
fn build_report_markdown(report: &CuratorRunReport) -> String {
    let mut md = String::new();

    md.push_str("# Curator Run Report\n\n");

    let model_str = report.model.as_deref().unwrap_or("unknown");
    let provider_str = report.provider.as_deref().unwrap_or("unknown");
    let dry_run_str = if report.dry_run { "yes" } else { "no" };

    md.push_str(&format!("- **Model**: {} ({})\n", model_str, provider_str));
    md.push_str(&format!("- **Duration**: {}s\n", report.duration_seconds));
    md.push_str(&format!("- **Dry run**: {}\n", dry_run_str));
    md.push('\n');

    // Auto-transitions
    md.push_str("## Auto-transitions\n\n");
    md.push_str(&format!(
        "- Checked: {}\n",
        report.auto_transitions.checked
    ));
    md.push_str(&format!(
        "- Marked stale: {}\n",
        report.auto_transitions.marked_stale
    ));
    md.push_str(&format!(
        "- Archived: {}\n",
        report.auto_transitions.archived
    ));
    md.push_str(&format!(
        "- Reactivated: {}\n",
        report.auto_transitions.reactivated
    ));
    md.push('\n');

    // LLM Pass
    md.push_str("## LLM Pass\n\n");
    md.push_str(&format!(
        "- Consolidated: {} skills\n",
        report.counts.consolidated_this_run
    ));
    md.push_str(&format!(
        "- Pruned: {} skills\n",
        report.counts.pruned_this_run
    ));
    md.push_str(&format!(
        "- Tool calls: {}\n",
        report.counts.tool_calls_total
    ));
    md.push('\n');

    if let Some(ref err) = report.llm_error {
        md.push_str(&format!("**LLM Error**: {}\n\n", err));
    }

    // Consolidated table
    md.push_str("### Consolidated\n\n");
    if report.consolidated.is_empty() {
        md.push_str("_None_\n\n");
    } else {
        md.push_str("| Skill | Into | Reason | Source |\n");
        md.push_str("|-------|------|--------|--------|\n");
        for entry in &report.consolidated {
            md.push_str(&format!(
                "| {} | {} | {} | {} |\n",
                entry.from, entry.into, entry.reason, entry.source
            ));
        }
        md.push('\n');
    }

    // Pruned table
    md.push_str("### Pruned\n\n");
    if report.pruned.is_empty() {
        md.push_str("_None_\n\n");
    } else {
        md.push_str("| Skill | Reason | Source |\n");
        md.push_str("|-------|--------|--------|\n");
        for entry in &report.pruned {
            md.push_str(&format!(
                "| {} | {} | {} |\n",
                entry.name, entry.reason, entry.source
            ));
        }
        md.push('\n');
    }

    md
}
