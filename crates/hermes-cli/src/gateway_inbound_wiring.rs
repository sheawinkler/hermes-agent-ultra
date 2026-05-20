//! Wire auxiliary vision + inbound preparer into gateway and tool registry.

use std::sync::Arc;

use hermes_agent::{
    build_auxiliary_client, register_agent_builtin_tools, AgentInboundPreparer, AuxiliaryBuildParams,
};
use hermes_config::GatewayConfig;
use hermes_core::{SkillProvider, TerminalBackend};
use hermes_gateway::Gateway;
use hermes_intelligence::auxiliary::AuxiliaryConfig;
use hermes_tools::ToolRegistry;

/// Parse `provider:model` from config (e.g. `custom:flowy/DeepSeek-V4-Flash`).
fn split_configured_model(model: &str) -> (Option<String>, Option<String>) {
    let trimmed = model.trim();
    if let Some((provider, rest)) = trimmed.split_once(':') {
        let provider = provider.trim();
        let rest = rest.trim();
        if !provider.is_empty() && !rest.is_empty() {
            return (Some(provider.to_string()), Some(rest.to_string()));
        }
    }
    (None, Some(trimmed.to_string()))
}

/// Build auxiliary client, vision tool backend, and gateway inbound preparer from config.
pub async fn wire_gateway_inbound_vision(
    gateway: &Arc<Gateway>,
    tool_registry: &Arc<ToolRegistry>,
    config: &GatewayConfig,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
) {
    let configured = config
        .model
        .as_deref()
        .unwrap_or("gpt-4o")
        .to_string();
    let (primary_provider, primary_model) = split_configured_model(&configured);

    let (auxiliary, _summary) = build_auxiliary_client(AuxiliaryBuildParams {
        config: AuxiliaryConfig::default(),
        primary_provider: primary_provider.clone(),
        primary_model: primary_model.clone(),
        llm_providers: config.llm_providers.clone(),
    });

    let auxiliary = Arc::new(auxiliary);
    register_agent_builtin_tools(
        tool_registry,
        terminal_backend,
        skill_provider,
        Some(auxiliary.clone()),
    );

    let preparer = Arc::new(AgentInboundPreparer::new(auxiliary));
    gateway.set_inbound_preparer(preparer).await;
}
