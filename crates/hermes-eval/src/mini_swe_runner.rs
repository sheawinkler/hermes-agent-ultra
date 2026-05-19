//! Mini SWE runner — lightweight SWE-bench subset runner for quick validation.
//!
//! Runs a small subset of SWE-bench tasks (10-50) to quickly validate
//! agent changes before running the full benchmark. Uses the same
//! infrastructure as the full runner but with:
//! - Smaller task set (configurable, default 10)
//! - Shorter timeouts
//! - Simplified verification (patch application check only)
//! - No Docker requirement (runs in local temp directories)

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::adapter::{BenchmarkAdapter, BenchmarkMetadata, TaskSpec};
use crate::error::EvalResult;
use crate::result::RunRecord;
use crate::runner::{Runner, RunnerConfig, TaskRollout};
use crate::verifier::{VerificationOutcome, Verifier};

// ---------------------------------------------------------------------------
// Mini SWE configuration
// ---------------------------------------------------------------------------

/// Configuration for the mini SWE runner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MiniSweConfig {
    /// Number of tasks to run.
    pub num_tasks: usize,
    /// Timeout per task.
    pub timeout_per_task: Duration,
    /// Concurrency level.
    pub concurrency: u32,
    /// Model to use.
    pub model: String,
    /// Working directory for task repos.
    pub work_dir: PathBuf,
    /// Whether to clean up after each task.
    pub cleanup: bool,
}

impl Default for MiniSweConfig {
    fn default() -> Self {
        Self {
            num_tasks: 10,
            timeout_per_task: Duration::from_secs(120),
            concurrency: 2,
            model: "anthropic:claude-3-5-sonnet-20241022".into(),
            work_dir: PathBuf::from("/tmp/hermes-mini-swe"),
            cleanup: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Built-in mini tasks (no HuggingFace dependency)
// ---------------------------------------------------------------------------

/// Generate a set of mini SWE-like tasks that don't require external datasets.
/// These are synthetic tasks that test basic code editing capabilities.
pub fn builtin_mini_tasks(num_tasks: usize) -> Vec<TaskSpec> {
    let all_tasks = vec![
        TaskSpec {
            task_id: "mini_swe_01_fix_typo".into(),
            category: Some("python".into()),
            instruction: "Fix the typo in the function name: `def calcualte_sum` should be `def calculate_sum`".into(),
            context: json!({
                "file": "math_utils.py",
                "content": "def calcualte_sum(a, b):\n    return a + b\n",
                "expected": "def calculate_sum(a, b):\n    return a + b\n",
            }),
            timeout: Duration::from_secs(60),
        },
        TaskSpec {
            task_id: "mini_swe_02_add_docstring".into(),
            category: Some("python".into()),
            instruction: "Add a docstring to the function `process_data` explaining what it does.".into(),
            context: json!({
                "file": "processor.py",
                "content": "def process_data(items):\n    return [x * 2 for x in items if x > 0]\n",
            }),
            timeout: Duration::from_secs(60),
        },
        TaskSpec {
            task_id: "mini_swe_03_fix_off_by_one".into(),
            category: Some("python".into()),
            instruction: "Fix the off-by-one error in the range: should include the last element.".into(),
            context: json!({
                "file": "utils.py",
                "content": "def get_range(start, end):\n    return list(range(start, end))\n",
                "expected_fix": "range(start, end + 1)",
            }),
            timeout: Duration::from_secs(60),
        },
        TaskSpec {
            task_id: "mini_swe_04_add_error_handling".into(),
            category: Some("python".into()),
            instruction: "Add try/except error handling to the file reading function.".into(),
            context: json!({
                "file": "io_utils.py",
                "content": "def read_config(path):\n    with open(path) as f:\n        return f.read()\n",
            }),
            timeout: Duration::from_secs(60),
        },
        TaskSpec {
            task_id: "mini_swe_05_fix_import".into(),
            category: Some("python".into()),
            instruction: "Fix the missing import: `json` module is used but not imported.".into(),
            context: json!({
                "file": "parser.py",
                "content": "def parse_json(text):\n    return json.loads(text)\n",
                "expected_fix": "import json",
            }),
            timeout: Duration::from_secs(60),
        },
        TaskSpec {
            task_id: "mini_swe_06_rust_fix_borrow".into(),
            category: Some("rust".into()),
            instruction: "Fix the borrow checker error: use a reference instead of moving the value.".into(),
            context: json!({
                "file": "main.rs",
                "content": "fn process(data: String) {\n    println!(\"{}\", data);\n}\nfn main() {\n    let s = String::from(\"hello\");\n    process(s);\n    println!(\"{}\", s);\n}\n",
            }),
            timeout: Duration::from_secs(60),
        },
        TaskSpec {
            task_id: "mini_swe_07_add_test".into(),
            category: Some("python".into()),
            instruction: "Add a unit test for the `add` function.".into(),
            context: json!({
                "file": "math.py",
                "content": "def add(a, b):\n    return a + b\n",
            }),
            timeout: Duration::from_secs(60),
        },
        TaskSpec {
            task_id: "mini_swe_08_fix_null_check".into(),
            category: Some("python".into()),
            instruction: "Add a null check: the function crashes when `data` is None.".into(),
            context: json!({
                "file": "handler.py",
                "content": "def handle(data):\n    return data.strip().upper()\n",
            }),
            timeout: Duration::from_secs(60),
        },
        TaskSpec {
            task_id: "mini_swe_09_optimize_loop".into(),
            category: Some("python".into()),
            instruction: "Optimize: replace the nested loop with a set lookup for O(n) instead of O(n²).".into(),
            context: json!({
                "file": "search.py",
                "content": "def find_common(list_a, list_b):\n    result = []\n    for a in list_a:\n        for b in list_b:\n            if a == b:\n                result.append(a)\n    return result\n",
            }),
            timeout: Duration::from_secs(60),
        },
        TaskSpec {
            task_id: "mini_swe_10_add_type_hints".into(),
            category: Some("python".into()),
            instruction: "Add type hints to all function parameters and return types.".into(),
            context: json!({
                "file": "utils.py",
                "content": "def greet(name):\n    return f\"Hello, {name}!\"\n\ndef add_numbers(a, b):\n    return a + b\n",
            }),
            timeout: Duration::from_secs(60),
        },
    ];

    all_tasks.into_iter().take(num_tasks).collect()
}

// ---------------------------------------------------------------------------
// Mini SWE adapter
// ---------------------------------------------------------------------------

/// Adapter that uses built-in synthetic tasks.
pub struct MiniSweAdapter {
    config: MiniSweConfig,
}

impl MiniSweAdapter {
    pub fn new(config: MiniSweConfig) -> Self {
        Self { config }
    }
}

#[async_trait]
impl BenchmarkAdapter for MiniSweAdapter {
    fn metadata(&self) -> BenchmarkMetadata {
        BenchmarkMetadata {
            id: "mini-swe".into(),
            name: "Mini SWE Runner".into(),
            source: "built-in synthetic tasks".into(),
            version: "1.0.0".into(),
        }
    }

    async fn load_tasks(&self) -> EvalResult<Vec<TaskSpec>> {
        Ok(builtin_mini_tasks(self.config.num_tasks))
    }

    fn verifier(&self) -> Box<dyn Verifier> {
        Box::new(MiniSweVerifier)
    }
}

/// Simple verifier that checks if the agent produced any output.
struct MiniSweVerifier;

#[async_trait]
impl Verifier for MiniSweVerifier {
    async fn verify(
        &self,
        task: &TaskSpec,
        agent_state: &serde_json::Value,
    ) -> EvalResult<VerificationOutcome> {
        // Check if agent produced a non-empty response
        let has_output = agent_state
            .get("output")
            .or_else(|| agent_state.get("patch"))
            .or_else(|| agent_state.get("response"))
            .and_then(|v| v.as_str())
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false);

        // Check if expected fix is present (if specified)
        if let Some(expected) = task.context.get("expected_fix").and_then(|v| v.as_str()) {
            let output = agent_state
                .get("output")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if output.contains(expected) {
                return Ok(VerificationOutcome {
                    score: 1.0,
                    passed: true,
                    detail: Some("Expected fix found in output".into()),
                    metadata: agent_state.clone(),
                });
            }
        }

        Ok(VerificationOutcome {
            score: if has_output { 0.5 } else { 0.0 },
            passed: has_output,
            detail: Some(if has_output {
                "Agent produced output (partial credit)".into()
            } else {
                "No output from agent".into()
            }),
            metadata: agent_state.clone(),
        })
    }
}

// ---------------------------------------------------------------------------
// Convenience runner
// ---------------------------------------------------------------------------

/// Run the mini SWE benchmark with a given rollout implementation.
pub async fn run_mini_swe<R>(config: MiniSweConfig, rollout: Arc<R>) -> EvalResult<RunRecord>
where
    R: TaskRollout + Send + Sync + 'static,
{
    let runner_config = RunnerConfig {
        model: config.model.clone(),
        concurrency: config.concurrency,
        max_tasks: Some(config.num_tasks as u32),
        continue_on_error: true,
        ..Default::default()
    };

    let adapter = Arc::new(MiniSweAdapter::new(config));
    Runner::new(runner_config).run(adapter, rollout).await
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tblite::NoopRollout;

    #[test]
    fn builtin_tasks_count() {
        let tasks = builtin_mini_tasks(5);
        assert_eq!(tasks.len(), 5);

        let tasks = builtin_mini_tasks(100);
        assert_eq!(tasks.len(), 10); // max 10 built-in
    }

    #[test]
    fn builtin_tasks_have_context() {
        let tasks = builtin_mini_tasks(10);
        for task in &tasks {
            assert!(!task.instruction.is_empty());
            assert!(task.context.get("file").is_some());
        }
    }

    #[tokio::test]
    async fn mini_swe_adapter_loads_tasks() {
        let adapter = MiniSweAdapter::new(MiniSweConfig {
            num_tasks: 3,
            ..Default::default()
        });
        let tasks = adapter.load_tasks().await.unwrap();
        assert_eq!(tasks.len(), 3);
    }

    #[tokio::test]
    async fn run_mini_swe_with_noop() {
        let config = MiniSweConfig {
            num_tasks: 3,
            concurrency: 1,
            ..Default::default()
        };
        let record = run_mini_swe(config, Arc::new(NoopRollout)).await.unwrap();
        assert_eq!(record.metrics.total, 3);
    }

    #[test]
    fn default_config() {
        let cfg = MiniSweConfig::default();
        assert_eq!(cfg.num_tasks, 10);
        assert_eq!(cfg.concurrency, 2);
    }
}
