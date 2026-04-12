//! Session management configuration.

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// SessionType
// ---------------------------------------------------------------------------

/// The kind of session (DM, group, thread).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionType {
    Dm,
    Group,
    Thread,
}

impl fmt::Display for SessionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionType::Dm => write!(f, "dm"),
            SessionType::Group => write!(f, "group"),
            SessionType::Thread => write!(f, "thread"),
        }
    }
}

// ---------------------------------------------------------------------------
// DailyReset / IdleReset
// ---------------------------------------------------------------------------

/// Daily reset at a specific hour (0-23). Invalid values clamp to 0 with a warning.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DailyReset {
    pub at_hour: u8,
}

impl DailyReset {
    /// Validate and return a corrected DailyReset, clamping out-of-range hours to 0.
    pub fn validate(&self) -> Self {
        if self.at_hour > 23 {
            tracing::warn!(
                "DailyReset at_hour={} is out of range 0-23, falling back to 0",
                self.at_hour
            );
            Self { at_hour: 0 }
        } else {
            self.clone()
        }
    }
}

/// Idle timeout reset after N minutes of inactivity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdleReset {
    pub timeout_minutes: u64,
}

// ---------------------------------------------------------------------------
// SessionResetPolicy
// ---------------------------------------------------------------------------

/// Policy for when a session context is reset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SessionResetPolicy {
    /// Reset daily at the given hour (0-23).
    Daily { at_hour: u8 },
    /// Reset after N minutes of idle time.
    Idle { timeout_minutes: u64 },
    /// Both daily and idle reset policies.
    Both { daily: DailyReset, idle: IdleReset },
    /// Never reset the session automatically.
    None,
}

impl SessionResetPolicy {
    /// Validate any contained DailyReset values, clamping as needed.
    pub fn validate(&self) -> Self {
        match self {
            SessionResetPolicy::Daily { at_hour } => {
                let validated = DailyReset { at_hour: *at_hour }.validate();
                SessionResetPolicy::Daily {
                    at_hour: validated.at_hour,
                }
            }
            SessionResetPolicy::Idle { timeout_minutes } => SessionResetPolicy::Idle {
                timeout_minutes: *timeout_minutes,
            },
            SessionResetPolicy::Both { daily, idle } => SessionResetPolicy::Both {
                daily: daily.validate(),
                idle: idle.clone(),
            },
            SessionResetPolicy::None => SessionResetPolicy::None,
        }
    }
}

impl Default for SessionResetPolicy {
    fn default() -> Self {
        SessionResetPolicy::Idle {
            timeout_minutes: 30,
        }
    }
}

// ---------------------------------------------------------------------------
// SessionConfig
// ---------------------------------------------------------------------------

/// Session management configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionConfig {
    /// Policy for when to reset the session context.
    #[serde(default)]
    pub reset_policy: SessionResetPolicy,

    /// Maximum number of context messages to keep (None = unlimited).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_messages: Option<usize>,

    /// Whether to enable context compression.
    #[serde(default)]
    pub compression_enabled: bool,

    /// Per-platform overrides for the reset policy.
    #[serde(default)]
    pub platform_overrides: HashMap<String, SessionResetPolicy>,

    /// Per-session-type overrides for the reset policy.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub session_type_overrides: HashMap<SessionType, SessionResetPolicy>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            reset_policy: SessionResetPolicy::default(),
            max_context_messages: None,
            compression_enabled: false,
            platform_overrides: HashMap::new(),
            session_type_overrides: HashMap::new(),
        }
    }
}

impl SessionConfig {
    /// Round-trip serialize to serde_json::Value.
    pub fn to_value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or(serde_json::Value::Null)
    }

    /// Round-trip deserialize from serde_json::Value.
    pub fn from_value(value: serde_json::Value) -> Result<Self, String> {
        serde_json::from_value(value).map_err(|e| e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn daily_reset_validate_in_range() {
        let dr = DailyReset { at_hour: 12 };
        let validated = dr.validate();
        assert_eq!(validated.at_hour, 12);
    }

    #[test]
    fn daily_reset_validate_out_of_range() {
        let dr = DailyReset { at_hour: 25 };
        let validated = dr.validate();
        assert_eq!(validated.at_hour, 0); // clamped
    }

    #[test]
    fn session_reset_policy_default() {
        let policy = SessionResetPolicy::default();
        match policy {
            SessionResetPolicy::Idle { timeout_minutes } => {
                assert_eq!(timeout_minutes, 30);
            }
            _ => panic!("expected Idle default"),
        }
    }

    #[test]
    fn session_config_roundtrip() {
        let mut cfg = SessionConfig::default();
        cfg.reset_policy = SessionResetPolicy::Daily { at_hour: 3 };
        cfg.max_context_messages = Some(100);
        cfg.compression_enabled = true;

        let value = cfg.to_value();
        let back = SessionConfig::from_value(value).unwrap();
        assert_eq!(back.max_context_messages, Some(100));
        assert!(back.compression_enabled);

        match back.reset_policy {
            SessionResetPolicy::Daily { at_hour } => assert_eq!(at_hour, 3),
            _ => panic!("expected Daily policy"),
        }
    }

    #[test]
    fn session_type_serde() {
        let st = SessionType::Dm;
        let json = serde_json::to_string(&st).unwrap();
        assert_eq!(json, "\"dm\"");
        let back: SessionType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, SessionType::Dm);
    }

    #[test]
    fn both_policy_validate() {
        let policy = SessionResetPolicy::Both {
            daily: DailyReset { at_hour: 99 },
            idle: IdleReset {
                timeout_minutes: 60,
            },
        };
        let validated = policy.validate();
        match validated {
            SessionResetPolicy::Both { daily, idle } => {
                assert_eq!(daily.at_hour, 0); // clamped from 99
                assert_eq!(idle.timeout_minutes, 60);
            }
            _ => panic!("expected Both"),
        }
    }
}
