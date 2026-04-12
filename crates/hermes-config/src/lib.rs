//! # hermes-config
//!
//! Configuration management for the hermes-agent system.
//!
//! Handles loading, merging, validating, and providing access to all
//! configuration sources including YAML, JSON, environment variables,
//! and sensible defaults.

pub mod config;
pub mod loader;
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
    ApprovalConfig, GatewayConfig, LlmProviderConfig, McpServerEntry, ProfileConfig, ProxyConfig,
    SkillsSettings, TerminalBackendType, TerminalConfig, ToolsSettings,
};
pub use loader::{
    apply_user_config_patch, load_config, load_user_config_file, save_config_yaml,
    user_config_field_display, validate_config, ConfigError,
};
pub use merge::{deep_merge, merge_configs};
pub use paths::{
    cli_config_path, config_path, cron_dir, env_path, gateway_json_path, gateway_pid_path,
    gateway_pid_path_in, hermes_home, memory_path, sessions_dir, skills_dir, state_dir, user_path,
};
pub use platform::{PlatformConfig, UnauthorizedDmBehavior};
pub use session::{DailyReset, IdleReset, SessionConfig, SessionResetPolicy, SessionType};
pub use streaming::StreamingConfig;
