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
    ///
    /// Generates deterministic trajectories with lightweight heuristic responses
    /// so local training/eval workflows can run without requiring a model
    /// checkpoint service.
    pub fn generate_batch(&self, prompts: &[String]) -> Vec<BatchTrajectory> {
        prompts
            .iter()
            .enumerate()
            .map(|(idx, prompt)| BatchTrajectory {
                id: format!("traj-{}", idx + 1),
                prompt: prompt.clone(),
                response: build_baseline_response(prompt, self.config.max_turns),
                created_at: Utc::now(),
            })
            .collect()
    }

    /// Backward-compatible alias.
    pub fn generate_stub(&self, prompts: &[String]) -> Vec<BatchTrajectory> {
        self.generate_batch(prompts)
    }
}

fn build_baseline_response(prompt: &str, max_turns: usize) -> String {
    let lower = prompt.to_lowercase();
    let style = if lower.contains("bug") || lower.contains("fix") {
        "diagnostic"
    } else if lower.contains("plan") || lower.contains("strategy") {
        "planning"
    } else if lower.contains("test") || lower.contains("verify") {
        "verification"
    } else {
        "general"
    };
    format!(
        "[baseline-{style}] steps_budget={max_turns}; response: {}",
        prompt.chars().take(180).collect::<String>()
    )
}
