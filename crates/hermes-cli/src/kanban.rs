use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::Utc;
use hermes_core::AgentError;
use serde::{Deserialize, Serialize};
use serde_json::json;

const KANBAN_STATE_DIR: &str = "alpha/kanban";
const KANBAN_STORE_FILE: &str = "boards.json";
const KANBAN_SCHEMA_VERSION: u32 = 1;
const DEFAULT_BOARD_ID: &str = "main";
const CONTEXTLATTICE_DEFAULT_ORCHESTRATOR_URL: &str = "http://127.0.0.1:8075";
const CONTEXTLATTICE_KANBAN_FILE: &str = "notes/hermes-kanban.md";
const KANBAN_ATTACHMENTS_ENV: &str = "HERMES_KANBAN_ATTACHMENTS_ROOT";
const DEFAULT_GOAL_MAX_TURNS: u32 = 20;

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
    #[serde(default)]
    pub attachments: Vec<KanbanAttachment>,
    #[serde(default)]
    pub goal_mode: bool,
    #[serde(default)]
    pub goal_max_turns: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KanbanAttachment {
    pub id: String,
    pub filename: String,
    pub stored_path: String,
    #[serde(default)]
    pub content_type: Option<String>,
    pub size: u64,
    #[serde(default)]
    pub uploaded_by: Option<String>,
    pub created_at: String,
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
    pub goal_mode: bool,
    pub goal_max_turns: Option<u32>,
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
    let raw = std::fs::read_to_string(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::InvalidData {
            corrupt_store_error(&path, format!("read failed: {e}"))
        } else {
            AgentError::Io(format!("read {} failed: {}", path.display(), e))
        }
    })?;
    let mut store = serde_json::from_str::<KanbanStore>(&raw)
        .map_err(|e| corrupt_store_error(&path, format!("parse failed: {e}")))?;
    if store.boards.is_empty() {
        return Err(corrupt_store_error(
            &path,
            "store has no boards; refusing to auto-initialize over existing state",
        ));
    }
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
        attachments: Vec::new(),
        goal_mode: input.goal_mode,
        goal_max_turns: input.goal_max_turns,
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

pub fn attachments_root(board: &KanbanBoard) -> PathBuf {
    if let Ok(override_root) = std::env::var(KANBAN_ATTACHMENTS_ENV) {
        let trimmed = override_root.trim();
        if !trimmed.is_empty() {
            return expand_tilde_path(trimmed);
        }
    }
    hermes_config::hermes_home()
        .join(KANBAN_STATE_DIR)
        .join("attachments")
        .join(slugify_board_id(&board.id))
}

pub fn task_attachments_dir(board: &KanbanBoard, task_id: &str) -> PathBuf {
    attachments_root(board).join(sanitize_path_segment(task_id))
}

pub fn add_attachment_to_task(
    board: &mut KanbanBoard,
    task_ref: &str,
    source_path: impl AsRef<Path>,
    uploaded_by: Option<String>,
) -> Result<KanbanAttachment, AgentError> {
    let task_idx = board
        .tasks
        .iter()
        .position(|task| {
            task.id.eq_ignore_ascii_case(task_ref) || task.title.eq_ignore_ascii_case(task_ref)
        })
        .ok_or_else(|| AgentError::Config(format!("Task not found: {task_ref}")))?;
    let source = expand_tilde_path(source_path.as_ref().to_string_lossy().as_ref());
    let metadata = std::fs::metadata(&source).map_err(|e| {
        AgentError::Io(format!(
            "read attachment {} failed: {}",
            source.display(),
            e
        ))
    })?;
    if !metadata.is_file() {
        return Err(AgentError::Config(format!(
            "Attachment source is not a file: {}",
            source.display()
        )));
    }
    let raw_name = source
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| AgentError::Config("attachment filename is required".to_string()))?;
    let safe_name = sanitize_filename(raw_name);
    if safe_name.is_empty() {
        return Err(AgentError::Config(
            "attachment filename is required".to_string(),
        ));
    }

    let task_id = board.tasks[task_idx].id.clone();
    let dest_dir = task_attachments_dir(board, &task_id);
    std::fs::create_dir_all(&dest_dir)
        .map_err(|e| AgentError::Io(format!("create {} failed: {}", dest_dir.display(), e)))?;
    let dest = unique_attachment_path(&dest_dir, &safe_name);
    std::fs::copy(&source, &dest).map_err(|e| {
        AgentError::Io(format!(
            "copy attachment {} -> {} failed: {}",
            source.display(),
            dest.display(),
            e
        ))
    })?;
    let stored_path = dest.canonicalize().unwrap_or(dest);
    let stored_name = stored_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(&safe_name)
        .to_string();
    let attachment = KanbanAttachment {
        id: next_attachment_id(&board.tasks[task_idx]),
        filename: stored_name,
        stored_path: stored_path.display().to_string(),
        content_type: guess_content_type(&source),
        size: metadata.len(),
        uploaded_by: normalize_optional(uploaded_by),
        created_at: now_rfc3339(),
    };
    let now = now_rfc3339();
    board.tasks[task_idx].attachments.push(attachment.clone());
    board.tasks[task_idx].updated_at = now.clone();
    board.updated_at = now;
    Ok(attachment)
}

pub fn remove_attachment_from_task(
    board: &mut KanbanBoard,
    task_ref: &str,
    attachment_ref: &str,
) -> Result<Option<KanbanAttachment>, AgentError> {
    let Some(task_idx) = board.tasks.iter().position(|task| {
        task.id.eq_ignore_ascii_case(task_ref) || task.title.eq_ignore_ascii_case(task_ref)
    }) else {
        return Ok(None);
    };
    let Some(att_idx) = board.tasks[task_idx]
        .attachments
        .iter()
        .position(|attachment| {
            attachment.id.eq_ignore_ascii_case(attachment_ref)
                || attachment.filename.eq_ignore_ascii_case(attachment_ref)
        })
    else {
        return Ok(None);
    };
    let attachment = board.tasks[task_idx].attachments.remove(att_idx);
    let stored = PathBuf::from(&attachment.stored_path);
    if stored.is_file() {
        let _ = std::fs::remove_file(&stored);
    }
    let now = now_rfc3339();
    board.tasks[task_idx].updated_at = now.clone();
    board.updated_at = now;
    Ok(Some(attachment))
}

pub fn build_worker_context(task: &KanbanTask) -> String {
    let mut lines = vec![format!("Execute Kanban task {}: {}", task.id, task.title)];
    if let Some(desc) = task
        .description
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        lines.push(format!("Details: {desc}"));
    }
    if !task.depends_on.is_empty() {
        lines.push(format!("Dependencies: {}", task.depends_on.join(", ")));
    }
    if !task.attachments.is_empty() {
        lines.push("Attachments:".to_string());
        lines.push(
            "Read attached files with file or terminal tools at these absolute paths:".to_string(),
        );
        for attachment in &task.attachments {
            let size_kb = attachment.size.div_ceil(1024);
            let content_type = attachment
                .content_type
                .as_deref()
                .filter(|s| !s.is_empty())
                .map(|s| format!(", {s}"))
                .unwrap_or_default();
            let size = if size_kb == 0 {
                String::new()
            } else {
                format!(", {size_kb} KB")
            };
            lines.push(format!(
                "- {}{}{} -> {}",
                attachment.filename, content_type, size, attachment.stored_path
            ));
        }
    }
    if task.goal_mode {
        let max_turns = task.goal_max_turns.unwrap_or(DEFAULT_GOAL_MAX_TURNS);
        lines.push(format!(
            "Goal mode: enabled (max {max_turns} turns). Continue in this same work session until the task is genuinely done, then mark it done with a summary; if blocked, mark it blocked with the reason. Do not silently stop while work remains."
        ));
    }
    lines.join("\n")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KanbanGoalLoopOutcome {
    CompletedByWorker,
    BlockedByWorker,
    BlockedBudget,
    Stopped,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KanbanGoalLoopResult {
    pub outcome: KanbanGoalLoopOutcome,
    pub turns_used: u32,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KanbanGoalJudgment {
    Done(String),
    Continue(String),
}

pub type KanbanGoalStatusFn<'a> = Box<dyn FnMut() -> KanbanLane + 'a>;
pub type KanbanGoalJudgeFn<'a> = Box<dyn FnMut(&str, &str) -> KanbanGoalJudgment + 'a>;
pub type KanbanGoalRunTurnFn<'a> = Box<dyn FnMut(&str) -> String + 'a>;
pub type KanbanGoalBlockFn<'a> = Box<dyn FnMut(&str) + 'a>;

pub struct KanbanGoalLoopCallbacks<'a> {
    pub task_status: KanbanGoalStatusFn<'a>,
    pub judge: KanbanGoalJudgeFn<'a>,
    pub run_turn: KanbanGoalRunTurnFn<'a>,
    pub block_task: KanbanGoalBlockFn<'a>,
}

pub fn run_kanban_goal_loop(
    task_id: &str,
    goal_text: &str,
    first_response: &str,
    max_turns: u32,
    callbacks: KanbanGoalLoopCallbacks<'_>,
) -> KanbanGoalLoopResult {
    let KanbanGoalLoopCallbacks {
        mut task_status,
        mut judge,
        mut run_turn,
        mut block_task,
    } = callbacks;
    let max_turns = max_turns.max(1);
    let mut turns_used = 1u32;
    let mut last_response = first_response.to_string();
    let mut nudged_to_finalize = false;

    loop {
        match task_status() {
            KanbanLane::Done => {
                return KanbanGoalLoopResult {
                    outcome: KanbanGoalLoopOutcome::CompletedByWorker,
                    turns_used,
                    reason: "worker completed the task".to_string(),
                };
            }
            KanbanLane::Blocked => {
                return KanbanGoalLoopResult {
                    outcome: KanbanGoalLoopOutcome::BlockedByWorker,
                    turns_used,
                    reason: "worker blocked the task".to_string(),
                };
            }
            KanbanLane::Todo | KanbanLane::Doing => {}
        }

        let (prompt, reason) = match judge(goal_text, &last_response) {
            KanbanGoalJudgment::Done(reason) => {
                if nudged_to_finalize {
                    let reason = format!(
                        "Goal-mode worker output looked complete but task {task_id} never moved to done after finalize nudge: {reason}"
                    );
                    block_task(&reason);
                    return KanbanGoalLoopResult {
                        outcome: KanbanGoalLoopOutcome::BlockedBudget,
                        turns_used,
                        reason,
                    };
                }
                nudged_to_finalize = true;
                (
                    format!(
                        "[The kanban task appears complete, but it is still open]\nReason: {}\n\nMark task {} done with a concise summary, or block it with the remaining blocker.",
                        truncate_for_prompt(&reason, 400),
                        task_id
                    ),
                    reason,
                )
            }
            KanbanGoalJudgment::Continue(reason) => (
                format!(
                    "[Continuing kanban goal-mode task {}]\nReason: {}\n\nTake the next concrete step toward completing the task. When finished, mark it done; if blocked, mark it blocked. Do not stop without changing task state.",
                    task_id,
                    truncate_for_prompt(&reason, 400)
                ),
                reason,
            ),
        };

        if turns_used >= max_turns {
            let reason = format!(
                "Goal-mode worker exhausted its turn budget ({turns_used}/{max_turns}) without completing task {task_id}. Last judge reason: {}",
                truncate_for_prompt(&reason, 300)
            );
            block_task(&reason);
            return KanbanGoalLoopResult {
                outcome: KanbanGoalLoopOutcome::BlockedBudget,
                turns_used,
                reason,
            };
        }

        last_response = run_turn(&prompt);
        turns_used = turns_used.saturating_add(1);

        if last_response.trim().is_empty() {
            return KanbanGoalLoopResult {
                outcome: KanbanGoalLoopOutcome::Stopped,
                turns_used,
                reason: "worker returned empty response".to_string(),
            };
        }
    }
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
    match write_contextlattice_checkpoint(&topic_path, CONTEXTLATTICE_KANBAN_FILE, &content) {
        Ok(()) => KanbanCheckpointResult {
            attempted: true,
            succeeded: true,
            detail: format!("ContextLattice checkpointed to topic `{topic_path}`."),
        },
        Err(err) => KanbanCheckpointResult {
            attempted: true,
            succeeded: false,
            detail: format!("ContextLattice checkpoint failed: {err}"),
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

fn corrupt_store_error(path: &Path, reason: impl AsRef<str>) -> AgentError {
    let backup = backup_corrupt_store(path);
    let backup_detail = backup
        .as_ref()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "<backup failed>".to_string());
    AgentError::Config(format!(
        "Refusing to open corrupt kanban store at {}: {}. Original preserved; backup at {}.",
        path.display(),
        reason.as_ref(),
        backup_detail
    ))
}

fn backup_corrupt_store(path: &Path) -> Option<PathBuf> {
    let resolved = path.canonicalize().ok()?;
    let parent = resolved.parent()?.to_path_buf();
    let base_name = resolved.file_name()?.to_string_lossy();
    let timestamp = Utc::now().format("%Y%m%dT%H%M%SZ");
    let base_backup_name = format!("{base_name}.corrupt.{timestamp}");
    for idx in 0..100 {
        let backup_name = if idx == 0 {
            base_backup_name.clone()
        } else {
            format!("{base_backup_name}.{idx}")
        };
        let candidate = parent.join(backup_name);
        if candidate.parent() != Some(parent.as_path()) || candidate.exists() {
            continue;
        }
        if std::fs::copy(&resolved, &candidate).is_ok() {
            return Some(candidate);
        }
    }
    None
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

fn expand_tilde_path(raw: &str) -> PathBuf {
    if raw == "~" {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(raw));
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    PathBuf::from(raw)
}

fn sanitize_path_segment(raw: &str) -> String {
    let sanitized = raw
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' || ch == '.' {
                ch
            } else {
                '_'
            }
        })
        .collect::<String>()
        .trim_matches('.')
        .trim_matches('_')
        .to_string();
    if sanitized.is_empty() {
        "item".to_string()
    } else {
        sanitized
    }
}

fn sanitize_filename(raw: &str) -> String {
    sanitize_path_segment(
        Path::new(raw)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(raw),
    )
}

fn unique_attachment_path(dir: &Path, filename: &str) -> PathBuf {
    let base = Path::new(filename);
    let stem = base
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("attachment");
    let ext = base.extension().and_then(|s| s.to_str()).unwrap_or("");
    let first = dir.join(filename);
    if !first.exists() {
        return first;
    }
    for idx in 1..10_000 {
        let candidate_name = if ext.is_empty() {
            format!("{stem}-{idx}")
        } else {
            format!("{stem}-{idx}.{ext}")
        };
        let candidate = dir.join(candidate_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    dir.join(format!("{}-{}", stem, uuid::Uuid::new_v4().simple()))
}

fn next_attachment_id(task: &KanbanTask) -> String {
    let mut max_seen = 0u64;
    for attachment in &task.attachments {
        if let Some(raw) = attachment.id.strip_prefix("A-") {
            if let Ok(value) = raw.parse::<u64>() {
                max_seen = max_seen.max(value);
            }
        }
    }
    format!("A-{:04}", max_seen.saturating_add(1))
}

fn guess_content_type(path: &Path) -> Option<String> {
    let ext = path.extension()?.to_str()?.to_ascii_lowercase();
    let content_type = match ext.as_str() {
        "txt" | "md" | "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "json" | "yaml" | "yml"
        | "toml" => "text/plain",
        "pdf" => "application/pdf",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "csv" => "text/csv",
        "html" | "htm" => "text/html",
        _ => return None,
    };
    Some(content_type.to_string())
}

fn truncate_for_prompt(raw: &str, max_chars: usize) -> String {
    let mut out = raw.chars().take(max_chars).collect::<String>();
    if raw.chars().count() > max_chars {
        out.push_str("...");
    }
    out
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

fn contextlattice_orchestrator_url() -> String {
    std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL")
        .or_else(|_| std::env::var("MEMMCP_ORCHESTRATOR_URL"))
        .unwrap_or_else(|_| CONTEXTLATTICE_DEFAULT_ORCHESTRATOR_URL.to_string())
        .trim_end_matches('/')
        .to_string()
}

fn contextlattice_timeout() -> Duration {
    let seconds = std::env::var("HERMES_KANBAN_CONTEXTLATTICE_TIMEOUT_SECS")
        .or_else(|_| std::env::var("CONTEXTLATTICE_TIMEOUT_SECS"))
        .ok()
        .and_then(|raw| raw.parse::<f64>().ok())
        .unwrap_or(10.0)
        .clamp(1.0, 60.0);
    Duration::from_secs_f64(seconds)
}

fn write_contextlattice_checkpoint(
    topic_path: &str,
    file_name: &str,
    content: &str,
) -> Result<(), String> {
    let url = format!("{}/memory/write", contextlattice_orchestrator_url());
    let payload = json!({
        "projectName": "hermes-agent-ultra",
        "fileName": file_name,
        "topicPath": topic_path,
        "content": content,
    });
    let client = reqwest::blocking::Client::builder()
        .timeout(contextlattice_timeout())
        .build()
        .map_err(|err| err.to_string())?;
    let mut request = client.post(&url).json(&payload);
    if let Ok(api_key) = std::env::var("CONTEXTLATTICE_API_KEY") {
        if !api_key.trim().is_empty() {
            request = request.bearer_auth(api_key);
        }
    }
    let response = request.send().map_err(|err| err.to_string())?;
    let status = response.status();
    if status.is_success() {
        return Ok(());
    }
    let body = response.text().unwrap_or_default();
    let preview: String = body.chars().take(240).collect();
    Err(format!("HTTP {status}: {}", preview.trim()))
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
        std::env::set_var("HERMES_HOME", tmp.path());
        let result = f();
        match prev {
            Some(value) => std::env::set_var("HERMES_HOME", value),
            None => std::env::remove_var("HERMES_HOME"),
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
    fn load_store_refuses_malformed_existing_store_and_preserves_backup() {
        with_temp_home(|| {
            let path = kanban_store_path();
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            let original = b"{ not valid json";
            std::fs::write(&path, original).expect("write corrupt store");

            let err = load_store().expect_err("corrupt store should fail");
            let msg = err.to_string();
            assert!(msg.contains("Refusing to open corrupt kanban store"));
            assert!(msg.contains("parse failed"));
            assert_eq!(std::fs::read(&path).expect("read original"), original);

            let backups = corrupt_store_backups(path.parent().expect("parent"));
            assert_eq!(backups.len(), 1, "unexpected backups: {backups:?}");
            assert_eq!(std::fs::read(&backups[0]).expect("read backup"), original);
        });
    }

    #[test]
    fn load_store_refuses_semantically_empty_existing_store() {
        with_temp_home(|| {
            let path = kanban_store_path();
            std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
            let original = br#"{"schema_version":1,"current_board_id":"main","boards":[]}"#;
            std::fs::write(&path, original).expect("write empty store");

            let err = load_store().expect_err("empty existing store should fail closed");
            let msg = err.to_string();
            assert!(msg.contains("Refusing to open corrupt kanban store"));
            assert!(msg.contains("store has no boards"));
            assert_eq!(std::fs::read(&path).expect("read original"), original);

            let backups = corrupt_store_backups(path.parent().expect("parent"));
            assert_eq!(backups.len(), 1, "unexpected backups: {backups:?}");
            assert_eq!(std::fs::read(&backups[0]).expect("read backup"), original);
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
                    goal_mode: false,
                    goal_max_turns: None,
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
            std::env::set_var("HERMES_KANBAN_CONTEXTLATTICE_SYNC", "0");
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
            std::env::remove_var("HERMES_KANBAN_CONTEXTLATTICE_SYNC");
        });
    }

    #[test]
    fn attachments_copy_with_safe_unique_names_and_surface_in_worker_context() {
        with_temp_home(|| {
            let mut store = load_store().expect("load");
            let board = ensure_board(&mut store, None);
            let task = add_task(
                board,
                NewKanbanTaskInput {
                    title: "Read source".to_string(),
                    lane: KanbanLane::Todo,
                    priority: 3,
                    assignee: Some("worker".to_string()),
                    description: Some("summarize attached file".to_string()),
                    depends_on: vec![],
                    goal_mode: false,
                    goal_max_turns: None,
                },
            );
            let source_dir = tempfile::tempdir().expect("source tempdir");
            let source = source_dir.path().join("notes.txt");
            std::fs::write(&source, "hello attachment").expect("write source");

            let first =
                add_attachment_to_task(board, &task.id, &source, Some("tester".to_string()))
                    .expect("attach first");
            let second =
                add_attachment_to_task(board, &task.id, &source, None).expect("attach second");

            assert_eq!(first.id, "A-0001");
            assert_eq!(second.id, "A-0002");
            assert_eq!(first.filename, "notes.txt");
            assert_eq!(second.filename, "notes-1.txt");
            assert!(Path::new(&first.stored_path).is_file());
            assert!(Path::new(&second.stored_path).is_file());

            let task = find_task_mut(board, &task.id).expect("task");
            let context = build_worker_context(task);
            assert!(context.contains("Attachments:"));
            assert!(context.contains(&first.stored_path));
            assert!(context.contains("notes-1.txt"));
        });
    }

    #[test]
    fn remove_attachment_deletes_row_and_blob() {
        with_temp_home(|| {
            let mut store = load_store().expect("load");
            let board = ensure_board(&mut store, None);
            let task = add_task(
                board,
                NewKanbanTaskInput {
                    title: "Task".to_string(),
                    lane: KanbanLane::Todo,
                    priority: 3,
                    assignee: None,
                    description: None,
                    depends_on: vec![],
                    goal_mode: false,
                    goal_max_turns: None,
                },
            );
            let source_dir = tempfile::tempdir().expect("source tempdir");
            let source = source_dir.path().join("data.pdf");
            std::fs::write(&source, b"%PDF-1.4").expect("write source");
            let attachment =
                add_attachment_to_task(board, &task.id, &source, None).expect("attach");
            let stored = PathBuf::from(&attachment.stored_path);

            let removed =
                remove_attachment_from_task(board, &task.id, &attachment.id).expect("remove");
            assert_eq!(removed.expect("removed").id, attachment.id);
            assert!(!stored.exists());
            assert!(find_task_mut(board, &task.id)
                .expect("task")
                .attachments
                .is_empty());
        });
    }

    #[test]
    fn goal_mode_context_and_loop_cover_continue_complete_and_budget() {
        let task = KanbanTask {
            id: "K-0007".to_string(),
            title: "Ship feature".to_string(),
            lane: KanbanLane::Doing,
            priority: 1,
            assignee: Some("worker".to_string()),
            description: Some("must pass tests".to_string()),
            depends_on: vec!["K-0001".to_string()],
            blocked_reason: None,
            background_job_id: None,
            run_summary: None,
            created_at: now_rfc3339(),
            updated_at: now_rfc3339(),
            started_at: Some(now_rfc3339()),
            completed_at: None,
            attachments: Vec::new(),
            goal_mode: true,
            goal_max_turns: Some(3),
        };
        let context = build_worker_context(&task);
        assert!(context.contains("Goal mode: enabled (max 3 turns)"));
        assert!(context.contains("Dependencies: K-0001"));

        let mut statuses = [KanbanLane::Doing, KanbanLane::Done].into_iter();
        let mut prompts = Vec::new();
        let result = run_kanban_goal_loop(
            &task.id,
            "Ship feature",
            "started",
            5,
            KanbanGoalLoopCallbacks {
                task_status: Box::new(|| statuses.next().unwrap_or(KanbanLane::Done)),
                judge: Box::new(|_, _| KanbanGoalJudgment::Continue("needs more work".to_string())),
                run_turn: Box::new(|prompt: &str| {
                    prompts.push(prompt.to_string());
                    "continued".to_string()
                }),
                block_task: Box::new(|_| panic!("should not block")),
            },
        );
        assert_eq!(result.outcome, KanbanGoalLoopOutcome::CompletedByWorker);
        assert_eq!(prompts.len(), 1);
        assert!(prompts[0].contains("Continuing kanban goal-mode task"));

        let mut blocked_reason = String::new();
        let budget = run_kanban_goal_loop(
            &task.id,
            "Ship feature",
            "started",
            2,
            KanbanGoalLoopCallbacks {
                task_status: Box::new(|| KanbanLane::Doing),
                judge: Box::new(|_, _| KanbanGoalJudgment::Continue("not done".to_string())),
                run_turn: Box::new(|_| "still going".to_string()),
                block_task: Box::new(|reason: &str| blocked_reason = reason.to_string()),
            },
        );
        assert_eq!(budget.outcome, KanbanGoalLoopOutcome::BlockedBudget);
        assert!(blocked_reason.contains("turn budget"));
    }

    fn corrupt_store_backups(parent: &Path) -> Vec<PathBuf> {
        let mut backups = std::fs::read_dir(parent)
            .expect("read parent")
            .map(|entry| entry.expect("dir entry").path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .map(|name| name.starts_with("boards.json.corrupt."))
                    .unwrap_or(false)
            })
            .collect::<Vec<_>>();
        backups.sort();
        backups
    }
}
