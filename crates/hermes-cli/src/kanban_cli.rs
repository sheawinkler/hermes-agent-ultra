//! CLI-facing Kanban commands (`hermes kanban ...`).

use std::io::Write;

use hermes_core::AgentError;

use crate::kanban::{
    add_task, archive_done, claim_task, create_or_select_board, ensure_board, find_task_mut,
    load_store, maybe_checkpoint_to_contextlattice, move_task, save_store, set_blocked,
    KanbanActionInput, KanbanBoard, KanbanLane, NewKanbanTaskInput,
};

