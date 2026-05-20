use std::path::PathBuf;
use std::process::Command;

use chrono::Utc;
use hermes_core::AgentError;
use serde::{Deserialize, Serialize};

const KANBAN_STATE_DIR: &str = "alpha/kanban";
const KANBAN_STORE_FILE: &str = "boards.json";
const KANBAN_SCHEMA_VERSION: u32 = 1;
const DEFAULT_BOARD_ID: &str = "main";
const CONTEXTLATTICE_DEFAULT_SCRIPT: &str =
    "/Users/sheawinkler/Documents/Projects/scripts/agent_orchestration.py";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum KanbanLane {
    Todo,
    Doing,
    Blocked,
    Done,
}

impl KanbanLane {
    pub fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "todo" | "backlog" | "to-do" => Some(Self::Todo),
            "doing" | "in-progress" | "inprogress" | "running" => Some(Self::Doing),
            "blocked" | "stalled" => Some(Self::Blocked),
            "done" | "completed" => Some(Self::Done),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Todo => "todo",
            Self::Doing => "doing",
            Self::Blocked => "blocked",
            Self::Done => "done",
        }
    }

    pub fn all() -> [Self; 4] {
        [Self::Todo, Self::Doing, Self::Blocked, Self::Done]
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KanbanTask {
    pub id: String,
    pub title: String,
    pub lane: KanbanLane,
    pub priority: u8,
    pub assignee: Option<String>,
    pub description: Option<String>,
    pub depends_on: Vec<String>,
    pub blocked_reason: Option<String>,
    pub background_job_id: Option<String>,
    pub run_summary: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KanbanBoard {
    pub id: String,
    pub name: String,
    pub project_path: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    pub next_task_seq: u64,
    pub tasks: Vec<KanbanTask>,
    pub archived: Vec<KanbanTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KanbanStore {
    pub schema_version: u32,
    pub current_board_id: String,
    pub boards: Vec<KanbanBoard>,
}

#[derive(Debug, Clone)]
pub struct KanbanActionInput {
    pub action: String,
    pub task_id: Option<String>,
    pub lane: Option<KanbanLane>,
    pub summary: String,
}

#[derive(Debug, Clone)]
pub struct KanbanCheckpointResult {
    pub attempted: bool,
    pub succeeded: bool,
    pub detail: String,
}

#[derive(Debug, Clone)]
pub struct NewKanbanTaskInput {
    pub title: String,
    pub lane: KanbanLane,
    pub priority: u8,
    pub assignee: Option<String>,
    pub description: Option<String>,
    pub depends_on: Vec<String>,
}

impl Default for KanbanStore {
    fn default() -> Self {
        let now = now_rfc3339();
        Self {
            schema_version: KANBAN_SCHEMA_VERSION,
            current_board_id: DEFAULT_BOARD_ID.to_string(),
            boards: vec![KanbanBoard {
                id: DEFAULT_BOARD_ID.to_string(),
                name: "Main".to_string(),
                project_path: None,
                created_at: now.clone(),
                updated_at: now,
                next_task_seq: 1,
                tasks: Vec::new(),
                archived: Vec::new(),
            }],
        }
    }
}

pub fn kanban_store_path() -> PathBuf {
    hermes_config::hermes_home()
        .join(KANBAN_STATE_DIR)
        .join(KANBAN_STORE_FILE)
}

pub fn load_store() -> Result<KanbanStore, AgentError> {
    let path = kanban_store_path();
    if !path.exists() {
        let store = KanbanStore::default();
        save_store(&store)?;
        return Ok(store);
    }
    let raw = std::fs::read_to_string(&path)
        .map_err(|e| AgentError::Io(format!("read {} failed: {}", path.display(), e)))?;
    let mut store = serde_json::from_str::<KanbanStore>(&raw)
        .map_err(|e| AgentError::Config(format!("parse {} failed: {}", path.display(), e)))?;
    normalize_store(&mut store);
    Ok(store)
}

pub fn save_store(store: &KanbanStore) -> Result<PathBuf, AgentError> {
    let path = kanban_store_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("failed to create {}: {}", parent.display(), e)))?;
    }
    let serialized = serde_json::to_string_pretty(store)
        .map_err(|e| AgentError::Config(format!("serialize kanban store failed: {}", e)))?;
    std::fs::write(&path, serialized)
        .map_err(|e| AgentError::Io(format!("write {} failed: {}", path.display(), e)))?;
    Ok(path)
}

pub fn ensure_board<'a>(
    store: &'a mut KanbanStore,
    requested: Option<&str>,
) -> &'a mut KanbanBoard {
    if store.boards.is_empty() {
        *store = KanbanStore::default();
    }
    if let Some(raw) = requested {
        if let Some(idx) = store.boards.iter().position(|board| board.id == raw) {
            store.current_board_id = raw.to_string();
            return &mut store.boards[idx];
        }
        if let Some(idx) = store
            .boards
            .iter()
            .position(|board| board.name.eq_ignore_ascii_case(raw))
        {
            store.current_board_id = store.boards[idx].id.clone();
            return &mut store.boards[idx];
        }
    }
    if let Some(idx) = store
        .boards
        .iter()
        .position(|board| board.id == store.current_board_id)
    {
        return &mut store.boards[idx];
    }
    store.current_board_id = store.boards[0].id.clone();
    &mut store.boards[0]
}

pub fn create_or_select_board<'a>(
    store: &'a mut KanbanStore,
    name: &str,
    project_path: Option<String>,
) -> &'a mut KanbanBoard {
    if let Some(idx) = store
        .boards
        .iter()
        .position(|board| board.name.eq_ignore_ascii_case(name))
    {
        store.current_board_id = store.boards[idx].id.clone();
        if project_path.is_some() && store.boards[idx].project_path.is_none() {
            store.boards[idx].project_path = project_path;
            store.boards[idx].updated_at = now_rfc3339();
        }
        return &mut store.boards[idx];
    }
    let now = now_rfc3339();
    let board_id = slugify_board_id(name);
    let unique = unique_board_id(store, &board_id);
    store.boards.push(KanbanBoard {
        id: unique.clone(),
        name: name.trim().to_string(),
        project_path,
        created_at: now.clone(),
        updated_at: now,
        next_task_seq: 1,
        tasks: Vec::new(),
        archived: Vec::new(),
    });
    store.current_board_id = unique.clone();
    let idx = store
        .boards
        .iter()
        .position(|board| board.id == unique)
        .unwrap_or(0);
    &mut store.boards[idx]
}

pub fn add_task(board: &mut KanbanBoard, input: NewKanbanTaskInput) -> KanbanTask {
    let now = now_rfc3339();
    let id = format!("K-{:04}", board.next_task_seq.max(1));
    board.next_task_seq = board.next_task_seq.saturating_add(1);
    let task = KanbanTask {
        id,
        title: input.title.trim().to_string(),
        lane: input.lane,
        priority: input.priority.clamp(1, 5),
        assignee: normalize_optional(input.assignee),
        description: normalize_optional(input.description),
        depends_on: dedupe_strings(input.depends_on),
        blocked_reason: None,
        background_job_id: None,
        run_summary: None,
        created_at: now.clone(),
        updated_at: now.clone(),
        started_at: (input.lane == KanbanLane::Doing).then_some(now.clone()),
        completed_at: (input.lane == KanbanLane::Done).then_some(now),
    };
    board.updated_at = now_rfc3339();
    board.tasks.push(task.clone());
    task
}

pub fn find_task_mut<'a>(
    board: &'a mut KanbanBoard,
    id_or_title: &str,
) -> Option<&'a mut KanbanTask> {
    let needle = id_or_title.trim();
    if needle.is_empty() {
        return None;
    }
    if let Some(idx) = board
        .tasks
        .iter()
        .position(|task| task.id.eq_ignore_ascii_case(needle))
    {
        return board.tasks.get_mut(idx);
    }
    board
        .tasks
        .iter_mut()
        .find(|task| task.title.eq_ignore_ascii_case(needle))
}

pub fn move_task(task: &mut KanbanTask, lane: KanbanLane, note: Option<String>) {
    let now = now_rfc3339();
    task.lane = lane;
    task.updated_at = now.clone();
    if lane == KanbanLane::Doing && task.started_at.is_none() {
        task.started_at = Some(now.clone());
    }
    if lane == KanbanLane::Done {
        task.completed_at = Some(now.clone());
        if let Some(note) = normalize_optional(note) {
            task.run_summary = Some(note);
        }
    } else {
        task.completed_at = None;
    }
    if lane != KanbanLane::Blocked {
        task.blocked_reason = None;
    }
}

pub fn set_blocked(task: &mut KanbanTask, reason: Option<String>) {
    task.lane = KanbanLane::Blocked;
    task.blocked_reason = normalize_optional(reason);
    task.updated_at = now_rfc3339();
}

pub fn claim_task(task: &mut KanbanTask, assignee: Option<String>) {
    task.assignee = normalize_optional(assignee);
    if task.lane == KanbanLane::Todo {
        task.lane = KanbanLane::Doing;
        task.started_at = Some(now_rfc3339());
    }
    task.updated_at = now_rfc3339();
}

pub fn archive_done(board: &mut KanbanBoard) -> usize {
    let mut moved = Vec::new();
    board.tasks.retain(|task| {
        let done = task.lane == KanbanLane::Done;
        if done {
            moved.push(task.clone());
        }
        !done
    });
    let count = moved.len();
    board.archived.extend(moved);
    if count > 0 {
        board.updated_at = now_rfc3339();
    }
    count
}

pub fn lane_counts(board: &KanbanBoard) -> Vec<(KanbanLane, usize)> {
    let mut rows = Vec::new();
    for lane in KanbanLane::all() {
        rows.push((lane, board.tasks.iter().filter(|t| t.lane == lane).count()));
    }
    rows
}

pub fn maybe_checkpoint_to_contextlattice(
    board: &KanbanBoard,
    payload: KanbanActionInput,
) -> KanbanCheckpointResult {
    if !kanban_contextlattice_sync_enabled() {
        return KanbanCheckpointResult {
            attempted: false,
            succeeded: false,
            detail: "ContextLattice sync disabled via HERMES_KANBAN_CONTEXTLATTICE_SYNC."
                .to_string(),
        };
    }
    let Some(script_path) = contextlattice_script_path() else {
        return KanbanCheckpointResult {
            attempted: false,
            succeeded: false,
            detail: "ContextLattice orchestration script not found; skipped checkpoint."
                .to_string(),
        };
    };

    let mut content = format!(
        "kanban_action={} board_id={} board_name={} summary={}",
        payload.action,
        board.id,
        board.name,
        payload.summary.replace('\n', " ")
    );
    if let Some(task_id) = payload.task_id {
        content.push_str(&format!(" task_id={task_id}"));
    }
    if let Some(lane) = payload.lane {
        content.push_str(&format!(" lane={}", lane.as_str()));
    }

    let topic_path = format!("runbooks/kanban/{}", board.id);
    let output = Command::new("python3")
        .arg(&script_path)
        .arg("write")
        .arg("hermes-agent-ultra")
        .arg(&topic_path)
        .arg(content)
        .env(
            "MEMMCP_ORCHESTRATOR_URL",
            std::env::var("MEMMCP_ORCHESTRATOR_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8075".to_string()),
        )
        .env(
            "CONTEXTLATTICE_ORCHESTRATOR_URL",
            std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL")
                .unwrap_or_else(|_| "http://127.0.0.1:8075".to_string()),
        )
        .env(
            "CONTEXTLATTICE_AGENT_ID",
            std::env::var("CONTEXTLATTICE_AGENT_ID").unwrap_or_else(|_| "codex_gpt5".to_string()),
        )
        .env(
            "MEMMCP_AGENT_ID",
            std::env::var("MEMMCP_AGENT_ID").unwrap_or_else(|_| "codex_gpt5".to_string()),
        )
        .output();

    match output {
        Ok(out) if out.status.success() => KanbanCheckpointResult {
            attempted: true,
            succeeded: true,
            detail: format!("ContextLattice checkpointed to topic `{topic_path}`."),
        },
        Ok(out) => KanbanCheckpointResult {
            attempted: true,
            succeeded: false,
            detail: format!(
                "ContextLattice checkpoint failed (exit={}): {}",
                out.status.code().unwrap_or(-1),
                String::from_utf8_lossy(&out.stderr).trim()
            ),
        },
        Err(err) => KanbanCheckpointResult {
            attempted: true,
            succeeded: false,
            detail: format!("ContextLattice checkpoint error: {err}"),
        },
    }
}

fn normalize_store(store: &mut KanbanStore) {
    if store.schema_version == 0 {
        store.schema_version = KANBAN_SCHEMA_VERSION;
    }
    if store.boards.is_empty() {
        *store = KanbanStore::default();
        return;
    }
    if !store
        .boards
        .iter()
        .any(|board| board.id == store.current_board_id)
    {
        store.current_board_id = store.boards[0].id.clone();
    }
}

fn unique_board_id(store: &KanbanStore, base: &str) -> String {
    let clean = if base.is_empty() {
        DEFAULT_BOARD_ID.to_string()
    } else {
        base.to_string()
    };
    if !store.boards.iter().any(|board| board.id == clean) {
        return clean;
    }
    for idx in 2..10_000 {
        let candidate = format!("{clean}-{idx}");
        if !store.boards.iter().any(|board| board.id == candidate) {
            return candidate;
        }
    }
    format!("{}-{}", clean, uuid::Uuid::new_v4().simple())
}

fn slugify_board_id(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for ch in raw.trim().chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

fn dedupe_strings(values: Vec<String>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in values {
        let normalized = v.trim().to_string();
        if normalized.is_empty() {
            continue;
        }
        if out
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(&normalized))
        {
            continue;
        }
        out.push(normalized);
    }
    out
}

fn normalize_optional(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

fn kanban_contextlattice_sync_enabled() -> bool {
    std::env::var("HERMES_KANBAN_CONTEXTLATTICE_SYNC")
        .ok()
        .map(|v| v.trim().to_ascii_lowercase())
        .map(|v| matches!(v.as_str(), "1" | "true" | "yes" | "on"))
        .unwrap_or(true)
}

fn contextlattice_script_path() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(path) = std::env::var("HERMES_KANBAN_CONTEXTLATTICE_SCRIPT") {
        candidates.push(PathBuf::from(path));
    }
    candidates.push(PathBuf::from(CONTEXTLATTICE_DEFAULT_SCRIPT));
    if let Ok(cwd) = std::env::current_dir() {
        candidates.push(cwd.join("scripts/agent_orchestration.py"));
    }
    for candidate in candidates {
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env_lock;

    fn env_test_lock() -> std::sync::MutexGuard<'static, ()> {
        test_env_lock::lock()
    }

    fn with_temp_home<T>(f: impl FnOnce() -> T) -> T {
        let _guard = env_test_lock();
        let tmp = tempfile::tempdir().expect("tempdir");
        let prev = std::env::var_os("HERMES_HOME");
        crate::env_vars::set_var("HERMES_HOME", tmp.path());
        let result = f();
        match prev {
            Some(value) => crate::env_vars::set_var("HERMES_HOME", value),
            None => crate::env_vars::remove_var("HERMES_HOME"),
        }
        result
    }

    #[test]
    fn load_store_bootstraps_default_board() {
        with_temp_home(|| {
            let store = load_store().expect("load");
            assert_eq!(store.current_board_id, "main");
            assert_eq!(store.boards.len(), 1);
            assert_eq!(store.boards[0].name, "Main");
        });
    }

    #[test]
    fn add_move_and_archive_flow_works() {
        with_temp_home(|| {
            let mut store = load_store().expect("load");
            let board = ensure_board(&mut store, None);
            let task = add_task(
                board,
                NewKanbanTaskInput {
                    title: "Ship kanban".to_string(),
                    lane: KanbanLane::Todo,
                    priority: 2,
                    assignee: None,
                    description: None,
                    depends_on: vec![],
                },
            );
            assert_eq!(task.id, "K-0001");
            let task = find_task_mut(board, "K-0001").expect("task");
            claim_task(task, Some("worker-a".to_string()));
            assert_eq!(task.lane, KanbanLane::Doing);
            move_task(task, KanbanLane::Done, Some("verified".to_string()));
            assert_eq!(task.lane, KanbanLane::Done);
            assert!(task.completed_at.is_some());
            let moved = archive_done(board);
            assert_eq!(moved, 1);
            assert_eq!(board.tasks.len(), 0);
            assert_eq!(board.archived.len(), 1);
            save_store(&store).expect("save");
        });
    }

    #[test]
    fn contextlattice_checkpoint_disabled_path() {
        with_temp_home(|| {
            crate::env_vars::set_var("HERMES_KANBAN_CONTEXTLATTICE_SYNC", "0");
            let board = KanbanBoard {
                id: "main".to_string(),
                name: "Main".to_string(),
                project_path: None,
                created_at: now_rfc3339(),
                updated_at: now_rfc3339(),
                next_task_seq: 1,
                tasks: vec![],
                archived: vec![],
            };
            let res = maybe_checkpoint_to_contextlattice(
                &board,
                KanbanActionInput {
                    action: "add".to_string(),
                    task_id: Some("K-0001".to_string()),
                    lane: Some(KanbanLane::Todo),
                    summary: "test".to_string(),
                },
            );
            assert!(!res.attempted);
            crate::env_vars::remove_var("HERMES_KANBAN_CONTEXTLATTICE_SYNC");
        });
    }
}
