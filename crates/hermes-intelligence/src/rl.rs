//! RL Training Integration module.
//!
//! Provides types and utilities for reinforcement-learning-based agent training,
//! including trajectory recording, compression, batch generation, and an
//! RL toolset for lightweight local orchestration.

use chrono::{DateTime, Utc};
use hermes_core::{Message, ToolCall};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// TrajectoryOutcome
// ---------------------------------------------------------------------------

/// Outcome of a trajectory execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrajectoryOutcome {
    Success,
    Failed,
    Timeout,
}

// ---------------------------------------------------------------------------
// Trajectory
// ---------------------------------------------------------------------------

/// A recorded trajectory of an agent interaction, capturing the full
/// conversation, tool calls, outcome, and optional reward signal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trajectory {
    pub id: String,
    pub prompt: String,
    pub messages: Vec<Message>,
    pub tool_calls: Vec<ToolCall>,
    pub outcome: TrajectoryOutcome,
    pub reward: Option<f64>,
    pub timestamp: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// TrajectoryCompressor
// ---------------------------------------------------------------------------

/// Compresses a trajectory by removing redundant messages, keeping only key
/// decision points: the first message, the last message, and any messages
/// that contain tool calls.
#[derive(Debug, Clone, Default)]
pub struct TrajectoryCompressor;

impl TrajectoryCompressor {
    /// Create a new compressor.
    pub fn new() -> Self {
        Self
    }

    /// Compress the trajectory, returning a new trajectory with only key
    /// decision-point messages retained.
    ///
    /// Rules for retention:
    /// - Always keep the first message.
    /// - Always keep the last message (if different from the first).
    /// - Keep any message whose `tool_calls` field is `Some` and non-empty.
    /// - All other messages are discarded.
    pub fn compress(&self, trajectory: &Trajectory) -> Trajectory {
        if trajectory.messages.len() <= 2 {
            return trajectory.clone();
        }

        let mut compressed_messages = Vec::new();
        let last_idx = trajectory.messages.len() - 1;

        for (i, msg) in trajectory.messages.iter().enumerate() {
            let is_first = i == 0;
            let is_last = i == last_idx;
            let has_tool_calls = msg.tool_calls.as_ref().is_some_and(|tc| !tc.is_empty());

            if is_first || is_last || has_tool_calls {
                compressed_messages.push(msg.clone());
            }
        }

        // Deduplicate in case the first/last message also has tool calls
        // (they're already unique by index, so no dedup needed)

        Trajectory {
            id: trajectory.id.clone(),
            prompt: trajectory.prompt.clone(),
            messages: compressed_messages,
            tool_calls: trajectory.tool_calls.clone(),
            outcome: trajectory.outcome,
            reward: trajectory.reward,
            timestamp: trajectory.timestamp,
        }
    }
}

// ---------------------------------------------------------------------------
// BatchConfig
// ---------------------------------------------------------------------------

/// Configuration for batch trajectory generation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchConfig {
    pub max_trajectories: usize,
    pub max_turns_per_trajectory: usize,
    pub model: String,
    pub temperature: f64,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_trajectories: 32,
            max_turns_per_trajectory: 10,
            model: "gpt-4o".to_string(),
            temperature: 0.7,
        }
    }
}

// ---------------------------------------------------------------------------
// BatchGenerator
// ---------------------------------------------------------------------------

/// Generates batches of trajectories from a list of prompts.
///
/// Without a wired LLM, produces deterministic baseline single-turn trajectories
/// (user + heuristic assistant) so pipelines can exercise serde, compression,
/// and storage with stable outputs.
#[derive(Debug, Clone, Default)]
pub struct BatchGenerator;

impl BatchGenerator {
    /// Create a new batch generator.
    pub fn new() -> Self {
        Self
    }

    /// Generate a batch of trajectories for the given prompts.
    ///
    /// Caps output at `config.max_trajectories`. Each trajectory is a minimal
    /// two-message turn unless `prompts` is empty.
    pub fn generate_batch(&self, prompts: Vec<String>, config: &BatchConfig) -> Vec<Trajectory> {
        let cap = config.max_trajectories.max(1);
        prompts
            .into_iter()
            .take(cap)
            .map(|prompt| {
                let id = format!("traj-{}", Uuid::new_v4());
                let now = Utc::now();
                Trajectory {
                    id,
                    prompt: prompt.clone(),
                    messages: vec![
                        Message::user(prompt),
                        Message::assistant(Self::baseline_response(&config.model)),
                    ],
                    tool_calls: vec![],
                    outcome: TrajectoryOutcome::Success,
                    reward: None,
                    timestamp: now,
                }
            })
            .collect()
    }

    fn baseline_response(model: &str) -> String {
        format!(
            "Baseline rollout generated (model={}, strategy=deterministic-heuristic).",
            model
        )
    }
}

// ---------------------------------------------------------------------------
// Runtime RL training surface
// ---------------------------------------------------------------------------

/// Runtime status for a training run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TrainingStatus {
    Pending,
    Running,
    Completed,
    Failed,
    Stopped,
}

/// User-editable training configuration for the Rust RL control surface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrainingConfig {
    pub algo: String,
    pub learning_rate: f64,
    pub batch_size: usize,
    pub max_steps: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reward_model: Option<String>,
}

impl Default for TrainingConfig {
    fn default() -> Self {
        Self {
            algo: "ppo".to_string(),
            learning_rate: 1e-5,
            batch_size: 32,
            max_steps: 1_000,
            reward_model: None,
        }
    }
}

/// Progress metrics reported by the local RL run manager.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrainingMetrics {
    pub total_steps: usize,
    pub current_step: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reward_mean: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reward_std: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loss: Option<f64>,
}

/// A supported RL environment descriptor exposed by `rl_list_environments`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RlEnvironment {
    pub name: String,
    pub description: String,
    pub config_schema: serde_json::Value,
}

impl RlEnvironment {
    pub fn builtin_environments() -> Vec<Self> {
        vec![
            Self {
                name: "tinker".to_string(),
                description: "Local Tinker-style text and tool-use rollouts".to_string(),
                config_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "algo": {"type": "string", "enum": ["ppo", "dpo", "grpo"]},
                        "learning_rate": {"type": "number"},
                        "batch_size": {"type": "integer", "minimum": 1},
                        "max_steps": {"type": "integer", "minimum": 1}
                    }
                }),
            },
            Self {
                name: "atropos".to_string(),
                description: "Atropos-compatible agentic environment metadata".to_string(),
                config_schema: serde_json::json!({
                    "type": "object",
                    "properties": {
                        "task_family": {"type": "string"},
                        "reward_model": {"type": "string"},
                        "max_steps": {"type": "integer", "minimum": 1}
                    }
                }),
            },
            Self {
                name: "custom".to_string(),
                description: "Custom local RL environment configured by the caller".to_string(),
                config_schema: serde_json::json!({"type": "object"}),
            },
        ]
    }
}

/// A single tracked training run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrainingRun {
    pub id: String,
    pub environment: String,
    pub status: TrainingStatus,
    pub config: TrainingConfig,
    pub metrics: TrainingMetrics,
    pub started_at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finished_at: Option<DateTime<Utc>>,
}

/// In-process run manager used by the Rust `rl_*` tools.
pub struct RunManager {
    pub data_dir: PathBuf,
    runs: HashMap<String, TrainingRun>,
}

impl RunManager {
    pub fn new(data_dir: PathBuf) -> Self {
        Self {
            data_dir,
            runs: HashMap::new(),
        }
    }

    pub fn create_run(&mut self, environment: &str, config: TrainingConfig) -> String {
        let id = generate_run_id();
        let run = TrainingRun {
            id: id.clone(),
            environment: environment.to_string(),
            status: TrainingStatus::Pending,
            metrics: TrainingMetrics {
                total_steps: config.max_steps,
                ..TrainingMetrics::default()
            },
            config,
            started_at: Utc::now(),
            finished_at: None,
        };
        self.runs.insert(id.clone(), run);
        id
    }

    pub fn get_run(&self, id: &str) -> Option<&TrainingRun> {
        self.runs.get(id)
    }

    pub fn get_run_mut(&mut self, id: &str) -> Option<&mut TrainingRun> {
        self.runs.get_mut(id)
    }

    pub fn list_runs(&self) -> Vec<&TrainingRun> {
        let mut runs: Vec<_> = self.runs.values().collect();
        runs.sort_by(|a, b| {
            b.started_at
                .cmp(&a.started_at)
                .then_with(|| b.id.cmp(&a.id))
        });
        runs
    }

    pub fn set_status(&mut self, id: &str, status: TrainingStatus) -> bool {
        if let Some(run) = self.runs.get_mut(id) {
            if matches!(
                status,
                TrainingStatus::Completed | TrainingStatus::Failed | TrainingStatus::Stopped
            ) {
                run.finished_at = Some(Utc::now());
            }
            run.status = status;
            true
        } else {
            false
        }
    }

    pub fn update_metrics(&mut self, id: &str, metrics: TrainingMetrics) -> bool {
        if let Some(run) = self.runs.get_mut(id) {
            run.metrics = metrics;
            true
        } else {
            false
        }
    }
}

fn generate_run_id() -> String {
    static RUN_ID_COUNTER: AtomicU64 = AtomicU64::new(1);
    let seq = RUN_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("run-{}-{seq:04}", Utc::now().timestamp_millis())
}

/// Configuration for deterministic batch inference smoke paths.
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

/// Minimal trajectory emitted by the offline batch runner.
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

    /// Generate deterministic local trajectories without model credentials.
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

// ---------------------------------------------------------------------------
// RlToolset
// ---------------------------------------------------------------------------

/// High-level RL training toolset: environment listing, lightweight run
/// lifecycle management, and deterministic metric progression.
#[derive(Debug, Clone, Default)]
pub struct RlToolset;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RlSessionStatus {
    Running,
    Stopped,
    Completed,
}

impl RlSessionStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Completed => "completed",
        }
    }
}

#[derive(Debug, Clone)]
struct RlSessionState {
    environment: String,
    started_at: DateTime<Utc>,
    status: RlSessionStatus,
    total_episodes_target: u64,
}

static RL_ENV_CONFIGS: OnceLock<Mutex<HashMap<String, serde_json::Value>>> = OnceLock::new();
static RL_SESSIONS: OnceLock<Mutex<HashMap<String, RlSessionState>>> = OnceLock::new();

fn env_configs() -> &'static Mutex<HashMap<String, serde_json::Value>> {
    RL_ENV_CONFIGS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn sessions() -> &'static Mutex<HashMap<String, RlSessionState>> {
    RL_SESSIONS.get_or_init(|| Mutex::new(HashMap::new()))
}

impl RlToolset {
    /// Create a new RL toolset.
    pub fn new() -> Self {
        Self
    }

    /// List available training environments.
    ///
    /// Returns the built-in environment set supported by the lightweight
    /// local RL orchestration path.
    pub fn list_environments(&self) -> Vec<String> {
        vec![
            "code-generation".to_string(),
            "tool-use".to_string(),
            "multi-step-reasoning".to_string(),
        ]
    }

    /// Configure a training environment.
    pub fn configure_environment(&self, environment: &str, config: &serde_json::Value) -> String {
        let mut lock = env_configs().lock().expect("rl env config lock poisoned");
        lock.insert(environment.to_string(), config.clone());
        "configured".to_string()
    }

    /// Start a training run.
    ///
    /// Returns a unique session id (orchestration should persist and map it).
    pub fn start_training(&self, environment: &str) -> String {
        let session_id = format!("rl-session-{}", Utc::now().timestamp_millis());
        let mut lock = sessions().lock().expect("rl sessions lock poisoned");
        lock.insert(
            session_id.clone(),
            RlSessionState {
                environment: environment.to_string(),
                started_at: Utc::now(),
                status: RlSessionStatus::Running,
                total_episodes_target: 120,
            },
        );
        session_id
    }

    /// Stop a running training session.
    ///
    pub fn stop_training(&self, session_id: &str) -> String {
        let mut lock = sessions().lock().expect("rl sessions lock poisoned");
        if let Some(state) = lock.get_mut(session_id) {
            state.status = RlSessionStatus::Stopped;
            "stopped".to_string()
        } else {
            "session_not_found".to_string()
        }
    }

    /// Get results from a completed (or running) training session.
    ///
    pub fn get_results(&self, session_id: &str) -> serde_json::Value {
        let mut lock = sessions().lock().expect("rl sessions lock poisoned");
        let Some(state) = lock.get_mut(session_id) else {
            return serde_json::json!({
                "status": "unknown_session",
                "session_id": session_id,
                "metrics": {
                    "reward_mean": 0.0,
                    "reward_std": 0.0,
                    "episodes": 0,
                }
            });
        };

        let elapsed_secs = (Utc::now() - state.started_at).num_seconds().max(0) as u64;
        let mut episodes = (elapsed_secs.saturating_mul(6)).min(state.total_episodes_target);
        if state.status == RlSessionStatus::Stopped {
            episodes = episodes.min(state.total_episodes_target.saturating_sub(1));
        }
        if state.status == RlSessionStatus::Running && episodes >= state.total_episodes_target {
            state.status = RlSessionStatus::Completed;
        }

        let progress = (episodes as f64 / state.total_episodes_target as f64).clamp(0.0, 1.0);
        let reward_mean = (progress * 1.7 - 0.3).clamp(-1.0, 1.0);
        let reward_std = (1.0 - progress).clamp(0.03, 0.8);

        serde_json::json!({
            "status": state.status.as_str(),
            "session_id": session_id,
            "environment": state.environment,
            "metrics": {
                "reward_mean": reward_mean,
                "reward_std": reward_std,
                "episodes": episodes,
                "episodes_target": state.total_episodes_target,
            }
        })
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use hermes_core::MessageRole;

    fn make_test_trajectory() -> Trajectory {
        let now = Utc::now();
        Trajectory {
            id: "test-001".to_string(),
            prompt: "Write a hello world program".to_string(),
            messages: vec![
                Message::system("You are a helpful assistant"),
                Message {
                    role: MessageRole::User,
                    content: Some("Write a hello world program".to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                    reasoning_content: None,
                    cache_control: None,
                },
                Message {
                    role: MessageRole::Assistant,
                    content: Some("Let me use a tool".to_string()),
                    tool_calls: Some(vec![ToolCall {
                        id: "tc-1".to_string(),
                        function: hermes_core::FunctionCall {
                            name: "run_code".to_string(),
                            arguments: "{}".to_string(),
                        },
                        extra_content: None,
                    }]),
                    tool_call_id: None,
                    name: None,
                    reasoning_content: None,
                    cache_control: None,
                },
                Message {
                    role: MessageRole::Tool,
                    content: Some("output: Hello, World!".to_string()),
                    tool_calls: None,
                    tool_call_id: Some("tc-1".to_string()),
                    name: None,
                    reasoning_content: None,
                    cache_control: None,
                },
                Message {
                    role: MessageRole::Assistant,
                    content: Some("Here is your program".to_string()),
                    tool_calls: None,
                    tool_call_id: None,
                    name: None,
                    reasoning_content: None,
                    cache_control: None,
                },
            ],
            tool_calls: vec![ToolCall {
                id: "tc-1".to_string(),
                function: hermes_core::FunctionCall {
                    name: "run_code".to_string(),
                    arguments: "{}".to_string(),
                },
                extra_content: None,
            }],
            outcome: TrajectoryOutcome::Success,
            reward: Some(1.0),
            timestamp: now,
        }
    }

    #[test]
    fn test_trajectory_outcome_serde() {
        let outcome = TrajectoryOutcome::Success;
        let json = serde_json::to_string(&outcome).unwrap();
        assert_eq!(json, "\"success\"");
        let de: TrajectoryOutcome = serde_json::from_str(&json).unwrap();
        assert_eq!(de, outcome);
    }

    #[test]
    fn test_trajectory_serde_roundtrip() {
        let t = make_test_trajectory();
        let json = serde_json::to_string(&t).unwrap();
        let de: Trajectory = serde_json::from_str(&json).unwrap();
        assert_eq!(de.id, t.id);
        assert_eq!(de.prompt, t.prompt);
        assert_eq!(de.outcome, t.outcome);
        assert_eq!(de.reward, t.reward);
    }

    #[test]
    fn test_compressor_keeps_first_last_and_tool_call_messages() {
        let t = make_test_trajectory();
        // 5 messages: system, user, assistant (tool_calls), tool, assistant
        assert_eq!(t.messages.len(), 5);

        let compressed = TrajectoryCompressor::new().compress(&t);
        // Should keep: index 0 (first), 2 (has tool_calls), 4 (last) = 3 messages
        assert_eq!(compressed.messages.len(), 3);
        assert_eq!(compressed.messages[0].role, MessageRole::System);
        // Index 2 had tool calls
        assert!(compressed.messages[1].tool_calls.is_some());
        assert_eq!(compressed.messages[2].role, MessageRole::Assistant);
    }

    #[test]
    fn test_compressor_short_trajectory_unchanged() {
        let t = Trajectory {
            id: "short".to_string(),
            prompt: "hi".to_string(),
            messages: vec![Message::system("hello")],
            tool_calls: vec![],
            outcome: TrajectoryOutcome::Timeout,
            reward: None,
            timestamp: Utc::now(),
        };
        let compressed = TrajectoryCompressor::new().compress(&t);
        assert_eq!(compressed.messages.len(), 1);
    }

    #[test]
    fn test_batch_generator_trajectories() {
        let gen = BatchGenerator::new();
        let config = BatchConfig::default();
        let result = gen.generate_batch(vec!["prompt1".to_string()], &config);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].messages.len(), 2);
        assert_eq!(result[0].outcome, TrajectoryOutcome::Success);
        assert!(result[0]
            .messages
            .get(1)
            .and_then(|m| m.content.as_deref())
            .unwrap_or("")
            .contains("Baseline rollout generated"));
    }

    #[test]
    fn test_rl_toolset_lifecycle() {
        let ts = RlToolset::new();
        assert!(!ts.list_environments().is_empty());
        assert_eq!(
            ts.configure_environment("test", &serde_json::Value::Null),
            "configured"
        );
        let id = ts.start_training("test");
        assert!(id.starts_with("rl-session-"));
        let running = ts.get_results(&id);
        assert_eq!(running["status"], "running");
        assert_eq!(ts.stop_training(&id), "stopped");
        let stopped = ts.get_results(&id);
        assert_eq!(stopped["status"], "stopped");
    }

    #[test]
    fn test_batch_config_default() {
        let config = BatchConfig::default();
        assert_eq!(config.max_trajectories, 32);
        assert_eq!(config.max_turns_per_trajectory, 10);
        assert_eq!(config.model, "gpt-4o");
        assert!((config.temperature - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn training_status_serde_uses_snake_case() {
        let json = serde_json::to_string(&TrainingStatus::Running).unwrap();
        assert_eq!(json, "\"running\"");
        let parsed: TrainingStatus = serde_json::from_str("\"stopped\"").unwrap();
        assert_eq!(parsed, TrainingStatus::Stopped);
    }

    #[test]
    fn run_manager_tracks_status_metrics_and_sorted_runs() {
        let mut manager = RunManager::new(PathBuf::from("/tmp/hermes-rl-test"));
        let config = TrainingConfig {
            max_steps: 42,
            ..TrainingConfig::default()
        };
        let run_id = manager.create_run("tinker", config.clone());

        let run = manager.get_run(&run_id).unwrap();
        assert_eq!(run.environment, "tinker");
        assert_eq!(run.status, TrainingStatus::Pending);
        assert_eq!(run.metrics.total_steps, 42);
        assert_eq!(run.config, config);

        assert!(manager.set_status(&run_id, TrainingStatus::Running));
        assert!(manager.update_metrics(
            &run_id,
            TrainingMetrics {
                total_steps: 42,
                current_step: 7,
                reward_mean: Some(0.25),
                reward_std: Some(0.5),
                loss: Some(0.75),
            },
        ));
        let run = manager.get_run(&run_id).unwrap();
        assert_eq!(run.status, TrainingStatus::Running);
        assert_eq!(run.metrics.current_step, 7);

        assert!(manager.set_status(&run_id, TrainingStatus::Stopped));
        let run = manager.get_run(&run_id).unwrap();
        assert_eq!(run.status, TrainingStatus::Stopped);
        assert!(run.finished_at.is_some());
        assert_eq!(manager.list_runs().len(), 1);
    }

    #[test]
    fn rl_environments_expose_tinker_atropos_and_custom() {
        let envs = RlEnvironment::builtin_environments();
        let names: Vec<_> = envs.iter().map(|e| e.name.as_str()).collect();
        assert!(names.contains(&"tinker"));
        assert!(names.contains(&"atropos"));
        assert!(names.contains(&"custom"));
        assert!(envs
            .iter()
            .all(|e| e.config_schema.get("type").and_then(|v| v.as_str()) == Some("object")));
    }

    #[test]
    fn batch_runner_generates_style_tagged_baseline_responses() {
        let runner = BatchRunner::new(BatchRunnerConfig {
            max_parallel_jobs: 2,
            max_turns: 5,
        });
        let prompts = vec!["Fix the flaky test and verify it".to_string()];
        let trajectories = runner.generate_batch(&prompts);

        assert_eq!(trajectories.len(), 1);
        assert_eq!(trajectories[0].id, "traj-1");
        assert_eq!(trajectories[0].prompt, prompts[0]);
        assert!(trajectories[0].response.contains("[baseline-diagnostic]"));
        assert!(trajectories[0].response.contains("steps_budget=5"));
    }
}
