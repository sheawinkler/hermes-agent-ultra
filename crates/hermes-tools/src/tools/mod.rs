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
pub mod managed_tool_gateway;
pub mod memory;
pub mod mixture_of_agents;
pub mod messaging;
pub mod osv_check;
pub mod process_registry;
pub mod rl_training;
pub mod transcription;
pub mod tool_result_storage;
pub mod tts_premium;
pub mod url_safety;
pub mod env_passthrough;
pub mod credential_files;
pub mod session_search;
pub mod skill_commands;
pub mod skill_utils;
pub mod skills;
pub mod terminal;
pub mod todo;
pub mod tts;
pub mod voice_mode;
pub mod vision;
pub mod web;