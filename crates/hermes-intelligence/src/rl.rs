//! RL Training Integration module.
//!
//! Provides types and utilities for reinforcement-learning-based agent training,
//! including trajectory recording, compression, batch generation, and an
//! RL toolset for lightweight local orchestration.

use chrono::{DateTime, Utc};
use hermes_core::{Message, ToolCall};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use uuid::Uuid;

static ATOMIC_WRITE_COUNTER: AtomicU64 = AtomicU64::new(1);

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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// Dataset row accepted by the Rust batch runner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchDatasetItem {
    pub prompt_index: usize,
    pub prompt: String,
    #[serde(default, skip_serializing_if = "Map::is_empty")]
    pub metadata: Map<String, Value>,
}

/// Per-batch checkpoint counters.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchCheckpointStats {
    pub processed: usize,
    pub skipped: usize,
}

/// Resume checkpoint for deterministic batch generation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchRunCheckpoint {
    pub run_name: String,
    #[serde(default)]
    pub completed_prompts: Vec<usize>,
    #[serde(default)]
    pub completed_prompt_texts: Vec<String>,
    #[serde(default)]
    pub batch_stats: HashMap<String, BatchCheckpointStats>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_updated: Option<DateTime<Utc>>,
}

impl BatchRunCheckpoint {
    pub fn new(run_name: impl Into<String>) -> Self {
        Self {
            run_name: run_name.into(),
            completed_prompts: Vec::new(),
            completed_prompt_texts: Vec::new(),
            batch_stats: HashMap::new(),
            last_updated: None,
        }
    }

    pub fn from_json(raw: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(raw)
    }

    pub fn to_json_pretty(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn load_from_path(path: impl AsRef<Path>) -> io::Result<Option<Self>> {
        let path = path.as_ref();
        match fs::read_to_string(path) {
            Ok(raw) => serde_json::from_str(&raw)
                .map(Some)
                .map_err(invalid_data_error),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(err),
        }
    }

    pub fn save_atomic_to_path(&self, path: impl AsRef<Path>) -> io::Result<()> {
        atomic_json_write(path, self)
    }
}

pub fn atomic_json_write<T: Serialize>(path: impl AsRef<Path>, value: &T) -> io::Result<()> {
    let path = path.as_ref();
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;

    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("checkpoint.json");
    let unique = ATOMIC_WRITE_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp_path = parent.join(format!(
        ".{file_name}.tmp.{}.{}.{}",
        std::process::id(),
        Utc::now().timestamp_nanos_opt().unwrap_or_default(),
        unique
    ));

    let write_result = (|| {
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp_path)?;
        serde_json::to_writer_pretty(&mut file, value).map_err(invalid_data_error)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        drop(file);
        fs::rename(&temp_path, path)?;
        if let Ok(dir) = fs::File::open(parent) {
            let _ = dir.sync_all();
        }
        Ok(())
    })();

    if write_result.is_err() {
        let _ = fs::remove_file(&temp_path);
    }

    write_result
}

fn invalid_data_error(err: serde_json::Error) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, err)
}

/// Batch dataset runner configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchDatasetRunConfig {
    pub run_name: String,
    pub batch_size: usize,
    pub distribution: String,
    pub model: String,
    pub max_turns: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_samples: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ephemeral_system_prompt: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_toolsets: Vec<String>,
}

impl Default for BatchDatasetRunConfig {
    fn default() -> Self {
        Self {
            run_name: "default".to_string(),
            batch_size: 10,
            distribution: "default".to_string(),
            model: "baseline-local".to_string(),
            max_turns: 10,
            max_samples: None,
            ephemeral_system_prompt: None,
            selected_toolsets: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BatchToolStats {
    pub count: usize,
    pub success: usize,
    pub failure: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchTrajectoryEntry {
    pub prompt_index: usize,
    pub conversations: Vec<Message>,
    pub trajectory: BatchTrajectory,
    pub completed: bool,
    pub partial: bool,
    pub api_calls: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub toolsets_used: Vec<String>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tool_stats: HashMap<String, BatchToolStats>,
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchRunStatistics {
    pub run_name: String,
    pub distribution: String,
    pub total_prompts: usize,
    pub total_batches: usize,
    pub batch_size: usize,
    pub model: String,
    pub processed: usize,
    pub skipped: usize,
    #[serde(default)]
    pub checkpoints_written: usize,
    pub ephemeral_system_prompt_used: bool,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tool_statistics: HashMap<String, BatchToolStats>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BatchRunReport {
    pub trajectories: Vec<BatchTrajectoryEntry>,
    pub checkpoint: BatchRunCheckpoint,
    pub statistics: BatchRunStatistics,
}

impl BatchRunReport {
    pub fn trajectories_jsonl(&self) -> Result<String, serde_json::Error> {
        let mut lines = Vec::with_capacity(self.trajectories.len());
        for trajectory in &self.trajectories {
            lines.push(serde_json::to_string(trajectory)?);
        }
        Ok(lines.join("\n"))
    }
}

#[derive(Debug, Clone, Default)]
pub struct BatchDatasetRunner;

impl BatchDatasetRunner {
    pub fn new() -> Self {
        Self
    }

    pub fn run(
        &self,
        dataset: &[BatchDatasetItem],
        config: &BatchDatasetRunConfig,
        checkpoint: Option<BatchRunCheckpoint>,
    ) -> BatchRunReport {
        self.run_internal(dataset, config, checkpoint, None)
            .expect("in-memory batch dataset run cannot fail")
    }

    pub fn run_with_checkpoint_path(
        &self,
        dataset: &[BatchDatasetItem],
        config: &BatchDatasetRunConfig,
        checkpoint_path: impl AsRef<Path>,
    ) -> io::Result<BatchRunReport> {
        let checkpoint_path = checkpoint_path.as_ref();
        let checkpoint = BatchRunCheckpoint::load_from_path(checkpoint_path)?;
        self.run_internal(dataset, config, checkpoint, Some(checkpoint_path))
    }

    fn run_internal(
        &self,
        dataset: &[BatchDatasetItem],
        config: &BatchDatasetRunConfig,
        checkpoint: Option<BatchRunCheckpoint>,
        checkpoint_path: Option<&Path>,
    ) -> io::Result<BatchRunReport> {
        let batch_size = config.batch_size.max(1);
        let limit = config
            .max_samples
            .unwrap_or(dataset.len())
            .min(dataset.len());
        let items = &dataset[..limit];
        let total_batches = if items.is_empty() {
            0
        } else {
            items.len().div_ceil(batch_size)
        };

        let mut checkpoint =
            checkpoint.unwrap_or_else(|| BatchRunCheckpoint::new(&config.run_name));
        if checkpoint.run_name != config.run_name {
            checkpoint = BatchRunCheckpoint::new(&config.run_name);
        }
        let completed_indices: HashSet<usize> =
            checkpoint.completed_prompts.iter().copied().collect();
        let completed_texts: HashSet<String> =
            checkpoint.completed_prompt_texts.iter().cloned().collect();

        let runner = BatchRunner::new(BatchRunnerConfig {
            max_parallel_jobs: 1,
            max_turns: config.max_turns,
        });
        let mut trajectories = Vec::new();
        let mut completed = completed_indices;
        let mut completed_prompt_texts = completed_texts;
        let mut total_skipped = 0usize;
        let mut total_processed = 0usize;
        let mut checkpoints_written = 0usize;
        let mut tool_statistics: HashMap<String, BatchToolStats> = HashMap::new();

        for (batch_num, batch) in items.chunks(batch_size).enumerate() {
            let mut batch_processed = 0usize;
            let mut batch_skipped = 0usize;

            for item in batch {
                if completed.contains(&item.prompt_index)
                    || completed_prompt_texts.contains(item.prompt.trim())
                {
                    completed.insert(item.prompt_index);
                    completed_prompt_texts.insert(item.prompt.trim().to_string());
                    batch_skipped += 1;
                    continue;
                }

                let generated = runner.generate_batch(std::slice::from_ref(&item.prompt));
                let Some(trajectory) = generated.into_iter().next() else {
                    batch_skipped += 1;
                    continue;
                };

                completed.insert(item.prompt_index);
                completed_prompt_texts.insert(item.prompt.trim().to_string());
                batch_processed += 1;

                for toolset in &config.selected_toolsets {
                    tool_statistics.entry(toolset.clone()).or_default();
                }

                let mut metadata = item.metadata.clone();
                metadata.insert("batch_num".to_string(), Value::from(batch_num));
                metadata.insert("model".to_string(), Value::from(config.model.clone()));
                metadata.insert("run_name".to_string(), Value::from(config.run_name.clone()));

                trajectories.push(BatchTrajectoryEntry {
                    prompt_index: item.prompt_index,
                    conversations: vec![
                        Message::user(item.prompt.clone()),
                        Message::assistant(trajectory.response.clone()),
                    ],
                    trajectory,
                    completed: true,
                    partial: false,
                    api_calls: 1,
                    toolsets_used: config.selected_toolsets.clone(),
                    tool_stats: HashMap::new(),
                    metadata: Value::Object(metadata),
                });
            }

            total_processed += batch_processed;
            total_skipped += batch_skipped;
            checkpoint.batch_stats.insert(
                batch_num.to_string(),
                BatchCheckpointStats {
                    processed: batch_processed,
                    skipped: batch_skipped,
                },
            );

            refresh_checkpoint_progress(&mut checkpoint, &completed, &completed_prompt_texts);
            if let Some(path) = checkpoint_path {
                checkpoint.save_atomic_to_path(path)?;
                checkpoints_written += 1;
            }
        }

        refresh_checkpoint_progress(&mut checkpoint, &completed, &completed_prompt_texts);

        Ok(BatchRunReport {
            trajectories,
            checkpoint,
            statistics: BatchRunStatistics {
                run_name: config.run_name.clone(),
                distribution: config.distribution.clone(),
                total_prompts: items.len(),
                total_batches,
                batch_size,
                model: config.model.clone(),
                processed: total_processed,
                skipped: total_skipped,
                checkpoints_written,
                ephemeral_system_prompt_used: config.ephemeral_system_prompt.is_some(),
                tool_statistics,
            },
        })
    }
}

fn refresh_checkpoint_progress(
    checkpoint: &mut BatchRunCheckpoint,
    completed: &HashSet<usize>,
    completed_prompt_texts: &HashSet<String>,
) {
    let mut completed_prompts: Vec<_> = completed.iter().copied().collect();
    completed_prompts.sort_unstable();
    let mut prompt_texts: Vec<_> = completed_prompt_texts.iter().cloned().collect();
    prompt_texts.sort();
    checkpoint.completed_prompts = completed_prompts;
    checkpoint.completed_prompt_texts = prompt_texts;
    checkpoint.last_updated = Some(Utc::now());
}

pub fn parse_batch_jsonl_dataset(raw: &str) -> Result<Vec<BatchDatasetItem>, String> {
    let mut items = Vec::new();
    for (line_idx, line) in raw.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let value: Value = serde_json::from_str(line)
            .map_err(|err| format!("line {}: invalid JSON: {err}", line_idx + 1))?;
        let Some(prompt) = prompt_from_dataset_value(&value) else {
            continue;
        };
        let mut metadata = match value {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        metadata.remove("prompt");
        metadata.remove("conversations");
        items.push(BatchDatasetItem {
            prompt_index: items.len(),
            prompt,
            metadata,
        });
    }

    if items.is_empty() {
        Err("no valid batch dataset prompts found".to_string())
    } else {
        Ok(items)
    }
}

fn prompt_from_dataset_value(value: &Value) -> Option<String> {
    if let Some(prompt) = value.get("prompt").and_then(Value::as_str) {
        let trimmed = prompt.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    let conversations = value.get("conversations")?.as_array()?;
    for message in conversations {
        let Some(role) = message
            .get("role")
            .or_else(|| message.get("from"))
            .and_then(Value::as_str)
        else {
            continue;
        };
        if role == "user" || role == "human" {
            let content = message
                .get("content")
                .or_else(|| message.get("value"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim();
            if !content.is_empty() {
                return Some(content.to_string());
            }
        }
    }
    None
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

    #[test]
    fn parse_batch_jsonl_dataset_accepts_prompt_and_conversation_rows() {
        let raw = r#"
{"prompt":" Plan a verification slice ","source":"direct"}
{"conversations":[{"role":"system","content":"ignore"},{"role":"user","content":"Fix the bug"}],"difficulty":"medium"}
{"conversations":[{"from":"assistant","value":"hello"},{"from":"human","value":"Write tests"}]}
{"conversations":[{"role":"assistant","content":"no user prompt"}]}

"#;

        let items = parse_batch_jsonl_dataset(raw).unwrap();

        assert_eq!(items.len(), 3);
        assert_eq!(items[0].prompt_index, 0);
        assert_eq!(items[0].prompt, "Plan a verification slice");
        assert_eq!(items[0].metadata["source"], "direct");
        assert_eq!(items[1].prompt, "Fix the bug");
        assert_eq!(items[1].metadata["difficulty"], "medium");
        assert_eq!(items[2].prompt, "Write tests");
        assert!(items[1].metadata.get("conversations").is_none());
    }

    #[test]
    fn parse_batch_jsonl_dataset_reports_invalid_or_empty_inputs() {
        let invalid = parse_batch_jsonl_dataset("{not json").unwrap_err();
        assert!(invalid.contains("line 1: invalid JSON"));

        let empty = parse_batch_jsonl_dataset(r#"{"prompt":"   "}"#).unwrap_err();
        assert_eq!(empty, "no valid batch dataset prompts found");
    }

    #[test]
    fn batch_dataset_runner_updates_checkpoint_and_resumes_by_index_and_prompt() {
        let dataset = vec![
            BatchDatasetItem {
                prompt_index: 0,
                prompt: "Fix the parser".to_string(),
                metadata: Map::new(),
            },
            BatchDatasetItem {
                prompt_index: 1,
                prompt: "Plan the rollout".to_string(),
                metadata: Map::new(),
            },
            BatchDatasetItem {
                prompt_index: 2,
                prompt: "Verify the behavior".to_string(),
                metadata: Map::new(),
            },
        ];
        let mut prior = BatchRunCheckpoint::new("resume-run");
        prior.completed_prompts = vec![0];
        prior.completed_prompt_texts = vec!["Plan the rollout".to_string()];
        let config = BatchDatasetRunConfig {
            run_name: "resume-run".to_string(),
            batch_size: 2,
            selected_toolsets: vec!["rl_training".to_string()],
            ..BatchDatasetRunConfig::default()
        };

        let report = BatchDatasetRunner::new().run(&dataset, &config, Some(prior));

        assert_eq!(report.statistics.total_prompts, 3);
        assert_eq!(report.statistics.total_batches, 2);
        assert_eq!(report.statistics.processed, 1);
        assert_eq!(report.statistics.skipped, 2);
        assert_eq!(report.trajectories.len(), 1);
        assert_eq!(report.trajectories[0].prompt_index, 2);
        assert_eq!(report.trajectories[0].toolsets_used, ["rl_training"]);
        assert_eq!(report.checkpoint.completed_prompts, [0, 1, 2]);
        assert!(report
            .checkpoint
            .completed_prompt_texts
            .contains(&"Verify the behavior".to_string()));
        assert_eq!(
            report.checkpoint.batch_stats.get("0").unwrap(),
            &BatchCheckpointStats {
                processed: 0,
                skipped: 2,
            }
        );
        assert_eq!(
            report.checkpoint.batch_stats.get("1").unwrap(),
            &BatchCheckpointStats {
                processed: 1,
                skipped: 0,
            }
        );
    }

    #[test]
    fn batch_dataset_runner_omits_ephemeral_system_prompt_from_outputs() {
        let dataset = vec![BatchDatasetItem {
            prompt_index: 0,
            prompt: "Test prompt leakage".to_string(),
            metadata: Map::new(),
        }];
        let config = BatchDatasetRunConfig {
            run_name: "ephemeral-run".to_string(),
            ephemeral_system_prompt: Some("never persist this system prompt".to_string()),
            ..BatchDatasetRunConfig::default()
        };

        let report = BatchDatasetRunner::new().run(&dataset, &config, None);
        let jsonl = report.trajectories_jsonl().unwrap();
        let checkpoint = report.checkpoint.to_json_pretty().unwrap();

        assert!(report.statistics.ephemeral_system_prompt_used);
        assert!(!jsonl.contains("never persist this system prompt"));
        assert!(!checkpoint.contains("never persist this system prompt"));
        assert_eq!(
            report.trajectories[0].conversations[0].content.as_deref(),
            Some("Test prompt leakage")
        );
    }

    #[test]
    fn batch_dataset_runner_respects_max_samples_and_nonzero_batch_size() {
        let dataset: Vec<_> = (0..5)
            .map(|idx| BatchDatasetItem {
                prompt_index: idx,
                prompt: format!("prompt {idx}"),
                metadata: Map::new(),
            })
            .collect();
        let config = BatchDatasetRunConfig {
            run_name: "limited-run".to_string(),
            batch_size: 0,
            max_samples: Some(3),
            ..BatchDatasetRunConfig::default()
        };

        let report = BatchDatasetRunner::new().run(&dataset, &config, None);

        assert_eq!(report.statistics.batch_size, 1);
        assert_eq!(report.statistics.total_prompts, 3);
        assert_eq!(report.statistics.total_batches, 3);
        assert_eq!(report.statistics.processed, 3);
        assert_eq!(report.checkpoint.completed_prompts, [0, 1, 2]);
        assert_eq!(report.trajectories.len(), 3);
    }

    #[test]
    fn atomic_json_write_replaces_without_temp_file_leaks() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("checkpoint.json");

        atomic_json_write(&path, &serde_json::json!({"version": 1})).unwrap();
        atomic_json_write(&path, &serde_json::json!({"version": 2, "ok": true})).unwrap();

        let value: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(value["version"], 2);
        assert_eq!(value["ok"], true);

        let leaked_tmp_files: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp."))
            .collect();
        assert!(leaked_tmp_files.is_empty());
    }

    #[test]
    fn batch_dataset_runner_persists_atomic_checkpoint_per_batch_and_resumes_from_file() {
        let tmp = tempfile::tempdir().unwrap();
        let checkpoint_path = tmp.path().join("checkpoints").join("run.json");
        let dataset: Vec<_> = (0..4)
            .map(|idx| BatchDatasetItem {
                prompt_index: idx,
                prompt: format!("prompt {idx}"),
                metadata: Map::new(),
            })
            .collect();
        let config = BatchDatasetRunConfig {
            run_name: "atomic-run".to_string(),
            batch_size: 2,
            ..BatchDatasetRunConfig::default()
        };

        let first = BatchDatasetRunner::new()
            .run_with_checkpoint_path(&dataset, &config, &checkpoint_path)
            .unwrap();
        assert_eq!(first.statistics.processed, 4);
        assert_eq!(first.statistics.skipped, 0);
        assert_eq!(first.statistics.checkpoints_written, 2);
        assert_eq!(first.checkpoint.completed_prompts, [0, 1, 2, 3]);

        let persisted = BatchRunCheckpoint::load_from_path(&checkpoint_path)
            .unwrap()
            .unwrap();
        assert_eq!(persisted.run_name, "atomic-run");
        assert_eq!(persisted.completed_prompts, [0, 1, 2, 3]);
        assert_eq!(
            persisted.batch_stats.get("0").unwrap(),
            &BatchCheckpointStats {
                processed: 2,
                skipped: 0,
            }
        );
        assert_eq!(
            persisted.batch_stats.get("1").unwrap(),
            &BatchCheckpointStats {
                processed: 2,
                skipped: 0,
            }
        );

        let resumed = BatchDatasetRunner::new()
            .run_with_checkpoint_path(&dataset, &config, &checkpoint_path)
            .unwrap();
        assert_eq!(resumed.statistics.processed, 0);
        assert_eq!(resumed.statistics.skipped, 4);
        assert_eq!(resumed.statistics.checkpoints_written, 2);
        assert!(resumed.trajectories.is_empty());
        assert_eq!(resumed.checkpoint.completed_prompts, [0, 1, 2, 3]);
    }
}
