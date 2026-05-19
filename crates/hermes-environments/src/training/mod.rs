//! Training / benchmark loops (PARITY_PLAN Week 3).
//!
//! Trajectory types support [`hermes_eval::trajectory_compressor`]; full env
//! implementations land incrementally per `PARITY_PLAN.md`.

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Trajectory recording (rs parity)
// ---------------------------------------------------------------------------

/// A single step in a trajectory.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrajectoryStep {
    pub step: usize,
    pub observation: String,
    pub action: String,
    pub tool_name: Option<String>,
    pub tool_params: Option<serde_json::Value>,
    pub tool_result: Option<String>,
    pub reward: f64,
    pub done: bool,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub tokens_input: u64,
    pub tokens_output: u64,
}

/// A complete trajectory for one episode.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trajectory {
    pub task_id: String,
    pub model: String,
    pub steps: Vec<TrajectoryStep>,
    pub total_reward: f64,
    pub success: bool,
    pub duration: Duration,
    pub total_tokens_input: u64,
    pub total_tokens_output: u64,
    pub total_cost_usd: f64,
    pub metadata: HashMap<String, serde_json::Value>,
}

impl Trajectory {
    pub fn new(task_id: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            task_id: task_id.into(),
            model: model.into(),
            steps: Vec::new(),
            total_reward: 0.0,
            success: false,
            duration: Duration::ZERO,
            total_tokens_input: 0,
            total_tokens_output: 0,
            total_cost_usd: 0.0,
            metadata: HashMap::new(),
        }
    }

    pub fn add_step(&mut self, step: TrajectoryStep) {
        self.total_tokens_input += step.tokens_input;
        self.total_tokens_output += step.tokens_output;
        self.total_reward += step.reward;
        if step.done {
            self.success = step.reward > 0.0;
        }
        self.steps.push(step);
    }

    pub fn num_steps(&self) -> usize {
        self.steps.len()
    }
}

// ---------------------------------------------------------------------------
// Environment traits (skeleton)
// ---------------------------------------------------------------------------

/// Placeholder for a training or benchmark episode (one task + trajectory).
pub trait HermesEpisode: Send {
    fn task_id(&self) -> &str;
}

/// Long-term: environment that loads tasks, runs the agent loop, emits trajectories.
pub trait HermesBaseEnv: Send + Sync {
    fn dataset_id(&self) -> &str;
}
