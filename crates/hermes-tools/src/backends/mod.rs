//! Real backend implementations for tool handlers.
//!
//! Each submodule provides a concrete implementation of a backend trait
//! defined in the `tools` module, connecting to real APIs, file systems,
//! databases, and external services.

pub mod web;
pub mod file;
pub mod memory;
pub mod session_search;
pub mod vision;
pub mod image_gen;
pub mod todo;
pub mod clarify;
pub mod code_execution;
pub mod delegation;
pub mod cronjob;
pub mod messaging;
pub mod homeassistant;
pub mod tts;
pub mod browser;
