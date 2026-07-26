//! # hermes-config
//!
//! Configuration management for the hermes-agent system.
//!
//! Handles loading, merging, validating, and providing access to all
//! configuration sources including YAML, JSON, environment variables,
//! and sensible defaults.

pub mod config;
pub mod loader;
pub mod managed_gateway;
pub mod managed_node;
pub mod media_policy;
pub mod merge;
pub mod paths;
pub mod platform;
mod python_platform_env;
mod python_yaml_compat;
pub mod roundtrip_tests;
pub mod session;
pub mod skill_trust;
pub mod streaming;

// Re-export key types for convenience
pub use config::{
    default_auxiliary_task_configs, normalize_service_tier, AgentLoopBehaviorConfig,
    ApprovalConfig, AuxiliaryTaskConfig, CheapModelRouteConfig, DelegationConfig, DisplayConfig,
    ExecutionModeConfigValue, GatewayConfig, LlmProviderConfig, McpServerEntry, ModelSwitchConfig,
    PlatformDisplayConfig, ProfileConfig, ProxyConfig, QuickCommandConfig, SecurityConfig,
    SessionsMaintenanceConfig, SkillsSettings, SmartModelRoutingConfig, StaleAuxiliaryAssignment,
    TerminalBackendType, TerminalConfig, TerminalHomeMode, ToolOutputConfig, ToolsSettings,
    WebConfig, WebsiteBlocklistConfig, DEFAULT_TOOL_OUTPUT_MAX_BYTES,
    DEFAULT_TOOL_OUTPUT_MAX_LINES, DEFAULT_TOOL_OUTPUT_MAX_LINE_LENGTH,
};
pub use loader::{
    apply_user_config_patch, atomic_json_write, atomic_json_write_pretty, atomic_replace_file,
    atomic_write_bytes, atomic_yaml_write, load_config, load_effective_config_yaml_value,
    load_prefill_messages, load_prefill_messages_file, load_selected_profile_execution_mode,
    load_user_config_file, managed_scope_dir, redact_url_credentials_for_display,
    resolve_prefill_messages_file, resolve_prefill_messages_path, resolve_runtime_execution_mode,
    save_config_yaml, set_user_config_value, user_config_field_display, validate_config,
    ConfigError, ConfigSetResult, EXECUTION_MODE_ENV_KEY,
};
pub use managed_gateway::{
    build_vendor_gateway_url, coerce_modal_mode, env_var_enabled, get_tool_gateway_scheme,
    has_direct_modal_credentials, is_managed_tool_gateway_ready, managed_nous_tools_enabled,
    read_nous_access_token, resolve_managed_tool_gateway, resolve_modal_backend_state,
    resolve_openai_audio_api_key, GatewayBuilder, GatewaySchemeError, ManagedToolGatewayConfig,
    ModalBackendState, ModalMode, NousProviderState, ResolveOptions, SelectedBackend, TokenReader,
    DEFAULT_TOOL_GATEWAY_DOMAIN,
};
pub use managed_node::{
    find_hermes_node_executable, find_hermes_node_executable_in, find_node_executable,
    iter_hermes_node_dirs, iter_hermes_node_dirs_in, with_hermes_node_path_var,
    with_hermes_node_path_var_for_home,
};
pub use media_policy::{
    default_media_cache_dir, validate_external_media_upload_path, validate_media_delivery_path,
    validate_media_file_handle, ValidatedExternalMediaUpload, ValidatedMediaDelivery,
};
pub use merge::{deep_merge, merge_configs};
pub use paths::{
    cli_config_path, config_path, cron_dir, env_path, gateway_json_path, gateway_pid_path,
    gateway_pid_path_in, hermes_home, memory_path, sessions_dir, skills_dir, state_dir, user_path,
};
pub use platform::{PlatformConfig, UnauthorizedDmBehavior};
pub use session::{DailyReset, IdleReset, SessionConfig, SessionResetPolicy, SessionType};
pub use skill_trust::SkillTrustAuthorityLayout;
pub use streaming::StreamingConfig;
