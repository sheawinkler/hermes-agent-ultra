//! Benchmark adapter trait.
//!
//! Each supported benchmark (Terminal-Bench 2.0, SWE-bench, etc.) implements
//! [`BenchmarkAdapter`] to define:
//! - how to load the task dataset,
//! - what inputs / environment / resource limits each task requires,
//! - how to verify task outputs (delegates to a [`Verifier`]).

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::time::Duration;

use crate::error::EvalResult;
use crate::verifier::{VerificationOutcome, Verifier};

/// Static metadata for a benchmark.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BenchmarkMetadata {
    /// Short identifier, e.g. `"terminal-bench-2"`.
    pub id: String,
    /// Display name, e.g. `"Terminal-Bench 2.0"`.
    pub name: String,
    /// Upstream dataset source (URL or HuggingFace slug).
    pub source: String,
    /// Version string for reproducibility.
    pub version: String,
}

/// A single task from a benchmark.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskSpec {
    pub task_id: String,
    pub category: Option<String>,
    /// Natural-language instruction given to the agent.
    pub instruction: String,
    /// Arbitrary JSON blob consumed by the benchmark adapter
    /// (docker image ref, test script, fixtures, etc.).
    pub context: serde_json::Value,
    /// Per-task timeout. The runner enforces this as a hard cap.
    pub timeout: Duration,
}

/// Implemented by each concrete benchmark (terminal-bench-2, swe-bench, ...).
#[async_trait]
pub trait BenchmarkAdapter: Send + Sync {
    fn metadata(&self) -> BenchmarkMetadata;

    /// Load / discover all tasks for this benchmark.
    async fn load_tasks(&self) -> EvalResult<Vec<TaskSpec>>;

    /// Build the verifier that will judge each task's outcome.
    ///
    /// A verifier is typically a Docker-based script runner that executes
    /// the benchmark's test suite against the agent's final state.
    fn verifier(&self) -> Box<dyn Verifier>;

    /// Optional: adapter-specific post-processing hook that runs after
    /// the verifier finishes but before aggregation.
    async fn post_process(
        &self,
        _task: &TaskSpec,
        outcome: VerificationOutcome,
    ) -> EvalResult<VerificationOutcome> {
        Ok(outcome)
    }
}
