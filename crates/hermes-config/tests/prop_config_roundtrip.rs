//! Property 1: Config serialization roundtrip consistency
//! **Validates: Requirements 19.1, 19.2, 19.3, 19.4, 2.6**
//!
//! For any valid configuration object (GatewayConfig, SessionResetPolicy,
//! PlatformConfig, StreamingConfig), serializing to JSON then deserializing
//! should produce an equivalent result. Same for YAML.

use proptest::prelude::*;
use std::collections::HashMap;

use hermes_config::{
    AgentLoopBehaviorConfig, ApprovalConfig, DailyReset, GatewayConfig, IdleReset, PlatformConfig,
    ProfileConfig, SecurityConfig, SessionConfig, SessionResetPolicy, SessionsMaintenanceConfig,
    SkillsSettings, SmartModelRoutingConfig, StreamingConfig, TerminalConfig, ToolsSettings,
    UnauthorizedDmBehavior,
};
use hermes_core::BudgetConfig;

// ---------------------------------------------------------------------------
// Strategies
// ---------------------------------------------------------------------------

fn arb_streaming_config() -> impl Strategy<Value = StreamingConfig> {
    (
        proptest::bool::ANY,
        100u64..10_000,
        1usize..500,
        256usize..16_384,
    )
        .prop_map(
            |(enabled, edit_interval_ms, buffer_threshold, max_message_length)| StreamingConfig {
                enabled,
                edit_interval_ms,
                buffer_threshold,
                max_message_length,
            },
        )
}

fn arb_session_reset_policy() -> impl Strategy<Value = SessionResetPolicy> {
    prop_oneof![
        Just(SessionResetPolicy::None),
        (0u8..24).prop_map(|h| SessionResetPolicy::Daily { at_hour: h }),
        (1u64..1440).prop_map(|m| SessionResetPolicy::Idle { timeout_minutes: m }),
        (0u8..24, 1u64..1440).prop_map(|(h, m)| SessionResetPolicy::Both {
            daily: DailyReset { at_hour: h },
            idle: IdleReset { timeout_minutes: m },
        }),
    ]
}

fn arb_unauthorized_dm() -> impl Strategy<Value = UnauthorizedDmBehavior> {
    prop_oneof![
        Just(UnauthorizedDmBehavior::Pair),
        Just(UnauthorizedDmBehavior::Ignore),
    ]
}

fn arb_platform_config() -> impl Strategy<Value = PlatformConfig> {
    (
        proptest::bool::ANY,
        proptest::option::of("[a-z]{4,16}"),
        proptest::option::of("https://[a-z]{4,12}\\.com/hook"),
        proptest::option::of(proptest::bool::ANY),
        arb_unauthorized_dm(),
        proptest::bool::ANY,
        proptest::option::of("[a-z]{3,10}"),
    )
        .prop_map(
            |(enabled, token, webhook_url, require_mention, dm_behavior, per_user, home)| {
                PlatformConfig {
                    enabled,
                    token,
                    webhook_url,
                    require_mention,
                    unauthorized_dm_behavior: dm_behavior,
                    group_sessions_per_user: per_user,
                    home_channel: home,
                    allowed_users: Vec::new(),
                    admin_users: Vec::new(),
                    extra: HashMap::new(),
                }
            },
        )
}

fn arb_session_config() -> impl Strategy<Value = SessionConfig> {
    (
        arb_session_reset_policy(),
        proptest::option::of(1usize..1000),
        proptest::bool::ANY,
    )
        .prop_map(|(reset_policy, max_ctx, compression)| SessionConfig {
            reset_policy,
            max_context_messages: max_ctx,
            compression_enabled: compression,
            platform_overrides: HashMap::new(),
            session_type_overrides: HashMap::new(),
        })
}

fn arb_gateway_config() -> impl Strategy<Value = GatewayConfig> {
    (
        proptest::option::of("[a-z0-9\\-]{3,20}"),
        proptest::option::of("[a-z]{3,12}"),
        1u32..100,
        arb_session_config(),
        arb_streaming_config(),
    )
        .prop_map(
            |(model, personality, max_turns, session, streaming)| GatewayConfig {
                model,
                personality,
                max_turns,
                system_prompt: None,
                tools: vec!["bash".into(), "read".into()],
                budget: BudgetConfig::default(),
                platforms: HashMap::new(),
                platform_toolsets: hermes_config::config::default_platform_toolsets(),
                session,
                sessions: SessionsMaintenanceConfig::default(),
                streaming,
                terminal: TerminalConfig::default(),
                llm_providers: HashMap::new(),
                smart_model_routing: SmartModelRoutingConfig::default(),
                proxy: None,
                approval: ApprovalConfig::default(),
                security: SecurityConfig::default(),
                skills: SkillsSettings::default(),
                tools_config: ToolsSettings::default(),
                mcp_servers: Vec::new(),
                profile: ProfileConfig::default(),
                agent: AgentLoopBehaviorConfig::default(),
                home_dir: None,
            },
        )
}

// ---------------------------------------------------------------------------
// Property tests
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig::with_cases(100))]

    #[test]
    fn prop_streaming_config_json_roundtrip(config in arb_streaming_config()) {
        let json = serde_json::to_string(&config).unwrap();
        let back: StreamingConfig = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&config, &back);
    }

    #[test]
    fn prop_streaming_config_yaml_roundtrip(config in arb_streaming_config()) {
        let yaml = serde_yaml::to_string(&config).unwrap();
        let back: StreamingConfig = serde_yaml::from_str(&yaml).unwrap();
        prop_assert_eq!(&config, &back);
    }

    #[test]
    fn prop_session_reset_policy_json_roundtrip(policy in arb_session_reset_policy()) {
        let json = serde_json::to_string(&policy).unwrap();
        let back: SessionResetPolicy = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&policy, &back);
    }

    #[test]
    fn prop_session_reset_policy_yaml_roundtrip(policy in arb_session_reset_policy()) {
        let yaml = serde_yaml::to_string(&policy).unwrap();
        let back: SessionResetPolicy = serde_yaml::from_str(&yaml).unwrap();
        prop_assert_eq!(&policy, &back);
    }

    #[test]
    fn prop_platform_config_json_roundtrip(config in arb_platform_config()) {
        let json = serde_json::to_string(&config).unwrap();
        let back: PlatformConfig = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&config, &back);
    }

    #[test]
    fn prop_platform_config_yaml_roundtrip(config in arb_platform_config()) {
        let yaml = serde_yaml::to_string(&config).unwrap();
        let back: PlatformConfig = serde_yaml::from_str(&yaml).unwrap();
        prop_assert_eq!(&config, &back);
    }

    #[test]
    fn prop_gateway_config_json_roundtrip(config in arb_gateway_config()) {
        let json = serde_json::to_string(&config).unwrap();
        let back: GatewayConfig = serde_json::from_str(&json).unwrap();
        prop_assert_eq!(&config, &back);
    }

    #[test]
    fn prop_gateway_config_yaml_roundtrip(config in arb_gateway_config()) {
        let yaml = serde_yaml::to_string(&config).unwrap();
        let back: GatewayConfig = serde_yaml::from_str(&yaml).unwrap();
        prop_assert_eq!(&config, &back);
    }
}
