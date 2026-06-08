//! Tool implementations module
//!
//! Each submodule implements one or more tool handlers that conform
//! to the `ToolHandler` trait from `hermes-core`.

pub mod ansi_strip;
pub mod binary_extensions;
pub mod browser;
pub mod budget_config;
pub mod capture;
pub mod clarify;
pub mod code_execution;
pub mod computer_use;
pub mod content_framework;
pub mod credential_files;
pub mod cronjob;
pub mod dashboard_control;
pub mod debug_helpers;
pub mod delegation;
pub mod env_passthrough;
pub mod env_probe;
pub mod feishu;
pub mod file;
pub mod fuzzy_match;
pub mod homeassistant;
pub mod image_gen;
pub mod interrupt;
pub mod managed_tool_gateway;
pub mod meeting_notes;
pub mod memory;
pub mod messaging;
pub mod mixture_of_agents;
pub mod osv_check;
pub mod path_security;
pub mod patch_parser;
pub mod process_registry;
pub mod schema_sanitizer;
pub mod session_search;
pub mod skill_commands;
pub mod skill_utils;
pub mod skills;
pub mod terminal;
pub mod todo;
pub mod tool_output_limits;
pub mod tool_result_storage;
pub mod transcription;
pub mod tts;
pub mod tts_premium;
pub mod url_safety;
pub mod video;
pub mod vision;
pub mod voice_mode;
pub mod web;
