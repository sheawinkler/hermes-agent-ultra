//! RL training configuration.

use serde::{Deserialize, Serialize};

/// Configuration for an RL training run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingConfig {
    /// Algorithm to use (e.g. "ppo", "dpo", "grpo").
    pub algo: String,
    /// Learning rate.
    pub learning_rate: f64,
    /// Batch size for updates.
    pub batch_size: usize,
    /// Maximum training steps.
    pub max_steps: usize,
    /// Optional reward model identifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reward_model: Option<String>,
}

impl Default for TrainingConfig {
    fn default() -> Self {
        Self {
            algo: "ppo".to_string(),
            learning_rate: 3e-4,
            batch_size: 64,
            max_steps: 1000,
            reward_model: None,
        }
    }
}
