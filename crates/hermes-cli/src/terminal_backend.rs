use std::path::Path;
use std::sync::Arc;

use hermes_config::GatewayConfig;
use hermes_core::{AgentError, ResolvedExecutionMode, TerminalBackend};
use hermes_environments::{BackendManager, ExecutionBackendBinding};
use hermes_tools::ToolRegistry;

/// Build the runtime terminal backend from gateway config.
pub fn build_terminal_backend(config: &GatewayConfig) -> Arc<dyn TerminalBackend> {
    let manager = BackendManager::new(config.terminal.clone());
    manager.terminal_backend()
}

/// Build the exact terminal backend and containment context for a resolved
/// execution mode. Closed modes fail before registration unless the configured
/// backend can issue a hardened Docker context.
pub fn build_execution_backend_binding(
    config: &GatewayConfig,
    execution_mode: ResolvedExecutionMode,
) -> Result<ExecutionBackendBinding, AgentError> {
    BackendManager::execution_backend_binding(config.terminal.clone(), execution_mode)
        .map_err(|error| AgentError::Config(format!("execution backend startup failed: {error}")))
}

/// Bind the authoritative skill trust locations derived from the already
/// resolved state root. No environment or default-path fallback is consulted.
pub fn bind_skill_trust_authority_paths(
    registry: &ToolRegistry,
    state_root: &Path,
) -> Result<(), AgentError> {
    let layout = hermes_config::SkillTrustAuthorityLayout::from_resolved_state_root(state_root)
        .map_err(|error| AgentError::Config(error.to_string()))?;
    let authority = layout
        .build_skill_trust_authority_paths()
        .map_err(|error| AgentError::Config(error.to_string()))?;
    registry.bind_skill_trust_authority_paths(&authority);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn backend_from_default_config_executes_local_command() {
        let cfg = GatewayConfig::default();
        let backend = build_terminal_backend(&cfg);
        let out = backend
            .execute_command("echo backend-ok", Some(20), None, false, false)
            .await
            .expect("backend command should run");
        assert_eq!(out.exit_code, 0);
        assert!(out.stdout.contains("backend-ok"));
    }
}
