//! Wire auxiliary vision + inbound preparer + voice/STT into gateway and tool registry.

use std::sync::Arc;

use hermes_agent::{
    build_auxiliary_client, register_agent_builtin_tools_with_voice, AgentInboundPreparer,
    AuxiliaryBuildParams,
};
use hermes_config::GatewayConfig;
use hermes_core::{SkillProvider, TerminalBackend};
use hermes_gateway::voice::VoiceManager;
use hermes_gateway::voice_config::voice_config_from_app;
use hermes_gateway::Gateway;
use hermes_intelligence::auxiliary::AuxiliaryConfig;
use hermes_tools::{ToolRegistry, VoiceMediaToolConfig};

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

/// Build auxiliary client, vision tool backend, gateway inbound preparer, and voice runtime from config.
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
    let voice_tools = VoiceMediaToolConfig {
        tts: config.tts.clone(),
        stt: config.stt.clone(),
    };
    register_agent_builtin_tools_with_voice(
        tool_registry,
        terminal_backend,
        skill_provider,
        Some(auxiliary.clone()),
        Some(voice_tools),
    );

    let preparer = Arc::new(AgentInboundPreparer::new(auxiliary));
    gateway.set_inbound_preparer(preparer).await;

    let (voice_cfg, stt_enabled) =
        voice_config_from_app(config.tts.as_ref(), config.stt.as_ref());
    let stt_config = config.stt.clone().unwrap_or_default();
    let manager = Arc::new(VoiceManager::with_stt_config(voice_cfg, stt_config));
    gateway.set_voice_runtime(manager, stt_enabled).await;
}
