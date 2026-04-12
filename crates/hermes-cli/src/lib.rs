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

pub mod app;
pub mod auth;
pub mod banner;
pub mod checklist;
pub mod claw_migrate;
pub mod cli;
pub mod commands;
pub mod copilot_auth;
pub mod doctor;
pub mod env_loader;
pub mod gateway_cmd;
pub mod mcp_config;
pub mod model_switch;
pub mod profiles;
pub mod providers;
pub mod setup;
pub mod skin_engine;
pub mod skills_config;
pub mod theme;
pub mod tools_config;
pub mod tui;
pub mod update;

// Re-export primary types
pub use app::App;
pub use checklist::{curses_checklist, ChecklistResult};
pub use claw_migrate::{MigrateOptions, MigrationResult, run_migration};
pub use cli::{Cli, CliCommand};
pub use commands::CommandResult;
pub use theme::Theme;
pub use tui::{Event, InputMode, ToolOutputSection, Tui};