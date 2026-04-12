use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchRunnerConfig {
    pub max_parallel_jobs: usize,
    pub max_turns: usize,
}

impl Default for BatchRunnerConfig {
    fn default() -> Self {
        Self {
            max_parallel_jobs: 4,
            max_turns: 32,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchTrajectory {
    pub id: String,
    pub prompt: String,
    pub response: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone)]
pub struct BatchRunner {
    config: BatchRunnerConfig,
}

impl BatchRunner {
    pub fn new(config: BatchRunnerConfig) -> Self {
        Self { config }
    }

    pub fn config(&self) -> &BatchRunnerConfig {
        &self.config
    }

    /// Baseline offline generator used by RL pipelines.
    pub fn generate_stub(&self, prompts: &[String]) -> Vec<BatchTrajectory> {
        prompts
            .iter()
            .enumerate()
            .map(|(idx, prompt)| BatchTrajectory {
                id: format!("traj-{}", idx + 1),
                prompt: prompt.clone(),
                response: format!(
                    "Stub trajectory for prompt '{}' (max_turns={})",
                    prompt, self.config.max_turns
                ),
                created_at: Utc::now(),
            })
            .collect()
    }
}
