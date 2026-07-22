//! Versioned, fail-closed evaluation receipts and their promotion ledger.
//!
//! A receipt is deliberately assembled from a verified replay plan plus typed
//! [`RunRecord`] task data.  Free-form verifier details and JSON metadata remain
//! available as diagnostics, but cannot enter the promotion-authority path.

use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use hermes_core::MemoryExportEvidenceDigest;
use serde::Serialize;
use thiserror::Error;
use uuid::Uuid;

#[cfg(feature = "agent-loop")]
use hermes_agent::{
    DeterministicReplayReport, ExactVerifiedRunEvidence, ReplayEvidenceCount,
    ReplayEvidenceCounters, ReplayFidelity as ReplayPlanFidelity, VerifiedRunReplayPlan,
};

use crate::result::{RunRecord, TaskStatus, Telemetry};
use crate::verifier::VerificationOutcome;

/// Stable identifier for the serialized evaluation receipt format.
pub const EVALUATION_RECEIPT_SCHEMA: &str = "hermes.evaluation.receipt";
/// Current version of [`EVALUATION_RECEIPT_SCHEMA`].
pub const EVALUATION_RECEIPT_SCHEMA_VERSION: u32 = 1;
/// Digest algorithm used for receipt canonical payloads.
pub const EVALUATION_RECEIPT_DIGEST_ALGORITHM: &str = "sha-256";

/// Failure to construct or verify promotion-authoritative evaluation evidence.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EvaluationReceiptError {
    #[error("missing required evaluation evidence: {field}")]
    Missing { field: &'static str },

    #[error("malformed evaluation evidence: {field}")]
    Malformed { field: &'static str },

    #[error("inconsistent evaluation evidence: {field}")]
    Inconsistent { field: &'static str },

    #[error("evaluation receipt canonicalization failed")]
    Canonicalization,
}

/// Result alias for typed evaluation receipt operations.
pub type EvaluationReceiptResult<T> = Result<T, EvaluationReceiptError>;

/// Canonical lowercase hexadecimal SHA-256 evidence.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct EvidenceDigest(String);

impl EvidenceDigest {
    /// Parse a lowercase 64-character SHA-256 digest.
    pub fn sha256(value: impl Into<String>) -> EvaluationReceiptResult<Self> {
        Self::parse("SHA-256 digest", value.into())
    }

    fn parse(field: &'static str, value: String) -> EvaluationReceiptResult<Self> {
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(EvaluationReceiptError::Malformed { field });
        }
        Ok(Self(value))
    }

    /// Borrow the lowercase hexadecimal representation.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EvidenceDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
struct EvaluationRunId(String);

impl EvaluationRunId {
    fn parse(value: impl Into<String>) -> EvaluationReceiptResult<Self> {
        let value = value.into();
        let parsed = Uuid::parse_str(&value)
            .map_err(|_| EvaluationReceiptError::Malformed { field: "run ID" })?;
        let canonical = parsed.hyphenated().to_string();
        if value != canonical {
            return Err(EvaluationReceiptError::Malformed { field: "run ID" });
        }
        Ok(Self(value))
    }

    fn as_str(&self) -> &str {
        &self.0
    }
}

fn checked_label(
    field: &'static str,
    value: impl Into<String>,
    maximum_length: usize,
) -> EvaluationReceiptResult<String> {
    let value = value.into();
    if value.is_empty() {
        return Err(EvaluationReceiptError::Missing { field });
    }
    if value.len() > maximum_length
        || !value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b'-' | b'/' | b':' | b'@' | b'+')
        })
    {
        return Err(EvaluationReceiptError::Malformed { field });
    }
    Ok(value)
}

fn checked_nonnegative_number(
    field: &'static str,
    value: f64,
) -> EvaluationReceiptResult<f64> {
    if !value.is_finite() || value.is_sign_negative() {
        return Err(EvaluationReceiptError::Malformed { field });
    }
    Ok(value)
}

/// Immutable version and content digest for the baseline used by an evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct BaselineEvidence {
    version: String,
    digest: EvidenceDigest,
}

impl BaselineEvidence {
    pub fn new(
        version: impl Into<String>,
        digest: impl Into<String>,
    ) -> EvaluationReceiptResult<Self> {
        Ok(Self {
            version: checked_label("baseline version", version, 256)?,
            digest: EvidenceDigest::parse("baseline digest", digest.into())?,
        })
    }

    pub fn version(&self) -> &str {
        &self.version
    }

    pub fn digest(&self) -> &EvidenceDigest {
        &self.digest
    }
}

/// Fixed benchmark holdout selection, committed content, and deterministic
/// sample cardinality.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FixedHoldoutEvidence {
    id: String,
    digest: EvidenceDigest,
    seed: u64,
    sample_count: u64,
}

impl FixedHoldoutEvidence {
    pub fn new(
        id: impl Into<String>,
        digest: impl Into<String>,
        seed: u64,
        sample_count: u64,
    ) -> EvaluationReceiptResult<Self> {
        if sample_count == 0 {
            return Err(EvaluationReceiptError::Missing {
                field: "fixed holdout sample count",
            });
        }
        Ok(Self {
            id: checked_label("fixed holdout ID", id, 512)?,
            digest: EvidenceDigest::parse("fixed holdout digest", digest.into())?,
            seed,
            sample_count,
        })
    }

    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn digest(&self) -> &EvidenceDigest {
        &self.digest
    }

    pub const fn seed(&self) -> u64 {
        self.seed
    }

    pub const fn sample_count(&self) -> u64 {
        self.sample_count
    }
}

/// Metric recomputed from typed task statuses, never verifier prose or metadata.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetricEvidence {
    name: String,
    value: f64,
}

impl MetricEvidence {
    fn pass_ratio(name: impl Into<String>, passed: u64, total: u64) -> EvaluationReceiptResult<Self> {
        if total == 0 {
            return Err(EvaluationReceiptError::Missing {
                field: "evaluated task results",
            });
        }
        let value = checked_nonnegative_number("metric value", passed as f64 / total as f64)?;
        Ok(Self {
            name: checked_label("metric name", name, 256)?,
            value,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub const fn value(&self) -> f64 {
        self.value
    }
}

/// Typed measurements recomputed from a [`RunRecord`]'s task statuses, costs,
/// and durations. `RunRecord.metrics` is intentionally not trusted here.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvaluationEvidence {
    run_id: EvaluationRunId,
    task_count: u64,
    metric: MetricEvidence,
    cost_usd: f64,
    latency_ms: u64,
    p95_latency_ms: u64,
}

impl EvaluationEvidence {
    /// Recompute the evidence from typed task fields only.
    pub fn from_run_record(
        record: &RunRecord,
        metric_name: impl Into<String>,
    ) -> EvaluationReceiptResult<Self> {
        let run_id = EvaluationRunId::parse(record.run_id.clone())?;
        let total = u64::try_from(record.tasks.len()).map_err(|_| {
            EvaluationReceiptError::Malformed {
                field: "evaluated task results",
            }
        })?;
        if total == 0 {
            return Err(EvaluationReceiptError::Missing {
                field: "evaluated task results",
            });
        }

        let mut passed = 0_u64;
        let mut cost_usd = 0.0_f64;
        let mut latency = Duration::ZERO;
        let mut task_latency_ms = Vec::with_capacity(record.tasks.len());

        for task in &record.tasks {
            let task_cost = match &task.cost_usd {
                Telemetry::Available(cost) => cost.usd(),
                Telemetry::Unavailable => {
                    return Err(EvaluationReceiptError::Missing {
                        field: "task cost telemetry",
                    });
                }
            };
            let task_cost = checked_nonnegative_number("task cost", task_cost)?;
            cost_usd = checked_nonnegative_number("total cost", cost_usd + task_cost)?;
            latency = latency.checked_add(task.duration).ok_or(
                EvaluationReceiptError::Malformed {
                    field: "total latency",
                },
            )?;
            task_latency_ms.push(u64::try_from(task.duration.as_millis()).map_err(|_| {
                EvaluationReceiptError::Malformed {
                    field: "task latency",
                }
            })?);
            if matches!(task.status, TaskStatus::Passed) {
                passed = passed.checked_add(1).ok_or(EvaluationReceiptError::Malformed {
                    field: "passed task count",
                })?;
            }
        }

        let latency_ms = u64::try_from(latency.as_millis()).map_err(|_| {
            EvaluationReceiptError::Malformed {
                field: "total latency",
            }
        })?;
        task_latency_ms.sort_unstable();
        let p95_rank = total
            .checked_mul(95)
            .and_then(|value| value.checked_add(99))
            .ok_or(EvaluationReceiptError::Malformed {
                field: "p95 latency rank",
            })?
            / 100;
        let p95_index = usize::try_from(p95_rank - 1).map_err(|_| {
            EvaluationReceiptError::Malformed {
                field: "p95 latency rank",
            }
        })?;
        let p95_latency_ms = *task_latency_ms.get(p95_index).ok_or(
            EvaluationReceiptError::Malformed {
                field: "p95 latency",
            },
        )?;
        Ok(Self {
            run_id,
            task_count: total,
            metric: MetricEvidence::pass_ratio(metric_name, passed, total)?,
            cost_usd,
            latency_ms,
            p95_latency_ms,
        })
    }

    pub fn run_id(&self) -> &str {
        self.run_id.as_str()
    }

    pub const fn task_count(&self) -> u64 {
        self.task_count
    }

    pub fn metric(&self) -> &MetricEvidence {
        &self.metric
    }

    pub const fn cost_usd(&self) -> f64 {
        self.cost_usd
    }

    /// Sum of all task durations.
    pub const fn latency_ms(&self) -> u64 {
        self.latency_ms
    }

    /// Nearest-rank p95 of typed per-task durations.
    pub const fn p95_latency_ms(&self) -> u64 {
        self.p95_latency_ms
    }
}

/// A content-addressed reproduction artifact that can be resolved by production
/// replay tooling.  The opaque portion is constrained so prose cannot masquerade
/// as an artifact reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReproductionArtifactEvidence {
    reference: String,
    digest: EvidenceDigest,
}

impl ReproductionArtifactEvidence {
    pub fn new(
        reference: impl Into<String>,
        digest: impl Into<String>,
    ) -> EvaluationReceiptResult<Self> {
        let reference = reference.into();
        let Some(opaque) = reference.strip_prefix("artifact://") else {
            return Err(EvaluationReceiptError::Malformed {
                field: "reproduction artifact reference",
            });
        };
        if opaque.is_empty()
            || opaque.len() > 2_048
            || !opaque.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/' | b':')
            })
        {
            return Err(EvaluationReceiptError::Malformed {
                field: "reproduction artifact reference",
            });
        }
        Ok(Self {
            reference,
            digest: EvidenceDigest::parse("reproduction artifact digest", digest.into())?,
        })
    }

    pub fn reference(&self) -> &str {
        &self.reference
    }

    pub fn digest(&self) -> &EvidenceDigest {
        &self.digest
    }
}

/// Version of the evaluator that produced a receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct EvaluatorVersion(String);

impl EvaluatorVersion {
    pub fn new(value: impl Into<String>) -> EvaluationReceiptResult<Self> {
        Ok(Self(checked_label("evaluator version", value, 256)?))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// All non-replay inputs required to bind a verified plan into a receipt.
#[derive(Debug, Clone)]
pub struct EvaluationReceiptInputs {
    baseline: BaselineEvidence,
    holdout: FixedHoldoutEvidence,
    evaluation: EvaluationEvidence,
    reproduction_artifact: ReproductionArtifactEvidence,
    evaluator_version: EvaluatorVersion,
}

impl EvaluationReceiptInputs {
    pub fn new(
        baseline: BaselineEvidence,
        holdout: FixedHoldoutEvidence,
        evaluation: EvaluationEvidence,
        reproduction_artifact: ReproductionArtifactEvidence,
        evaluator_version: EvaluatorVersion,
    ) -> EvaluationReceiptResult<Self> {
        if holdout.sample_count() != evaluation.task_count() {
            return Err(EvaluationReceiptError::Inconsistent {
                field: "fixed holdout sample count and evaluated task count",
            });
        }
        Ok(Self {
            baseline,
            holdout,
            evaluation,
            reproduction_artifact,
            evaluator_version,
        })
    }

    pub fn baseline(&self) -> &BaselineEvidence {
        &self.baseline
    }

    pub fn holdout(&self) -> &FixedHoldoutEvidence {
        &self.holdout
    }

    pub fn evaluation(&self) -> &EvaluationEvidence {
        &self.evaluation
    }

    pub fn reproduction_artifact(&self) -> &ReproductionArtifactEvidence {
        &self.reproduction_artifact
    }

    pub fn evaluator_version(&self) -> &EvaluatorVersion {
        &self.evaluator_version
    }
}

/// Integrity status of the export backing an [`EvaluationReceipt`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunExportIntegrityStatus {
    Verified,
    Unverified,
}

/// Signature verification evidence relative to a caller-supplied policy.
///
/// It is not release authentication or promotion authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SignatureVerificationEvidence {
    proof_digest: EvidenceDigest,
    policy_id: String,
    policy_digest: EvidenceDigest,
}

impl SignatureVerificationEvidence {
    pub fn proof_digest(&self) -> &EvidenceDigest {
        &self.proof_digest
    }

    pub fn policy_id(&self) -> &str {
        &self.policy_id
    }

    pub fn policy_digest(&self) -> &EvidenceDigest {
        &self.policy_digest
    }

    #[cfg(feature = "agent-loop")]
    fn from_exact_evidence(evidence: &ExactVerifiedRunEvidence) -> EvaluationReceiptResult<Self> {
        Ok(Self {
            proof_digest: EvidenceDigest::parse(
                "signature proof digest",
                evidence.proof_digest().to_owned(),
            )?,
            policy_id: checked_label("signature policy ID", evidence.policy_id(), 512)?,
            policy_digest: EvidenceDigest::parse(
                "signature policy digest",
                evidence.policy_digest().to_owned(),
            )?,
        })
    }
}

/// Authentication status of the export backing an [`EvaluationReceipt`].
///
/// JCS encoding, a SHA-256 digest, and an owner-only filesystem path establish
/// integrity, not signer authentication. `SignatureVerified` records detached
/// signature evidence relative to a caller-supplied policy, never promotion
/// authentication.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunExportAuthenticationStatus {
    Unauthenticated,
    SignatureVerified(SignatureVerificationEvidence),
}

/// Deterministic replay fidelity asserted by typed reconstruction evidence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayFidelity {
    Exact,
    MetadataOnly,
    Unavailable,
}

/// A terminal proof accepted by the replay verifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalValidity {
    Verified,
    NotTerminal,
}

/// A typed operation count. Unavailable means the verified replay evidence did
/// not classify that operation kind; it is deliberately distinct from zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "availability", content = "value", rename_all = "snake_case")]
pub enum EvidenceCount {
    Available(u64),
    Unavailable,
}

impl EvidenceCount {
    pub const fn available(value: u64) -> Self {
        Self::Available(value)
    }

    pub const fn unavailable() -> Self {
        Self::Unavailable
    }

    pub const fn value(self) -> Option<u64> {
        match self {
            Self::Available(value) => Some(value),
            Self::Unavailable => None,
        }
    }

    pub const fn is_available(self) -> bool {
        matches!(self, Self::Available(_))
    }
}

/// Closed operation-kind evidence from the verified replay plan.
///
/// Each category is independently available or unavailable. An unavailable
/// category never aliases to zero and prevents promotion authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ReplayOperationCounts {
    model_call_count: EvidenceCount,
    tool_call_count: EvidenceCount,
    delegation_count: EvidenceCount,
    other_count: EvidenceCount,
    failure_count: EvidenceCount,
}

impl ReplayOperationCounts {
    pub const fn model_call_count(&self) -> EvidenceCount {
        self.model_call_count
    }

    pub const fn tool_call_count(&self) -> EvidenceCount {
        self.tool_call_count
    }

    pub const fn delegation_count(&self) -> EvidenceCount {
        self.delegation_count
    }

    pub const fn other_count(&self) -> EvidenceCount {
        self.other_count
    }

    pub const fn failure_count(&self) -> EvidenceCount {
        self.failure_count
    }

    pub const fn all_available(&self) -> bool {
        self.model_call_count.is_available()
            && self.tool_call_count.is_available()
            && self.delegation_count.is_available()
            && self.other_count.is_available()
            && self.failure_count.is_available()
    }

    #[cfg(test)]
    const fn unavailable() -> Self {
        Self {
            model_call_count: EvidenceCount::Unavailable,
            tool_call_count: EvidenceCount::Unavailable,
            delegation_count: EvidenceCount::Unavailable,
            other_count: EvidenceCount::Unavailable,
            failure_count: EvidenceCount::Unavailable,
        }
    }
}


#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct VerifiedRunExportEvidence {
    run_id: EvaluationRunId,
    digest: EvidenceDigest,
    integrity_status: RunExportIntegrityStatus,
    authentication_status: RunExportAuthenticationStatus,
    authentication_authority_available: bool,
    replay_fidelity: ReplayFidelity,
    required_evidence_complete: bool,
    terminal_validity: TerminalValidity,
    operation_counts: ReplayOperationCounts,
    exact_reconstruction_digest: Option<EvidenceDigest>,
}

impl VerifiedRunExportEvidence {
    fn integrity_verified(
        run_id: impl Into<String>,
        digest: impl Into<String>,
        replay_fidelity: ReplayFidelity,
        required_evidence_complete: bool,
        operation_counts: ReplayOperationCounts,
    ) -> EvaluationReceiptResult<Self> {
        Ok(Self {
            run_id: EvaluationRunId::parse(run_id.into())?,
            digest: EvidenceDigest::parse("run-export digest", digest.into())?,
            integrity_status: RunExportIntegrityStatus::Verified,
            authentication_status: RunExportAuthenticationStatus::Unauthenticated,
            authentication_authority_available: false,
            replay_fidelity,
            required_evidence_complete,
            terminal_validity: TerminalValidity::Verified,
            operation_counts,
            exact_reconstruction_digest: None,
        })
    }

    #[cfg(feature = "agent-loop")]
    fn exact_verified(evidence: &ExactVerifiedRunEvidence) -> EvaluationReceiptResult<Self> {
        if !evidence.integrity_verified()
            || !evidence.signature_verified()
            || !evidence.obligations_complete()
            || !matches!(evidence.fidelity(), ReplayPlanFidelity::Exact)
            || evidence.promotion_authoritative()
            || evidence.authorizes_effect_execution()
        {
            return Err(EvaluationReceiptError::Inconsistent {
                field: "exact verified replay evidence",
            });
        }
        Ok(Self {
            run_id: EvaluationRunId::parse(evidence.run_id().to_string())?,
            digest: EvidenceDigest::parse(
                "run-export digest",
                evidence.export_digest().to_owned(),
            )?,
            integrity_status: RunExportIntegrityStatus::Verified,
            authentication_status: RunExportAuthenticationStatus::SignatureVerified(
                SignatureVerificationEvidence::from_exact_evidence(evidence)?,
            ),
            authentication_authority_available: evidence.authentication_authority_available(),
            replay_fidelity: ReplayFidelity::Exact,
            required_evidence_complete: true,
            terminal_validity: TerminalValidity::Verified,
            operation_counts: operation_counts_from_replay_counters(evidence.counters()),
            exact_reconstruction_digest: Some(EvidenceDigest::parse(
                "exact reconstruction digest",
                evidence.reconstruction_digest().to_owned(),
            )?),
        })
    }
}

#[cfg(feature = "agent-loop")]
fn evidence_count_from_replay(value: ReplayEvidenceCount) -> EvidenceCount {
    match value {
        ReplayEvidenceCount::Available(value) => EvidenceCount::Available(value),
        ReplayEvidenceCount::Unavailable => EvidenceCount::Unavailable,
    }
}

#[cfg(feature = "agent-loop")]
fn operation_counts_from_replay_counters(counters: ReplayEvidenceCounters) -> ReplayOperationCounts {
    ReplayOperationCounts {
        model_call_count: evidence_count_from_replay(counters.model_call_count),
        tool_call_count: evidence_count_from_replay(counters.tool_call_count),
        delegation_count: evidence_count_from_replay(counters.delegation_count),
        other_count: evidence_count_from_replay(counters.other_effect_count),
        failure_count: evidence_count_from_replay(counters.failure_count),
    }
}

#[cfg(feature = "agent-loop")]
fn operation_counts_from_plan(plan: &VerifiedRunReplayPlan) -> ReplayOperationCounts {
    operation_counts_from_replay_counters(plan.counters())
}

#[cfg(feature = "agent-loop")]
fn replay_fidelity_from_report(fidelity: ReplayPlanFidelity) -> ReplayFidelity {
    match fidelity {
        ReplayPlanFidelity::Exact => ReplayFidelity::Exact,
        ReplayPlanFidelity::MetadataOnly => ReplayFidelity::MetadataOnly,
        ReplayPlanFidelity::Unavailable => ReplayFidelity::Unavailable,
    }
}

#[cfg(feature = "agent-loop")]
fn evidence_from_verified_plan(
    plan: &VerifiedRunReplayPlan,
    replay_fidelity: ReplayFidelity,
) -> EvaluationReceiptResult<VerifiedRunExportEvidence> {
    if !plan.integrity_verified() {
        return Err(EvaluationReceiptError::Inconsistent {
            field: "verified replay-plan integrity",
        });
    }
    if !plan.terminal_valid() {
        return Err(EvaluationReceiptError::Inconsistent {
            field: "verified replay-plan terminal validity",
        });
    }
    let summary = plan.summary();
    if !summary.evidence_complete() {
        return Err(EvaluationReceiptError::Missing {
            field: "verified replay-plan delivery evidence",
        });
    }
    VerifiedRunExportEvidence::integrity_verified(
        summary.run_id.to_string(),
        summary.digest.clone(),
        replay_fidelity,
        true,
        operation_counts_from_plan(plan),
    )
}

#[cfg(feature = "agent-loop")]
fn evidence_from_reconstruction_report(
    plan: &VerifiedRunReplayPlan,
    report: &DeterministicReplayReport,
) -> EvaluationReceiptResult<VerifiedRunExportEvidence> {
    let summary = plan.summary();
    let artifact_count = u64::try_from(plan.artifact_references().len()).map_err(|_| {
        EvaluationReceiptError::Malformed {
            field: "replay artifact count",
        }
    })?;
    if report.run_id() != summary.run_id
        || report.export_digest() != summary.digest.as_str()
        || report.event_count() != summary.event_count
        || report.artifact_count() != artifact_count
        || report.terminal_reason() != plan.terminal_reason()
        || report.final_outcome() != plan.final_outcome()
        || report.counters() != plan.counters()
    {
        return Err(EvaluationReceiptError::Inconsistent {
            field: "reconstruction report and verified replay plan",
        });
    }
    evidence_from_verified_plan(plan, replay_fidelity_from_report(report.fidelity()))
}

/// No currently available evidence is a nonconstructible release-authority
/// capability bound to owner, tenant, run, and policy digest. P4 must provide
/// that capability before this can become true.
fn export_promotion_eligible(_export: &VerifiedRunExportEvidence) -> bool {
    false
}

fn receipt_promotion_eligible(_receipt: &EvaluationReceipt) -> bool {
    false
}

/// Versioned, canonical and self-digesting evaluation receipt.
///
/// Its fields are private and the only production constructor binds a verified
/// replay plan.  This prevents a free-form [`VerificationOutcome`] from being
/// promoted into authority by construction.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EvaluationReceipt {
    schema: &'static str,
    version: u32,
    run_id: EvaluationRunId,
    run_export_digest: EvidenceDigest,
    run_export_integrity: RunExportIntegrityStatus,
    run_export_authentication: RunExportAuthenticationStatus,
    authentication_authority_available: bool,
    replay_fidelity: ReplayFidelity,
    exact_reconstruction_digest: Option<EvidenceDigest>,
    required_evidence_complete: bool,
    baseline: BaselineEvidence,
    fixed_holdout: FixedHoldoutEvidence,
    metric: MetricEvidence,
    cost_usd: f64,
    latency_ms: u64,
    evaluated_task_count: u64,
    p95_latency_ms: u64,
    operation_counts: ReplayOperationCounts,
    reproduction_artifact: ReproductionArtifactEvidence,
    evaluator_version: EvaluatorVersion,
    terminal_validity: TerminalValidity,
    promotion_eligible: bool,
    receipt_digest: String,
}

impl EvaluationReceipt {
    fn from_verified_evidence(
        export: VerifiedRunExportEvidence,
        inputs: EvaluationReceiptInputs,
    ) -> EvaluationReceiptResult<Self> {
        if export.run_id != inputs.evaluation.run_id {
            return Err(EvaluationReceiptError::Inconsistent {
                field: "evaluation run ID and verified export run ID",
            });
        }
        if !matches!(export.integrity_status, RunExportIntegrityStatus::Verified) {
            return Err(EvaluationReceiptError::Inconsistent {
                field: "run-export integrity",
            });
        }
        if !export.required_evidence_complete {
            return Err(EvaluationReceiptError::Missing {
                field: "verified run-export evidence",
            });
        }
        if !matches!(export.terminal_validity, TerminalValidity::Verified) {
            return Err(EvaluationReceiptError::Inconsistent {
                field: "verified terminal validity",
            });
        }

        if !export.operation_counts.all_available() {
            return Err(EvaluationReceiptError::Missing {
                field: "verified replay operation counts",
            });
        }
        let promotion_eligible = export_promotion_eligible(&export);
        let mut receipt = Self {
            schema: EVALUATION_RECEIPT_SCHEMA,
            version: EVALUATION_RECEIPT_SCHEMA_VERSION,
            run_id: export.run_id,
            run_export_digest: export.digest,
            run_export_integrity: export.integrity_status,
            run_export_authentication: export.authentication_status,
            authentication_authority_available: export.authentication_authority_available,
            replay_fidelity: export.replay_fidelity,
            exact_reconstruction_digest: export.exact_reconstruction_digest,
            required_evidence_complete: export.required_evidence_complete,
            baseline: inputs.baseline,
            fixed_holdout: inputs.holdout,
            metric: inputs.evaluation.metric,
            cost_usd: inputs.evaluation.cost_usd,
            latency_ms: inputs.evaluation.latency_ms,
            evaluated_task_count: inputs.evaluation.task_count,
            p95_latency_ms: inputs.evaluation.p95_latency_ms,
            operation_counts: export.operation_counts,
            reproduction_artifact: inputs.reproduction_artifact,
            evaluator_version: inputs.evaluator_version,
            terminal_validity: export.terminal_validity,
            promotion_eligible,
            receipt_digest: String::new(),
        };
        receipt.receipt_digest = receipt_digest(&receipt.canonical_bytes()?);
        receipt.verify()?;
        Ok(receipt)
    }

    /// Create a structural, integrity-verified receipt directly from one
    /// immutable replay plan. Structural-only plans are metadata-only; a
    /// receipt fails closed unless every typed operation count is available.
    #[cfg(feature = "agent-loop")]
    pub fn from_verified_replay_plan(
        plan: &VerifiedRunReplayPlan,
        inputs: EvaluationReceiptInputs,
    ) -> EvaluationReceiptResult<Self> {
        Self::from_verified_evidence(
            evidence_from_verified_plan(plan, ReplayFidelity::MetadataOnly)?,
            inputs,
        )
    }

    /// Bind a metadata-only reconstruction report to the same immutable plan.
    ///
    /// An exact report must first become [`ExactVerifiedRunEvidence`]; callers
    /// cannot mint signature-bound exact receipt evidence from separate inputs.
    #[cfg(feature = "agent-loop")]
    pub fn from_reconstruction_report(
        plan: &VerifiedRunReplayPlan,
        report: &DeterministicReplayReport,
        inputs: EvaluationReceiptInputs,
    ) -> EvaluationReceiptResult<Self> {
        let evidence = evidence_from_reconstruction_report(plan, report)?;
        if matches!(evidence.replay_fidelity, ReplayFidelity::Exact) {
            return Err(EvaluationReceiptError::Missing {
                field: "exact verified run evidence",
            });
        }
        Self::from_verified_evidence(evidence, inputs)
    }

    /// Bind a nonconstructible exact P3 evidence handoff to typed evaluation
    /// inputs. The result is the only P3 surface suitable for P4 receipt
    /// binding; it remains promotion-ineligible.
    #[cfg(feature = "agent-loop")]
    pub fn from_exact_verified_run_evidence(
        evidence: &ExactVerifiedRunEvidence,
        inputs: EvaluationReceiptInputs,
    ) -> EvaluationReceiptResult<ExactEvaluationReceiptEvidence> {
        let receipt = Self::from_verified_evidence(
            VerifiedRunExportEvidence::exact_verified(evidence)?,
            inputs,
        )?;
        let reconstruction_digest = receipt
            .exact_reconstruction_digest()
            .cloned()
            .ok_or(EvaluationReceiptError::Missing {
                field: "exact reconstruction digest",
            })?;
        let signature_verification = match receipt.run_export_authentication() {
            RunExportAuthenticationStatus::SignatureVerified(verification) => verification.clone(),
            RunExportAuthenticationStatus::Unauthenticated => {
                return Err(EvaluationReceiptError::Missing {
                    field: "exact signature verification",
                });
            }
        };
        Ok(ExactEvaluationReceiptEvidence {
            receipt,
            reconstruction_digest,
            signature_verification,
        })
    }

    /// Canonical payload bytes bound by [`Self::receipt_digest`].
    pub fn canonical_bytes(&self) -> EvaluationReceiptResult<Vec<u8>> {
        canonical_json_bytes(&ReceiptPayload::from(self))
    }

    /// Canonical complete document bytes, including the digest field.
    pub fn canonical_document_bytes(&self) -> EvaluationReceiptResult<Vec<u8>> {
        canonical_json_bytes(self)
    }

    /// Verify the receipt's schema, typed evidence, finite values, and digest.
    ///
    /// Authentication and exact fidelity are intentionally not conflated with
    /// integrity. An unsigned, integrity-verified receipt is valid diagnostic
    /// evidence but is not promotion eligible.
    pub fn verify(&self) -> EvaluationReceiptResult<()> {
        if self.schema != EVALUATION_RECEIPT_SCHEMA || self.version != EVALUATION_RECEIPT_SCHEMA_VERSION {
            return Err(EvaluationReceiptError::Inconsistent {
                field: "receipt schema/version",
            });
        }
        if !matches!(self.run_export_integrity, RunExportIntegrityStatus::Verified)
            || !self.required_evidence_complete
            || !matches!(self.terminal_validity, TerminalValidity::Verified)
        {
            return Err(EvaluationReceiptError::Inconsistent {
                field: "verified export terminal evidence",
            });
        }
        match (self.replay_fidelity, &self.exact_reconstruction_digest) {
            (ReplayFidelity::Exact, Some(digest)) => {
                if !matches!(
                    &self.run_export_authentication,
                    RunExportAuthenticationStatus::SignatureVerified(_)
                ) {
                    return Err(EvaluationReceiptError::Inconsistent {
                        field: "exact receipt signature verification",
                    });
                }
                EvidenceDigest::parse("exact reconstruction digest", digest.0.clone())?;
                if self.authentication_authority_available
                    && !matches!(
                        &self.run_export_authentication,
                        RunExportAuthenticationStatus::SignatureVerified(_)
                    )
                {
                    return Err(EvaluationReceiptError::Inconsistent {
                        field: "authentication authority without signature verification",
                    });
                }
            }
            (ReplayFidelity::Exact, None) => {
                return Err(EvaluationReceiptError::Missing {
                    field: "exact reconstruction digest",
                });
            }
            (_, Some(_)) => {
                return Err(EvaluationReceiptError::Inconsistent {
                    field: "non-exact reconstruction digest",
                });
            }
            (_, None) => {}
        }
        if !self.operation_counts.all_available() {
            return Err(EvaluationReceiptError::Missing {
                field: "verified replay operation counts",
            });
        }
        if self.promotion_eligible != receipt_promotion_eligible(self) {
            return Err(EvaluationReceiptError::Inconsistent {
                field: "promotion eligibility",
            });
        }
        checked_nonnegative_number("metric value", self.metric.value)?;
        checked_nonnegative_number("cost", self.cost_usd)?;
        EvidenceDigest::parse("run-export digest", self.run_export_digest.0.clone())?;
        EvidenceDigest::parse("baseline digest", self.baseline.digest.0.clone())?;
        EvidenceDigest::parse("fixed holdout digest", self.fixed_holdout.digest.0.clone())?;
        EvidenceDigest::parse(
            "reproduction artifact digest",
            self.reproduction_artifact.digest.0.clone(),
        )?;
        EvidenceDigest::parse("receipt digest", self.receipt_digest.clone())?;
        if receipt_digest(&self.canonical_bytes()?) != self.receipt_digest {
            return Err(EvaluationReceiptError::Inconsistent {
                field: "receipt digest",
            });
        }
        Ok(())
    }

    pub fn schema(&self) -> &str {
        self.schema
    }

    pub const fn version(&self) -> u32 {
        self.version
    }

    pub fn run_id(&self) -> &str {
        self.run_id.as_str()
    }

    pub fn run_export_digest(&self) -> &EvidenceDigest {
        &self.run_export_digest
    }

    pub const fn run_export_integrity(&self) -> RunExportIntegrityStatus {
        self.run_export_integrity
    }

    pub fn run_export_authentication(&self) -> &RunExportAuthenticationStatus {
        &self.run_export_authentication
    }

    /// Whether the upstream export schema established concrete authentication
    /// authority. Current P3 proof evidence always reports `false`.
    pub const fn authentication_authority_available(&self) -> bool {
        self.authentication_authority_available
    }

    pub const fn replay_fidelity(&self) -> ReplayFidelity {
        self.replay_fidelity
    }

    pub fn exact_reconstruction_digest(&self) -> Option<&EvidenceDigest> {
        self.exact_reconstruction_digest.as_ref()
    }

    pub const fn required_evidence_complete(&self) -> bool {
        self.required_evidence_complete
    }

    pub fn baseline(&self) -> &BaselineEvidence {
        &self.baseline
    }

    pub fn fixed_holdout(&self) -> &FixedHoldoutEvidence {
        &self.fixed_holdout
    }

    pub fn metric(&self) -> &MetricEvidence {
        &self.metric
    }

    pub const fn cost_usd(&self) -> f64 {
        self.cost_usd
    }

    pub const fn latency_ms(&self) -> u64 {
        self.latency_ms
    }

    pub const fn evaluated_task_count(&self) -> u64 {
        self.evaluated_task_count
    }

    /// Nearest-rank p95 of typed per-task rollout durations.
    pub const fn p95_latency_ms(&self) -> u64 {
        self.p95_latency_ms
    }

    pub const fn operation_counts(&self) -> &ReplayOperationCounts {
        &self.operation_counts
    }

    pub const fn tool_call_count(&self) -> EvidenceCount {
        self.operation_counts.tool_call_count()
    }

    pub const fn failure_count(&self) -> EvidenceCount {
        self.operation_counts.failure_count()
    }

    pub fn reproduction_artifact(&self) -> &ReproductionArtifactEvidence {
        &self.reproduction_artifact
    }

    pub fn evaluator_version(&self) -> &EvaluatorVersion {
        &self.evaluator_version
    }

    pub const fn terminal_validity(&self) -> TerminalValidity {
        self.terminal_validity
    }

    /// Always false until P4 supplies a concrete, nonconstructible
    /// release-authority capability. Policy-relative signature verification,
    /// integrity, and exact replay alone cannot authorize promotion.
    pub const fn promotion_eligible(&self) -> bool {
        self.promotion_eligible
    }

    pub fn receipt_digest(&self) -> &str {
        &self.receipt_digest
    }
}

/// Nonconstructible P3 evaluation evidence bound to one exact, signed,
/// obligation-complete replay reconstruction.
///
/// It exposes only typed commitments and measurements for P4 binding. It
/// neither grants promotion authority nor exposes a mutable receipt, raw
/// transcript, proof bytes, artifact reader, provider, or tool capability.
#[cfg(feature = "agent-loop")]
#[derive(Debug)]
pub struct ExactEvaluationReceiptEvidence {
    receipt: EvaluationReceipt,
    reconstruction_digest: EvidenceDigest,
    signature_verification: SignatureVerificationEvidence,
}

#[cfg(feature = "agent-loop")]
impl ExactEvaluationReceiptEvidence {
    pub fn verify(&self) -> EvaluationReceiptResult<()> {
        self.receipt.verify()?;
        if !matches!(self.receipt.replay_fidelity(), ReplayFidelity::Exact)
            || self.receipt.exact_reconstruction_digest() != Some(&self.reconstruction_digest)
            || !matches!(
                self.receipt.run_export_authentication(),
                RunExportAuthenticationStatus::SignatureVerified(verification)
                    if verification == &self.signature_verification
            )
            || self.receipt.promotion_eligible()
        {
            return Err(EvaluationReceiptError::Inconsistent {
                field: "exact evaluation receipt evidence",
            });
        }
        Ok(())
    }

    pub fn schema(&self) -> &str {
        self.receipt.schema()
    }

    pub const fn version(&self) -> u32 {
        self.receipt.version()
    }

    pub fn receipt_digest(&self) -> &str {
        self.receipt.receipt_digest()
    }

    pub fn run_id(&self) -> &str {
        self.receipt.run_id()
    }

    pub fn run_export_digest(&self) -> &EvidenceDigest {
        self.receipt.run_export_digest()
    }

    pub fn reconstruction_digest(&self) -> &EvidenceDigest {
        &self.reconstruction_digest
    }

    pub const fn run_export_integrity(&self) -> RunExportIntegrityStatus {
        self.receipt.run_export_integrity()
    }

    pub const fn authentication_authority_available(&self) -> bool {
        self.receipt.authentication_authority_available()
    }

    pub fn signature_verification(&self) -> &SignatureVerificationEvidence {
        &self.signature_verification
    }

    pub fn baseline(&self) -> &BaselineEvidence {
        self.receipt.baseline()
    }

    pub fn fixed_holdout(&self) -> &FixedHoldoutEvidence {
        self.receipt.fixed_holdout()
    }

    pub fn metric(&self) -> &MetricEvidence {
        self.receipt.metric()
    }

    pub const fn cost_usd(&self) -> f64 {
        self.receipt.cost_usd()
    }

    /// Sum of typed per-task rollout durations.
    pub const fn latency_ms(&self) -> u64 {
        self.receipt.latency_ms()
    }

    pub const fn evaluated_task_count(&self) -> u64 {
        self.receipt.evaluated_task_count()
    }

    /// Nearest-rank p95 of typed per-task rollout durations.
    pub const fn p95_latency_ms(&self) -> u64 {
        self.receipt.p95_latency_ms()
    }

    pub const fn operation_counts(&self) -> &ReplayOperationCounts {
        self.receipt.operation_counts()
    }

    pub const fn tool_call_count(&self) -> EvidenceCount {
        self.receipt.tool_call_count()
    }

    pub const fn failure_count(&self) -> EvidenceCount {
        self.receipt.failure_count()
    }

    pub fn reproduction_artifact(&self) -> &ReproductionArtifactEvidence {
        self.receipt.reproduction_artifact()
    }

    pub fn evaluator_version(&self) -> &EvaluatorVersion {
        self.receipt.evaluator_version()
    }

    pub const fn terminal_validity(&self) -> TerminalValidity {
        self.receipt.terminal_validity()
    }
}

#[derive(Serialize)]
struct ReceiptPayload<'a> {
    schema: &'a str,
    version: u32,
    run_id: &'a EvaluationRunId,
    run_export_digest: &'a EvidenceDigest,
    run_export_integrity: RunExportIntegrityStatus,
    run_export_authentication: &'a RunExportAuthenticationStatus,
    authentication_authority_available: bool,
    replay_fidelity: ReplayFidelity,
    exact_reconstruction_digest: Option<&'a EvidenceDigest>,
    required_evidence_complete: bool,
    baseline: &'a BaselineEvidence,
    fixed_holdout: &'a FixedHoldoutEvidence,
    metric: &'a MetricEvidence,
    cost_usd: f64,
    latency_ms: u64,
    evaluated_task_count: u64,
    p95_latency_ms: u64,
    operation_counts: &'a ReplayOperationCounts,
    reproduction_artifact: &'a ReproductionArtifactEvidence,
    evaluator_version: &'a EvaluatorVersion,
    terminal_validity: TerminalValidity,
    promotion_eligible: bool,
}

impl<'a> From<&'a EvaluationReceipt> for ReceiptPayload<'a> {
    fn from(receipt: &'a EvaluationReceipt) -> Self {
        Self {
            schema: receipt.schema,
            version: receipt.version,
            run_id: &receipt.run_id,
            run_export_digest: &receipt.run_export_digest,
            run_export_integrity: receipt.run_export_integrity,
            run_export_authentication: &receipt.run_export_authentication,
            authentication_authority_available: receipt.authentication_authority_available,
            replay_fidelity: receipt.replay_fidelity,
            exact_reconstruction_digest: receipt.exact_reconstruction_digest.as_ref(),
            required_evidence_complete: receipt.required_evidence_complete,
            baseline: &receipt.baseline,
            fixed_holdout: &receipt.fixed_holdout,
            metric: &receipt.metric,
            cost_usd: receipt.cost_usd,
            latency_ms: receipt.latency_ms,
            evaluated_task_count: receipt.evaluated_task_count,
            p95_latency_ms: receipt.p95_latency_ms,
            operation_counts: &receipt.operation_counts,
            reproduction_artifact: &receipt.reproduction_artifact,
            evaluator_version: &receipt.evaluator_version,
            terminal_validity: receipt.terminal_validity,
            promotion_eligible: receipt.promotion_eligible,
        }
    }
}

fn canonical_json_bytes(value: &impl Serialize) -> EvaluationReceiptResult<Vec<u8>> {
    #[cfg(feature = "agent-loop")]
    {
        let canonical = hermes_agent::run_ledger::CanonicalJson::from_value(value)
            .map_err(|_| EvaluationReceiptError::Canonicalization)?;
        return Ok(canonical.as_str().as_bytes().to_vec());
    }

    #[cfg(not(feature = "agent-loop"))]
    serde_json::to_vec(value).map_err(|_| EvaluationReceiptError::Canonicalization)
}

fn receipt_digest(bytes: &[u8]) -> String {
    // hermes-core owns the workspace's SHA-256 implementation.  This receipt
    // uses it only as a byte-to-digest primitive and retains the established
    // lowercase hex representation used by run exports.
    let digest = MemoryExportEvidenceDigest::sha256_bytes(bytes);
    digest
        .as_str()
        .strip_prefix("sha256:")
        .expect("hermes-core SHA-256 evidence digest always has its canonical prefix")
        .to_string()
}

/// Free-form verifier output retained for troubleshooting only.
///
/// This type is intentionally not convertible into [`EvaluationReceipt`] or
/// [`PromotionAuthority`].
#[derive(Debug, Clone, Serialize)]
pub struct LegacyEvaluationDiagnostic {
    score: f64,
    passed: bool,
    detail: Option<String>,
    metadata: serde_json::Value,
}

impl From<&VerificationOutcome> for LegacyEvaluationDiagnostic {
    fn from(outcome: &VerificationOutcome) -> Self {
        Self {
            score: outcome.score,
            passed: outcome.passed,
            detail: outcome.detail.clone(),
            metadata: outcome.metadata.clone(),
        }
    }
}

impl LegacyEvaluationDiagnostic {
    pub const fn score(&self) -> f64 {
        self.score
    }

    pub const fn passed(&self) -> bool {
        self.passed
    }

    pub fn detail(&self) -> Option<&str> {
        self.detail.as_deref()
    }

    pub fn metadata(&self) -> &serde_json::Value {
        &self.metadata
    }
}

/// In-memory receipt ledger indexed by immutable run ID.
#[derive(Debug, Default)]
pub struct EvaluationLedger {
    receipts: BTreeMap<String, EvaluationReceipt>,
    diagnostics: Vec<LegacyEvaluationDiagnostic>,
}

impl EvaluationLedger {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a self-verifying receipt.  Re-recording identical evidence is
    /// idempotent; a second digest for the same immutable run is rejected.
    pub fn record_receipt(&mut self, receipt: EvaluationReceipt) -> EvaluationReceiptResult<bool> {
        receipt.verify()?;
        let run_id = receipt.run_id().to_string();
        match self.receipts.get(&run_id) {
            Some(existing) if existing.receipt_digest() == receipt.receipt_digest() => Ok(false),
            Some(_) => Err(EvaluationReceiptError::Inconsistent {
                field: "immutable run receipt",
            }),
            None => {
                self.receipts.insert(run_id, receipt);
                Ok(true)
            }
        }
    }

    /// Record legacy verifier material for diagnostics. It is deliberately
    /// kept separate from typed receipts and promotion authority.
    pub fn record_diagnostic(&mut self, diagnostic: LegacyEvaluationDiagnostic) {
        self.diagnostics.push(diagnostic);
    }

    /// The only promotion-authoritative view. Diagnostics and unsigned or
    /// metadata-only receipts never influence it.
    pub fn promotion_authority(&self, run_id: &str) -> Option<PromotionAuthority<'_>> {
        self.receipts
            .get(run_id)
            .filter(|receipt| receipt.promotion_eligible())
            .map(|receipt| PromotionAuthority { receipt })
    }

    pub fn diagnostics(&self) -> &[LegacyEvaluationDiagnostic] {
        &self.diagnostics
    }

    pub fn receipt_count(&self) -> usize {
        self.receipts.len()
    }
}

/// Borrowed capability proving that a receipt entered the typed ledger.
#[derive(Debug, Clone, Copy)]
pub struct PromotionAuthority<'a> {
    receipt: &'a EvaluationReceipt,
}

impl<'a> PromotionAuthority<'a> {
    pub fn receipt(self) -> &'a EvaluationReceipt {
        self.receipt
    }
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use super::*;
    use crate::adapter::BenchmarkMetadata;
    use crate::result::{
        AggregateMetrics, CostUsd, PricingProvenance, TaskResult, Telemetry,
    };

    const RUN_ID: &str = "123e4567-e89b-12d3-a456-426614174000";
    const DIGEST_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const DIGEST_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    const DIGEST_C: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

    fn counts(tool_call_count: u64, failure_count: u64) -> ReplayOperationCounts {
        ReplayOperationCounts {
            model_call_count: EvidenceCount::Available(1),
            tool_call_count: EvidenceCount::Available(tool_call_count),
            delegation_count: EvidenceCount::Available(0),
            other_count: EvidenceCount::Available(0),
            failure_count: EvidenceCount::Available(failure_count),
        }
    }

    fn task(status: TaskStatus, cost_usd: f64, duration_ms: u64, outcome: VerificationOutcome) -> TaskResult {
        TaskResult {
            task_id: format!("task-{duration_ms}"),
            category: None,
            status,
            outcome,
            agent_turns: Telemetry::Available(0),
            retry_count: Telemetry::Available(0),
            duration: Duration::from_millis(duration_ms),
            tokens_input: Telemetry::Available(0),
            tokens_output: Telemetry::Available(0),
            cost_usd: Telemetry::Available(
                CostUsd::new(cost_usd, PricingProvenance::agent_result_usage_estimate())
                    .expect("valid test cost"),
            ),
            provenance: Default::default(),
            error: None,
        }
    }

    fn record(tasks: Vec<TaskResult>) -> RunRecord {
        RunRecord {
            run_id: RUN_ID.to_string(),
            benchmark: BenchmarkMetadata {
                id: "typed-eval".to_string(),
                name: "Typed eval".to_string(),
                source: "fixture".to_string(),
                version: "v1".to_string(),
            },
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            seed: 7,
            concurrency: 1,
            model: "fixture-model".to_string(),
            tasks,
            // Deliberately inconsistent values prove they are not trusted by
            // EvaluationEvidence.
            metrics: AggregateMetrics {
                total: 99,
                passed: 99,
                failed: 0,
                timeout: 0,
                error: 0,
                skipped: 0,
                pass_at_1: 1.0,
                total_duration: Duration::ZERO,
                total_tokens_input: Telemetry::Available(0),
                total_tokens_output: Telemetry::Available(0),
                total_cost_usd: Telemetry::Available(0.0),
                total_agent_turns: Telemetry::Available(0),
                total_retries: Telemetry::Available(0),
            },
        }
    }

    fn inputs() -> EvaluationReceiptInputs {
        let typed_record = record(vec![
            task(
                TaskStatus::Passed,
                1.25,
                25,
                VerificationOutcome {
                    score: 0.0,
                    passed: false,
                    detail: Some("arbitrary prose must not affect the metric".to_string()),
                    metadata: json!({"forged": true}),
                },
            ),
            task(
                TaskStatus::Error,
                0.75,
                75,
                VerificationOutcome {
                    score: 1.0,
                    passed: true,
                    detail: Some("also diagnostic-only".to_string()),
                    metadata: json!({"pass": true}),
                },
            ),
        ]);
        EvaluationReceiptInputs::new(
            BaselineEvidence::new("baseline-v1", DIGEST_A).expect("baseline"),
            FixedHoldoutEvidence::new("holdout-v1", DIGEST_B, 42, 2).expect("holdout"),
            EvaluationEvidence::from_run_record(&typed_record, "pass@1").expect("evaluation"),
            ReproductionArtifactEvidence::new("artifact://run/fixture", DIGEST_B)
                .expect("artifact"),
            EvaluatorVersion::new("hermes-eval/1.0.0").expect("version"),
        )
        .expect("inputs")
    }

    fn receipt() -> EvaluationReceipt {
        EvaluationReceipt::from_verified_evidence(
            VerifiedRunExportEvidence::integrity_verified(
                RUN_ID,
                DIGEST_C,
                ReplayFidelity::MetadataOnly,
                true,
                counts(3, 1),
            )
            .expect("export"),
            inputs(),
        )
        .expect("receipt")
    }

    #[test]
    fn complete_receipt_binds_only_typed_evidence() {
        let receipt = receipt();
        assert_eq!(receipt.schema(), EVALUATION_RECEIPT_SCHEMA);
        assert_eq!(receipt.version(), EVALUATION_RECEIPT_SCHEMA_VERSION);
        assert_eq!(receipt.run_id(), RUN_ID);
        assert_eq!(receipt.run_export_digest().as_str(), DIGEST_C);
        assert_eq!(receipt.run_export_integrity(), RunExportIntegrityStatus::Verified);
        assert_eq!(
            receipt.run_export_authentication(),
            &RunExportAuthenticationStatus::Unauthenticated
        );
        assert_eq!(receipt.replay_fidelity(), ReplayFidelity::MetadataOnly);
        assert!(receipt.required_evidence_complete());
        assert!(!receipt.promotion_eligible());
        assert_eq!(receipt.baseline().version(), "baseline-v1");
        assert_eq!(receipt.baseline().digest().as_str(), DIGEST_A);
        assert_eq!(receipt.fixed_holdout().id(), "holdout-v1");
        assert_eq!(receipt.fixed_holdout().seed(), 42);
        assert_eq!(receipt.fixed_holdout().digest().as_str(), DIGEST_B);
        assert_eq!(receipt.fixed_holdout().sample_count(), 2);
        assert_eq!(receipt.metric().name(), "pass@1");
        assert_eq!(receipt.metric().value(), 0.5);
        assert_eq!(receipt.cost_usd(), 2.0);
        assert_eq!(receipt.latency_ms(), 100);
        assert_eq!(receipt.tool_call_count(), EvidenceCount::Available(3));
        assert_eq!(receipt.failure_count(), EvidenceCount::Available(1));
        assert_eq!(
            receipt.reproduction_artifact().reference(),
            "artifact://run/fixture"
        );
        assert_eq!(receipt.reproduction_artifact().digest().as_str(), DIGEST_B);
        assert_eq!(receipt.evaluator_version().as_str(), "hermes-eval/1.0.0");
        assert_eq!(receipt.terminal_validity(), TerminalValidity::Verified);
        receipt.verify().expect("self-verifying receipt");
    }

    #[test]
    fn evidence_constructors_reject_every_required_text_or_digest_field() {
        for invalid in ["", " baseline", "baseline value", "baseline\nvalue"] {
            assert!(BaselineEvidence::new(invalid, DIGEST_A).is_err());
        }
        assert!(BaselineEvidence::new("baseline-v1", "not-a-digest").is_err());
        assert!(FixedHoldoutEvidence::new("", DIGEST_B, 0, 1).is_err());
        assert!(FixedHoldoutEvidence::new("holdout-v1", "bad", 0, 1).is_err());
        assert!(FixedHoldoutEvidence::new("holdout-v1", DIGEST_B, 0, 0).is_err());
        assert!(ReproductionArtifactEvidence::new("", DIGEST_A).is_err());
        assert!(ReproductionArtifactEvidence::new("file://artifact", DIGEST_A).is_err());
        assert!(ReproductionArtifactEvidence::new("artifact://has prose", DIGEST_A).is_err());
        assert!(ReproductionArtifactEvidence::new("artifact://valid", "bad").is_err());
        assert!(EvaluatorVersion::new("").is_err());
        assert!(EvaluatorVersion::new("evaluator version").is_err());
        assert!(EvidenceDigest::sha256("ABCDEF").is_err());
    }

    #[test]
    fn typed_measurements_reject_malformed_numeric_or_missing_evidence() {
        assert!(EvaluationEvidence::from_run_record(&record(Vec::new()), "pass@1").is_err());

        for invalid_cost in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -0.0, -1.0] {
            assert!(CostUsd::new(
                invalid_cost,
                PricingProvenance::agent_result_usage_estimate(),
            )
            .is_err());
        }

        let mut missing_cost = record(vec![task(
            TaskStatus::Passed,
            0.0,
            1,
            VerificationOutcome::pass(),
        )]);
        missing_cost.tasks[0].cost_usd = Telemetry::Unavailable;
        assert!(EvaluationEvidence::from_run_record(&missing_cost, "pass@1").is_err());

        let invalid_run = RunRecord {
            run_id: "not-a-uuid".to_string(),
            ..record(vec![task(
                TaskStatus::Passed,
                0.0,
                1,
                VerificationOutcome::pass(),
            )])
        };
        assert!(EvaluationEvidence::from_run_record(&invalid_run, "pass@1").is_err());
        let valid = record(vec![task(
            TaskStatus::Passed,
            0.0,
            1,
            VerificationOutcome::pass(),
        )]);
        assert!(EvaluationEvidence::from_run_record(&valid, "").is_err());
    }

    #[test]
    fn receipt_rejects_inconsistent_verified_evidence() {
        let mismatched_record = RunRecord {
            run_id: "123e4567-e89b-12d3-a456-426614174001".to_string(),
            ..record(vec![task(
                TaskStatus::Passed,
                0.0,
                1,
                VerificationOutcome::pass(),
            )])
        };
        let inputs = EvaluationReceiptInputs::new(
            BaselineEvidence::new("baseline-v1", DIGEST_A).expect("baseline"),
            FixedHoldoutEvidence::new("holdout-v1", DIGEST_B, 42, 1).expect("holdout"),
            EvaluationEvidence::from_run_record(&mismatched_record, "pass@1").expect("evaluation"),
            ReproductionArtifactEvidence::new("artifact://run/fixture", DIGEST_B)
                .expect("artifact"),
            EvaluatorVersion::new("hermes-eval/1.0.0").expect("version"),
        )
        .expect("inputs");
        assert!(EvaluationReceipt::from_verified_evidence(
            VerifiedRunExportEvidence::integrity_verified(
                RUN_ID,
                DIGEST_C,
                ReplayFidelity::Exact,
                true,
                counts(0, 0),
            )
            .expect("export"),
            inputs,
        )
        .is_err());
        assert!(VerifiedRunExportEvidence::integrity_verified(
            "not-a-run",
            DIGEST_C,
            ReplayFidelity::Exact,
            true,
            counts(0, 0),
        )
        .is_err());
        assert!(VerifiedRunExportEvidence::integrity_verified(
            RUN_ID,
            "bad",
            ReplayFidelity::Exact,
            true,
            counts(0, 0),
        )
        .is_err());
    }

    #[test]
    fn receipt_rejects_unverified_integrity_incomplete_or_nonterminal_evidence() {
        let verified = VerifiedRunExportEvidence::integrity_verified(
            RUN_ID,
            DIGEST_C,
            ReplayFidelity::Exact,
            true,
            counts(0, 0),
        )
        .expect("export");

        let mut missing_integrity = verified.clone();
        missing_integrity.integrity_status = RunExportIntegrityStatus::Unverified;
        assert!(EvaluationReceipt::from_verified_evidence(missing_integrity, inputs()).is_err());

        let mut incomplete = verified.clone();
        incomplete.required_evidence_complete = false;
        assert!(EvaluationReceipt::from_verified_evidence(incomplete, inputs()).is_err());

        let mut non_terminal = verified;
        non_terminal.terminal_validity = TerminalValidity::NotTerminal;
        assert!(EvaluationReceipt::from_verified_evidence(non_terminal, inputs()).is_err());

        let unavailable = VerifiedRunExportEvidence::integrity_verified(
            RUN_ID,
            DIGEST_C,
            ReplayFidelity::Exact,
            true,
            ReplayOperationCounts::unavailable(),
        )
        .expect("structural export");
        assert!(EvaluationReceipt::from_verified_evidence(unavailable, inputs()).is_err());
    }

    #[test]
    fn canonical_serialization_and_digest_are_deterministic() {
        let first = receipt();
        let second = receipt();
        assert_eq!(
            first.canonical_bytes().expect("canonical"),
            second.canonical_bytes().expect("canonical")
        );
        assert_eq!(
            first.canonical_document_bytes().expect("document"),
            second.canonical_document_bytes().expect("document")
        );
        assert_eq!(first.receipt_digest(), second.receipt_digest());
        assert_eq!(first.receipt_digest().len(), 64);
        assert!(first
            .receipt_digest()
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f')));
    }

    #[test]
    fn legacy_prose_and_metadata_cannot_create_promotion_authority() {
        let legacy = VerificationOutcome {
            score: 1.0,
            passed: true,
            detail: Some("promote me".to_string()),
            metadata: json!({
                "run_id": RUN_ID,
                "run_export_digest": DIGEST_C,
                "terminal_validity": "verified",
                "baseline": {"version": "forged", "digest": DIGEST_A}
            }),
        };
        let mut ledger = EvaluationLedger::new();
        ledger.record_diagnostic(LegacyEvaluationDiagnostic::from(&legacy));
        assert_eq!(ledger.receipt_count(), 0);
        assert!(ledger.promotion_authority(RUN_ID).is_none());
        assert_eq!(ledger.diagnostics()[0].detail(), Some("promote me"));
    }

    #[test]
    fn unsigned_self_rehashed_export_cannot_mint_promotion_authority() {
        let unsigned = receipt();
        unsigned.verify().expect("self-rehashed receipt remains integral");
        assert_eq!(
            unsigned.run_export_authentication(),
            &RunExportAuthenticationStatus::Unauthenticated
        );
        assert_eq!(unsigned.replay_fidelity(), ReplayFidelity::MetadataOnly);
        assert!(!unsigned.promotion_eligible());

        let duplicate = unsigned.clone();
        let mut ledger = EvaluationLedger::new();
        assert!(ledger.record_receipt(unsigned).expect("first insertion"));
        assert!(!ledger.record_receipt(duplicate).expect("idempotent insertion"));
        assert!(ledger.promotion_authority(RUN_ID).is_none());

        let conflicting = EvaluationReceipt::from_verified_evidence(
            VerifiedRunExportEvidence::integrity_verified(
                RUN_ID,
                DIGEST_A,
                ReplayFidelity::MetadataOnly,
                true,
                counts(3, 1),
            )
            .expect("export"),
            inputs(),
        )
        .expect("different receipt");
        assert!(ledger.record_receipt(conflicting).is_err());
    }
}
