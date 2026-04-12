//! Real backend implementations for tool handlers.
//!
//! Each submodule provides a concrete implementation of a backend trait
//! defined in the `tools` module, connecting to real APIs, file systems,
//! databases, and external services.

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
pub mod todo;
pub mod tts;
pub mod vision;
pub mod web;
