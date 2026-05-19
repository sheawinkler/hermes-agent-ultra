//! Trajectory compressor — reduce trajectory size for storage and training.
//!
//! Strategies:
//! - **Truncate**: Remove steps beyond a max count
//! - **Summarize**: Replace long tool outputs with summaries
//! - **Deduplicate**: Merge consecutive identical actions
//! - **Filter**: Remove low-information steps (empty observations, no-op actions)
//! - **Token budget**: Compress to fit within a token budget

use serde::{Deserialize, Serialize};

use hermes_environments::training::{Trajectory, TrajectoryStep};

// ---------------------------------------------------------------------------
// Compression config
// ---------------------------------------------------------------------------

/// Compression strategy.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CompressionStrategy {
    /// Keep only the last N steps.
    Truncate,
    /// Summarize long tool outputs.
    Summarize,
    /// Remove consecutive duplicate actions.
    Deduplicate,
    /// Remove low-information steps.
    Filter,
    /// Apply all strategies in sequence.
    All,
}

/// Configuration for trajectory compression.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionConfig {
    /// Maximum number of steps to keep.
    pub max_steps: Option<usize>,
    /// Maximum characters per tool output.
    pub max_output_chars: usize,
    /// Maximum characters per observation.
    pub max_observation_chars: usize,
    /// Whether to remove steps with empty observations and no tool calls.
    pub remove_empty: bool,
    /// Strategy to apply.
    pub strategy: CompressionStrategy,
}

impl Default for CompressionConfig {
    fn default() -> Self {
        Self {
            max_steps: None,
            max_output_chars: 2000,
            max_observation_chars: 4000,
            remove_empty: true,
            strategy: CompressionStrategy::All,
        }
    }
}

// ---------------------------------------------------------------------------
// Compression result
// ---------------------------------------------------------------------------

/// Statistics from compression.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressionStats {
    pub original_steps: usize,
    pub compressed_steps: usize,
    pub original_chars: usize,
    pub compressed_chars: usize,
    pub compression_ratio: f64,
}

// ---------------------------------------------------------------------------
// Compressor
// ---------------------------------------------------------------------------

/// Compress a trajectory according to the given config.
pub fn compress_trajectory(
    trajectory: &Trajectory,
    config: &CompressionConfig,
) -> (Trajectory, CompressionStats) {
    let original_steps = trajectory.steps.len();
    let original_chars = estimate_chars(&trajectory.steps);

    let mut steps = trajectory.steps.clone();

    match config.strategy {
        CompressionStrategy::Truncate => {
            steps = truncate_steps(steps, config);
        }
        CompressionStrategy::Summarize => {
            steps = summarize_outputs(steps, config);
        }
        CompressionStrategy::Deduplicate => {
            steps = deduplicate_steps(steps);
        }
        CompressionStrategy::Filter => {
            steps = filter_empty_steps(steps, config);
        }
        CompressionStrategy::All => {
            steps = filter_empty_steps(steps, config);
            steps = deduplicate_steps(steps);
            steps = summarize_outputs(steps, config);
            steps = truncate_steps(steps, config);
        }
    }

    // Re-index steps
    for (i, step) in steps.iter_mut().enumerate() {
        step.step = i;
    }

    let compressed_chars = estimate_chars(&steps);
    let compression_ratio = if original_chars > 0 {
        1.0 - (compressed_chars as f64 / original_chars as f64)
    } else {
        0.0
    };

    let mut compressed = trajectory.clone();
    compressed.steps = steps;

    let stats = CompressionStats {
        original_steps,
        compressed_steps: compressed.steps.len(),
        original_chars,
        compressed_chars,
        compression_ratio,
    };

    (compressed, stats)
}

/// Batch compress multiple trajectories.
pub fn compress_batch(
    trajectories: &[Trajectory],
    config: &CompressionConfig,
) -> Vec<(Trajectory, CompressionStats)> {
    trajectories
        .iter()
        .map(|t| compress_trajectory(t, config))
        .collect()
}

// ---------------------------------------------------------------------------
// Strategy implementations
// ---------------------------------------------------------------------------

fn truncate_steps(steps: Vec<TrajectoryStep>, config: &CompressionConfig) -> Vec<TrajectoryStep> {
    match config.max_steps {
        Some(max) if steps.len() > max => {
            // Keep first step (context) and last N-1 steps
            let mut result = Vec::with_capacity(max);
            if max > 0 {
                result.push(steps[0].clone());
            }
            if max > 1 {
                let skip = steps.len() - (max - 1);
                result.extend_from_slice(&steps[skip..]);
            }
            result
        }
        _ => steps,
    }
}

fn summarize_outputs(
    mut steps: Vec<TrajectoryStep>,
    config: &CompressionConfig,
) -> Vec<TrajectoryStep> {
    for step in &mut steps {
        // Truncate tool results
        if let Some(ref mut result) = step.tool_result {
            if result.len() > config.max_output_chars {
                let truncated = &result[..config.max_output_chars];
                let last_newline = truncated.rfind('\n').unwrap_or(config.max_output_chars);
                *result = format!(
                    "{}...\n[truncated: {} → {} chars]",
                    &result[..last_newline],
                    result.len(),
                    last_newline
                );
            }
        }

        // Truncate observations
        if step.observation.len() > config.max_observation_chars {
            let truncated = &step.observation[..config.max_observation_chars];
            let last_newline = truncated
                .rfind('\n')
                .unwrap_or(config.max_observation_chars);
            step.observation = format!(
                "{}...\n[truncated: {} → {} chars]",
                &step.observation[..last_newline],
                step.observation.len(),
                last_newline
            );
        }
    }
    steps
}

fn deduplicate_steps(steps: Vec<TrajectoryStep>) -> Vec<TrajectoryStep> {
    if steps.len() < 2 {
        return steps;
    }

    let mut result = Vec::with_capacity(steps.len());
    result.push(steps[0].clone());

    for step in steps.into_iter().skip(1) {
        let last = result.last().unwrap();
        // Skip if same action and same tool
        if step.action == last.action && step.tool_name == last.tool_name {
            continue;
        }
        result.push(step);
    }

    result
}

fn filter_empty_steps(
    steps: Vec<TrajectoryStep>,
    config: &CompressionConfig,
) -> Vec<TrajectoryStep> {
    if !config.remove_empty {
        return steps;
    }

    steps
        .into_iter()
        .filter(|step| {
            // Keep steps that have meaningful content
            !step.observation.trim().is_empty()
                || !step.action.trim().is_empty()
                || step.tool_name.is_some()
                || step.done
                || step.reward != 0.0
        })
        .collect()
}

fn estimate_chars(steps: &[TrajectoryStep]) -> usize {
    steps
        .iter()
        .map(|s| {
            s.observation.len()
                + s.action.len()
                + s.tool_result.as_ref().map(|r| r.len()).unwrap_or(0)
        })
        .sum()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_step(i: usize, action: &str, output: &str) -> TrajectoryStep {
        TrajectoryStep {
            step: i,
            observation: format!("obs-{i}"),
            action: action.to_string(),
            tool_name: Some("terminal".into()),
            tool_params: None,
            tool_result: Some(output.to_string()),
            reward: 0.0,
            done: false,
            timestamp: chrono::Utc::now(),
            tokens_input: 10,
            tokens_output: 5,
        }
    }

    fn make_trajectory(n: usize) -> Trajectory {
        let mut traj = Trajectory::new("test", "model");
        for i in 0..n {
            traj.add_step(make_step(i, &format!("action-{i}"), &format!("output-{i}")));
        }
        traj
    }

    #[test]
    fn truncate_keeps_first_and_last() {
        let traj = make_trajectory(10);
        let config = CompressionConfig {
            max_steps: Some(5),
            strategy: CompressionStrategy::Truncate,
            ..Default::default()
        };
        let (compressed, stats) = compress_trajectory(&traj, &config);
        assert_eq!(compressed.steps.len(), 5);
        assert_eq!(stats.original_steps, 10);
        assert_eq!(stats.compressed_steps, 5);
    }

    #[test]
    fn summarize_truncates_long_output() {
        let mut traj = Trajectory::new("test", "model");
        traj.add_step(TrajectoryStep {
            step: 0,
            observation: "x".repeat(100),
            action: "cmd".into(),
            tool_name: Some("terminal".into()),
            tool_params: None,
            tool_result: Some("y".repeat(5000)),
            reward: 0.0,
            done: false,
            timestamp: chrono::Utc::now(),
            tokens_input: 10,
            tokens_output: 5,
        });

        let config = CompressionConfig {
            max_output_chars: 200,
            strategy: CompressionStrategy::Summarize,
            ..Default::default()
        };
        let (compressed, _) = compress_trajectory(&traj, &config);
        let result = compressed.steps[0].tool_result.as_ref().unwrap();
        assert!(result.len() < 5000);
        assert!(result.contains("truncated"));
    }

    #[test]
    fn deduplicate_removes_consecutive_same_actions() {
        let mut traj = Trajectory::new("test", "model");
        traj.add_step(make_step(0, "ls", "files"));
        traj.add_step(make_step(1, "ls", "files")); // duplicate
        traj.add_step(make_step(2, "cat file.txt", "content"));
        traj.add_step(make_step(3, "cat file.txt", "content")); // duplicate

        let config = CompressionConfig {
            strategy: CompressionStrategy::Deduplicate,
            ..Default::default()
        };
        let (compressed, _) = compress_trajectory(&traj, &config);
        assert_eq!(compressed.steps.len(), 2);
    }

    #[test]
    fn filter_removes_empty_steps() {
        let mut traj = Trajectory::new("test", "model");
        traj.add_step(make_step(0, "action", "output"));
        traj.add_step(TrajectoryStep {
            step: 1,
            observation: "".into(),
            action: "".into(),
            tool_name: None,
            tool_params: None,
            tool_result: None,
            reward: 0.0,
            done: false,
            timestamp: chrono::Utc::now(),
            tokens_input: 0,
            tokens_output: 0,
        });
        traj.add_step(make_step(2, "action2", "output2"));

        let config = CompressionConfig {
            strategy: CompressionStrategy::Filter,
            remove_empty: true,
            ..Default::default()
        };
        let (compressed, _) = compress_trajectory(&traj, &config);
        assert_eq!(compressed.steps.len(), 2);
    }

    #[test]
    fn all_strategy_combines() {
        let traj = make_trajectory(20);
        let config = CompressionConfig {
            max_steps: Some(10),
            strategy: CompressionStrategy::All,
            ..Default::default()
        };
        let (compressed, stats) = compress_trajectory(&traj, &config);
        assert!(compressed.steps.len() <= 10);
        assert!(stats.compression_ratio >= 0.0);
    }

    #[test]
    fn batch_compress() {
        let trajs = vec![make_trajectory(5), make_trajectory(10)];
        let config = CompressionConfig::default();
        let results = compress_batch(&trajs, &config);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn empty_trajectory() {
        let traj = Trajectory::new("test", "model");
        let config = CompressionConfig::default();
        let (compressed, stats) = compress_trajectory(&traj, &config);
        assert_eq!(compressed.steps.len(), 0);
        assert_eq!(stats.compression_ratio, 0.0);
    }
}
