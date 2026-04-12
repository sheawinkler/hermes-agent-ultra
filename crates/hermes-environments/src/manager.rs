//! Backend manager – orchestrates the active terminal backend.

use std::sync::Arc;

use hermes_config::{TerminalBackendType, TerminalConfig};
use hermes_core::{AgentError, CommandOutput, TerminalBackend};

use crate::local::LocalBackend;

#[cfg(feature = "docker")]
use crate::docker::DockerBackend;

#[cfg(feature = "ssh")]
use crate::ssh::SshBackend;

#[cfg(feature = "daytona")]
use crate::daytona::DaytonaBackend;

#[cfg(feature = "modal")]
use crate::modal::ModalBackend;

#[cfg(feature = "singularity")]
use crate::singularity::SingularityBackend;

/// Manages the active terminal backend and provides a unified interface
/// for command execution and file operations regardless of backend type.
pub struct BackendManager {
    current_backend: Arc<dyn TerminalBackend>,
    config: TerminalConfig,
}

impl BackendManager {
    /// Create a new `BackendManager` with the given configuration.
    ///
    /// The initial backend is selected based on `config.backend`.
    /// If the selected backend type is not available (e.g. compiled without
    /// the corresponding feature), falls back to [`LocalBackend`].
    pub fn new(config: TerminalConfig) -> Self {
        let backend: Arc<dyn TerminalBackend> = Self::create_backend(&config);
        Self {
            current_backend: backend,
            config,
        }
    }

    /// Create the appropriate backend based on configuration.
    fn create_backend(config: &TerminalConfig) -> Arc<dyn TerminalBackend> {
        match config.backend {
            TerminalBackendType::Local => {
                Arc::new(LocalBackend::new(config.timeout, config.max_output_size))
            }
            #[cfg(feature = "docker")]
            TerminalBackendType::Docker => {
                Arc::new(DockerBackend::new(
                    None,
                    None,
                    config.timeout,
                    config.max_output_size,
                ))
            }
            #[cfg(feature = "ssh")]
            TerminalBackendType::Ssh => {
                Arc::new(SshBackend::new(
                    "localhost".to_string(),
                    22,
                    None,
                    None,
                    config.timeout,
                    config.max_output_size,
                ))
            }
            #[cfg(feature = "daytona")]
            TerminalBackendType::Daytona => {
                Arc::new(DaytonaBackend::new(
                    None,
                    None,
                    None,
                    config.timeout,
                    config.max_output_size,
                ))
            }
            #[cfg(feature = "modal")]
            TerminalBackendType::Modal => {
                Arc::new(ModalBackend::new(
                    None,
                    None,
                    config.timeout,
                    config.max_output_size,
                ))
            }
            #[cfg(feature = "singularity")]
            TerminalBackendType::Singularity => {
                Arc::new(SingularityBackend::new(
                    None,
                    None,
                    config.timeout,
                    config.max_output_size,
                ))
            }
            #[allow(unreachable_patterns)]
            _ => {
                tracing::warn!(
                    "Backend type {:?} not available (feature not enabled); falling back to local",
                    config.backend
                );
                Arc::new(LocalBackend::new(config.timeout, config.max_output_size))
            }
        }
    }

    /// Execute a command through the active backend.
    pub async fn execute_command(
        &self,
        command: &str,
        timeout: Option<u64>,
        workdir: Option<&str>,
        background: bool,
        pty: bool,
    ) -> Result<CommandOutput, AgentError> {
        self.current_backend
            .execute_command(command, timeout, workdir, background, pty)
            .await
    }

    /// Read a file through the active backend.
    pub async fn read_file(
        &self,
        path: &str,
        offset: Option<u64>,
        limit: Option<u64>,
    ) -> Result<String, AgentError> {
        self.current_backend.read_file(path, offset, limit).await
    }

    /// Write a file through the active backend.
    pub async fn write_file(&self, path: &str, content: &str) -> Result<(), AgentError> {
        self.current_backend.write_file(path, content).await
    }

    /// Check if a file exists through the active backend.
    pub async fn file_exists(&self, path: &str) -> Result<bool, AgentError> {
        self.current_backend.file_exists(path).await
    }

    /// Switch to a different backend type at runtime.
    ///
    /// Creates a new backend of the specified type and replaces the active one.
    /// If the feature for the requested backend is not compiled in, returns an error.
    pub fn switch_backend(&mut self, backend_type: TerminalBackendType) -> Result<(), AgentError> {
        // Validate that the backend type is available
        match backend_type {
            TerminalBackendType::Local => {}
            #[cfg(feature = "docker")]
            TerminalBackendType::Docker => {}
            #[cfg(feature = "ssh")]
            TerminalBackendType::Ssh => {}
            #[cfg(feature = "daytona")]
            TerminalBackendType::Daytona => {}
            #[cfg(feature = "modal")]
            TerminalBackendType::Modal => {}
            #[cfg(feature = "singularity")]
            TerminalBackendType::Singularity => {}
            #[allow(unreachable_patterns)]
            _ => {
                return Err(AgentError::Config(format!(
                    "Backend type {:?} is not available (feature not enabled at compile time)",
                    backend_type
                )));
            }
        }

        self.config.backend = backend_type;
        self.current_backend = Self::create_backend(&self.config);
        Ok(())
    }

    /// Get a reference to the current configuration.
    pub fn config(&self) -> &TerminalConfig {
        &self.config
    }

    /// Get the current backend type.
    pub fn backend_type(&self) -> TerminalBackendType {
        self.config.backend
    }
}