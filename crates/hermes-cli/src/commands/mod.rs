//! Slash command handler (Requirement 9.2).
//!
//! Defines and dispatches all supported `/` commands in the interactive
//! REPL, and provides auto-completion suggestions.

use std::path::{Path, PathBuf};

use hermes_core::AgentError;

mod autocomplete;
mod catalog;
mod cli_handlers;
mod slash_dispatch;
mod slash_registry;

pub(crate) mod approval;
pub(crate) mod auth_cmd;
pub(crate) mod background;
pub(crate) mod browser;
pub(crate) mod claims;
pub(crate) mod compress;
pub(crate) mod diagnostics;
pub(crate) mod infra;
pub(crate) mod integrations;
pub(crate) mod kanban;
pub(crate) mod misc;
pub(crate) mod model;
pub(crate) mod objective;
pub(crate) mod ops;
pub(crate) mod plan;
pub(crate) mod policy;
pub(crate) mod quorum;
pub(crate) mod runtime_ui;
pub(crate) mod session;
pub mod skills;
pub(crate) mod skills_infra;
pub(crate) mod studio_ops;
pub(crate) mod swarm;

pub use autocomplete::{SLASH_COMMANDS, autocomplete, autocomplete_contextual, help_for};
pub use slash_dispatch::handle_slash_command;

pub(crate) use catalog::{detect_repo_root_from_cwd, provider_health_snapshot};

pub use background::recover_queued_background_jobs;
pub use kanban::run_kanban_command;

pub(crate) use misc::{
    discover_repo_root_for_about, handle_personality_command, handle_raw_command,
    handle_reasoning_command, handle_trigger_triage_command, handle_verbose_command,
    handle_yolo_command, read_json_file, replay_enabled_runtime,
};

/// Result of handling a slash command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandResult {
    /// The command was fully handled (no further action needed).
    Handled,
    /// The command requires the agent to process a follow-up message.
    NeedsAgent,
    /// Run the agent with a skill slash invocation message.
    RunAgent(String),
    /// The user requested to quit the application.
    Quit,
}

pub(crate) fn secret_stdout_allowed() -> bool {
    std::env::var("HERMES_ALLOW_SECRET_STDOUT")
        .ok()
        .is_some_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

pub(crate) fn mask_secret_value(secret: &str) -> String {
    if secret.is_empty() {
        return "(empty)".to_string();
    }
    if secret.len() <= 8 {
        return "*".repeat(secret.len());
    }
    format!(
        "{}***{}",
        &secret[..4],
        &secret[secret.len().saturating_sub(4)..]
    )
}

pub(crate) fn emit_command_output(
    host: &mut impl crate::app::TranscriptRuntime,
    text: impl Into<String>,
) {
    let rendered = text.into();
    if host.stream_attached() {
        host.push_ui_assistant(rendered);
    } else {
        println!("{}", rendered);
    }
}

pub(crate) fn format_personality_catalog(
    current_personality: Option<&str>,
    builtin_descriptions: &[(&str, &str)],
) -> String {
    let mut out = String::from("## Built-in personalities\n\n");
    if let Some(current) = current_personality.filter(|v| !v.trim().is_empty()) {
        out.push_str(&format!("Current: `{}`\n\n", current));
    } else {
        out.push_str("Current: `(none)`\n\n");
    }
    out.push_str("Use `/personality <name>` to switch.\n\n");
    for (name, usage) in builtin_descriptions {
        out.push_str(&format!("- `{}`\n  {}\n\n", name, usage));
    }
    out.trim_end().to_string()
}

pub(crate) fn yes_no(flag: bool) -> &'static str {
    if flag { "yes" } else { "no" }
}

pub(crate) fn replay_log_path_for_session(session_id: &str) -> PathBuf {
    let sid = if session_id.trim().is_empty() {
        "session".to_string()
    } else {
        session_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    hermes_config::hermes_home()
        .join("logs")
        .join("replay")
        .join(format!("{}.jsonl", sid))
}

pub(crate) fn replay_trace_integrity(path: &Path) -> Result<(usize, usize, usize), AgentError> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        AgentError::Io(format!(
            "Failed to read replay log {}: {}",
            path.display(),
            e
        ))
    })?;
    let mut entries = 0usize;
    let mut parse_errors = 0usize;
    let mut chain_breaks = 0usize;
    let mut prev_hash = String::from("seed");
    for line in raw.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        entries += 1;
        match serde_json::from_str::<serde_json::Value>(trimmed) {
            Ok(val) => {
                let curr = val.get("event_hash").and_then(|v| v.as_str()).unwrap_or("");
                let expected_prev = val.get("prev_hash").and_then(|v| v.as_str()).unwrap_or("");
                if curr.is_empty() || expected_prev.is_empty() {
                    parse_errors += 1;
                } else if expected_prev != prev_hash {
                    chain_breaks += 1;
                }
                if !curr.is_empty() {
                    prev_hash = curr.to_string();
                }
            }
            Err(_) => {
                parse_errors += 1;
            }
        }
    }
    Ok((entries, parse_errors, chain_breaks))
}

pub(crate) fn truncate_chars(input: &str, max_len: usize) -> String {
    if max_len == 0 {
        return String::new();
    }
    if input.chars().count() <= max_len {
        return input.to_string();
    }
    let mut out: String = input.chars().take(max_len.saturating_sub(1)).collect();
    out.push('\u{2026}');
    out
}

#[cfg(any(feature = "talk", feature = "talk-rockchip"))]
pub use cli_handlers::handle_cli_talk;
pub(crate) use cli_handlers::{
    discover_plugin_surface, render_plugin_surface_table, whatsapp_cloud_setup_impl,
};
pub use cli_handlers::{
    handle_cli_acp, handle_cli_backup, handle_cli_chat, handle_cli_claw, handle_cli_contribute,
    handle_cli_external_plugin_subcommand, handle_cli_import, handle_cli_insights,
    handle_cli_interest, handle_cli_login, handle_cli_logout, handle_cli_mcp, handle_cli_media,
    handle_cli_meeting, handle_cli_memory, handle_cli_pairing, handle_cli_plugins,
    handle_cli_server, handle_cli_sessions, handle_cli_version, handle_cli_whatsapp,
};

#[cfg(test)]
pub(crate) use approval::{handle_approve_command, handle_deny_command, handle_gquota_command};
#[cfg(test)]
pub(crate) use autocomplete::canonical_command;
#[cfg(test)]
pub(crate) use background::handle_queue_command;
#[cfg(test)]
pub(crate) use catalog::feedback_log_path;
#[cfg(test)]
pub(crate) use catalog::handle_feedback_command;
#[cfg(test)]
pub(crate) use cli_handlers::query_mode_tools_enabled;
#[cfg(test)]
pub(crate) use cli_handlers::{
    ACP_MULTIMODAL_PREFIX, acp_history_to_messages, apply_cli_chat_runtime_env,
    remove_sentrux_mcp_profile, resolve_cli_chat_provider_model, upsert_sentrux_mcp_profile,
};
#[cfg(test)]
pub(crate) use diagnostics::{handle_debug_dump_command, handle_image_command};
#[cfg(test)]
pub(crate) use infra::extract_embedding_diag_line;
#[cfg(test)]
pub(crate) use kanban::parse_kanban_add;
#[cfg(test)]
pub(crate) use misc::{
    SubconsciousQueueState, SubconsciousTask, TriggerTriageAssessment, TriggerTriageDecision,
    append_triage_learning_feedback, evaluate_trigger_triage, parse_reasoning_effort,
    save_subconscious_state, triage_learning_bias, trigger_triage_learning_state_path,
};
#[cfg(test)]
pub(crate) use plan::handle_plan_command;
#[cfg(test)]
pub(crate) use policy::walkthrough_events_path;
#[cfg(test)]
pub(crate) use session::{handle_rollback_command, handle_snapshot_command};
#[cfg(test)]
pub(crate) use studio_ops::{
    extract_marker_paths, normalize_repo_relative_path, specpatch_block_reason,
};

#[cfg(test)]
mod tests;
