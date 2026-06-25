//! # hermes-cli
//!
//! CLI/TUI interface for Hermes Agent.
//!
//! This crate implements the terminal user interface (Requirement 9):
//! - Interactive REPL with streaming output (9.1, 9.5)
//! - Slash command support with auto-completion (9.2)
//! - Ctrl+C interrupt for tool execution (9.3)
//! - Message history display (9.4)
//! - CLI argument parsing via clap (9.7)
//! - Theme/skin engine (9.8)

pub mod acp_command;
pub mod alpha_runtime;
pub mod app;
pub mod auth;
pub mod auth_main;
pub mod banner;
pub mod checklist;
pub mod claw_migrate;
pub mod cli;
pub mod commands;
pub mod config_env;
pub mod copilot_auth;
pub mod cron_delivery;
pub mod dep_ensure;
pub mod env_loader;
pub mod env_vars;
pub mod gateway_cmd;
pub mod gateway_handlers;
pub mod gateway_inbound_wiring;
pub mod gateway_main;
pub mod gateway_plan_mode;
pub mod gateway_process;
pub mod gateway_runtime;
pub mod gateway_runtime_defaults;
pub mod kanban;
pub mod live_messaging;
pub mod lumio;
pub mod mcp_config;
pub mod media_wiring;
pub mod moa_wiring;
pub mod model_switch;
pub mod oneshot;
pub mod pairing_store;
pub mod paths;
pub mod plan_mode;
pub mod platform_toolsets;
pub mod profiles;
pub mod prompt;
pub mod providers;
pub mod runtime_dep_install;
pub mod runtime_tool_wiring;
pub mod skills_config;
pub mod skills_runtime;
pub mod skin_engine;
pub mod startup_metrics;
pub mod state_paths;
pub mod systems;
#[cfg(any(feature = "talk", feature = "talk-rockchip"))]
pub mod talk_embedded;
pub mod teams_pipeline_cli;
pub mod terminal_backend;
pub mod theme;
pub mod tool_preview;
pub mod tools_config;
pub mod tui;
pub mod update;
pub mod webhook_delivery;
pub mod whatsapp_wizard;

#[cfg(test)]
pub(crate) mod test_env_lock;

// Re-export primary types
pub use app::App;
pub use checklist::{
    ChecklistResult, SelectResult, curses_checklist, curses_select, curses_select_embedded,
    key_event_is_actionable, prefer_plain_checklist, prompt_choice,
};
pub use claw_migrate::{MigrateOptions, MigrationResult, run_migration};
pub use cli::{Cli, CliCommand, completion_command};
pub use commands::CommandResult;
pub use theme::Theme;
pub use tui::{Event, InputMode, ToolOutputSection, Tui};
