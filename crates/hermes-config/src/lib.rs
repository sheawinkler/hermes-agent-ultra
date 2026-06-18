//! # hermes-config
//!
//! Configuration management for the hermes-agent system.
//!
//! Handles loading, merging, validating, and providing access to all
//! configuration sources including YAML, JSON, environment variables,
//! and sensible defaults.

pub mod config;
pub mod dep_check;
pub mod dep_gate;
pub mod insights;
pub mod interest;
pub mod loader;
pub mod managed_gateway;
pub mod merge;
pub mod migrate;
pub mod paths;
pub mod platform;
mod python_platform_env;
mod python_yaml_compat;
pub mod roundtrip_tests;
pub mod server;
pub mod session;
pub mod streaming;
pub mod voice;
pub mod web_research;

// Re-export key types for convenience
pub use config::{
    AgentLoopBehaviorConfig, ApprovalConfig, CheapModelRouteConfig, DisplayConfig, GatewayConfig,
    LlmProviderConfig, McpServerEntry, PlatformDisplayConfig, ProfileConfig, PromptCachingConfig,
    ProxyConfig, QuickCommandConfig, SecurityConfig, SessionsMaintenanceConfig, SkillsSettings,
    SmartModelRoutingConfig, TerminalBackendType, TerminalConfig, ToolsSettings,
    normalize_service_tier,
};
pub use dep_check::{
    RuntimeDep, is_available as dep_is_available, missing_deps as dep_missing,
    supplemental_path_entries as dep_supplemental_path_entries,
};
pub use dep_gate::{await_tool_deps, deps_for_tool, spawn_background_install};
pub use insights::{InsightsConfig, InsightsContributionConfig};
pub use interest::InterestConfig;
pub use loader::{
    ConfigError, ConfigSetResult, apply_user_config_patch, atomic_json_write,
    atomic_json_write_pretty, atomic_write_bytes, atomic_yaml_write, load_config,
    load_prefill_messages, load_prefill_messages_file, load_user_config_file,
    resolve_prefill_messages_file, save_config_yaml, set_user_config_value,
    user_config_field_display, validate_config,
};
pub use managed_gateway::{
    DEFAULT_TOOL_GATEWAY_DOMAIN, GatewayBuilder, GatewaySchemeError, ManagedToolGatewayConfig,
    ModalBackendState, ModalMode, NousProviderState, ResolveOptions, SelectedBackend, TokenReader,
    build_vendor_gateway_url, coerce_modal_mode, env_var_enabled, get_tool_gateway_scheme,
    has_direct_modal_credentials, is_managed_tool_gateway_ready, managed_nous_tools_enabled,
    read_nous_access_token, resolve_managed_tool_gateway, resolve_modal_backend_state,
    resolve_openai_audio_api_key,
};
pub use merge::{deep_merge, merge_configs};
pub use migrate::{ensure_migrated_hermes_home, legacy_hermes_home_candidates, project_hermes_dir};
pub use paths::{
    INTERMEDIATE_HOME_DIR, LEGACY_HOME_DIR, LEGACY_PROJECT_HOME_DIR,
    LOCALAPPDATA_SUBDIR_INTERMEDIATE, LOCALAPPDATA_SUBDIR_LEGACY, LOCALAPPDATA_SUBDIR_NEW,
    PRIMARY_HOME_DIR, PROJECT_HOME_DIR, cli_config_path, config_path, cron_dir,
    default_home_without_migration, env_path, expand_tilde, gateway_json_path, gateway_pid_path,
    gateway_pid_path_in, hermes_home, interest_db_path, interest_db_path_in,
    intermediate_home_basename, legacy_home_basename, memory_path, primary_home_basename,
    resolve_agent_path, resolve_outbound_media_path, session_temp_dir, sessions_dir, skills_dir,
    state_db_path, state_db_path_in, state_dir, user_home_dir, user_path,
};
pub use platform::{PlatformConfig, UnauthorizedDmBehavior, extra_string, platform_token_or_extra};
pub use server::{
    DEFAULT_SERVER_LLM_MODEL, DEFAULT_WECHAT_FLOWY_SERVER_BASE, ServerAuthConfig, ServerConfig,
    ServerLlmConfig, ServerLoginMethod, default_wechat_app_id_for_channel,
    is_valid_wechat_open_app_id,
};
pub use session::{DailyReset, IdleReset, SessionConfig, SessionResetPolicy, SessionType};
pub use streaming::StreamingConfig;
pub use voice::{
    DiarizationProvider, MeetingConfig, MeetingTranscriptionMode, SttConfig, SttGroqConfig,
    SttLocalConfig, SttMistralConfig, SttOpenAiConfig, SttXaiConfig, TtsConfig, TtsEdgeConfig,
    TtsElevenLabsConfig, TtsGeminiConfig, TtsMiniMaxConfig, TtsMistralConfig, TtsOpenAiConfig,
    TtsPiperConfig, TtsProviderEntry, TtsXaiConfig,
};
pub use web_research::WebResearchConfig;
