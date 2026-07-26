//! Backend manager – orchestrates the active terminal backend.

use hermes_config::{TerminalBackendType, TerminalConfig};
use std::sync::Arc;

use crate::containment::{ExecutionContainmentAdmissionError, ExecutionContainmentContext};
use hermes_core::{
    AgentError, CommandOutput, ExecutionMode, ResolvedExecutionMode, TerminalBackend,
};

use crate::local::LocalBackend;

#[cfg(feature = "docker")]
use crate::docker::DockerBackend;
#[cfg(feature = "docker")]
use crate::docker::DockerContainmentCertificationError;

#[cfg(feature = "ssh")]
use crate::ssh::SshBackend;

#[cfg(feature = "daytona")]
use crate::daytona::DaytonaBackend;

#[cfg(feature = "modal")]
use crate::modal::ModalBackend;

#[cfg(feature = "singularity")]
use crate::singularity::SingularityBackend;

/// A terminal backend and the sole containment context allowed to govern it.
///
/// The pair prevents a closed-mode caller from certifying one backend and then
/// dispatching tools through another.
pub struct ExecutionBackendBinding {
    pub terminal_backend: Arc<dyn TerminalBackend>,
    pub containment_context: Arc<ExecutionContainmentContext>,
}

/// Typed startup failure for a requested execution-mode/backend binding.
#[derive(Debug, thiserror::Error)]
pub enum ExecutionBackendBindingError {
    #[error(
        "closed execution mode {mode} requires a fresh hardened Docker backend, not configured backend {backend:?}"
    )]
    UnsupportedClosedBackend {
        mode: ExecutionMode,
        backend: TerminalBackendType,
    },
    #[error("closed execution Docker support is unavailable in this build")]
    DockerSupportUnavailable,
    #[error(transparent)]
    ManualContext(#[from] ExecutionContainmentAdmissionError),
    #[cfg(feature = "docker")]
    #[error(transparent)]
    DockerCertification(#[from] DockerContainmentCertificationError),
}

/// Manages the active terminal backend and provides a unified interface
/// for command execution and file operations regardless of backend type.
pub struct BackendManager {
    current_backend: Arc<dyn TerminalBackend>,
    active_backend_type: TerminalBackendType,
    config: TerminalConfig,
}

impl BackendManager {
    /// Create a new `BackendManager` with the given configuration.
    ///
    /// The initial backend is selected based on `config.backend`.
    /// If the selected backend type is not available (e.g. compiled without
    /// the corresponding feature), falls back to [`LocalBackend`].
    pub fn new(config: TerminalConfig) -> Self {
        let (backend, active_backend_type) = Self::create_backend(&config);
        Self {
            current_backend: backend,
            active_backend_type,
            config,
        }
    }

    /// Bind a terminal backend to the exact containment context that admitted
    /// its resolved execution mode.
    ///
    /// Closed modes never call the legacy fallback factory: they require a
    /// concrete fresh Docker backend so the context and dispatch backend are
    /// the same `Arc`. Manual mode retains the explicit host-local context.
    pub fn execution_backend_binding(
        config: TerminalConfig,
        mode: ResolvedExecutionMode,
    ) -> Result<ExecutionBackendBinding, ExecutionBackendBindingError> {
        if !mode.mode.requires_containment_certificate() {
            let manager = Self::new(config);
            return Ok(ExecutionBackendBinding {
                terminal_backend: manager.terminal_backend(),
                containment_context: Arc::new(ExecutionContainmentContext::manual(mode)?),
            });
        }

        match config.backend {
            TerminalBackendType::Docker => {
                #[cfg(feature = "docker")]
                {
                    let backend = Arc::new(DockerBackend::new(
                        config.docker_container_id,
                        config.docker_image,
                        config.docker_mount_cwd_to_workspace,
                        config.docker_run_as_host_user,
                        config.container_cpu,
                        config.container_memory,
                        config.container_disk,
                        config.container_persistent,
                        config.docker_env,
                        config.docker_forward_env,
                        config.docker_volumes,
                        config.timeout,
                        config.max_output_size,
                    ));
                    let containment_context = Arc::new(backend.closed_execution_context(mode)?);
                    let terminal_backend: Arc<dyn TerminalBackend> = backend;
                    return Ok(ExecutionBackendBinding {
                        terminal_backend,
                        containment_context,
                    });
                }
                #[cfg(not(feature = "docker"))]
                {
                    let _ = mode;
                    return Err(ExecutionBackendBindingError::DockerSupportUnavailable);
                }
            }
            backend => Err(ExecutionBackendBindingError::UnsupportedClosedBackend {
                mode: mode.mode,
                backend,
            }),
        }
    }

    /// Create the appropriate backend based on configuration.
    fn create_backend(config: &TerminalConfig) -> (Arc<dyn TerminalBackend>, TerminalBackendType) {
        match config.backend {
            TerminalBackendType::Local => (
                Arc::new(LocalBackend::from_terminal_config(config)),
                TerminalBackendType::Local,
            ),
            #[cfg(feature = "docker")]
            TerminalBackendType::Docker => (
                Arc::new(DockerBackend::new(
                    config.docker_container_id.clone(),
                    config.docker_image.clone(),
                    config.docker_mount_cwd_to_workspace,
                    config.docker_run_as_host_user,
                    config.container_cpu,
                    config.container_memory,
                    config.container_disk,
                    config.container_persistent,
                    config.docker_env.clone(),
                    config.docker_forward_env.clone(),
                    config.docker_volumes.clone(),
                    config.timeout,
                    config.max_output_size,
                )),
                TerminalBackendType::Docker,
            ),
            #[cfg(feature = "ssh")]
            TerminalBackendType::Ssh => (
                Arc::new(SshBackend::new(
                    config
                        .ssh_host
                        .clone()
                        .unwrap_or_else(|| "localhost".to_string()),
                    config.ssh_port.unwrap_or(22),
                    config.ssh_user.clone(),
                    config.ssh_key_path.clone(),
                    config.timeout,
                    config.max_output_size,
                )),
                TerminalBackendType::Ssh,
            ),
            #[cfg(feature = "daytona")]
            TerminalBackendType::Daytona => (
                Arc::new(DaytonaBackend::new(
                    None,
                    None,
                    None,
                    config.timeout,
                    config.max_output_size,
                )),
                TerminalBackendType::Daytona,
            ),
            #[cfg(feature = "modal")]
            TerminalBackendType::Modal => (
                Arc::new(ModalBackend::new(
                    None,
                    None,
                    config.timeout,
                    config.max_output_size,
                )),
                TerminalBackendType::Modal,
            ),
            #[cfg(feature = "singularity")]
            TerminalBackendType::Singularity => (
                Arc::new(SingularityBackend::new(
                    None,
                    None,
                    config.timeout,
                    config.max_output_size,
                )),
                TerminalBackendType::Singularity,
            ),
            #[allow(unreachable_patterns)]
            _ => {
                tracing::warn!(
                    "Backend type {:?} not available (feature not enabled); falling back to local",
                    config.backend
                );
                (
                    Arc::new(LocalBackend::from_terminal_config(config)),
                    TerminalBackendType::Local,
                )
            }
        }
    }

    /// Clone the currently selected backend.
    pub fn terminal_backend(&self) -> Arc<dyn TerminalBackend> {
        self.current_backend.clone()
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
        let (backend, active_backend_type) = Self::create_backend(&self.config);
        self.current_backend = backend;
        self.active_backend_type = active_backend_type;
        Ok(())
    }

    /// Get a reference to the current configuration.
    pub fn config(&self) -> &TerminalConfig {
        &self.config
    }

    /// Get the current backend type.
    pub fn backend_type(&self) -> TerminalBackendType {
        self.active_backend_type
    }

    /// Get the configured backend type from the provided config.
    pub fn configured_backend_type(&self) -> TerminalBackendType {
        self.config.backend
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_config::TerminalConfig;

    #[test]
    fn local_backend_selected_from_default_config() {
        let manager = BackendManager::new(TerminalConfig::default());
        assert_eq!(manager.backend_type(), TerminalBackendType::Local);
        assert_eq!(
            manager.configured_backend_type(),
            TerminalBackendType::Local
        );
    }

    #[test]
    fn closed_execution_binding_refuses_local_before_any_fallback_backend_is_built() {
        let error = BackendManager::execution_backend_binding(
            TerminalConfig::default(),
            ResolvedExecutionMode {
                mode: ExecutionMode::Reviewed,
                source: hermes_core::ExecutionModeSource::Cli,
            },
        )
        .err()
        .expect("closed mode must not use the local fallback backend");
        assert!(matches!(
            error,
            ExecutionBackendBindingError::UnsupportedClosedBackend {
                mode: ExecutionMode::Reviewed,
                backend: TerminalBackendType::Local,
            }
        ));
    }

    #[test]
    fn manual_execution_binding_remains_explicit_host_local() {
        let binding = BackendManager::execution_backend_binding(
            TerminalConfig::default(),
            ResolvedExecutionMode {
                mode: ExecutionMode::Manual,
                source: hermes_core::ExecutionModeSource::Cli,
            },
        )
        .expect("manual host binding");
        assert!(binding.containment_context.is_manual_host());
        assert!(binding.containment_context.sandbox_digest().is_none());
    }

    #[cfg(feature = "docker")]
    #[test]
    fn closed_execution_binding_accepts_only_the_fresh_pinned_docker_factory() {
        let binding = BackendManager::execution_backend_binding(
            TerminalConfig {
                backend: TerminalBackendType::Docker,
                docker_image: Some(format!("registry.example/hermes@sha256:{}", "e".repeat(64))),
                ..TerminalConfig::default()
            },
            ResolvedExecutionMode {
                mode: ExecutionMode::Reviewed,
                source: hermes_core::ExecutionModeSource::Cli,
            },
        )
        .expect("fresh pinned Docker binding");
        assert!(binding.containment_context.is_certified_dispatch_context());
        assert_eq!(
            binding.containment_context.mode().mode,
            ExecutionMode::Reviewed
        );
        assert!(binding.containment_context.sandbox_digest().is_some());
    }

    #[cfg(not(feature = "docker"))]
    #[test]
    fn unavailable_backend_falls_back_to_local_but_keeps_configured_type() {
        let cfg = TerminalConfig {
            backend: TerminalBackendType::Docker,
            ..TerminalConfig::default()
        };
        let manager = BackendManager::new(cfg);
        assert_eq!(manager.backend_type(), TerminalBackendType::Local);
        assert_eq!(
            manager.configured_backend_type(),
            TerminalBackendType::Docker
        );
    }
}
