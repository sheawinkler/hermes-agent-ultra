//! # hermes-environments
//!
//! Terminal backend systems and environment management for Hermes Agent.
//!
//! This crate provides multiple backends for executing commands and managing
//! files in different environments (local, Docker, SSH, Daytona, Modal,
//! Singularity). The [`BackendManager`] orchestrates which backend is active
//! and allows runtime switching.

pub mod file_sync;
pub mod local;
pub mod manager;

#[cfg(feature = "docker")]
pub mod docker;

#[cfg(feature = "ssh")]
pub mod ssh;

#[cfg(feature = "daytona")]
pub mod daytona;

#[cfg(feature = "modal")]
pub mod modal;

#[cfg(feature = "modal")]
pub mod managed_modal;

#[cfg(feature = "singularity")]
pub mod singularity;

// Re-export core trait and local types
pub use file_sync::FileSync;
pub use hermes_core::TerminalBackend;
pub use local::LocalBackend;
pub use manager::BackendManager;
