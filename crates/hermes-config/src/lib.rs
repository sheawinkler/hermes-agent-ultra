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
pub mod roundtrip_tests;
pub mod session;
pub mod streaming;

// Re-export key types for convenience
pub use config::{
    ApprovalConfig, GatewayConfig, LlmProviderConfig, ProxyConfig, TerminalBackendType,
    TerminalConfig,
};
pub use loader::{load_config, validate_config, ConfigError};
pub use merge::{deep_merge, merge_configs};
pub use paths::{
    config_path, cron_dir, env_path, gateway_json_path, hermes_home, memory_path, sessions_dir,
    skills_dir, user_path,
};
pub use platform::{PlatformConfig, UnauthorizedDmBehavior};
pub use session::{
    DailyReset, IdleReset, SessionConfig, SessionResetPolicy, SessionType,
};
pub use streaming::StreamingConfig;