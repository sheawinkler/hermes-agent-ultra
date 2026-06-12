//! Structured persistence for background memory/skill review (P0-B evolution ledger).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Instant;

use hermes_core::Message;
use hermes_core::MessageRole;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent_config::AgentConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewTrigger {
    MemoryNudge,
    SkillNudge,
    Combined,
}

impl ReviewTrigger {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MemoryNudge => "memory_nudge",
            Self::SkillNudge => "skill_nudge",
            Self::Combined => "combined",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewStatus {
    Started,
    Completed,
    Failed,
}

impl ReviewStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Started => "started",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewToolAction {
    pub name: String,
    pub success: bool,
    pub target: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewEvent {
    pub id: String,
    pub ts_utc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_key: Option<String>,
    pub trigger: ReviewTrigger,
    pub status: ReviewStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tools: Vec<ReviewToolAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary_chat: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn evolution_ledger_enabled(config: &AgentConfig) -> bool {
    if !config.evolution_ledger_enabled {
        return false;
    }
    !std::env::var("HERMES_EVOLUTION_LEDGER")
        .ok()
        .is_some_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "0" | "false" | "off" | "no"
            )
        })
}

pub fn resolve_hermes_home(config: &AgentConfig) -> PathBuf {
    config
        .hermes_home
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(hermes_config::paths::hermes_home)
}

pub fn reviews_path_for_home(home: &Path) -> PathBuf {
    hermes_config::paths::evolution_reviews_path_in(home)
}

fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339()
}

pub fn append_event(home: &Path, event: &ReviewEvent, max_entries: u32) -> std::io::Result<()> {
    let path = reviews_path_for_home(home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let line = serde_json::to_string(event).map_err(std::io::Error::other)?;
    {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(file, "{line}")?;
    }
    if max_entries > 0 {
        rotate_if_needed(&path, max_entries)?;
    }
    Ok(())
}

fn rotate_if_needed(path: &Path, max_entries: u32) -> std::io::Result<()> {
    let content = std::fs::read_to_string(path)?;
    let lines: Vec<&str> = content.lines().filter(|l| !l.trim().is_empty()).collect();
    if lines.len() <= max_entries as usize {
        return Ok(());
    }
    let keep = &lines[lines.len() - max_entries as usize..];
    let mut file = std::fs::File::create(path)?;
    for line in keep {
        writeln!(file, "{line}")?;
    }
    Ok(())
}

pub fn read_all_events(home: &Path) -> Vec<ReviewEvent> {
    let path = reviews_path_for_home(home);
    let Ok(content) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    content
        .lines()
        .filter_map(|line| serde_json::from_str::<ReviewEvent>(line.trim()).ok())
        .collect()
}

pub fn read_recent(home: &Path, limit: usize) -> Vec<ReviewEvent> {
    let mut events: Vec<ReviewEvent> = read_all_events(home)
        .into_iter()
        .filter(|e| matches!(e.status, ReviewStatus::Completed | ReviewStatus::Failed))
        .collect();
    events.reverse();
    events.truncate(limit);
    events
}

pub fn review_trigger(review_memory: bool, review_skills: bool) -> Option<ReviewTrigger> {
    match (review_memory, review_skills) {
        (true, true) => Some(ReviewTrigger::Combined),
        (true, false) => Some(ReviewTrigger::MemoryNudge),
        (false, true) => Some(ReviewTrigger::SkillNudge),
        _ => None,
    }
}

pub fn new_review_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn started_event(
    id: String,
    session_key: Option<String>,
    trigger: ReviewTrigger,
) -> ReviewEvent {
    ReviewEvent {
        id,
        ts_utc: now_rfc3339(),
        session_key,
        trigger,
        status: ReviewStatus::Started,
        duration_ms: None,
        tools: Vec::new(),
        summary_chat: None,
        error: None,
    }
}

pub fn completed_event(
    id: String,
    session_key: Option<String>,
    trigger: ReviewTrigger,
    duration_ms: u64,
    tools: Vec<ReviewToolAction>,
    summary_chat: Option<String>,
) -> ReviewEvent {
    ReviewEvent {
        id,
        ts_utc: now_rfc3339(),
        session_key,
        trigger,
        status: ReviewStatus::Completed,
        duration_ms: Some(duration_ms),
        tools,
        summary_chat,
        error: None,
    }
}

pub fn failed_event(
    id: String,
    session_key: Option<String>,
    trigger: ReviewTrigger,
    duration_ms: u64,
    error: String,
) -> ReviewEvent {
    ReviewEvent {
        id,
        ts_utc: now_rfc3339(),
        session_key,
        trigger,
        status: ReviewStatus::Failed,
        duration_ms: Some(duration_ms),
        tools: Vec::new(),
        summary_chat: None,
        error: Some(error),
    }
}

pub struct ReviewTimer {
    start: Instant,
}

impl ReviewTimer {
    pub fn start() -> Self {
        Self {
            start: Instant::now(),
        }
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.start.elapsed().as_millis().min(u64::MAX as u128) as u64
    }
}

fn is_safe_background_review_message(message: &str) -> bool {
    let trimmed = message.trim();
    if trimmed.is_empty() || trimmed.len() > 200 {
        return false;
    }
    let lower = trimmed.to_ascii_lowercase();
    if lower.contains("status:")
        || lower.contains("status=")
        || lower.contains("token")
        || lower.contains("api_key")
        || lower.contains("secret")
        || lower.contains("credential")
    {
        return false;
    }
    if (trimmed.contains('{') && trimmed.contains('}')) || trimmed.contains('\n') {
        return false;
    }
    true
}

fn chat_action_label(message: &str, target: &str) -> Option<String> {
    if message.is_empty() || !is_safe_background_review_message(message) {
        return None;
    }
    let lower = message.to_ascii_lowercase();
    if lower.contains("created") || lower.contains("updated") {
        return Some(message.to_string());
    }
    if lower.contains("added") || (!target.is_empty() && lower.contains("add")) {
        let label = match target {
            "memory" => "Memory",
            "user" => "User profile",
            _ => target,
        };
        if label.is_empty() {
            return None;
        }
        return Some(format!("{label} updated"));
    }
    if message.contains("Entry added") {
        let label = match target {
            "memory" => "Memory",
            "user" => "User profile",
            _ => target,
        };
        if label.is_empty() {
            return None;
        }
        return Some(format!("{label} updated"));
    }
    if lower.contains("removed") || lower.contains("replaced") {
        let label = match target {
            "memory" => "Memory",
            "user" => "User profile",
            _ => target,
        };
        if label.is_empty() {
            return None;
        }
        return Some(format!("{label} updated"));
    }
    None
}

pub fn extract_review_tools(messages: &[Message]) -> Vec<ReviewToolAction> {
    let mut actions = Vec::new();
    for msg in messages {
        if !matches!(msg.role, MessageRole::Tool) {
            continue;
        }
        let Some(raw) = msg.content.as_deref() else {
            continue;
        };
        let Ok(data) = serde_json::from_str::<Value>(raw) else {
            continue;
        };
        let success = data
            .get("success")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        let message = data
            .get("message")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        let target = data
            .get("target")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .trim()
            .to_string();
        if !success && message.is_empty() {
            continue;
        }
        let name = msg
            .name
            .clone()
            .unwrap_or_else(|| "tool".to_string());
        actions.push(ReviewToolAction {
            name,
            success,
            target,
            message,
        });
    }
    actions
}

pub fn summarize_review_for_chat(messages: &[Message]) -> Option<String> {
    let tools = extract_review_tools(messages);
    let mut actions: Vec<String> = Vec::new();
    for tool in tools {
        if !tool.success {
            continue;
        }
        if let Some(label) = chat_action_label(&tool.message, &tool.target) {
            actions.push(label);
        }
    }
    if actions.is_empty() {
        return None;
    }
    let mut deduped: Vec<String> = Vec::new();
    for action in actions {
        if !deduped.iter().any(|a| a == &action) {
            deduped.push(action);
        }
    }
    Some(format!("\u{1F9E0} {}", deduped.join(" \u{00B7} ")))
}

/// Load nudge intervals from `config.yaml` when available; otherwise use defaults.
pub fn status_agent_config() -> AgentConfig {
    let mut cfg = AgentConfig::default();
    let path = hermes_config::paths::config_path();
    if let Ok(gateway) = hermes_config::load_user_config_file(&path) {
        cfg.memory_nudge_interval = gateway.agent.memory_nudge_interval;
        cfg.skill_creation_nudge_interval = gateway.agent.skill_creation_nudge_interval;
        cfg.background_review_enabled = gateway.agent.background_review_enabled;
    }
    cfg
}

pub fn format_evolve_status(home: &Path, config: &AgentConfig) -> String {
    let recent = read_recent(home, 5);
    let mut lines = vec!["Evolution status (runtime)".to_string()];
    lines.push(format!(
        "- background review: {}",
        if config.background_review_enabled {
            "enabled"
        } else {
            "disabled"
        }
    ));
    lines.push(format!(
        "- memory nudge interval: every {} user turn(s)",
        config.memory_nudge_interval
    ));
    lines.push(format!(
        "- skill nudge interval: every {} tool iteration(s)",
        config.skill_creation_nudge_interval
    ));
    lines.push(format!(
        "- ledger: {}",
        if evolution_ledger_enabled(config) {
            reviews_path_for_home(home).display().to_string()
        } else {
            "disabled".to_string()
        }
    ));

    if recent.is_empty() {
        lines.push("- last review: none yet".to_string());
    } else if let Some(last) = recent.first() {
        let summary = last
            .summary_chat
            .as_deref()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| {
                last.tools
                    .iter()
                    .find(|t| t.success && !t.message.is_empty())
                    .map(|t| t.message.as_str())
            })
            .unwrap_or("(no changes recorded)");
        lines.push(format!(
            "- last review: {} · {} · {}",
            last.ts_utc,
            last.trigger.as_str(),
            summary
        ));
        let completed = recent
            .iter()
            .filter(|e| e.status == ReviewStatus::Completed)
            .count();
        let failed = recent
            .iter()
            .filter(|e| e.status == ReviewStatus::Failed)
            .count();
        lines.push(format!(
            "- recent (last {}): {} completed, {} failed",
            recent.len(),
            completed,
            failed
        ));
    }

    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::Message;

    #[test]
    fn append_and_read_recent_roundtrip() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        let id = new_review_id();
        append_event(
            home,
            &started_event(id.clone(), Some("sess:1".into()), ReviewTrigger::MemoryNudge),
            10,
        )
        .expect("append started");
        append_event(
            home,
            &completed_event(
                id,
                Some("sess:1".into()),
                ReviewTrigger::MemoryNudge,
                42,
                vec![ReviewToolAction {
                    name: "memory".into(),
                    success: true,
                    target: "user".into(),
                    message: "User profile updated".into(),
                }],
                Some("\u{1F9E0} User profile updated".into()),
            ),
            10,
        )
        .expect("append completed");

        let recent = read_recent(home, 5);
        assert_eq!(recent.len(), 1);
        assert_eq!(recent[0].status, ReviewStatus::Completed);
        assert_eq!(recent[0].tools.len(), 1);
    }

    #[test]
    fn ledger_rotate_drops_oldest() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        for i in 0..5 {
            append_event(
                home,
                &completed_event(
                    format!("id-{i}"),
                    None,
                    ReviewTrigger::MemoryNudge,
                    1,
                    Vec::new(),
                    None,
                ),
                3,
            )
            .expect("append");
        }
        let all = read_all_events(home);
        assert_eq!(all.len(), 3);
        assert_eq!(all.first().map(|e| e.id.as_str()), Some("id-2"));
    }

    #[test]
    fn extract_review_tools_parses_successful_memory() {
        let msgs = vec![Message::tool_result_with_name(
            "call-1",
            "memory",
            r#"{"success":true,"message":"User profile updated","target":"user"}"#,
        )];
        let tools = extract_review_tools(&msgs);
        assert_eq!(tools.len(), 1);
        assert!(tools[0].success);
        let summary = summarize_review_for_chat(&msgs).expect("summary");
        assert!(summary.contains("User profile"));
    }

    #[test]
    fn format_evolve_status_empty_ledger() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let cfg = AgentConfig::default();
        let text = format_evolve_status(tmp.path(), &cfg);
        assert!(text.contains("Evolution status"));
        assert!(text.contains("last review: none yet"));
    }

    #[test]
    fn format_evolve_status_with_completed() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path();
        append_event(
            home,
            &completed_event(
                new_review_id(),
                None,
                ReviewTrigger::Combined,
                100,
                vec![],
                Some("\u{1F9E0} Memory updated".into()),
            ),
            10,
        )
        .expect("append");
        let text = format_evolve_status(home, &AgentConfig::default());
        assert!(text.contains("last review:"));
        assert!(text.contains("combined"));
    }

    #[test]
    fn review_timer_elapsed_is_nonzero_after_sleep() {
        let timer = ReviewTimer::start();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(timer.elapsed_ms() >= 1);
    }
}
