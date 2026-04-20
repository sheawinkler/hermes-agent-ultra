//! Output verification for benchmark tasks.
//!
//! A [`Verifier`] is the component that decides whether an agent's final
//! state / output satisfies the benchmark's success criteria. For
//! Terminal-Bench, this is typically running a test suite inside the same
//! Docker sandbox as the task.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::adapter::TaskSpec;
use crate::error::EvalResult;

/// The result of a single verification attempt.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerificationOutcome {
    /// Primary pass/fail signal in [0.0, 1.0]. Most benchmarks use binary
    /// (0.0 or 1.0) but partial scoring is supported.
    pub score: f64,
    /// Whether the task should count as passed for pass@1 aggregation.
    pub passed: bool,
    /// Free-form human-readable detail (test output, diff, error stack).
    pub detail: Option<String>,
    /// Optional structured metadata for post-hoc analysis.
    pub metadata: serde_json::Value,
}

impl VerificationOutcome {
    pub fn pass() -> Self {
        Self {
            score: 1.0,
            passed: true,
            detail: None,
            metadata: serde_json::Value::Null,
        }
    }

    pub fn fail(reason: impl Into<String>) -> Self {
        Self {
            score: 0.0,
            passed: false,
            detail: Some(reason.into()),
            metadata: serde_json::Value::Null,
        }
    }
}

/// Interface for benchmark-specific verification. Implementations are
/// typically Docker-script runners or pure-in-process checkers.
#[async_trait]
pub trait Verifier: Send + Sync {
    async fn verify(
        &self,
        task: &TaskSpec,
        agent_final_state: &serde_json::Value,
    ) -> EvalResult<VerificationOutcome>;
}
