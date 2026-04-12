//! Tool implementations module
//!
//! Each submodule implements one or more tool handlers that conform
//! to the `ToolHandler` trait from `hermes-core`.

pub mod browser;
pub mod clarify;
pub mod code_execution;
pub mod cronjob;
pub mod delegation;
pub mod file;
pub mod homeassistant;
pub mod image_gen;
pub mod memory;
pub mod messaging;
pub mod session_search;
pub mod skills;
pub mod terminal;
pub mod todo;
pub mod tts;
pub mod vision;
pub mod web;