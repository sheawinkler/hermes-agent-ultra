//! Benchmark runner.
//!
//! Orchestrates execution of benchmark tasks with optional parallelism via
//! [`futures::stream::StreamExt::buffer_unordered`]. Per-task work is delegated
//! to a [`TaskRollout`]; verification uses the adapter's [`crate::verifier::Verifier`].

use std::sync::Arc;
use std::time::Instant;

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::adapter::{BenchmarkAdapter, TaskSpec};
use crate::error::{EvalError, EvalResult};
use crate::result::{AggregateMetrics, RunRecord, TaskResult, TaskStatus};
use crate::verifier::VerificationOutcome;

/// Configuration for a benchmark run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerConfig {
    /// Model identifier (e.g. `"anthropic/claude-sonnet-4"`).
    pub model: String,
    /// Maximum concurrent tasks. Defaults to the number of CPU cores.
    pub concurrency: u32,
    /// Optional cap on tasks (e.g. for smoke-test subsets).
    pub max_tasks: Option<u32>,
    /// Comma-separated tokens matched against task id or category (substring, case-insensitive).
    pub task_filter: Option<String>,
    /// Seed for any randomised subsampling.
    pub seed: u64,
    /// Whether to continue on per-task error (true) or fail the whole run (false).
    pub continue_on_error: bool,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        Self {
            model: String::new(),
            concurrency: num_cpus_fallback(),
            max_tasks: None,
            task_filter: None,
            seed: 0,
            continue_on_error: true,
        }
    }
}

/// Drives a single task: produce an agent final state blob for verification.
#[async_trait]
pub trait TaskRollout: Send + Sync {
    async fn execute(&self, task: &TaskSpec) -> EvalResult<serde_json::Value>;
}

/// Benchmark runner.
pub struct Runner {
    pub config: RunnerConfig,
}

impl Runner {
    pub fn new(config: RunnerConfig) -> Self {
        Self { config }
    }

    /// Run all (filtered) tasks: rollout → verify → optional post-process, then aggregate.
    ///
    /// `adapter` and `rollout` are wrapped in [`Arc`] to allow concurrent task execution.
    pub async fn run<A, R>(&self, adapter: Arc<A>, rollout: Arc<R>) -> EvalResult<RunRecord>
    where
        A: BenchmarkAdapter + Send + Sync + 'static,
        R: TaskRollout + Send + Sync + 'static,
    {
        let started_at = chrono::Utc::now();
        let run_id = Uuid::new_v4().to_string();
        let meta = adapter.metadata();

        let mut tasks = adapter.load_tasks().await?;
        apply_task_filters(
            &mut tasks,
            self.config.max_tasks,
            self.config.task_filter.as_deref(),
        );

        if tasks.is_empty() {
            return Err(EvalError::DatasetLoad(
                "no tasks after filter (empty dataset or filter too strict)".into(),
            ));
        }

        let concurrency = self.config.concurrency.max(1) as usize;
        let model = self.config.model.clone();
        let seed = self.config.seed;
        let continue_on_error = self.config.continue_on_error;

        let mut indexed_results: Vec<(usize, TaskResult)> =
            stream::iter(tasks.into_iter().enumerate())
                .map(|(idx, task)| {
                    let adapter = adapter.clone();
                    let rollout = rollout.clone();
                    async move {
                        let tr = run_one_task(adapter, rollout, task).await;
                        (idx, tr)
                    }
                })
                .buffer_unordered(concurrency)
                .collect()
                .await;

        indexed_results.sort_by_key(|(i, _)| *i);
        let task_results: Vec<TaskResult> = indexed_results.into_iter().map(|(_, tr)| tr).collect();

        if !continue_on_error {
            if let Some(bad) = task_results.iter().find(|t| {
                matches!(
                    t.status,
                    TaskStatus::Error | TaskStatus::Failed | TaskStatus::Timeout
                )
            }) {
                return Err(EvalError::TaskExecution(format!(
                    "task {} status {:?}: {:?}",
                    bad.task_id, bad.status, bad.error
                )));
            }
        }

        let metrics = aggregate_metrics(&task_results);
        let finished_at = chrono::Utc::now();

        Ok(RunRecord {
            run_id,
            benchmark: meta,
            started_at,
            finished_at: Some(finished_at),
            seed,
            concurrency: self.config.concurrency,
            model,
            tasks: task_results,
            metrics,
        })
    }
}

async fn run_one_task<A, R>(adapter: Arc<A>, rollout: Arc<R>, task: TaskSpec) -> TaskResult
where
    A: BenchmarkAdapter + Send + Sync + ?Sized,
    R: TaskRollout + Send + Sync + ?Sized,
{
    let start = Instant::now();

    match tokio::time::timeout(task.timeout, rollout.execute(&task)).await {
        Err(_) => TaskResult {
            task_id: task.task_id.clone(),
            category: task.category.clone(),
            status: TaskStatus::Timeout,
            outcome: VerificationOutcome::fail("rollout timed out"),
            agent_turns: 0,
            duration: start.elapsed(),
            tokens_input: 0,
            tokens_output: 0,
            cost_usd: 0.0,
            error: Some(format!("timeout after {:?}", task.timeout)),
        },
        Ok(Err(e)) => TaskResult {
            task_id: task.task_id.clone(),
            category: task.category.clone(),
            status: TaskStatus::Error,
            outcome: VerificationOutcome::fail(e.to_string()),
            agent_turns: 0,
            duration: start.elapsed(),
            tokens_input: 0,
            tokens_output: 0,
            cost_usd: 0.0,
            error: Some(e.to_string()),
        },
        Ok(Ok(state)) => {
            let verifier = adapter.verifier();
            match verifier.verify(&task, &state).await {
                Err(e) => TaskResult {
                    task_id: task.task_id.clone(),
                    category: task.category.clone(),
                    status: TaskStatus::Error,
                    outcome: VerificationOutcome::fail(e.to_string()),
                    agent_turns: 0,
                    duration: start.elapsed(),
                    tokens_input: 0,
                    tokens_output: 0,
                    cost_usd: 0.0,
                    error: Some(e.to_string()),
                },
                Ok(mut outcome) => match adapter.post_process(&task, outcome.clone()).await {
                    Err(e) => TaskResult {
                        task_id: task.task_id.clone(),
                        category: task.category.clone(),
                        status: TaskStatus::Error,
                        outcome,
                        agent_turns: 0,
                        duration: start.elapsed(),
                        tokens_input: 0,
                        tokens_output: 0,
                        cost_usd: 0.0,
                        error: Some(e.to_string()),
                    },
                    Ok(o) => {
                        outcome = o;
                        let status = if outcome.passed {
                            TaskStatus::Passed
                        } else {
                            TaskStatus::Failed
                        };
                        TaskResult {
                            task_id: task.task_id.clone(),
                            category: task.category.clone(),
                            status,
                            outcome,
                            agent_turns: 1,
                            duration: start.elapsed(),
                            tokens_input: 0,
                            tokens_output: 0,
                            cost_usd: 0.0,
                            error: None,
                        }
                    }
                },
            }
        }
    }
}

fn apply_task_filters(tasks: &mut Vec<TaskSpec>, max_tasks: Option<u32>, filter: Option<&str>) {
    if let Some(f) = filter {
        let tokens: Vec<String> = f
            .split(',')
            .map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        if !tokens.is_empty() {
            tasks.retain(|t| {
                tokens.iter().any(|tok| {
                    t.task_id.to_lowercase().contains(tok.as_str())
                        || t.category
                            .as_ref()
                            .map(|c| c.to_lowercase().contains(tok.as_str()))
                            .unwrap_or(false)
                })
            });
        }
    }
    if let Some(n) = max_tasks {
        tasks.truncate(n as usize);
    }
}

fn aggregate_metrics(tasks: &[TaskResult]) -> AggregateMetrics {
    let total = tasks.len() as u32;
    let mut passed = 0u32;
    let mut failed = 0u32;
    let mut timeout = 0u32;
    let mut error = 0u32;
    let mut skipped = 0u32;
    let mut total_duration = std::time::Duration::ZERO;
    let mut total_tokens_in = 0u64;
    let mut total_tokens_out = 0u64;
    let mut total_cost = 0.0f64;

    for t in tasks {
        total_duration += t.duration;
        total_tokens_in += t.tokens_input;
        total_tokens_out += t.tokens_output;
        total_cost += t.cost_usd;
        match t.status {
            TaskStatus::Passed => passed += 1,
            TaskStatus::Failed => failed += 1,
            TaskStatus::Timeout => timeout += 1,
            TaskStatus::Error => error += 1,
            TaskStatus::Skipped => skipped += 1,
        }
    }

    let pass_at_1 = if total > 0 {
        passed as f64 / total as f64
    } else {
        0.0
    };

    AggregateMetrics {
        total,
        passed,
        failed,
        timeout,
        error,
        skipped,
        pass_at_1,
        total_duration,
        total_tokens_input: total_tokens_in,
        total_tokens_output: total_tokens_out,
        total_cost_usd: total_cost,
    }
}

fn num_cpus_fallback() -> u32 {
    std::thread::available_parallelism()
        .map(|n| n.get() as u32)
        .unwrap_or(4)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{BenchmarkAdapter, BenchmarkMetadata, TaskSpec};
    use crate::verifier::{VerificationOutcome, Verifier};

    struct EmptyAdapter;

    #[async_trait]
    impl BenchmarkAdapter for EmptyAdapter {
        fn metadata(&self) -> BenchmarkMetadata {
            BenchmarkMetadata {
                id: "test".into(),
                name: "test".into(),
                source: "test".into(),
                version: "0".into(),
            }
        }

        async fn load_tasks(&self) -> EvalResult<Vec<TaskSpec>> {
            Ok(vec![])
        }

        fn verifier(&self) -> Box<dyn Verifier> {
            struct V;
            #[async_trait]
            impl Verifier for V {
                async fn verify(
                    &self,
                    _: &TaskSpec,
                    _: &serde_json::Value,
                ) -> EvalResult<VerificationOutcome> {
                    Ok(VerificationOutcome::pass())
                }
            }
            Box::new(V)
        }
    }

    struct OkRollout;

    #[async_trait]
    impl TaskRollout for OkRollout {
        async fn execute(&self, _: &TaskSpec) -> EvalResult<serde_json::Value> {
            Ok(serde_json::json!({}))
        }
    }

    #[tokio::test]
    async fn run_errors_on_empty_filtered_dataset() {
        let runner = Runner::new(RunnerConfig {
            task_filter: Some("nonexistent-xyz".into()),
            ..Default::default()
        });
        let err = runner
            .run(Arc::new(EmptyAdapter), Arc::new(OkRollout))
            .await
            .unwrap_err();
        assert!(matches!(err, EvalError::DatasetLoad(_)));
    }
}
