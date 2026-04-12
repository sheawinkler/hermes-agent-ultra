//! RL environment definitions.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// An available RL environment.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RlEnvironment {
    pub name: String,
    /// "tinker", "atropos", or "custom"
    pub env_type: String,
    pub description: String,
    pub config_schema: Value,
}

impl RlEnvironment {
    /// Return the built-in environment catalogue.
    pub fn builtin_environments() -> Vec<RlEnvironment> {
        vec![
            RlEnvironment {
                name: "tinker".to_string(),
                env_type: "tinker".to_string(),
                description: "Tinker environment for lightweight RL experimentation with tool-use trajectories.".to_string(),
                config_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "max_turns": { "type": "integer", "default": 32 },
                        "reward_type": { "type": "string", "enum": ["binary", "scalar"] }
                    }
                }),
            },
            RlEnvironment {
                name: "atropos".to_string(),
                env_type: "atropos".to_string(),
                description: "Atropos environment for multi-step reasoning and chain-of-thought RL.".to_string(),
                config_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "max_turns": { "type": "integer", "default": 64 },
                        "chain_of_thought": { "type": "boolean", "default": true }
                    }
                }),
            },
            RlEnvironment {
                name: "custom".to_string(),
                env_type: "custom".to_string(),
                description: "Custom environment defined by a user-provided reward function.".to_string(),
                config_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "reward_script": { "type": "string" },
                        "max_turns": { "type": "integer", "default": 32 }
                    }
                }),
            },
        ]
    }
}
