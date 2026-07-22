//! Run / task result records for evaluation runs.

use chrono::{DateTime, Utc};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize};
use std::time::Duration;

use crate::adapter::BenchmarkMetadata;
use crate::error::{EvalError, EvalResult};
use crate::verifier::VerificationOutcome;

/// A measurement that was either observed authoritatively or was unavailable.
///
/// `Unavailable` is deliberately distinct from a measured zero.  Aggregate
/// metrics retain this distinction rather than treating absent telemetry as
/// free, empty, or instantaneous.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "availability", content = "value", rename_all = "snake_case")]
pub enum Telemetry<T> {
    Available(T),
    Unavailable,
}

impl<T> Telemetry<T> {
    pub const fn available(value: T) -> Self {
        Self::Available(value)
    }

    pub const fn unavailable() -> Self {
        Self::Unavailable
    }

    pub const fn is_available(&self) -> bool {
        matches!(self, Self::Available(_))
    }

    pub fn as_ref(&self) -> Telemetry<&T> {
        match self {
            Self::Available(value) => Telemetry::Available(value),
            Self::Unavailable => Telemetry::Unavailable,
        }
    }
}

impl<T: Copy> Telemetry<T> {
    pub const fn value(&self) -> Option<T> {
        match self {
            Self::Available(value) => Some(*value),
            Self::Unavailable => None,
        }
    }
}

impl<T> Default for Telemetry<T> {
    fn default() -> Self {
        Self::Unavailable
    }
}

/// Source for a cost attached to rollout usage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingSource {
    /// The estimate attached to `hermes_core::AgentResult.usage`.
    AgentResultUsageEstimate,
    /// Cost reported directly by the model provider.
    ProviderReported,
    /// A separately declared estimate supplied by another rollout implementation.
    ExternalEstimate,
}

/// Identifies the price source and the model/provider to which it applies.
///
/// Model and provider are independently unavailable when the authoritative
/// result does not identify them.  A configured route is not substituted for
/// an observed route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PricingProvenance {
    pub source: PricingSource,
    #[serde(default)]
    pub provider: Telemetry<String>,
    #[serde(default)]
    pub model: Telemetry<String>,
}

impl PricingProvenance {
    pub fn new(
        source: PricingSource,
        provider: Telemetry<String>,
        model: Telemetry<String>,
    ) -> Self {
        Self {
            source,
            provider,
            model,
        }
    }

    pub fn agent_result_usage_estimate() -> Self {
        Self::new(
            PricingSource::AgentResultUsageEstimate,
            Telemetry::Unavailable,
            Telemetry::Unavailable,
        )
    }
}

/// A validated USD cost with the price provenance that produced it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CostUsd {
    usd: f64,
    pricing: PricingProvenance,
}

impl CostUsd {
    /// Reject non-finite and negative values, including signed negative zero,
    /// at the telemetry boundary.
    pub fn new(usd: f64, pricing: PricingProvenance) -> EvalResult<Self> {
        if !usd.is_finite() || usd.is_sign_negative() {
            return Err(EvalError::TaskExecution(
                "cost_usd must be finite and nonnegative".into(),
            ));
        }
        Ok(Self { usd, pricing })
    }

    pub const fn usd(&self) -> f64 {
        self.usd
    }

    pub const fn pricing(&self) -> &PricingProvenance {
        &self.pricing
    }
}

impl<'de> Deserialize<'de> for CostUsd {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct RawCostUsd {
            usd: f64,
            pricing: PricingProvenance,
        }

        let raw = RawCostUsd::deserialize(deserializer)?;
        Self::new(raw.usd, raw.pricing).map_err(|error| D::Error::custom(error.to_string()))
    }
}

/// Identifiers and evidence references surfaced by the authoritative rollout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutProvenance {
    #[serde(default)]
    pub run_id: Telemetry<String>,
    #[serde(default)]
    pub evidence_id: Telemetry<String>,
    #[serde(default)]
    pub evidence_digest: Telemetry<String>,
}

impl RolloutProvenance {
    pub fn unavailable() -> Self {
        Self {
            run_id: Telemetry::Unavailable,
            evidence_id: Telemetry::Unavailable,
            evidence_digest: Telemetry::Unavailable,
        }
    }
}

impl Default for RolloutProvenance {
    fn default() -> Self {
        Self::unavailable()
    }
}

/// Authoritative telemetry observed during one rollout.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskTelemetry {
    #[serde(default)]
    pub tokens_input: Telemetry<u64>,
    #[serde(default)]
    pub tokens_output: Telemetry<u64>,
    #[serde(default)]
    pub cost_usd: Telemetry<CostUsd>,
    #[serde(default)]
    pub total_turns: Telemetry<u32>,
    #[serde(default)]
    pub retry_count: Telemetry<u32>,
    #[serde(default)]
    pub provenance: RolloutProvenance,
}

impl TaskTelemetry {
    pub fn unavailable() -> Self {
        Self {
            tokens_input: Telemetry::Unavailable,
            tokens_output: Telemetry::Unavailable,
            cost_usd: Telemetry::Unavailable,
            total_turns: Telemetry::Unavailable,
            retry_count: Telemetry::Unavailable,
            provenance: RolloutProvenance::unavailable(),
        }
    }
}

impl Default for TaskTelemetry {
    fn default() -> Self {
        Self::unavailable()
    }
}

/// Terminal state of a single evaluated task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Passed,
    Failed,
    Timeout,
    Error,
    Skipped,
}

/// Per-task result record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskResult {
    pub task_id: String,
    pub category: Option<String>,
    pub status: TaskStatus,
    /// Legacy verifier material for reporting and diagnostics only. It is not
    /// accepted by `eval_ledger` promotion-authority APIs.
    pub outcome: VerificationOutcome,
    /// Total agent turns observed by the rollout.
    #[serde(default)]
    pub agent_turns: Telemetry<u32>,
    #[serde(default)]
    pub retry_count: Telemetry<u32>,
    pub duration: Duration,
    /// Tokens consumed by the agent across the rollout.
    #[serde(default)]
    pub tokens_input: Telemetry<u64>,
    #[serde(default)]
    pub tokens_output: Telemetry<u64>,
    /// Validated cost and pricing provenance across the rollout.
    #[serde(default)]
    pub cost_usd: Telemetry<CostUsd>,
    /// Run/evidence references supplied by the authoritative rollout.
    #[serde(default)]
    pub provenance: RolloutProvenance,
    pub error: Option<String>,
}

/// Aggregate metrics across all tasks in a run.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AggregateMetrics {
    pub total: u32,
    pub passed: u32,
    pub failed: u32,
    pub timeout: u32,
    pub error: u32,
    pub skipped: u32,
    /// Pass@1 = passed / total.
    pub pass_at_1: f64,
    pub total_duration: Duration,
    #[serde(default)]
    pub total_tokens_input: Telemetry<u64>,
    #[serde(default)]
    pub total_tokens_output: Telemetry<u64>,
    #[serde(default)]
    pub total_cost_usd: Telemetry<f64>,
    #[serde(default)]
    pub total_agent_turns: Telemetry<u64>,
    #[serde(default)]
    pub total_retries: Telemetry<u64>,
}

/// A complete evaluation run record. Written to disk for reproducibility
/// and to power `hermes bench compare`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunRecord {
    pub run_id: String,
    pub benchmark: BenchmarkMetadata,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Seed used for any randomised subsampling, for reproducibility.
    pub seed: u64,
    /// Concurrency level used for the rollout.
    pub concurrency: u32,
    /// Model identifier the agent was driven by, for provenance.
    pub model: String,
    pub tasks: Vec<TaskResult>,
    pub metrics: AggregateMetrics,
}
