//! Roundtrip (serialization <-> deserialization) tests for configuration types.
//!
//! Requirement 19: For all valid config objects, serializing then deserializing
//! MUST produce an equivalent object.

#[cfg(test)]
mod tests {
    use serde::{Deserialize, Serialize};
    use std::collections::HashMap;

    use crate::config::GatewayConfig;
    use crate::platform::{PlatformConfig, UnauthorizedDmBehavior};
    use crate::session::{DailyReset, IdleReset, SessionConfig, SessionResetPolicy, SessionType};
    use crate::streaming::StreamingConfig;

    /// Helper: assert that serializing to JSON then deserializing produces an
    /// equivalent object.
    fn assert_json_roundtrip<T>(original: &T)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let json = serde_json::to_string(original).expect("serialization failed");
        let roundtripped: T = serde_json::from_str(&json).expect("deserialization failed");
        assert_eq!(
            original, &roundtripped,
            "JSON roundtrip failed.\nOriginal:   {:?}\nSerialized: {}\nRoundtrip: {:?}",
            original, json, roundtripped
        );
    }

    /// Helper: assert that serializing to YAML then deserializing produces an
    /// equivalent object.
    fn assert_yaml_roundtrip<T>(original: &T)
    where
        T: Serialize + for<'de> Deserialize<'de> + PartialEq + std::fmt::Debug,
    {
        let yaml = serde_yaml::to_string(original).expect("YAML serialization failed");
        let roundtripped: T = serde_yaml::from_str(&yaml).expect("YAML deserialization failed");
        assert_eq!(
            original, &roundtripped,
            "YAML roundtrip failed.\nOriginal:   {:?}\nSerialized: {}\nRoundtrip: {:?}",
            original, yaml, roundtripped
        );
    }

    // -----------------------------------------------------------------------
    // SessionResetPolicy roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn test_session_reset_policy_none() {
        let policy = SessionResetPolicy::None;
        assert_json_roundtrip(&policy);
        assert_yaml_roundtrip(&policy);
    }

    #[test]
    fn test_session_reset_policy_daily() {
        let policy = SessionResetPolicy::Daily { at_hour: 3 };
        assert_json_roundtrip(&policy);
        assert_yaml_roundtrip(&policy);
    }

    #[test]
    fn test_session_reset_policy_idle() {
        let policy = SessionResetPolicy::Idle {
            timeout_minutes: 30,
        };
        assert_json_roundtrip(&policy);
        assert_yaml_roundtrip(&policy);
    }

    #[test]
    fn test_session_reset_policy_both() {
        let policy = SessionResetPolicy::Both {
            daily: DailyReset { at_hour: 0 },
            idle: IdleReset {
                timeout_minutes: 60,
            },
        };
        assert_json_roundtrip(&policy);
        assert_yaml_roundtrip(&policy);
    }

    #[test]
    fn test_session_reset_policy_all_variants() {
        let policies = vec![
            SessionResetPolicy::None,
            SessionResetPolicy::Daily { at_hour: 0 },
            SessionResetPolicy::Daily { at_hour: 23 },
            SessionResetPolicy::Idle { timeout_minutes: 1 },
            SessionResetPolicy::Idle {
                timeout_minutes: 1440,
            },
            SessionResetPolicy::Both {
                daily: DailyReset { at_hour: 12 },
                idle: IdleReset {
                    timeout_minutes: 120,
                },
            },
        ];
        for policy in &policies {
            assert_json_roundtrip(policy);
            assert_yaml_roundtrip(policy);
        }
    }

    // -----------------------------------------------------------------------
    // SessionConfig roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn test_session_config_default() {
        let config = SessionConfig::default();
        assert_json_roundtrip(&config);
        assert_yaml_roundtrip(&config);
    }

    #[test]
    fn test_session_config_custom() {
        let mut config = SessionConfig::default();
        config.reset_policy = SessionResetPolicy::Daily { at_hour: 5 };
        config.compression_enabled = true;
        config.max_context_messages = Some(100);
        assert_json_roundtrip(&config);
        assert_yaml_roundtrip(&config);
    }

    // -----------------------------------------------------------------------
    // PlatformConfig roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn test_platform_config_default() {
        let config = PlatformConfig::default();
        assert_json_roundtrip(&config);
        assert_yaml_roundtrip(&config);
    }

    #[test]
    fn test_platform_config_full() {
        let config = PlatformConfig {
            enabled: true,
            token: Some("[REDACTED_SLACK_TOKEN]".to_string()),
            webhook_url: Some("https://hooks.example.com/webhook".to_string()),
            require_mention: Some(true),
            unauthorized_dm_behavior: UnauthorizedDmBehavior::Pair,
            group_sessions_per_user: true,
            home_channel: Some("general".to_string()),
            allowed_users: vec!["user1".to_string()],
            admin_users: vec!["admin1".to_string()],
            extra: {
                let mut m = HashMap::new();
                m.insert("channel".to_string(), serde_json::json!("general"));
                m
            },
        };
        assert_json_roundtrip(&config);
        assert_yaml_roundtrip(&config);
    }

    #[test]
    fn test_platform_config_minimal() {
        let config = PlatformConfig {
            enabled: false,
            token: None,
            webhook_url: None,
            require_mention: None,
            unauthorized_dm_behavior: UnauthorizedDmBehavior::Ignore,
            group_sessions_per_user: false,
            home_channel: None,
            allowed_users: vec![],
            admin_users: vec![],
            extra: HashMap::new(),
        };
        assert_json_roundtrip(&config);
        assert_yaml_roundtrip(&config);
    }

    // -----------------------------------------------------------------------
    // StreamingConfig roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn test_streaming_config_default() {
        let config = StreamingConfig::default();
        assert_json_roundtrip(&config);
        assert_yaml_roundtrip(&config);
    }

    #[test]
    fn test_streaming_config_custom() {
        let config = StreamingConfig {
            enabled: true,
            edit_interval_ms: 500,
            buffer_threshold: 100,
            max_message_length: 8192,
        };
        assert_json_roundtrip(&config);
        assert_yaml_roundtrip(&config);
    }

    // -----------------------------------------------------------------------
    // GatewayConfig roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn test_gateway_config_default() {
        let config = GatewayConfig::default();
        assert_json_roundtrip(&config);
        assert_yaml_roundtrip(&config);
    }

    #[test]
    fn test_gateway_config_with_platforms() {
        let mut config = GatewayConfig::default();
        config.model = Some("gpt-4o".to_string());
        config.personality = Some("helpful".to_string());
        config.max_turns = 25;

        let platform = PlatformConfig {
            enabled: true,
            token: Some("test-token".to_string()),
            webhook_url: None,
            require_mention: Some(false),
            unauthorized_dm_behavior: UnauthorizedDmBehavior::Pair,
            group_sessions_per_user: false,
            home_channel: None,
            allowed_users: vec![],
            admin_users: vec![],
            extra: HashMap::new(),
        };
        config.platforms.insert("discord".to_string(), platform);

        config.session.reset_policy = SessionResetPolicy::Both {
            daily: DailyReset { at_hour: 3 },
            idle: IdleReset {
                timeout_minutes: 60,
            },
        };
        config.streaming.enabled = true;

        assert_json_roundtrip(&config);
        assert_yaml_roundtrip(&config);
    }

    // -----------------------------------------------------------------------
    // SessionType roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn test_session_type_roundtrip() {
        for st in &[SessionType::Dm, SessionType::Group, SessionType::Thread] {
            assert_json_roundtrip(st);
            assert_yaml_roundtrip(st);
        }
    }

    // -----------------------------------------------------------------------
    // UnauthorizedDmBehavior roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn test_unauthorized_dm_behavior_roundtrip() {
        for beh in &[UnauthorizedDmBehavior::Pair, UnauthorizedDmBehavior::Ignore] {
            assert_json_roundtrip(beh);
            assert_yaml_roundtrip(beh);
        }
    }

    // -----------------------------------------------------------------------
    // DailyReset / IdleReset roundtrip
    // -----------------------------------------------------------------------

    #[test]
    fn test_daily_reset_roundtrip() {
        for hour in [0u8, 3, 12, 23] {
            let dr = DailyReset { at_hour: hour };
            assert_json_roundtrip(&dr);
            assert_yaml_roundtrip(&dr);
        }
    }

    #[test]
    fn test_idle_reset_roundtrip() {
        for mins in [1u64, 5, 30, 60, 1440] {
            let ir = IdleReset {
                timeout_minutes: mins,
            };
            assert_json_roundtrip(&ir);
            assert_yaml_roundtrip(&ir);
        }
    }
}
