//! Training status and metrics tracking.

use serde::{Deserialize, Serialize};

/// Status of a training run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TrainingStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Stopped,
}

impl std::fmt::Display for TrainingStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Pending => write!(f, "pending"),
            Self::Running => write!(f, "running"),
            Self::Completed => write!(f, "completed"),
            Self::Failed => write!(f, "failed"),
            Self::Stopped => write!(f, "stopped"),
        }
    }
}

/// Metrics collected during a training run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingMetrics {
    pub current_step: usize,
    pub total_steps: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loss: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reward_mean: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reward_std: Option<f64>,
}

impl Default for TrainingMetrics {
    fn default() -> Self {
        Self {
            current_step: 0,
            total_steps: 0,
            loss: None,
            reward_mean: None,
            reward_std: None,
        }
    }
}
