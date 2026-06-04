//! Turn prep/finalize infrastructure hooks (Python `conversation_loop` E-segment).

use hermes_core::{Message, MessageRole};

use crate::agent_loop::AgentLoop;
use crate::codex_responses_adapter::summarize_user_message_for_log_str;
use crate::conversation_loop::TurnFinalizeMeta;

/// Remove private empty-response retry scaffolding and rewind orphan tool tails.
pub fn drop_trailing_empty_response_scaffolding(messages: &mut Vec<Message>) {
    let mut dropped_scaffolding = false;
    while let Some(last) = messages.last() {
        let is_scaffolding = last.role == MessageRole::Assistant
            && last
                .content
                .as_deref()
                .map(|c| c.contains("(empty)") || c.starts_with("[SYSTEM] Maximum conversation turns"))
                .unwrap_or(false);
        if !is_scaffolding {
            break;
        }
        messages.pop();
        dropped_scaffolding = true;
    }
    if !dropped_scaffolding {
        return;
    }
    while messages.last().is_some_and(|m| m.role == MessageRole::Tool) {
        messages.pop();
    }
    if messages.last().is_some_and(|m| {
        m.role == MessageRole::Assistant && m.tool_calls.as_ref().is_some_and(|t| !t.is_empty())
    }) {
        messages.pop();
    }
}

fn trajectories_enabled(agent: &AgentLoop) -> bool {
    if agent.config().save_trajectories {
        return true;
    }
    std::env::var("HERMES_SAVE_TRAJECTORIES")
        .ok()
        .map(|raw| {
            matches!(
                raw.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
        .unwrap_or(false)
}

impl AgentLoop {
    pub(crate) fn apply_turn_prep_infrastructure_hooks(&self) {
        let rt = self.primary_runtime_snapshot();
        let provider = rt.provider.as_deref().unwrap_or("");
        hermes_intelligence::runtime_main::set_runtime_main(provider, &self.active_model());
    }

    pub(crate) fn apply_turn_finalize_infrastructure_hooks(
        &self,
        meta: &TurnFinalizeMeta,
        messages: &mut Vec<Message>,
        loop_result: &hermes_core::AgentResult,
        completed: bool,
    ) {
        drop_trailing_empty_response_scaffolding(messages);
        let trajectory_user = summarize_user_message_for_log_str(&meta.inbound_user_message);
        self.maybe_save_turn_trajectory(&trajectory_user, messages, completed);
        self.cleanup_task_resources(&meta.task_id);
        if loop_result.turn_exit_reason.contains("max_iterations")
            && hermes_tools::kanban_task_from_env().is_some()
        {
            let max_iters = self.config().max_turns;
            let err = format!(
                "iteration budget exhausted after {} API calls (max {})",
                loop_result.api_calls, max_iters
            );
            hermes_tools::record_iteration_budget_exhausted(
                loop_result.api_calls,
                max_iters,
                &err,
            );
        }
    }

    fn maybe_save_turn_trajectory(
        &self,
        user_message: &str,
        messages: &[Message],
        completed: bool,
    ) {
        if !trajectories_enabled(self) {
            return;
        }
        let Some(sp) = self.session_persistence() else {
            return;
        };
        let sid = self.config().session_id.clone().unwrap_or_else(|| "anonymous".to_string());
        let sid = sid.trim();
        let sid = if sid.is_empty() { "anonymous" } else { sid };
        if let Err(err) = sp.save_trajectory(sid, messages, user_message, completed) {
            tracing::warn!(session_id = %sid, "save_trajectory: {}", err);
        }
    }

    fn cleanup_task_resources(&self, task_id: &str) {
        if task_id.trim().is_empty() {
            return;
        }
        hermes_tools::cleanup_task_resources(task_id);
    }
}
