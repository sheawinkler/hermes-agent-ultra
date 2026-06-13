//! Bounded invariant coverage: config serialization roundtrip consistency
//! **Validates: Requirements 19.1, 19.2, 19.3, 19.4, 2.6**
//!
//! For representative valid configuration objects, serializing to JSON/YAML
//! and deserializing should produce an equivalent result.

use std::collections::HashMap;

use hermes_config::{
    AgentLoopBehaviorConfig, ApprovalConfig, DailyReset, GatewayConfig, IdleReset, PlatformConfig,
    ProfileConfig, SecurityConfig, SessionConfig, SessionResetPolicy, SessionsMaintenanceConfig,
    SkillsSettings, SmartModelRoutingConfig, StreamingConfig, TerminalConfig, ToolOutputConfig,
    ToolsSettings, UnauthorizedDmBehavior,
};
use hermes_core::BudgetConfig;
use serde::{de::DeserializeOwned, Serialize};

fn assert_json_roundtrip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let json = serde_json::to_string(value).unwrap();
    let back: T = serde_json::from_str(&json).unwrap();
    assert_eq!(value, &back);
}

fn assert_yaml_roundtrip<T>(value: &T)
where
    T: Serialize + DeserializeOwned + PartialEq + std::fmt::Debug,
{
    let yaml = serde_yaml::to_string(value).unwrap();
    let back: T = serde_yaml::from_str(&yaml).unwrap();
    assert_eq!(value, &back);
}

fn streaming_config_cases() -> Vec<StreamingConfig> {
    vec![
        StreamingConfig {
            enabled: false,
            edit_interval_ms: 100,
            buffer_threshold: 1,
            max_message_length: 256,
        },
        StreamingConfig {
            enabled: true,
            edit_interval_ms: 10_000,
            buffer_threshold: 500,
            max_message_length: 16_384,
        },
        StreamingConfig {
            enabled: true,
            edit_interval_ms: 750,
            buffer_threshold: 64,
            max_message_length: 4_096,
        },
    ]
}

fn session_reset_policy_cases() -> Vec<SessionResetPolicy> {
    vec![
        SessionResetPolicy::None,
        SessionResetPolicy::Daily { at_hour: 0 },
        SessionResetPolicy::Daily { at_hour: 23 },
        SessionResetPolicy::Idle { timeout_minutes: 1 },
        SessionResetPolicy::Idle {
            timeout_minutes: 1_440,
        },
        SessionResetPolicy::Both {
            daily: DailyReset { at_hour: 0 },
            idle: IdleReset { timeout_minutes: 1 },
        },
        SessionResetPolicy::Both {
            daily: DailyReset { at_hour: 23 },
            idle: IdleReset {
                timeout_minutes: 1_440,
            },
        },
    ]
}

fn platform_config_cases() -> Vec<PlatformConfig> {
    let mut extra = HashMap::new();
    extra.insert("thread_mode".to_string(), serde_json::json!("per-channel"));

    vec![
        PlatformConfig {
            enabled: false,
            token: None,
            webhook_url: None,
            require_mention: None,
            unauthorized_dm_behavior: UnauthorizedDmBehavior::Ignore,
            group_sessions_per_user: false,
            home_channel: None,
            allowed_users: Vec::new(),
            admin_users: Vec::new(),
            extra: HashMap::new(),
        },
        PlatformConfig {
            enabled: true,
            token: Some("token-alpha".to_string()),
            webhook_url: Some("https://example.com/hook".to_string()),
            require_mention: Some(true),
            unauthorized_dm_behavior: UnauthorizedDmBehavior::Pair,
            group_sessions_per_user: true,
            home_channel: Some("ops".to_string()),
            allowed_users: vec!["alice".to_string(), "bob".to_string()],
            admin_users: vec!["admin".to_string()],
            extra,
        },
    ]
}

fn session_config_cases() -> Vec<SessionConfig> {
    vec![
        SessionConfig {
            reset_policy: SessionResetPolicy::None,
            max_context_messages: None,
            compression_enabled: false,
            platform_overrides: HashMap::new(),
            session_type_overrides: HashMap::new(),
        },
        SessionConfig {
            reset_policy: SessionResetPolicy::Daily { at_hour: 6 },
            max_context_messages: Some(1),
            compression_enabled: true,
            platform_overrides: HashMap::new(),
            session_type_overrides: HashMap::new(),
        },
        SessionConfig {
            reset_policy: SessionResetPolicy::Both {
                daily: DailyReset { at_hour: 23 },
                idle: IdleReset {
                    timeout_minutes: 1_440,
                },
            },
            max_context_messages: Some(1_000),
            compression_enabled: true,
            platform_overrides: HashMap::new(),
            session_type_overrides: HashMap::new(),
        },
    ]
}

fn gateway_config(
    model: Option<&str>,
    personality: Option<&str>,
    max_turns: u32,
    session: SessionConfig,
    streaming: StreamingConfig,
) -> GatewayConfig {
    GatewayConfig {
        model: model.map(str::to_string),
        personality: personality.map(str::to_string),
        max_turns,
        system_prompt: None,
        prefill_messages_file: None,
        tools: vec!["bash".into(), "read".into()],
        budget: BudgetConfig::default(),
        tool_output: ToolOutputConfig::default(),
        platforms: HashMap::new(),
        platform_toolsets: hermes_config::config::default_platform_toolsets(),
        delegation: Default::default(),
        session,
        sessions: SessionsMaintenanceConfig::default(),
        streaming,
        display: Default::default(),
        terminal: TerminalConfig::default(),
        web: Default::default(),
        llm_providers: HashMap::new(),
        fallback_model: None,
        fallback_models: Vec::new(),
        smart_model_routing: SmartModelRoutingConfig::default(),
        auxiliary: Default::default(),
        quick_commands: Default::default(),
        kanban: Default::default(),
        tts: serde_json::Value::Null,
        proxy: None,
        approval: ApprovalConfig::default(),
        security: SecurityConfig::default(),
        skills: SkillsSettings::default(),
        tools_config: ToolsSettings::default(),
        mcp_servers: Vec::new(),
        profile: ProfileConfig::default(),
        agent: AgentLoopBehaviorConfig::default(),
        home_dir: None,
    }
}

fn gateway_config_cases() -> Vec<GatewayConfig> {
    let sessions = session_config_cases();
    let streaming = streaming_config_cases();

    vec![
        gateway_config(None, None, 1, sessions[0].clone(), streaming[0].clone()),
        gateway_config(
            Some("claude-sonnet"),
            Some("ops"),
            32,
            sessions[1].clone(),
            streaming[1].clone(),
        ),
        gateway_config(
            Some("gpt-4.1"),
            Some("reviewer"),
            100,
            sessions[2].clone(),
            streaming[2].clone(),
        ),
    ]
}

#[test]
fn streaming_config_json_roundtrip() {
    for config in streaming_config_cases() {
        assert_json_roundtrip(&config);
    }
}

#[test]
fn streaming_config_yaml_roundtrip() {
    for config in streaming_config_cases() {
        assert_yaml_roundtrip(&config);
    }
}

#[test]
fn session_reset_policy_json_roundtrip() {
    for policy in session_reset_policy_cases() {
        assert_json_roundtrip(&policy);
    }
}

#[test]
fn session_reset_policy_yaml_roundtrip() {
    for policy in session_reset_policy_cases() {
        assert_yaml_roundtrip(&policy);
    }
}

#[test]
fn platform_config_json_roundtrip() {
    for config in platform_config_cases() {
        assert_json_roundtrip(&config);
    }
}

#[test]
fn platform_config_yaml_roundtrip() {
    for config in platform_config_cases() {
        assert_yaml_roundtrip(&config);
    }
}

#[test]
fn gateway_config_json_roundtrip() {
    for config in gateway_config_cases() {
        assert_json_roundtrip(&config);
    }
}

#[test]
fn gateway_config_yaml_roundtrip() {
    for config in gateway_config_cases() {
        assert_yaml_roundtrip(&config);
    }
}
