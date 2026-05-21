//! Tool registry wiring with auxiliary vision backend.

use std::sync::Arc;

use hermes_core::{SkillProvider, TerminalBackend};
use hermes_intelligence::auxiliary::AuxiliaryClient;
use hermes_tools::{ToolRegistry, VisionBackend, VoiceMediaToolConfig};

use crate::vision_adapter::AuxiliaryVisionAdapter;

/// Register built-in tools, wiring vision tools through [`AuxiliaryClient`] when provided.
pub fn register_builtin_tools(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
    auxiliary: Option<Arc<AuxiliaryClient>>,
) {
    register_builtin_tools_with_voice(registry, terminal_backend, skill_provider, auxiliary, None);
}

/// Register built-in tools with optional tts/stt config from [`hermes_config::GatewayConfig`].
pub fn register_builtin_tools_with_voice(
    registry: &ToolRegistry,
    terminal_backend: Arc<dyn TerminalBackend>,
    skill_provider: Arc<dyn SkillProvider>,
    auxiliary: Option<Arc<AuxiliaryClient>>,
    voice: Option<VoiceMediaToolConfig>,
) {
    let vision_backend = auxiliary
        .map(|client| Arc::new(AuxiliaryVisionAdapter::new(client)) as Arc<dyn VisionBackend>);
    hermes_tools::register_builtin_tools_with_vision_and_voice(
        registry,
        terminal_backend,
        skill_provider,
        vision_backend,
        voice,
    );
}
