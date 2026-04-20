//! OpenThoughts **TBLite** minimal smoke adapter.
//!
//! Does **not** download HuggingFace datasets or run Docker — it exposes two tiny
//! [`TaskSpec`](crate::TaskSpec) entries whose metadata matches the upstream
//! benchmark id (`NousResearch/openthoughts-tblite`) for wiring checks and CI.
//! Verification is a no-op pass suitable only for smoke; real evals must swap
//! in a Docker- or script-based [`Verifier`](crate::verifier::Verifier).

use std::time::Duration;

use async_trait::async_trait;
use serde_json::json;

use crate::adapter::{BenchmarkAdapter, BenchmarkMetadata, TaskSpec};
use crate::error::EvalResult;
use crate::runner::TaskRollout;
use crate::verifier::{VerificationOutcome, Verifier};

/// Fixed smoke tasks (subset of TBLite-style instructions; no external I/O).
pub fn tblite_smoke_tasks() -> Vec<TaskSpec> {
    vec![
        TaskSpec {
            task_id: "tblite_smoke_01".into(),
            category: Some("smoke".into()),
            instruction: "TBLite smoke task 01: stub rollout only (no agent).".into(),
            context: json!({
                "benchmark": "openthoughts-tblite",
                "smoke": true,
                "upstream_dataset": "NousResearch/openthoughts-tblite",
            }),
            timeout: Duration::from_secs(30),
        },
        TaskSpec {
            task_id: "tblite_smoke_02".into(),
            category: Some("smoke".into()),
            instruction: "TBLite smoke task 02: stub rollout only (no agent).".into(),
            context: json!({
                "benchmark": "openthoughts-tblite",
                "smoke": true,
                "upstream_dataset": "NousResearch/openthoughts-tblite",
            }),
            timeout: Duration::from_secs(30),
        },
    ]
}

/// Adapter that loads [`tblite_smoke_tasks`] and uses [`SmokePassVerifier`].
#[derive(Debug, Default, Clone, Copy)]
pub struct TbliteSmokeAdapter;

#[async_trait]
impl BenchmarkAdapter for TbliteSmokeAdapter {
    fn metadata(&self) -> BenchmarkMetadata {
        BenchmarkMetadata {
            id: "openthoughts-tblite-smoke".into(),
            name: "OpenThoughts TBLite (smoke)".into(),
            source: "NousResearch/openthoughts-tblite".into(),
            version: "0.0.0-smoke".into(),
        }
    }

    async fn load_tasks(&self) -> EvalResult<Vec<TaskSpec>> {
        Ok(tblite_smoke_tasks())
    }

    fn verifier(&self) -> Box<dyn Verifier> {
        Box::new(SmokePassVerifier)
    }
}

/// Verifier that always passes and echoes the agent state into `metadata` for debugging.
#[derive(Debug, Default, Clone, Copy)]
pub struct SmokePassVerifier;

#[async_trait]
impl Verifier for SmokePassVerifier {
    async fn verify(
        &self,
        task: &TaskSpec,
        agent_final_state: &serde_json::Value,
    ) -> EvalResult<VerificationOutcome> {
        Ok(VerificationOutcome {
            score: 1.0,
            passed: true,
            detail: Some(format!("tblite smoke pass ({})", task.task_id)),
            metadata: agent_final_state.clone(),
        })
    }
}

/// Rollout stub: returns a JSON blob with the task id (no LLM, no tools).
#[derive(Debug, Default, Clone, Copy)]
pub struct NoopRollout;

#[async_trait]
impl TaskRollout for NoopRollout {
    async fn execute(&self, task: &TaskSpec) -> EvalResult<serde_json::Value> {
        Ok(json!({
            "smoke": true,
            "task_id": task.task_id,
            "note": "noop rollout — replace with hermes-agent driver for real evals",
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::{Runner, RunnerConfig};
    use std::sync::Arc;

    #[tokio::test]
    async fn smoke_adapter_loads_two_tasks() {
        let a = TbliteSmokeAdapter;
        let tasks = a.load_tasks().await.unwrap();
        assert_eq!(tasks.len(), 2);
        assert!(tasks[0].task_id.starts_with("tblite_smoke_"));
    }

    #[tokio::test]
    async fn smoke_run_passes_both_tasks() {
        let runner = Runner::new(RunnerConfig {
            model: "test-model".into(),
            concurrency: 2,
            ..Default::default()
        });
        let record = runner
            .run(
                Arc::new(TbliteSmokeAdapter::default()),
                Arc::new(NoopRollout),
            )
            .await
            .expect("smoke run");
        assert_eq!(record.metrics.total, 2);
        assert_eq!(record.metrics.passed, 2);
        assert_eq!(record.metrics.pass_at_1, 1.0);
        assert!(record
            .tasks
            .iter()
            .all(|t| t.status == crate::TaskStatus::Passed));
    }
}
