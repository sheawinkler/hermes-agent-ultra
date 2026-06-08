//! # hermes-config
//!
//! Configuration management for the hermes-agent system.
//!
//! Handles loading, merging, validating, and providing access to all
//! configuration sources including YAML, JSON, environment variables,
//! and sensible defaults.

pub mod config;
pub mod web_research;
pub mod dep_check;
pub mod insights;
pub mod interest;
pub mod voice;
pub mod loader;
pub mod managed_gateway;
pub mod merge;
pub mod paths;
pub mod platform;
mod python_platform_env;
mod python_yaml_compat;
pub mod roundtrip_tests;
pub mod session;
pub mod streaming;

// Re-export key types for convenience
pub use config::{
    AgentLoopBehaviorConfig, ApprovalConfig, CheapModelRouteConfig, GatewayConfig,
    LlmProviderConfig, McpServerEntry, ProfileConfig, PromptCachingConfig, ProxyConfig,
    SecurityConfig, SessionsMaintenanceConfig, SkillsSettings, SmartModelRoutingConfig,
    TerminalBackendType, TerminalConfig, ToolsSettings,
};
pub use dep_check::{RuntimeDep, is_available as dep_is_available};
pub use interest::InterestConfig;
pub use insights::{InsightsConfig, InsightsContributionConfig};
pub use web_research::WebResearchConfig;
pub use voice::{
    DiarizationProvider, MeetingConfig, MeetingTranscriptionMode, SttConfig, SttGroqConfig,
    SttLocalConfig, SttMistralConfig, SttOpenAiConfig, SttXaiConfig, TtsConfig, TtsEdgeConfig,
    TtsElevenLabsConfig, TtsGeminiConfig, TtsMiniMaxConfig, TtsMistralConfig, TtsOpenAiConfig,
    TtsPiperConfig, TtsProviderEntry, TtsXaiConfig,
};
pub use loader::{
    apply_user_config_patch, load_config, load_user_config_file, save_config_yaml,
    user_config_field_display, validate_config, ConfigError, YamlConfigError, YamlParseStage,
};
pub use managed_gateway::{
    build_vendor_gateway_url, coerce_modal_mode, env_var_enabled, get_tool_gateway_scheme,
    has_direct_modal_credentials, is_managed_tool_gateway_ready, managed_nous_tools_enabled,
    read_nous_access_token, resolve_managed_tool_gateway, resolve_modal_backend_state,
    resolve_openai_audio_api_key, GatewayBuilder, GatewaySchemeError, ManagedToolGatewayConfig,
    ModalBackendState, ModalMode, NousProviderState, ResolveOptions, SelectedBackend, TokenReader,
    DEFAULT_TOOL_GATEWAY_DOMAIN,
};
pub use merge::{deep_merge, merge_configs};
pub use paths::{
    cli_config_path, config_path, cron_dir, env_path, expand_tilde, gateway_json_path,
    gateway_pid_path, gateway_pid_path_in, hermes_home, interest_db_path, interest_db_path_in,
    memory_path, resolve_agent_path, resolve_outbound_media_path, session_temp_dir, sessions_dir,
    skills_dir, state_db_path, state_db_path_in, state_dir, user_path,
};
pub use platform::{extra_string, platform_token_or_extra, PlatformConfig, UnauthorizedDmBehavior};
pub use session::{DailyReset, IdleReset, SessionConfig, SessionResetPolicy, SessionType};
pub use streaming::StreamingConfig;
