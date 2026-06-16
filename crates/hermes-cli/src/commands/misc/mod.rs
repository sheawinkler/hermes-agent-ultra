//! Miscellaneous slash command handlers (extracted from `mod.rs`).
//!
//! Small/medium `/` commands that don't warrant their own module.

mod about;
mod config_cmd;
mod curator;
mod personality;
mod plan_mode;
mod provider_cmd;
mod raw;
mod reasoning;
mod runbook;
mod runtime_flags;
mod subconscious;
mod toolcards;
mod tools;
mod transcript;
mod triage;

pub(crate) use about::{discover_repo_root_for_about, handle_about_command, read_json_file};
pub(crate) use config_cmd::handle_config_command;
pub(crate) use curator::handle_curator_command;
pub(crate) use personality::handle_personality_command;
pub(crate) use plan_mode::handle_plan_mode_command;
pub(crate) use provider_cmd::handle_provider_command;
pub(crate) use raw::{handle_raw_command, replay_enabled_runtime};
pub(crate) use reasoning::handle_reasoning_command;
pub(crate) use runbook::handle_runbook_command;
pub(crate) use runtime_flags::{
    handle_status_command, handle_stop_command, handle_usage_command, handle_verbose_command,
    handle_yolo_command,
};
pub(crate) use subconscious::handle_subconscious_command;
pub(crate) use toolcards::handle_toolcards_command;
pub(crate) use tools::handle_tools_command;
pub(crate) use transcript::{handle_context_command, handle_history_command, handle_recap_command};
pub(crate) use triage::handle_trigger_triage_command;

#[cfg(test)]
pub(crate) use reasoning::parse_reasoning_effort;
#[cfg(test)]
pub(crate) use subconscious::{
    SubconsciousQueueState, SubconsciousTask, save_subconscious_state,
    subconscious_test_high_risk_state,
};
#[cfg(test)]
pub(crate) use triage::{
    TriggerTriageAssessment, TriggerTriageDecision, append_triage_learning_feedback,
    evaluate_trigger_triage, triage_learning_bias, trigger_triage_learning_state_path,
};
