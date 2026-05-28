//! Computer use toolset.
//!
//! Mirrors Python package layout:
//! - `backend`    abstract backend types
//! - `cua_backend` macOS cua-driver MCP backend
//! - `schema`     tool schema builder
//! - `tool`       dispatch + safety + fallback backend

pub mod backend;
pub mod cua_backend;
pub mod schema;
pub mod tool;

pub use tool::{ComputerUseHandler, check_computer_use_requirements};
