//! Benchmark runner.
//!
//! Orchestrates execution of benchmark tasks with optional parallelism via
//! [`futures::stream::StreamExt::buffer_unordered`]. Per-task work is delegated
//! to a [`TaskRollout`]; verification uses the adapter's [`crate::verifier::Verifier`].

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::adapter::{BenchmarkAdapter, TaskSpec};
use crate::error::{EvalError, EvalResult};
use crate::result::{
    AggregateMetrics, RunRecord, TaskResult, TaskStatus, TaskTelemetry, Telemetry,
};
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

/// Typed terminal state of a rollout before benchmark verification.
///
/// The verifier receives only `verifier_state`; telemetry is carried separately
/// and is never derived from JSON that a verifier returns or echoes.
#[derive(Debug, Clone)]
pub enum RolloutState {
    Completed { verifier_state: serde_json::Value },
    Failed { error: String },
}

/// Authoritative state and telemetry produced by one task rollout.
///
/// Implementations that observe telemetry before a handled execution failure
/// should return [`TaskRolloutResult::execution_failure`] so the runner can
/// retain it. A bare [`EvalError`] has no observed telemetry and is recorded as
/// explicitly unavailable.
#[derive(Debug, Clone)]
pub struct TaskRolloutResult {
    pub state: RolloutState,
    pub telemetry: TaskTelemetry,
}

impl TaskRolloutResult {
    pub fn completed(verifier_state: serde_json::Value, telemetry: TaskTelemetry) -> Self {
        Self {
            state: RolloutState::Completed { verifier_state },
            telemetry,
        }
    }

    pub fn execution_failure(error: impl Into<String>, telemetry: TaskTelemetry) -> Self {
        Self {
            state: RolloutState::Failed {
                error: error.into(),
            },
            telemetry,
        }
    }
}

/// Drives a single task and returns verifier state plus authoritative telemetry.
#[async_trait]
pub trait TaskRollout: Send + Sync {
    async fn execute(&self, task: &TaskSpec) -> EvalResult<TaskRolloutResult>;
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
        Err(_) => task_result(
            &task,
            TaskStatus::Timeout,
            VerificationOutcome::fail("rollout timed out"),
            start.elapsed(),
            TaskTelemetry::default(),
            Some(format!("timeout after {:?}", task.timeout)),
        ),
        Ok(Err(error)) => task_result(
            &task,
            TaskStatus::Error,
            VerificationOutcome::fail(error.to_string()),
            start.elapsed(),
            TaskTelemetry::default(),
            Some(error.to_string()),
        ),
        Ok(Ok(rollout_result)) => {
            let TaskRolloutResult { state, telemetry } = rollout_result;
            match state {
                RolloutState::Failed { error } => task_result(
                    &task,
                    TaskStatus::Error,
                    VerificationOutcome::fail(error.clone()),
                    start.elapsed(),
                    telemetry,
                    Some(error),
                ),
                RolloutState::Completed { verifier_state } => {
                    let verifier = adapter.verifier();
                    match verifier.verify(&task, &verifier_state).await {
                        Err(error) => task_result(
                            &task,
                            TaskStatus::Error,
                            VerificationOutcome::fail(error.to_string()),
                            start.elapsed(),
                            telemetry,
                            Some(error.to_string()),
                        ),
                        Ok(outcome) => match adapter.post_process(&task, outcome.clone()).await {
                            Err(error) => task_result(
                                &task,
                                TaskStatus::Error,
                                outcome,
                                start.elapsed(),
                                telemetry,
                                Some(error.to_string()),
                            ),
                            Ok(outcome) => {
                                let status = if outcome.passed {
                                    TaskStatus::Passed
                                } else {
                                    TaskStatus::Failed
                                };
                                task_result(
                                    &task,
                                    status,
                                    outcome,
                                    start.elapsed(),
                                    telemetry,
                                    None,
                                )
                            }
                        },
                    }
                }
            }
        }
    }
}

fn task_result(
    task: &TaskSpec,
    status: TaskStatus,
    outcome: VerificationOutcome,
    duration: Duration,
    telemetry: TaskTelemetry,
    error: Option<String>,
) -> TaskResult {
    TaskResult {
        task_id: task.task_id.clone(),
        category: task.category.clone(),
        status,
        outcome,
        agent_turns: telemetry.total_turns,
        retry_count: telemetry.retry_count,
        duration,
        tokens_input: telemetry.tokens_input,
        tokens_output: telemetry.tokens_output,
        cost_usd: telemetry.cost_usd,
        provenance: telemetry.provenance,
        error,
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
    let mut total_duration = Duration::ZERO;

    for task in tasks {
        total_duration += task.duration;
        match task.status {
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
        total_tokens_input: sum_u64_telemetry(tasks.iter().map(|task| &task.tokens_input)),
        total_tokens_output: sum_u64_telemetry(tasks.iter().map(|task| &task.tokens_output)),
        total_cost_usd: sum_cost_telemetry(tasks),
        total_agent_turns: sum_u32_telemetry(tasks.iter().map(|task| &task.agent_turns)),
        total_retries: sum_u32_telemetry(tasks.iter().map(|task| &task.retry_count)),
    }
}

fn sum_u64_telemetry<'a>(values: impl Iterator<Item = &'a Telemetry<u64>>) -> Telemetry<u64> {
    let mut total = 0_u64;
    let mut any = false;
    for value in values {
        let Some(value) = value.value() else {
            return Telemetry::Unavailable;
        };
        any = true;
        let Some(next) = total.checked_add(value) else {
            return Telemetry::Unavailable;
        };
        total = next;
    }
    if any {
        Telemetry::Available(total)
    } else {
        Telemetry::Unavailable
    }
}

fn sum_u32_telemetry<'a>(values: impl Iterator<Item = &'a Telemetry<u32>>) -> Telemetry<u64> {
    let mut total = 0_u64;
    let mut any = false;
    for value in values {
        let Some(value) = value.value() else {
            return Telemetry::Unavailable;
        };
        any = true;
        let Some(next) = total.checked_add(u64::from(value)) else {
            return Telemetry::Unavailable;
        };
        total = next;
    }
    if any {
        Telemetry::Available(total)
    } else {
        Telemetry::Unavailable
    }
}

fn sum_cost_telemetry(tasks: &[TaskResult]) -> Telemetry<f64> {
    if tasks.is_empty() {
        return Telemetry::Unavailable;
    }

    let mut total = 0.0_f64;
    for task in tasks {
        let Telemetry::Available(cost) = &task.cost_usd else {
            return Telemetry::Unavailable;
        };
        let cost = cost.usd();
        if !cost.is_finite() || cost.is_sign_negative() {
            return Telemetry::Unavailable;
        }
        total += cost;
        if !total.is_finite() || total.is_sign_negative() {
            return Telemetry::Unavailable;
        }
    }
    Telemetry::Available(total)
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
    use crate::result::{CostUsd, PricingProvenance, PricingSource};
    use crate::verifier::{VerificationOutcome, Verifier};

    struct TestAdapter {
        tasks: Vec<TaskSpec>,
        verifier_error: bool,
    }

    impl TestAdapter {
        fn new(tasks: Vec<TaskSpec>) -> Self {
            Self {
                tasks,
                verifier_error: false,
            }
        }

        fn with_verifier_error(tasks: Vec<TaskSpec>) -> Self {
            Self {
                tasks,
                verifier_error: true,
            }
        }
    }

    #[async_trait]
    impl BenchmarkAdapter for TestAdapter {
        fn metadata(&self) -> BenchmarkMetadata {
            BenchmarkMetadata {
                id: "test".into(),
                name: "test".into(),
                source: "test".into(),
                version: "0".into(),
            }
        }

        async fn load_tasks(&self) -> EvalResult<Vec<TaskSpec>> {
            Ok(self.tasks.clone())
        }

        fn verifier(&self) -> Box<dyn Verifier> {
            Box::new(TestVerifier {
                fail: self.verifier_error,
            })
        }
    }

    struct TestVerifier {
        fail: bool,
    }

    #[async_trait]
    impl Verifier for TestVerifier {
        async fn verify(
            &self,
            _: &TaskSpec,
            _: &serde_json::Value,
        ) -> EvalResult<VerificationOutcome> {
            if self.fail {
                Err(EvalError::Verification("injected verifier failure".into()))
            } else {
                Ok(VerificationOutcome::pass())
            }
        }
    }

    fn task(task_id: &str, timeout: Duration) -> TaskSpec {
        TaskSpec {
            task_id: task_id.into(),
            category: None,
            instruction: "test task".into(),
            context: serde_json::json!({}),
            timeout,
        }
    }

    fn valid_cost(usd: f64) -> CostUsd {
        CostUsd::new(
            usd,
            PricingProvenance::new(
                PricingSource::ExternalEstimate,
                Telemetry::Unavailable,
                Telemetry::Unavailable,
            ),
        )
        .expect("valid cost")
    }

    #[derive(Clone)]
    struct StaticRollout(TaskRolloutResult);

    #[async_trait]
    impl TaskRollout for StaticRollout {
        async fn execute(&self, _: &TaskSpec) -> EvalResult<TaskRolloutResult> {
            Ok(self.0.clone())
        }
    }

    struct ErrorRollout;

    #[async_trait]
    impl TaskRollout for ErrorRollout {
        async fn execute(&self, _: &TaskSpec) -> EvalResult<TaskRolloutResult> {
            Err(EvalError::TaskExecution("injected rollout error".into()))
        }
    }

    struct TimeoutRollout;

    #[async_trait]
    impl TaskRollout for TimeoutRollout {
        async fn execute(&self, _: &TaskSpec) -> EvalResult<TaskRolloutResult> {
            tokio::time::sleep(Duration::from_millis(50)).await;
            Ok(TaskRolloutResult::completed(
                serde_json::json!({}),
                TaskTelemetry::default(),
            ))
        }
    }

    struct TurnRetryRollout;

    #[async_trait]
    impl TaskRollout for TurnRetryRollout {
        async fn execute(&self, task: &TaskSpec) -> EvalResult<TaskRolloutResult> {
            let (turns, retries) = if task.task_id == "first" {
                (2, 1)
            } else {
                (3, 2)
            };
            Ok(TaskRolloutResult::completed(
                serde_json::json!({}),
                TaskTelemetry {
                    total_turns: Telemetry::Available(turns),
                    retry_count: Telemetry::Available(retries),
                    ..TaskTelemetry::default()
                },
            ))
        }
    }

    #[tokio::test]
    async fn run_errors_on_empty_filtered_dataset() {
        let runner = Runner::new(RunnerConfig {
            task_filter: Some("nonexistent-xyz".into()),
            ..Default::default()
        });
        let err = runner
            .run(
                Arc::new(TestAdapter::new(vec![task(
                    "present",
                    Duration::from_secs(1),
                )])),
                Arc::new(StaticRollout(TaskRolloutResult::completed(
                    serde_json::json!({}),
                    TaskTelemetry::default(),
                ))),
            )
            .await
            .unwrap_err();
        assert!(matches!(err, EvalError::DatasetLoad(_)));
    }

    #[tokio::test]
    async fn typed_telemetry_reaches_task_and_aggregate_and_ignores_verifier_json() {
        let telemetry = TaskTelemetry {
            tokens_input: Telemetry::Available(11),
            tokens_output: Telemetry::Available(7),
            cost_usd: Telemetry::Available(valid_cost(0.25)),
            total_turns: Telemetry::Available(4),
            retry_count: Telemetry::Available(1),
            ..TaskTelemetry::default()
        };
        let rollout = StaticRollout(TaskRolloutResult::completed(
            serde_json::json!({
                "usage": {
                    "prompt_tokens": 999_999,
                    "completion_tokens": 999_999,
                    "estimated_cost": 999.0,
                },
                "total_turns": 999_999,
                "retry_count": 999_999,
            }),
            telemetry,
        ));
        let record = Runner::new(RunnerConfig::default())
            .run(
                Arc::new(TestAdapter::new(vec![task("one", Duration::from_secs(1))])),
                Arc::new(rollout),
            )
            .await
            .expect("run");

        let task = &record.tasks[0];
        assert_eq!(task.tokens_input, Telemetry::Available(11));
        assert_eq!(task.tokens_output, Telemetry::Available(7));
        assert_eq!(task.agent_turns, Telemetry::Available(4));
        assert_eq!(task.retry_count, Telemetry::Available(1));
        assert!(matches!(
            task.cost_usd.as_ref(),
            Telemetry::Available(cost) if (cost.usd() - 0.25).abs() < f64::EPSILON
        ));
        assert_eq!(record.metrics.total_tokens_input, Telemetry::Available(11));
        assert_eq!(record.metrics.total_tokens_output, Telemetry::Available(7));
        assert_eq!(record.metrics.total_agent_turns, Telemetry::Available(4));
        assert_eq!(record.metrics.total_retries, Telemetry::Available(1));
        assert!(matches!(
            record.metrics.total_cost_usd,
            Telemetry::Available(cost) if (cost - 0.25).abs() < f64::EPSILON
        ));
    }

    #[tokio::test]
    async fn unavailable_telemetry_stays_unavailable_not_zero() {
        let record = Runner::new(RunnerConfig::default())
            .run(
                Arc::new(TestAdapter::new(vec![task("one", Duration::from_secs(1))])),
                Arc::new(StaticRollout(TaskRolloutResult::completed(
                    serde_json::json!({
                        "usage": {"prompt_tokens": 100, "completion_tokens": 50},
                        "total_turns": 3,
                    }),
                    TaskTelemetry::default(),
                ))),
            )
            .await
            .expect("run");

        let task = &record.tasks[0];
        assert!(matches!(task.tokens_input, Telemetry::Unavailable));
        assert!(matches!(task.tokens_output, Telemetry::Unavailable));
        assert!(matches!(task.cost_usd.as_ref(), Telemetry::Unavailable));
        assert!(matches!(task.agent_turns, Telemetry::Unavailable));
        assert!(matches!(task.retry_count, Telemetry::Unavailable));
        assert!(matches!(
            record.metrics.total_tokens_input,
            Telemetry::Unavailable
        ));
        assert!(matches!(
            record.metrics.total_tokens_output,
            Telemetry::Unavailable
        ));
        assert!(matches!(
            record.metrics.total_cost_usd,
            Telemetry::Unavailable
        ));
        assert!(matches!(
            record.metrics.total_agent_turns,
            Telemetry::Unavailable
        ));
        assert!(matches!(
            record.metrics.total_retries,
            Telemetry::Unavailable
        ));
    }

    #[test]
    fn invalid_cost_is_rejected() {
        let pricing = PricingProvenance::agent_result_usage_estimate();
        assert!(CostUsd::new(f64::NAN, pricing.clone()).is_err());
        assert!(CostUsd::new(-0.0, pricing.clone()).is_err());
        assert!(CostUsd::new(-0.01, pricing).is_err());
        assert!(serde_json::from_value::<CostUsd>(serde_json::json!({
            "usd": -0.01,
            "pricing": {"source": "external_estimate"},
        }))
        .is_err());
    }

    #[tokio::test]
    async fn handled_execution_failure_preserves_observed_telemetry() {
        let telemetry = TaskTelemetry {
            tokens_input: Telemetry::Available(5),
            total_turns: Telemetry::Available(2),
            retry_count: Telemetry::Available(1),
            ..TaskTelemetry::default()
        };
        let record = Runner::new(RunnerConfig::default())
            .run(
                Arc::new(TestAdapter::new(vec![task("one", Duration::from_secs(1))])),
                Arc::new(StaticRollout(TaskRolloutResult::execution_failure(
                    "handled execution failure",
                    telemetry,
                ))),
            )
            .await
            .expect("run");

        let task = &record.tasks[0];
        assert_eq!(task.status, TaskStatus::Error);
        assert_eq!(task.tokens_input, Telemetry::Available(5));
        assert_eq!(task.agent_turns, Telemetry::Available(2));
        assert_eq!(task.retry_count, Telemetry::Available(1));
    }

    #[tokio::test]
    async fn verifier_failure_preserves_rollout_telemetry() {
        let telemetry = TaskTelemetry {
            tokens_input: Telemetry::Available(8),
            tokens_output: Telemetry::Available(3),
            total_turns: Telemetry::Available(2),
            ..TaskTelemetry::default()
        };
        let record = Runner::new(RunnerConfig::default())
            .run(
                Arc::new(TestAdapter::with_verifier_error(vec![task(
                    "one",
                    Duration::from_secs(1),
                )])),
                Arc::new(StaticRollout(TaskRolloutResult::completed(
                    serde_json::json!({}),
                    telemetry,
                ))),
            )
            .await
            .expect("run");

        let task = &record.tasks[0];
        assert_eq!(task.status, TaskStatus::Error);
        assert_eq!(task.tokens_input, Telemetry::Available(8));
        assert_eq!(task.tokens_output, Telemetry::Available(3));
        assert_eq!(task.agent_turns, Telemetry::Available(2));
    }

    #[tokio::test]
    async fn timeout_and_bare_rollout_error_do_not_invent_zero_telemetry() {
        let timeout_record = Runner::new(RunnerConfig::default())
            .run(
                Arc::new(TestAdapter::new(vec![task(
                    "timeout",
                    Duration::from_millis(1),
                )])),
                Arc::new(TimeoutRollout),
            )
            .await
            .expect("timeout record");
        let error_record = Runner::new(RunnerConfig::default())
            .run(
                Arc::new(TestAdapter::new(vec![task(
                    "error",
                    Duration::from_secs(1),
                )])),
                Arc::new(ErrorRollout),
            )
            .await
            .expect("error record");

        for task in [&timeout_record.tasks[0], &error_record.tasks[0]] {
            assert!(matches!(task.tokens_input, Telemetry::Unavailable));
            assert!(matches!(task.tokens_output, Telemetry::Unavailable));
            assert!(matches!(task.cost_usd.as_ref(), Telemetry::Unavailable));
            assert!(matches!(task.agent_turns, Telemetry::Unavailable));
            assert!(matches!(task.retry_count, Telemetry::Unavailable));
        }
        assert_eq!(timeout_record.tasks[0].status, TaskStatus::Timeout);
        assert_eq!(error_record.tasks[0].status, TaskStatus::Error);
    }

    #[tokio::test]
    async fn retry_and_turn_counts_aggregate() {
        let record = Runner::new(RunnerConfig::default())
            .run(
                Arc::new(TestAdapter::new(vec![
                    task("first", Duration::from_secs(1)),
                    task("second", Duration::from_secs(1)),
                ])),
                Arc::new(TurnRetryRollout),
            )
            .await
            .expect("run");

        assert_eq!(record.metrics.total_agent_turns, Telemetry::Available(5));
        assert_eq!(record.metrics.total_retries, Telemetry::Available(3));
    }
}
