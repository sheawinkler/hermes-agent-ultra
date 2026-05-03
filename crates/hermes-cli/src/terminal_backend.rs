use std::sync::Arc;

use hermes_config::GatewayConfig;
use hermes_core::TerminalBackend;
use hermes_environments::BackendManager;

/// Build the runtime terminal backend from gateway config.
pub fn build_terminal_backend(config: &GatewayConfig) -> Arc<dyn TerminalBackend> {
    let manager = BackendManager::new(config.terminal.clone());
    manager.terminal_backend()
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
