//! Pure, fail-closed release promotion evaluation.
//!
//! This module deliberately accepts only evidence capabilities produced by the
//! signature-verification boundaries.  It never parses diagnostics, JSON, or
//! caller-supplied metric claims.

use crate::release_candidate::{
    ReleaseDigest, ReleasePromotionPolicy, ReleaseRevisionAlgorithm,
    VerifiedReleaseCandidateReceipt,
};
use crate::release_operations::{VerifiedRollbackDrillReceipt, VerifiedSoakReceipt};

/// Minimum authenticated soak time required for promotion.
pub const MINIMUM_SOAK_MILLIS: u64 = 24 * 60 * 60 * 1_000;
/// A rollback drill must complete strictly before this duration.
pub const MAXIMUM_ROLLBACK_DRILL_MILLIS_EXCLUSIVE: u64 = 5 * 60 * 1_000;

/// The closed promotion decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionDecision {
    /// Every required gate passed.
    Promote,
    /// At least one required gate failed.
    Reject,
}

/// A mandatory promotion gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionGate {
    /// The receipt carries the immutable P4 policy requirements.
    Policy,
    /// Candidate quality is not more than the permitted amount below incumbent quality.
    Quality,
    /// Candidate cost per task is not more than the permitted regression over incumbent cost.
    CostPerTask,
    /// Candidate p95 latency is not more than the permitted regression over incumbent latency.
    P95Latency,
    /// Every nonzero operation is covered by lineage.
    OperationLineage,
    /// The security report contains no critical or high findings.
    Security,
    /// Exact replay proof, export, and evaluation evidence are available.
    ExactReplayEvidence,
    /// Baseline, fixed holdout, and reproduction bindings are complete.
    RequiredBindings,
    /// Authenticated soak evidence is bound and at least 24 hours long.
    Soak,
    /// Authenticated rollback-drill evidence is bound and completes in under five minutes.
    RollbackDrill,
}

/// The numeric quantity whose validity or regression check failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionMetric {
    /// Task-quality rate.
    Quality,
    /// Cost divided by completed task count.
    CostPerTask,
    /// P95 task latency.
    P95Latency,
    /// Operation lineage coverage.
    OperationLineageCoverage,
}

/// An immutable policy invariant required by the P4 promotion authority.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionPolicyRequirement {
    /// Quality regression is capped at two percentage points.
    MaximumQualityRegression,
    /// Cost-per-task regression is capped at ten percent.
    MaximumCostPerTaskRegression,
    /// P95-latency regression is capped at ten percent.
    MaximumP95LatencyRegression,
    /// Lineage coverage must be complete.
    MinimumLineageCoverage,
    /// Critical findings are disallowed.
    MaximumCriticalFindings,
    /// High findings are disallowed.
    MaximumHighFindings,
    /// Exact replay evidence is mandatory.
    ExactReplayEvidenceRequired,
    /// Fixed-holdout evidence is mandatory.
    FixedHoldoutRequired,
    /// Reproduction evidence is mandatory.
    ReproductionRequired,
    /// A baseline is mandatory.
    BaselineRequired,
    /// Soak health observations may be no more than five minutes apart.
    MaximumSoakHeartbeatGap,
}

/// Evidence whose identity, scope, or policy binding did not match the candidate receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PromotionSupplementaryEvidence {
    /// The authenticated production soak receipt.
    Soak,
    /// The authenticated rollback-drill receipt.
    RollbackDrill,
}

/// One exact replay evidence item required for promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExactReplayEvidenceItem {
    /// The deterministic replay proof.
    ReplayProof,
    /// The deterministic replay export.
    ReplayExport,
    /// The typed evaluation receipt.
    EvaluationReceipt,
}

/// A binding that must be present in the release candidate receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseBinding {
    /// Candidate source repository identity.
    CandidateRepository,
    /// Candidate source commit identity.
    CandidateFullSha,
    /// Candidate source revision algorithm.
    CandidateRevisionAlgorithm,
    /// Candidate source tree identity.
    CandidateTreeDigest,
    /// Candidate artifact identity.
    CandidateArtifactDigest,
    /// Incumbent/baseline source repository identity.
    IncumbentRepository,
    /// Incumbent/baseline source commit identity.
    IncumbentFullSha,
    /// Incumbent/baseline source tree identity.
    IncumbentTreeDigest,
    /// Incumbent/baseline source revision algorithm.
    IncumbentRevisionAlgorithm,
    /// Incumbent/baseline artifact identity.
    IncumbentArtifactDigest,
    /// Fixed holdout identity.
    FixedHoldoutId,
    /// Fixed holdout digest.
    FixedHoldoutDigest,
    /// Nonempty fixed holdout sample count.
    FixedHoldoutSampleCount,
    /// Reproduction artifact identity.
    ReproductionArtifactId,
    /// Reproduction artifact digest.
    ReproductionArtifactDigest,
    /// Security report digest.
    SecurityReportDigest,
}

/// The typed reason an individual gate was rejected.
#[derive(Debug, Clone, PartialEq)]
pub enum PromotionGateFailure {
    /// The signed receipt's frozen policy does not satisfy a mandatory P4 invariant.
    PolicyRequirementMismatch {
        /// The invariant that did not match.
        requirement: PromotionPolicyRequirement,
    },
    /// A metric was nonfinite, sign-negative, or outside its closed domain.
    InvalidMetric {
        /// The invalid quantity.
        metric: PromotionMetric,
        /// The invalid observed value.
        value: f64,
    },
    /// Candidate quality fell below the policy's permitted lower bound.
    QualityRegressionExceeded {
        /// Candidate quality.
        candidate: f64,
        /// Incumbent quality.
        incumbent: f64,
        /// Maximum allowed absolute quality regression.
        maximum_regression: f64,
    },
    /// The incumbent value used as a lower-is-better denominator was zero.
    ZeroIncumbentDenominator {
        /// The affected lower-is-better metric.
        metric: PromotionMetric,
    },
    /// A lower-is-better metric exceeded the maximum allowed multiplier.
    RegressionExceeded {
        /// The affected metric.
        metric: PromotionMetric,
        /// Candidate value.
        candidate: f64,
        /// Incumbent value.
        incumbent: f64,
        /// Largest permitted candidate/incumbent multiplier.
        maximum_multiplier: f64,
    },
    /// No operations were present, so 100% lineage cannot be established.
    EmptyOperationLineage,
    /// The lineaged operation count was not exactly the total operation count.
    IncompleteOperationLineage {
        /// Operations with trusted lineage.
        lineaged_operations: u64,
        /// All operations in the evaluated release.
        total_operations: u64,
    },
    /// The derived coverage value is malformed or inconsistent with complete lineage.
    InvalidOperationLineageCoverage {
        /// The observed coverage value.
        value: f64,
    },
    /// A forbidden security finding was reported.
    SecurityFindingsPresent {
        /// Count of critical findings.
        critical: u64,
        /// Count of high findings.
        high: u64,
    },
    /// Exact evidence is marked unavailable.
    ExactReplayEvidenceUnavailable,
    /// A required exact evidence digest is absent.
    MissingExactReplayEvidence {
        /// The missing evidence item.
        item: ExactReplayEvidenceItem,
    },
    /// A required receipt binding is absent or invalid.
    MissingReleaseBinding {
        /// The missing or invalid binding.
        binding: ReleaseBinding,
    },
    /// Supplementary evidence was verified under a different policy digest.
    SupplementaryPolicyMismatch {
        /// The mismatched evidence receipt.
        evidence: PromotionSupplementaryEvidence,
    },
    /// Supplementary evidence belongs to a different tenant or authenticated subject.
    SupplementaryScopeMismatch {
        /// The mismatched evidence receipt.
        evidence: PromotionSupplementaryEvidence,
    },
    /// Supplementary evidence names a different release-candidate receipt digest.
    SupplementaryCandidateMismatch {
        /// The mismatched evidence receipt.
        evidence: PromotionSupplementaryEvidence,
    },
    /// The authenticated soak was shorter than 24 hours.
    InsufficientSoak {
        /// Observed authenticated soak duration.
        elapsed_millis: u64,
        /// Required minimum duration.
        minimum_millis: u64,
    },
    /// The authenticated rollback drill did not complete strictly before the limit.
    RollbackDrillTooSlow {
        /// Observed authenticated drill duration.
        elapsed_millis: u64,
        /// Exclusive maximum duration.
        maximum_exclusive_millis: u64,
    },
}

/// Pass/fail result for one mandatory gate.
#[derive(Debug, Clone, PartialEq)]
pub enum PromotionGateResult {
    /// The gate passed from typed, verified evidence.
    Passed,
    /// The gate was rejected for the stated typed reason.
    Failed(PromotionGateFailure),
}

impl PromotionGateResult {
    /// Whether this gate passed.
    pub const fn is_passed(&self) -> bool {
        matches!(self, Self::Passed)
    }

    /// The typed failure reason, when the gate failed.
    pub const fn failure(&self) -> Option<&PromotionGateFailure> {
        match self {
            Self::Passed => None,
            Self::Failed(failure) => Some(failure),
        }
    }
}

/// Result of one named mandatory gate.
#[derive(Debug, Clone, PartialEq)]
pub struct PromotionGateOutcome {
    gate: PromotionGate,
    result: PromotionGateResult,
}

impl PromotionGateOutcome {
    fn passed(gate: PromotionGate) -> Self {
        Self {
            gate,
            result: PromotionGateResult::Passed,
        }
    }

    fn failed(gate: PromotionGate, failure: PromotionGateFailure) -> Self {
        Self {
            gate,
            result: PromotionGateResult::Failed(failure),
        }
    }

    /// The mandatory gate that was evaluated.
    pub const fn gate(&self) -> PromotionGate {
        self.gate
    }

    /// The closed pass/fail result.
    pub const fn result(&self) -> &PromotionGateResult {
        &self.result
    }
}

/// Complete closed promotion decision for one verified release candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct PromotionVerdict {
    decision: PromotionDecision,
    policy_digest: ReleaseDigest,
    gates: [PromotionGateOutcome; 10],
}

impl PromotionVerdict {
    /// The closed decision.  Promotion is possible only when every gate passed.
    pub const fn decision(&self) -> PromotionDecision {
        self.decision
    }

    /// The immutable policy digest evaluated by this verdict.
    pub const fn policy_digest(&self) -> &ReleaseDigest {
        &self.policy_digest
    }

    /// Per-gate typed pass/fail outcomes in stable gate order.
    pub fn gates(&self) -> &[PromotionGateOutcome] {
        &self.gates
    }

    /// Whether the release may be promoted.
    pub const fn is_promotable(&self) -> bool {
        matches!(self.decision, PromotionDecision::Promote)
    }
}

/// Evaluate all P4 promotion gates from verified typed evidence only.
///
/// The concrete wrapper parameters are intentional: callers cannot substitute
/// unsigned receipt data or self-implemented evidence traits for the authority
/// minted by the sibling signature-verification modules.
///
/// ```compile_fail
/// use hermes_eval::{
///     evaluate_promotion, ReleaseCandidateReceipt, VerifiedRollbackDrillReceipt,
///     VerifiedSoakReceipt,
/// };
///
/// fn unsigned_receipt_cannot_authorize(
///     receipt: &ReleaseCandidateReceipt,
///     soak: &VerifiedSoakReceipt,
///     rollback: &VerifiedRollbackDrillReceipt,
/// ) {
///     // A raw receipt is not the nonconstructible verified capability.
///     let _ = evaluate_promotion(receipt, soak, rollback);
/// }
/// ```
pub fn evaluate_promotion(
    verified: &VerifiedReleaseCandidateReceipt,
    soak: &VerifiedSoakReceipt,
    rollback: &VerifiedRollbackDrillReceipt,
) -> PromotionVerdict {
    let receipt = verified.receipt();
    let policy = receipt.policy();
    let lineage = receipt.operation_lineage();
    let security = receipt.security_evidence();
    let replay_evidence = receipt.exact_replay_evidence();
    let has_replay_proof = !replay_evidence.proof_digest().as_str().is_empty();
    let has_replay_export = !replay_evidence.export_digest().as_str().is_empty();
    let has_evaluation_receipt = !replay_evidence
        .evaluation_receipt_digest()
        .as_str()
        .is_empty();
    let replay_evidence_available =
        has_replay_proof && has_replay_export && has_evaluation_receipt;
    let gates = [
        policy_gate(policy),
        quality_gate(
            receipt.candidate_metrics().quality(),
            receipt.incumbent_metrics().quality(),
            policy.max_quality_regression(),
        ),
        lower_is_better_gate(
            PromotionGate::CostPerTask,
            PromotionMetric::CostPerTask,
            PromotionPolicyRequirement::MaximumCostPerTaskRegression,
            receipt.candidate_metrics().cost_per_task(),
            receipt.incumbent_metrics().cost_per_task(),
            policy.max_cost_per_task_regression(),
        ),
        lower_is_better_gate(
            PromotionGate::P95Latency,
            PromotionMetric::P95Latency,
            PromotionPolicyRequirement::MaximumP95LatencyRegression,
            receipt.candidate_metrics().p95_latency(),
            receipt.incumbent_metrics().p95_latency(),
            policy.max_p95_latency_regression(),
        ),
        operation_lineage_gate(
            lineage.total_operations(),
            lineage.lineaged_operations(),
            lineage.coverage(),
            policy.minimum_lineage_coverage(),
        ),
        security_gate(
            security.critical_findings(),
            security.high_findings(),
            !security.report_digest().as_str().is_empty(),
            policy.max_critical_findings(),
            policy.max_high_findings(),
        ),
        exact_replay_evidence_gate(
            policy.requires_exact_replay_evidence(),
            replay_evidence_available,
            has_replay_proof,
            has_replay_export,
            has_evaluation_receipt,
        ),
        required_bindings_gate(policy, receipt),
        soak_gate(receipt, soak),
        rollback_gate(receipt, rollback),
    ];
    let decision = if gates.iter().all(|gate| gate.result.is_passed()) {
        PromotionDecision::Promote
    } else {
        PromotionDecision::Reject
    };

    PromotionVerdict {
        decision,
        policy_digest: policy.policy_digest().clone(),
        gates,
    }
}

fn policy_gate(policy: &ReleasePromotionPolicy) -> PromotionGateOutcome {
    const MAX_QUALITY_REGRESSION: f64 = 0.02;
    const MAX_COST_PER_TASK_REGRESSION: f64 = 0.10;
    const MAX_P95_LATENCY_REGRESSION: f64 = 0.10;
    const MAXIMUM_SOAK_HEARTBEAT_GAP_MILLIS: u64 = 5 * 60 * 1_000;

    let required = [
        (
            policy.max_quality_regression() == MAX_QUALITY_REGRESSION,
            PromotionPolicyRequirement::MaximumQualityRegression,
        ),
        (
            policy.max_cost_per_task_regression() == MAX_COST_PER_TASK_REGRESSION,
            PromotionPolicyRequirement::MaximumCostPerTaskRegression,
        ),
        (
            policy.max_p95_latency_regression() == MAX_P95_LATENCY_REGRESSION,
            PromotionPolicyRequirement::MaximumP95LatencyRegression,
        ),
        (
            policy.minimum_lineage_coverage() == 1.0,
            PromotionPolicyRequirement::MinimumLineageCoverage,
        ),
        (
            policy.max_critical_findings() == 0,
            PromotionPolicyRequirement::MaximumCriticalFindings,
        ),
        (
            policy.max_high_findings() == 0,
            PromotionPolicyRequirement::MaximumHighFindings,
        ),
        (
            policy.requires_exact_replay_evidence(),
            PromotionPolicyRequirement::ExactReplayEvidenceRequired,
        ),
        (
            policy.requires_fixed_holdout(),
            PromotionPolicyRequirement::FixedHoldoutRequired,
        ),
        (
            policy.requires_reproduction(),
            PromotionPolicyRequirement::ReproductionRequired,
        ),
        (
            policy.requires_baseline(),
            PromotionPolicyRequirement::BaselineRequired,
        ),
        (
            policy.maximum_soak_heartbeat_gap_ms() == MAXIMUM_SOAK_HEARTBEAT_GAP_MILLIS,
            PromotionPolicyRequirement::MaximumSoakHeartbeatGap,
        ),
    ];

    for (matches_requirement, requirement) in required {
        if !matches_requirement {
            return PromotionGateOutcome::failed(
                PromotionGate::Policy,
                PromotionGateFailure::PolicyRequirementMismatch { requirement },
            );
        }
    }
    PromotionGateOutcome::passed(PromotionGate::Policy)
}

fn quality_gate(
    candidate: f64,
    incumbent: f64,
    maximum_regression: f64,
) -> PromotionGateOutcome {
    if !valid_quality(candidate) {
        return PromotionGateOutcome::failed(
            PromotionGate::Quality,
            PromotionGateFailure::InvalidMetric {
                metric: PromotionMetric::Quality,
                value: candidate,
            },
        );
    }
    if !valid_quality(incumbent) {
        return PromotionGateOutcome::failed(
            PromotionGate::Quality,
            PromotionGateFailure::InvalidMetric {
                metric: PromotionMetric::Quality,
                value: incumbent,
            },
        );
    }

    if !maximum_regression.is_finite() || maximum_regression.is_sign_negative() {
        return PromotionGateOutcome::failed(
            PromotionGate::Quality,
            PromotionGateFailure::PolicyRequirementMismatch {
                requirement: PromotionPolicyRequirement::MaximumQualityRegression,
            },
        );
    }
    if candidate < incumbent - maximum_regression {
        return PromotionGateOutcome::failed(
            PromotionGate::Quality,
            PromotionGateFailure::QualityRegressionExceeded {
                candidate,
                incumbent,
                maximum_regression,
            },
        );
    }
    PromotionGateOutcome::passed(PromotionGate::Quality)
}

fn lower_is_better_gate(
    gate: PromotionGate,
    metric: PromotionMetric,
    policy_requirement: PromotionPolicyRequirement,
    candidate: f64,
    incumbent: f64,
    maximum_regression: f64,
) -> PromotionGateOutcome {
    if !valid_nonnegative_finite(candidate) {
        return PromotionGateOutcome::failed(
            gate,
            PromotionGateFailure::InvalidMetric {
                metric,
                value: candidate,
            },
        );
    }
    if !valid_nonnegative_finite(incumbent) {
        return PromotionGateOutcome::failed(
            gate,
            PromotionGateFailure::InvalidMetric {
                metric,
                value: incumbent,
            },
        );
    }
    if incumbent == 0.0 {
        return PromotionGateOutcome::failed(
            gate,
            PromotionGateFailure::ZeroIncumbentDenominator { metric },
        );
    }
    if !maximum_regression.is_finite() || maximum_regression.is_sign_negative() {
        return PromotionGateOutcome::failed(
            gate,
            PromotionGateFailure::PolicyRequirementMismatch {
                requirement: policy_requirement,
            },
        );
    }

    let maximum_multiplier = 1.0 + maximum_regression;
    let ratio = candidate / incumbent;
    if !ratio.is_finite() || ratio > maximum_multiplier {
        return PromotionGateOutcome::failed(
            gate,
            PromotionGateFailure::RegressionExceeded {
                metric,
                candidate,
                incumbent,
                maximum_multiplier,
            },
        );
    }
    PromotionGateOutcome::passed(gate)
}

fn valid_nonnegative_finite(value: f64) -> bool {
    value.is_finite() && !value.is_sign_negative()
}

fn valid_quality(value: f64) -> bool {
    valid_nonnegative_finite(value) && value <= 1.0
}

fn operation_lineage_gate(
    total_operations: u64,
    lineaged_operations: u64,
    coverage: f64,
    minimum_coverage: f64,
) -> PromotionGateOutcome {
    if !coverage.is_finite() || coverage.is_sign_negative() {
        return PromotionGateOutcome::failed(
            PromotionGate::OperationLineage,
            PromotionGateFailure::InvalidOperationLineageCoverage { value: coverage },
        );
    }
    if total_operations == 0 {
        return PromotionGateOutcome::failed(
            PromotionGate::OperationLineage,
            PromotionGateFailure::EmptyOperationLineage,
        );
    }
    if lineaged_operations != total_operations || coverage != minimum_coverage {
        return PromotionGateOutcome::failed(
            PromotionGate::OperationLineage,
            PromotionGateFailure::IncompleteOperationLineage {
                lineaged_operations,
                total_operations,
            },
        );
    }
    PromotionGateOutcome::passed(PromotionGate::OperationLineage)
}

fn security_gate(
    critical: u64,
    high: u64,
    report_present: bool,
    maximum_critical: u64,
    maximum_high: u64,
) -> PromotionGateOutcome {
    if !report_present {
        return PromotionGateOutcome::failed(
            PromotionGate::Security,
            PromotionGateFailure::MissingReleaseBinding {
                binding: ReleaseBinding::SecurityReportDigest,
            },
        );
    }
    if critical > maximum_critical || high > maximum_high {
        return PromotionGateOutcome::failed(
            PromotionGate::Security,
            PromotionGateFailure::SecurityFindingsPresent { critical, high },
        );
    }
    PromotionGateOutcome::passed(PromotionGate::Security)
}

fn exact_replay_evidence_gate(
    required: bool,
    available: bool,
    has_proof: bool,
    has_export: bool,
    has_evaluation_receipt: bool,
) -> PromotionGateOutcome {
    if !required {
        return PromotionGateOutcome::failed(
            PromotionGate::ExactReplayEvidence,
            PromotionGateFailure::PolicyRequirementMismatch {
                requirement: PromotionPolicyRequirement::ExactReplayEvidenceRequired,
            },
        );
    }
    for (present, item) in [
        (has_proof, ExactReplayEvidenceItem::ReplayProof),
        (has_export, ExactReplayEvidenceItem::ReplayExport),
        (
            has_evaluation_receipt,
            ExactReplayEvidenceItem::EvaluationReceipt,
        ),
    ] {
        if !present {
            return PromotionGateOutcome::failed(
                PromotionGate::ExactReplayEvidence,
                PromotionGateFailure::MissingExactReplayEvidence { item },
            );
        }
    }
    if !available {
        return PromotionGateOutcome::failed(
            PromotionGate::ExactReplayEvidence,
            PromotionGateFailure::ExactReplayEvidenceUnavailable,
        );
    }
    PromotionGateOutcome::passed(PromotionGate::ExactReplayEvidence)
}

fn required_bindings_gate(
    policy: &ReleasePromotionPolicy,
    receipt: &crate::release_candidate::ReleaseCandidateReceipt,
) -> PromotionGateOutcome {
    if !policy.requires_baseline() {
        return PromotionGateOutcome::failed(
            PromotionGate::RequiredBindings,
            PromotionGateFailure::PolicyRequirementMismatch {
                requirement: PromotionPolicyRequirement::BaselineRequired,
            },
        );
    }
    if !policy.requires_fixed_holdout() {
        return PromotionGateOutcome::failed(
            PromotionGate::RequiredBindings,
            PromotionGateFailure::PolicyRequirementMismatch {
                requirement: PromotionPolicyRequirement::FixedHoldoutRequired,
            },
        );
    }
    if !policy.requires_reproduction() {
        return PromotionGateOutcome::failed(
            PromotionGate::RequiredBindings,
            PromotionGateFailure::PolicyRequirementMismatch {
                requirement: PromotionPolicyRequirement::ReproductionRequired,
            },
        );
    }
    let candidate = receipt.candidate_source();
    let incumbent = receipt.incumbent_source();
    let holdout = receipt.holdout();
    let reproduction = receipt.reproduction();
    if let Some(failure) = missing_release_binding(&[
        (!candidate.repository().is_empty(), ReleaseBinding::CandidateRepository),
        (!candidate.full_sha().is_empty(), ReleaseBinding::CandidateFullSha),
        (
            matches!(
                candidate.revision_algorithm(),
                &ReleaseRevisionAlgorithm::GitSha1
            ),
            ReleaseBinding::CandidateRevisionAlgorithm,
        ),
        (
            !candidate.tree_digest().as_str().is_empty(),
            ReleaseBinding::CandidateTreeDigest,
        ),
        (
            !candidate.artifact_digest().as_str().is_empty(),
            ReleaseBinding::CandidateArtifactDigest,
        ),
        (!incumbent.repository().is_empty(), ReleaseBinding::IncumbentRepository),
        (!incumbent.full_sha().is_empty(), ReleaseBinding::IncumbentFullSha),
        (
            matches!(
                incumbent.revision_algorithm(),
                &ReleaseRevisionAlgorithm::GitSha1
            ),
            ReleaseBinding::IncumbentRevisionAlgorithm,
        ),
        (
            !incumbent.tree_digest().as_str().is_empty(),
            ReleaseBinding::IncumbentTreeDigest,
        ),
        (
            !incumbent.artifact_digest().as_str().is_empty(),
            ReleaseBinding::IncumbentArtifactDigest,
        ),
        (!holdout.holdout_id().is_empty(), ReleaseBinding::FixedHoldoutId),
        (
            !holdout.holdout_digest().as_str().is_empty(),
            ReleaseBinding::FixedHoldoutDigest,
        ),
        (
            holdout.sample_count() != 0,
            ReleaseBinding::FixedHoldoutSampleCount,
        ),
        (
            !reproduction.artifact_id().is_empty(),
            ReleaseBinding::ReproductionArtifactId,
        ),
        (
            !reproduction.artifact_digest().as_str().is_empty(),
            ReleaseBinding::ReproductionArtifactDigest,
        ),
    ]) {
        return PromotionGateOutcome::failed(PromotionGate::RequiredBindings, failure);
    }
    PromotionGateOutcome::passed(PromotionGate::RequiredBindings)
}

fn missing_release_binding(
    bindings: &[(bool, ReleaseBinding)],
) -> Option<PromotionGateFailure> {
    bindings
        .iter()
        .find(|(present, _)| !*present)
        .map(|(_, binding)| PromotionGateFailure::MissingReleaseBinding { binding: *binding })
}

fn soak_gate(
    receipt: &crate::release_candidate::ReleaseCandidateReceipt,
    soak: &VerifiedSoakReceipt,
) -> PromotionGateOutcome {
    if let Some(failure) = supplementary_binding_failure(
        PromotionSupplementaryEvidence::Soak,
        soak.policy().policy_id() == receipt.policy().policy_id()
            && soak.policy().policy_digest() == receipt.policy().policy_digest(),
        soak.scope().tenant_id() == receipt.tenant_id()
            && soak.scope().authenticated_subject() == receipt.authenticated_subject(),
        soak.candidate_receipt_digest() == receipt.receipt_digest(),
    ) {
        return PromotionGateOutcome::failed(PromotionGate::Soak, failure);
    }
    if let Some(failure) = soak_duration_failure(soak.elapsed_ms()) {
        return PromotionGateOutcome::failed(PromotionGate::Soak, failure);
    }
    PromotionGateOutcome::passed(PromotionGate::Soak)
}

fn rollback_gate(
    receipt: &crate::release_candidate::ReleaseCandidateReceipt,
    rollback: &VerifiedRollbackDrillReceipt,
) -> PromotionGateOutcome {
    if let Some(failure) = supplementary_binding_failure(
        PromotionSupplementaryEvidence::RollbackDrill,
        rollback.policy().policy_id() == receipt.policy().policy_id()
            && rollback.policy().policy_digest() == receipt.policy().policy_digest(),
        rollback.scope().tenant_id() == receipt.tenant_id()
            && rollback.scope().authenticated_subject() == receipt.authenticated_subject(),
        rollback.candidate_receipt_digest() == receipt.receipt_digest(),
    ) {
        return PromotionGateOutcome::failed(PromotionGate::RollbackDrill, failure);
    }
    if let Some(failure) = rollback_duration_failure(rollback.drill_duration_ms()) {
        return PromotionGateOutcome::failed(PromotionGate::RollbackDrill, failure);
    }
    PromotionGateOutcome::passed(PromotionGate::RollbackDrill)
}

fn supplementary_binding_failure(
    evidence: PromotionSupplementaryEvidence,
    policy_matches: bool,
    scope_matches: bool,
    candidate_matches: bool,
) -> Option<PromotionGateFailure> {
    if !policy_matches {
        return Some(PromotionGateFailure::SupplementaryPolicyMismatch { evidence });
    }
    if !scope_matches {
        return Some(PromotionGateFailure::SupplementaryScopeMismatch { evidence });
    }
    if !candidate_matches {
        return Some(PromotionGateFailure::SupplementaryCandidateMismatch { evidence });
    }
    None
}

fn soak_duration_failure(elapsed_millis: u64) -> Option<PromotionGateFailure> {
    (elapsed_millis < MINIMUM_SOAK_MILLIS).then_some(PromotionGateFailure::InsufficientSoak {
        elapsed_millis,
        minimum_millis: MINIMUM_SOAK_MILLIS,
    })
}

fn rollback_duration_failure(elapsed_millis: u64) -> Option<PromotionGateFailure> {
    (elapsed_millis >= MAXIMUM_ROLLBACK_DRILL_MILLIS_EXCLUSIVE).then_some(
        PromotionGateFailure::RollbackDrillTooSlow {
            elapsed_millis,
            maximum_exclusive_millis: MAXIMUM_ROLLBACK_DRILL_MILLIS_EXCLUSIVE,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure(outcome: &PromotionGateOutcome) -> &PromotionGateFailure {
        outcome
            .result()
            .failure()
            .expect("test fixture must exercise a rejection")
    }

    #[test]
    fn quality_threshold_is_inclusive_and_just_worse_rejects() {
        let incumbent = 0.80;
        let exact_boundary = incumbent - 0.02;
        assert!(quality_gate(exact_boundary, incumbent, 0.02)
            .result()
            .is_passed());
        assert!(matches!(
            failure(&quality_gate(exact_boundary - 0.000_001, incumbent, 0.02)),
            PromotionGateFailure::QualityRegressionExceeded { .. }
        ));
    }

    #[test]
    fn lower_is_better_thresholds_are_inclusive_and_just_over_rejects() {
        let cost_exact = lower_is_better_gate(
            PromotionGate::CostPerTask,
            PromotionMetric::CostPerTask,
            PromotionPolicyRequirement::MaximumCostPerTaskRegression,
            11.0,
            10.0,
            0.10,
        );
        assert!(cost_exact.result().is_passed());
        assert!(lower_is_better_gate(
            PromotionGate::CostPerTask,
            PromotionMetric::CostPerTask,
            PromotionPolicyRequirement::MaximumCostPerTaskRegression,
            0.0,
            10.0,
            0.10,
        )
        .result()
        .is_passed());
        assert!(matches!(
            failure(&lower_is_better_gate(
                PromotionGate::CostPerTask,
                PromotionMetric::CostPerTask,
                PromotionPolicyRequirement::MaximumCostPerTaskRegression,
                11.000_001,
                10.0,
                0.10,
            )),
            PromotionGateFailure::RegressionExceeded {
                metric: PromotionMetric::CostPerTask,
                ..
            }
        ));

        let latency_exact = lower_is_better_gate(
            PromotionGate::P95Latency,
            PromotionMetric::P95Latency,
            PromotionPolicyRequirement::MaximumP95LatencyRegression,
            110.0,
            100.0,
            0.10,
        );
        assert!(latency_exact.result().is_passed());
        assert!(matches!(
            failure(&lower_is_better_gate(
                PromotionGate::P95Latency,
                PromotionMetric::P95Latency,
                PromotionPolicyRequirement::MaximumP95LatencyRegression,
                110.000_001,
                100.0,
                0.10,
            )),
            PromotionGateFailure::RegressionExceeded {
                metric: PromotionMetric::P95Latency,
                ..
            }
        ));
    }

    #[test]
    fn lower_is_better_rejects_zero_incumbent_denominators() {
        for (gate, metric, policy_requirement) in [
            (
                PromotionGate::CostPerTask,
                PromotionMetric::CostPerTask,
                PromotionPolicyRequirement::MaximumCostPerTaskRegression,
            ),
            (
                PromotionGate::P95Latency,
                PromotionMetric::P95Latency,
                PromotionPolicyRequirement::MaximumP95LatencyRegression,
            ),
        ] {
            assert!(matches!(
                failure(&lower_is_better_gate(
                    gate,
                    metric,
                    policy_requirement,
                    0.0,
                    0.0,
                    0.10,
                )),
                PromotionGateFailure::ZeroIncumbentDenominator { metric: failed_metric }
                    if *failed_metric == metric
            ));
        }
    }

    #[test]
    fn nonfinite_metrics_reject_without_defaulting() {
        assert!(matches!(
            failure(&quality_gate(f64::NAN, 0.5, 0.02)),
            PromotionGateFailure::InvalidMetric {
                metric: PromotionMetric::Quality,
                value,
            } if value.is_nan()
        ));
        assert!(matches!(
            failure(&lower_is_better_gate(
                PromotionGate::CostPerTask,
                PromotionMetric::CostPerTask,
                PromotionPolicyRequirement::MaximumCostPerTaskRegression,
                f64::INFINITY,
                1.0,
                0.10,
            )),
            PromotionGateFailure::InvalidMetric {
                metric: PromotionMetric::CostPerTask,
                value,
            } if value.is_infinite()
        ));
    }

    #[test]
    fn lineage_requires_nonzero_total_and_complete_coverage() {
        assert!(matches!(
            failure(&operation_lineage_gate(0, 0, 0.0, 1.0)),
            PromotionGateFailure::EmptyOperationLineage
        ));
        assert!(matches!(
            failure(&operation_lineage_gate(10, 9, 0.9, 1.0)),
            PromotionGateFailure::IncompleteOperationLineage {
                lineaged_operations: 9,
                total_operations: 10,
            }
        ));
        assert!(operation_lineage_gate(10, 10, 1.0, 1.0)
            .result()
            .is_passed());
    }

    #[test]
    fn critical_or_high_security_findings_reject() {
        assert!(matches!(
            failure(&security_gate(1, 0, true, 0, 0)),
            PromotionGateFailure::SecurityFindingsPresent {
                critical: 1,
                high: 0,
            }
        ));
        assert!(matches!(
            failure(&security_gate(0, 1, true, 0, 0)),
            PromotionGateFailure::SecurityFindingsPresent {
                critical: 0,
                high: 1,
            }
        ));
        assert!(matches!(
            failure(&security_gate(0, 0, false, 0, 0)),
            PromotionGateFailure::MissingReleaseBinding {
                binding: ReleaseBinding::SecurityReportDigest,
            }
        ));
    }

    #[test]
    fn missing_baseline_holdout_or_reproduction_binding_rejects() {
        for binding in [
            ReleaseBinding::CandidateRevisionAlgorithm,
            ReleaseBinding::IncumbentArtifactDigest,
            ReleaseBinding::FixedHoldoutSampleCount,
            ReleaseBinding::ReproductionArtifactDigest,
        ] {
            assert!(matches!(
                missing_release_binding(&[(false, binding)]),
                Some(PromotionGateFailure::MissingReleaseBinding {
                    binding: failed_binding,
                }) if failed_binding == binding
            ));
        }
    }

    #[test]
    fn unavailable_or_missing_exact_replay_evidence_rejects() {
        assert!(matches!(
            failure(&exact_replay_evidence_gate(true, false, true, true, true)),
            PromotionGateFailure::ExactReplayEvidenceUnavailable
        ));
        assert!(matches!(
            failure(&exact_replay_evidence_gate(true, true, false, true, true)),
            PromotionGateFailure::MissingExactReplayEvidence {
                item: ExactReplayEvidenceItem::ReplayProof,
            }
        ));
        assert!(matches!(
            failure(&exact_replay_evidence_gate(true, true, true, false, true)),
            PromotionGateFailure::MissingExactReplayEvidence {
                item: ExactReplayEvidenceItem::ReplayExport,
            }
        ));
        assert!(matches!(
            failure(&exact_replay_evidence_gate(true, true, true, true, false)),
            PromotionGateFailure::MissingExactReplayEvidence {
                item: ExactReplayEvidenceItem::EvaluationReceipt,
            }
        ));
    }

    #[test]
    fn supplementary_policy_mismatch_rejects_before_duration_checks() {
        assert!(matches!(
            supplementary_binding_failure(
                PromotionSupplementaryEvidence::Soak,
                false,
                true,
                true,
            ),
            Some(PromotionGateFailure::SupplementaryPolicyMismatch {
                evidence: PromotionSupplementaryEvidence::Soak,
            })
        ));
        assert!(matches!(
            supplementary_binding_failure(
                PromotionSupplementaryEvidence::RollbackDrill,
                true,
                false,
                true,
            ),
            Some(PromotionGateFailure::SupplementaryScopeMismatch {
                evidence: PromotionSupplementaryEvidence::RollbackDrill,
            })
        ));
        assert!(matches!(
            supplementary_binding_failure(
                PromotionSupplementaryEvidence::RollbackDrill,
                true,
                true,
                false,
            ),
            Some(PromotionGateFailure::SupplementaryCandidateMismatch {
                evidence: PromotionSupplementaryEvidence::RollbackDrill,
            })
        ));
    }

    #[test]
    fn soak_requires_full_twenty_four_hours() {
        assert!(matches!(
            soak_duration_failure(MINIMUM_SOAK_MILLIS - 1_000),
            Some(PromotionGateFailure::InsufficientSoak {
                elapsed_millis,
                minimum_millis: MINIMUM_SOAK_MILLIS,
            }) if elapsed_millis == MINIMUM_SOAK_MILLIS - 1_000
        ));
        assert_eq!(soak_duration_failure(MINIMUM_SOAK_MILLIS), None);
    }

    #[test]
    fn rollback_drill_must_finish_strictly_before_five_minutes() {
        assert_eq!(
            rollback_duration_failure(MAXIMUM_ROLLBACK_DRILL_MILLIS_EXCLUSIVE - 1),
            None
        );
        assert!(matches!(
            rollback_duration_failure(MAXIMUM_ROLLBACK_DRILL_MILLIS_EXCLUSIVE),
            Some(PromotionGateFailure::RollbackDrillTooSlow {
                elapsed_millis: MAXIMUM_ROLLBACK_DRILL_MILLIS_EXCLUSIVE,
                maximum_exclusive_millis: MAXIMUM_ROLLBACK_DRILL_MILLIS_EXCLUSIVE,
            })
        ));
    }

    #[test]
    fn public_evaluator_requires_the_verified_receipt_capability() {
        let evaluator: fn(
            &VerifiedReleaseCandidateReceipt,
            &VerifiedSoakReceipt,
            &VerifiedRollbackDrillReceipt,
        ) -> PromotionVerdict = evaluate_promotion;
        let _ = evaluator;
    }
}
