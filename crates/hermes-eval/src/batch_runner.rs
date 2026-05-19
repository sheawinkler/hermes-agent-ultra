//! Batch runner — execute multiple benchmark runs with different configurations.
//!
//! Supports:
//! - Running the same benchmark across multiple models
//! - Running multiple benchmarks in sequence
//! - Parallel execution with configurable concurrency
//! - Result aggregation and comparison
//! - Resume from checkpoint on failure

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::adapter::BenchmarkAdapter;
use crate::error::{EvalError, EvalResult};
use crate::reporter::{JsonReporter, Reporter};
use crate::result::RunRecord;
use crate::runner::{Runner, RunnerConfig, TaskRollout};

// ---------------------------------------------------------------------------
// Batch configuration
// ---------------------------------------------------------------------------

/// A single run specification within a batch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchRunSpec {
    /// Unique identifier for this run.
    pub run_id: String,
    /// Model to use.
    pub model: String,
    /// Benchmark adapter identifier.
    pub benchmark: String,
    /// Optional task filter.
    pub task_filter: Option<String>,
    /// Maximum tasks to run.
    pub max_tasks: Option<u32>,
    /// Concurrency level.
    pub concurrency: Option<u32>,
}

/// Batch runner configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchConfig {
    /// List of runs to execute.
    pub runs: Vec<BatchRunSpec>,
    /// Output directory for results.
    pub output_dir: PathBuf,
    /// Whether to continue on individual run failure.
    pub continue_on_error: bool,
    /// Whether to skip runs that already have results.
    pub skip_completed: bool,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            runs: Vec::new(),
            output_dir: PathBuf::from("./eval-results"),
            continue_on_error: true,
            skip_completed: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Batch result
// ---------------------------------------------------------------------------

/// Result of a batch run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchResult {
    pub completed: Vec<BatchRunResult>,
    pub failed: Vec<BatchRunError>,
    pub skipped: Vec<String>,
    pub total_duration_secs: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchRunResult {
    pub run_id: String,
    pub model: String,
    pub benchmark: String,
    pub pass_at_1: f64,
    pub total_tasks: u32,
    pub passed: u32,
    pub duration_secs: f64,
    pub result_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BatchRunError {
    pub run_id: String,
    pub error: String,
}

// ---------------------------------------------------------------------------
// Batch runner
// ---------------------------------------------------------------------------

/// Execute a batch of benchmark runs.
pub struct BatchRunner {
    config: BatchConfig,
}

impl BatchRunner {
    pub fn new(config: BatchConfig) -> Self {
        Self { config }
    }

    /// Execute all runs in the batch.
    ///
    /// `adapter_factory` returns an adapter for each run spec.
    /// `rollout_factory` returns a rollout for each run spec.
    pub async fn run_all<A, R>(
        &self,
        adapter_factory: impl Fn(&BatchRunSpec) -> Option<Arc<A>>,
        rollout_factory: impl Fn(&BatchRunSpec) -> Arc<R>,
    ) -> EvalResult<BatchResult>
    where
        A: BenchmarkAdapter + Send + Sync + 'static,
        R: TaskRollout + Send + Sync + 'static,
    {
        let start = Instant::now();
        let mut completed = Vec::new();
        let mut failed = Vec::new();
        let mut skipped = Vec::new();

        // Ensure output directory exists
        std::fs::create_dir_all(&self.config.output_dir).map_err(|e| {
            EvalError::Other(format!(
                "Cannot create output dir {}: {}",
                self.config.output_dir.display(),
                e
            ))
        })?;

        for spec in &self.config.runs {
            // Check if already completed
            if self.config.skip_completed {
                let result_path = self.result_path(&spec.run_id);
                if result_path.exists() {
                    tracing::info!(run_id = %spec.run_id, "Skipping completed run");
                    skipped.push(spec.run_id.clone());
                    continue;
                }
            }

            tracing::info!(
                run_id = %spec.run_id,
                model = %spec.model,
                benchmark = %spec.benchmark,
                "Starting batch run"
            );

            let adapter = match adapter_factory(spec) {
                Some(a) => a,
                None => {
                    let err = format!("No adapter for benchmark '{}'", spec.benchmark);
                    if self.config.continue_on_error {
                        failed.push(BatchRunError {
                            run_id: spec.run_id.clone(),
                            error: err,
                        });
                        continue;
                    } else {
                        return Err(EvalError::TaskExecution(err));
                    }
                }
            };

            let rollout = rollout_factory(spec);
            let runner_config = RunnerConfig {
                model: spec.model.clone(),
                concurrency: spec.concurrency.unwrap_or(4),
                max_tasks: spec.max_tasks,
                task_filter: spec.task_filter.clone(),
                continue_on_error: true,
                ..Default::default()
            };

            let run_start = Instant::now();
            match Runner::new(runner_config).run(adapter, rollout).await {
                Ok(record) => {
                    let result_path = self.result_path(&spec.run_id);
                    if let Err(e) = self.save_record(&record, &result_path) {
                        tracing::warn!(error = %e, "Failed to save run record");
                    }

                    completed.push(BatchRunResult {
                        run_id: spec.run_id.clone(),
                        model: spec.model.clone(),
                        benchmark: spec.benchmark.clone(),
                        pass_at_1: record.metrics.pass_at_1,
                        total_tasks: record.metrics.total,
                        passed: record.metrics.passed,
                        duration_secs: run_start.elapsed().as_secs_f64(),
                        result_path: result_path.display().to_string(),
                    });
                }
                Err(e) => {
                    let err_msg = e.to_string();
                    tracing::error!(run_id = %spec.run_id, error = %err_msg, "Run failed");
                    if self.config.continue_on_error {
                        failed.push(BatchRunError {
                            run_id: spec.run_id.clone(),
                            error: err_msg,
                        });
                    } else {
                        return Err(e);
                    }
                }
            }
        }

        Ok(BatchResult {
            completed,
            failed,
            skipped,
            total_duration_secs: start.elapsed().as_secs_f64(),
        })
    }

    /// Generate a comparison table from batch results.
    pub fn comparison_table(result: &BatchResult) -> String {
        let mut table = String::new();
        table.push_str("| Run ID | Model | Benchmark | Pass@1 | Tasks | Duration |\n");
        table.push_str("|--------|-------|-----------|--------|-------|----------|\n");
        for r in &result.completed {
            table.push_str(&format!(
                "| {} | {} | {} | {:.1}% | {}/{} | {:.1}s |\n",
                r.run_id,
                r.model,
                r.benchmark,
                r.pass_at_1 * 100.0,
                r.passed,
                r.total_tasks,
                r.duration_secs,
            ));
        }
        for f in &result.failed {
            table.push_str(&format!(
                "| {} | — | — | FAILED | — | {} |\n",
                f.run_id, f.error
            ));
        }
        table
    }

    fn result_path(&self, run_id: &str) -> PathBuf {
        self.config.output_dir.join(format!("{}.json", run_id))
    }

    fn save_record(&self, record: &RunRecord, path: &Path) -> Result<(), EvalError> {
        let reporter = JsonReporter;
        reporter.write_run(record, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_config_default() {
        let cfg = BatchConfig::default();
        assert!(cfg.continue_on_error);
        assert!(cfg.skip_completed);
    }

    #[test]
    fn comparison_table_format() {
        let result = BatchResult {
            completed: vec![BatchRunResult {
                run_id: "run-1".into(),
                model: "gpt-4o".into(),
                benchmark: "tblite".into(),
                pass_at_1: 0.85,
                total_tasks: 100,
                passed: 85,
                duration_secs: 120.5,
                result_path: "/tmp/run-1.json".into(),
            }],
            failed: vec![],
            skipped: vec![],
            total_duration_secs: 120.5,
        };
        let table = BatchRunner::comparison_table(&result);
        assert!(table.contains("gpt-4o"));
        assert!(table.contains("85.0%"));
    }
}
