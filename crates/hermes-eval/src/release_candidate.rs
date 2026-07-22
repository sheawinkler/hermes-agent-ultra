//! Fail-closed, signed release-candidate receipts.
//!
//! A [`ReleaseCandidateReceipt`] is an unsigned, validated statement of
//! evidence. It is deliberately not promotion authority. Only
//! [`verify_release_candidate_receipt`] can construct a
//! [`VerifiedReleaseCandidateReceipt`], after canonical-byte, policy-scope,
//! trust-key, and Ed25519 checks all succeed.

use std::fmt;
use std::path::Path;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;

#[cfg(feature = "agent-loop")]
use hermes_agent::ExactVerifiedRunEvidence;

#[cfg(feature = "agent-loop")]
use crate::eval_ledger::{
    EvidenceCount, ExactEvaluationReceiptEvidence, RunExportIntegrityStatus, TerminalValidity,
};

/// Stable identifier for the canonical release-candidate receipt payload.
pub const RELEASE_CANDIDATE_RECEIPT_SCHEMA: &str = "hermes.release_candidate.receipt";
/// Current payload schema version.
pub const RELEASE_CANDIDATE_RECEIPT_SCHEMA_VERSION: u32 = 1;
/// Stable identifier for the detached receipt signature envelope.
pub const RELEASE_CANDIDATE_RECEIPT_ENVELOPE_SCHEMA: &str =
    "hermes.release_candidate.receipt.envelope";
/// Current detached-envelope schema version.
pub const RELEASE_CANDIDATE_RECEIPT_ENVELOPE_SCHEMA_VERSION: u32 = 1;
/// Digest algorithm used by all receipt commitments.
pub const RELEASE_CANDIDATE_DIGEST_ALGORITHM: &str = "sha-256";
/// Receipt-specific Ed25519 signing domain.
pub const RELEASE_CANDIDATE_SIGNATURE_DOMAIN: &str = "hermes.release_candidate.receipt.v1";
/// SQLite schema version for [`ReleaseCandidateReceiptLedger`].
pub const RELEASE_CANDIDATE_LEDGER_SCHEMA_VERSION: u32 = 1;

const MAX_CANONICAL_RECEIPT_BYTES: usize = 1_048_576;
const MAX_LABEL_LENGTH: usize = 256;
const LEDGER_SCHEMA_DESCRIPTOR: &str = concat!(
    "hermes.release_candidate.ledger|v1|",
    "meta(singleton, schema_version, schema_fingerprint)|",
    "receipts(receipt_id, receipt_digest, receipt_bytes, envelope_bytes)|",
    "receipt_id_primary_key|receipt_id_digest_unique"
);

/// Fail-closed error for release-candidate receipt construction, verification,
/// or durable storage.
#[derive(Debug, Error)]
pub enum ReleaseCandidateReceiptError {
    #[error("invalid {field}: {reason}")]
    InvalidField {
        field: &'static str,
        reason: String,
    },

    #[error("canonical JSON encoding failed: {0}")]
    Canonicalization(String),

    #[error("receipt bytes are not canonical RFC 8785 JSON")]
    NonCanonicalReceipt,

    #[error("signature envelope bytes are not canonical RFC 8785 JSON")]
    NonCanonicalEnvelope,

    #[error("receipt digest does not match the canonical payload")]
    ReceiptDigestMismatch,

    #[error("signature envelope is not bound to this receipt digest")]
    EnvelopeDigestMismatch,

    #[error("receipt policy is not authorized for this tenant and subject")]
    UntrustedPolicy,

    #[error("receipt signing key is not trusted for this tenant and subject")]
    UntrustedKey,

    #[error("trusted signing key is malformed")]
    MalformedTrustedKey,

    #[error("receipt signature is malformed")]
    MalformedSignature,

    #[error("receipt signature verification failed")]
    InvalidSignature,

    #[error("P3 exact evidence is unavailable or invalid: {0}")]
    P3Evidence(String),

    #[error("P3 exact evidence does not bind receipt field {0}")]
    P3EvidenceMismatch(&'static str),

    #[error("receipt ledger schema marker is invalid: expected version {expected_version}, fingerprint {expected_fingerprint}; found version {actual_version}, fingerprint {actual_fingerprint}")]
    LedgerSchemaMismatch {
        expected_version: u32,
        expected_fingerprint: String,
        actual_version: i64,
        actual_fingerprint: String,
    },

    #[error("receipt id {receipt_id} is already bound to different immutable bytes")]
    LedgerConflict { receipt_id: String },

    #[error("stored row receipt id does not match its canonical receipt")]
    LedgerReceiptIdMismatch,

    #[error("stored row digest does not match its canonical receipt")]
    LedgerDigestMismatch,

    #[error("SQLite operation failed: {0}")]
    Sqlite(String),
}

/// Result alias for this module.
pub type ReleaseCandidateReceiptResult<T> = Result<T, ReleaseCandidateReceiptError>;

macro_rules! label_type {
    ($name:ident, $field:literal, $doc:literal) => {
        #[doc = $doc]
        #[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            /// Creates a canonical nonempty identifier.
            pub fn new(value: impl Into<String>) -> ReleaseCandidateReceiptResult<Self> {
                Ok(Self(checked_label($field, value.into())?))
            }

            /// Borrows the canonical identifier.
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }
    };
}

label_type!(ReleaseReceiptId, "receipt_id", "Immutable release receipt identifier.");
label_type!(ReleaseTenantId, "tenant_id", "Authenticated tenant scope identifier.");
label_type!(
    AuthenticatedSubject,
    "authenticated_subject",
    "Authenticated principal within a tenant scope."
);
label_type!(ReleasePolicyId, "policy_id", "Frozen release-promotion policy identifier.");
label_type!(ReleaseSigningKeyId, "key_id", "Trusted release-signing key identifier.");

/// Canonical lowercase hexadecimal SHA-256 digest.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ReleaseDigest(String);

impl ReleaseDigest {
    /// Validates an explicit SHA-256 digest commitment.
    pub fn new(value: impl Into<String>) -> ReleaseCandidateReceiptResult<Self> {
        let value = value.into();
        if value.len() != 64 {
            return Err(invalid(
                "digest",
                "must be exactly 64 lowercase hexadecimal characters",
            ));
        }
        if !value.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()) {
            return Err(invalid(
                "digest",
                "must be lowercase hexadecimal",
            ));
        }
        Ok(Self(value))
    }

    /// Borrows the digest text.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn from_canonical_bytes(bytes: &[u8]) -> Self {
        Self(hex::encode(Sha256::digest(bytes)))
    }
}

impl fmt::Display for ReleaseDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Tenant and authenticated-subject scope used for trust lookup.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ReleaseReceiptScope {
    tenant_id: ReleaseTenantId,
    authenticated_subject: AuthenticatedSubject,
}

impl ReleaseReceiptScope {
    /// Creates a scope from validated tenant and authenticated-subject IDs.
    pub fn new(tenant_id: ReleaseTenantId, authenticated_subject: AuthenticatedSubject) -> Self {
        Self {
            tenant_id,
            authenticated_subject,
        }
    }

    /// Borrows the tenant identifier.
    pub fn tenant_id(&self) -> &ReleaseTenantId {
        &self.tenant_id
    }

    /// Borrows the authenticated subject.
    pub fn authenticated_subject(&self) -> &AuthenticatedSubject {
        &self.authenticated_subject
    }
}

/// Closed source-revision algorithm. P4 currently accepts only Git SHA-1;
/// Git SHA-256 needs an explicit future algorithm variant and validation path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseRevisionAlgorithm {
    /// A full 40-character lowercase Git SHA-1 commit ID.
    GitSha1,
}

impl ReleaseRevisionAlgorithm {
    fn parse_wire(value: String) -> ReleaseCandidateReceiptResult<Self> {
        match value.as_str() {
            "git_sha1" => Ok(Self::GitSha1),
            _ => Err(invalid(
                "revision_algorithm",
                "must be the supported closed algorithm git_sha1",
            )),
        }
    }

    const fn full_sha_length(self) -> usize {
        match self {
            Self::GitSha1 => 40,
        }
    }
}


/// Immutable repository, revision algorithm/commit, tree, and artifact identity
/// for a release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseSource {
    repository: String,
    revision_algorithm: ReleaseRevisionAlgorithm,
    full_sha: String,
    tree_digest: ReleaseDigest,
    artifact_digest: ReleaseDigest,
}

impl ReleaseSource {
    /// Creates a complete, validated repository source identity.
    pub fn new(
        repository: impl Into<String>,
        revision_algorithm: ReleaseRevisionAlgorithm,
        full_sha: impl Into<String>,
        tree_digest: ReleaseDigest,
        artifact_digest: ReleaseDigest,
    ) -> ReleaseCandidateReceiptResult<Self> {
        let repository = checked_label("repository", repository.into())?;
        let full_sha = full_sha.into();
        if full_sha.len() != revision_algorithm.full_sha_length()
            || !full_sha
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(invalid(
                "full_sha",
                "must match the exact lowercase hexadecimal length for revision_algorithm",
            ));
        }
        Ok(Self {
            repository,
            revision_algorithm,
            full_sha,
            tree_digest,
            artifact_digest,
        })
    }

    /// Repository identity.
    pub fn repository(&self) -> &str {
        &self.repository
    }

    /// Closed revision algorithm that determines the required full SHA length.
    pub fn revision_algorithm(&self) -> &ReleaseRevisionAlgorithm {
        &self.revision_algorithm
    }

    /// Full immutable Git commit ID.
    pub fn full_sha(&self) -> &str {
        &self.full_sha
    }

    /// Tree content commitment.
    pub fn tree_digest(&self) -> &ReleaseDigest {
        &self.tree_digest
    }

    /// Built artifact commitment.
    pub fn artifact_digest(&self) -> &ReleaseDigest {
        &self.artifact_digest
    }
}

/// Fixed holdout selection used to evaluate a candidate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseHoldoutEvidence {
    holdout_id: String,
    holdout_digest: ReleaseDigest,
    seed: u64,
    sample_count: u64,
}

impl ReleaseHoldoutEvidence {
    /// Creates complete fixed-holdout evidence.
    pub fn new(
        holdout_id: impl Into<String>,
        holdout_digest: ReleaseDigest,
        seed: u64,
        sample_count: u64,
    ) -> ReleaseCandidateReceiptResult<Self> {
        if sample_count == 0 {
            return Err(invalid("sample_count", "must be greater than zero"));
        }
        Ok(Self {
            holdout_id: checked_label("holdout_id", holdout_id.into())?,
            holdout_digest,
            seed,
            sample_count,
        })
    }

    /// Fixed holdout identity.
    pub fn holdout_id(&self) -> &str {
        &self.holdout_id
    }

    /// Fixed holdout content commitment.
    pub fn holdout_digest(&self) -> &ReleaseDigest {
        &self.holdout_digest
    }

    /// Deterministic holdout-selection seed.
    pub fn seed(&self) -> u64 {
        self.seed
    }

    /// Number of held-out samples.
    pub fn sample_count(&self) -> u64 {
        self.sample_count
    }
}

/// Candidate or incumbent quality, cost-per-task, and p95 latency metrics.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReleaseMetrics {
    quality: f64,
    cost_per_task: f64,
    p95_latency: f64,
}

impl ReleaseMetrics {
    /// Creates finite metrics. Quality is normalized to the inclusive `[0, 1]` range.
    pub fn new(
        quality: f64,
        cost_per_task: f64,
        p95_latency: f64,
    ) -> ReleaseCandidateReceiptResult<Self> {
        checked_metric("quality", quality, true)?;
        checked_metric("cost_per_task", cost_per_task, false)?;
        checked_metric("p95_latency", p95_latency, false)?;
        Ok(Self {
            quality,
            cost_per_task,
            p95_latency,
        })
    }

    /// Normalized quality score.
    pub fn quality(&self) -> f64 {
        self.quality
    }

    /// Cost per evaluated task.
    pub fn cost_per_task(&self) -> f64 {
        self.cost_per_task
    }

    /// p95 latency for evaluated tasks.
    pub fn p95_latency(&self) -> f64 {
        self.p95_latency
    }
}

/// Total operations expected from exact replay and operations with verified lineage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseOperationLineage {
    total_operations: u64,
    lineaged_operations: u64,
}

impl ReleaseOperationLineage {
    /// Creates lineage totals. Incomplete lineage is representable so the frozen
    /// promotion policy can reject it; absent and zero-total evidence is not.
    pub fn new(
        total_operations: u64,
        lineaged_operations: u64,
    ) -> ReleaseCandidateReceiptResult<Self> {
        if total_operations == 0 {
            return Err(invalid("total_operations", "must be greater than zero"));
        }
        if lineaged_operations > total_operations {
            return Err(invalid(
                "lineaged_operations",
                "cannot exceed total_operations",
            ));
        }
        Ok(Self {
            total_operations,
            lineaged_operations,
        })
    }

    /// Number of operations requiring lineage.
    pub fn total_operations(&self) -> u64 {
        self.total_operations
    }

    /// Number of operations with verified lineage.
    pub fn lineaged_operations(&self) -> u64 {
        self.lineaged_operations
    }

    /// Fraction of operations with verified lineage.
    pub fn coverage(&self) -> f64 {
        self.lineaged_operations as f64 / self.total_operations as f64
    }
}

/// Critical/high security finding counts and the immutable report commitment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseSecurityEvidence {
    critical_findings: u64,
    high_findings: u64,
    report_digest: ReleaseDigest,
}

impl ReleaseSecurityEvidence {
    /// Creates complete security-report evidence.
    pub fn new(
        critical_findings: u64,
        high_findings: u64,
        report_digest: ReleaseDigest,
    ) -> Self {
        Self {
            critical_findings,
            high_findings,
            report_digest,
        }
    }

    /// Number of critical findings.
    pub fn critical_findings(&self) -> u64 {
        self.critical_findings
    }

    /// Number of high-severity findings.
    pub fn high_findings(&self) -> u64 {
        self.high_findings
    }

    /// Security report commitment.
    pub fn report_digest(&self) -> &ReleaseDigest {
        &self.report_digest
    }
}

/// Content-addressed artifact sufficient to reproduce the release evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReleaseReproductionEvidence {
    artifact_id: String,
    artifact_digest: ReleaseDigest,
}

impl ReleaseReproductionEvidence {
    /// Creates complete reproduction-artifact evidence.
    pub fn new(
        artifact_id: impl Into<String>,
        artifact_digest: ReleaseDigest,
    ) -> ReleaseCandidateReceiptResult<Self> {
        Ok(Self {
            artifact_id: checked_label("artifact_id", artifact_id.into())?,
            artifact_digest,
        })
    }

    /// Reproduction artifact identity.
    pub fn artifact_id(&self) -> &str {
        &self.artifact_id
    }

    /// Reproduction artifact commitment.
    pub fn artifact_digest(&self) -> &ReleaseDigest {
        &self.artifact_digest
    }
}

/// Exact P3 detached-proof, run-export, reconstruction, and evaluation-receipt
/// commitments.
///
/// Public callers cannot create this evidence from strings. Production values
/// are minted only by [`exact_replay_evidence_from_verified_p3`] after binding
/// nonconstructible P3 proof/replay/evaluation evidence. The wire parser has a
/// private construction path because parsed bytes remain untrusted until the
/// detached P4 signature, scoped policy, and P3 evidence are verified.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExactReplayEvidence {
    proof_digest: ReleaseDigest,
    export_digest: ReleaseDigest,
    reconstruction_digest: ReleaseDigest,
    evaluation_receipt_digest: ReleaseDigest,
}

impl ExactReplayEvidence {
    /// Detached P3 proof commitment.
    pub fn proof_digest(&self) -> &ReleaseDigest {
        &self.proof_digest
    }

    /// Canonical run-export commitment.
    pub fn export_digest(&self) -> &ReleaseDigest {
        &self.export_digest
    }

    /// Exact reconstruction/transcript commitment.
    pub fn reconstruction_digest(&self) -> &ReleaseDigest {
        &self.reconstruction_digest
    }

    /// Typed evaluation receipt commitment.
    pub fn evaluation_receipt_digest(&self) -> &ReleaseDigest {
        &self.evaluation_receipt_digest
    }

    fn from_untrusted_wire(
        proof_digest: ReleaseDigest,
        export_digest: ReleaseDigest,
        reconstruction_digest: ReleaseDigest,
        evaluation_receipt_digest: ReleaseDigest,
    ) -> Self {
        Self {
            proof_digest,
            export_digest,
            reconstruction_digest,
            evaluation_receipt_digest,
        }
    }

    #[cfg(test)]
    fn for_test(
        proof_digest: ReleaseDigest,
        export_digest: ReleaseDigest,
        evaluation_receipt_digest: ReleaseDigest,
    ) -> Self {
        Self::from_untrusted_wire(
            proof_digest.clone(),
            export_digest,
            proof_digest,
            evaluation_receipt_digest,
        )
    }
}

/// Derives receipt exact-evidence commitments only from nonconstructible P3
/// exact replay and evaluation evidence.
#[cfg(feature = "agent-loop")]
pub fn exact_replay_evidence_from_verified_p3(
    exact_run_evidence: &ExactVerifiedRunEvidence,
    evaluation_evidence: &ExactEvaluationReceiptEvidence,
) -> ReleaseCandidateReceiptResult<ExactReplayEvidence> {
    evaluation_evidence
        .verify()
        .map_err(|error| ReleaseCandidateReceiptError::P3Evidence(error.to_string()))?;
    if exact_run_evidence.run_id().to_string() != evaluation_evidence.run_id() {
        return Err(ReleaseCandidateReceiptError::P3EvidenceMismatch("run_id"));
    }
    if exact_run_evidence.export_digest() != evaluation_evidence.run_export_digest().as_str() {
        return Err(ReleaseCandidateReceiptError::P3EvidenceMismatch(
            "run_export_digest",
        ));
    }
    if exact_run_evidence.proof_digest()
        != evaluation_evidence
            .signature_verification()
            .proof_digest()
            .as_str()
    {
        return Err(ReleaseCandidateReceiptError::P3EvidenceMismatch(
            "proof_digest",
        ));
    }
    if exact_run_evidence.reconstruction_digest()
        != evaluation_evidence.reconstruction_digest().as_str()
    {
        return Err(ReleaseCandidateReceiptError::P3EvidenceMismatch(
            "reconstruction_digest",
        ));
    }
    if exact_run_evidence.policy_id()
        != evaluation_evidence.signature_verification().policy_id()
        || exact_run_evidence.policy_digest()
            != evaluation_evidence
                .signature_verification()
                .policy_digest()
                .as_str()
    {
        return Err(ReleaseCandidateReceiptError::P3EvidenceMismatch(
            "P3 trust-policy provenance",
        ));
    }
    if !matches!(
        evaluation_evidence.run_export_integrity(),
        RunExportIntegrityStatus::Verified
    ) || !matches!(evaluation_evidence.terminal_validity(), TerminalValidity::Verified)
    {
        return Err(ReleaseCandidateReceiptError::P3Evidence(
            "exact evaluation receipt lacks verified export/terminal evidence".into(),
        ));
    }
    Ok(ExactReplayEvidence::from_untrusted_wire(
        ReleaseDigest::new(exact_run_evidence.proof_digest().to_owned())?,
        ReleaseDigest::new(exact_run_evidence.export_digest().to_owned())?,
        ReleaseDigest::new(exact_run_evidence.reconstruction_digest().to_owned())?,
        ReleaseDigest::new(evaluation_evidence.receipt_digest().to_owned())?,
    ))
}

#[cfg(feature = "agent-loop")]
fn bind_inputs_to_verified_p3(
    inputs: &ReleaseCandidateReceiptInputs,
    exact_run_evidence: &ExactVerifiedRunEvidence,
    evaluation_evidence: &ExactEvaluationReceiptEvidence,
) -> ReleaseCandidateReceiptResult<ExactReplayEvidence> {
    let exact_replay_evidence =
        exact_replay_evidence_from_verified_p3(exact_run_evidence, evaluation_evidence)?;

    let evaluated_task_count = evaluation_evidence.evaluated_task_count();
    if evaluated_task_count == 0 {
        return Err(ReleaseCandidateReceiptError::P3Evidence(
            "exact evaluation receipt has no evaluated tasks".into(),
        ));
    }
    let derived_candidate_metrics = ReleaseMetrics::new(
        evaluation_evidence.metric().value(),
        evaluation_evidence.cost_usd() / evaluated_task_count as f64,
        evaluation_evidence.p95_latency_ms() as f64,
    )?;
    if inputs.candidate_metrics != derived_candidate_metrics {
        return Err(ReleaseCandidateReceiptError::P3EvidenceMismatch(
            "candidate_metrics",
        ));
    }

    let fixed_holdout = evaluation_evidence.fixed_holdout();
    if inputs.holdout.holdout_id() != fixed_holdout.id()
        || inputs.holdout.holdout_digest().as_str() != fixed_holdout.digest().as_str()
        || inputs.holdout.seed() != fixed_holdout.seed()
        || inputs.holdout.sample_count() != fixed_holdout.sample_count()
    {
        return Err(ReleaseCandidateReceiptError::P3EvidenceMismatch("fixed_holdout"));
    }

    let reproduction = evaluation_evidence.reproduction_artifact();
    if inputs.reproduction.artifact_id() != reproduction.reference()
        || inputs.reproduction.artifact_digest().as_str() != reproduction.digest().as_str()
    {
        return Err(ReleaseCandidateReceiptError::P3EvidenceMismatch(
            "reproduction_artifact",
        ));
    }
    if inputs.incumbent_source.artifact_digest().as_str()
        != evaluation_evidence.baseline().digest().as_str()
    {
        return Err(ReleaseCandidateReceiptError::P3EvidenceMismatch(
            "incumbent_baseline_digest",
        ));
    }

    let operation_counts = evaluation_evidence.operation_counts();
    let total_operations = [
        operation_counts.model_call_count(),
        operation_counts.tool_call_count(),
        operation_counts.delegation_count(),
        operation_counts.other_count(),
        operation_counts.failure_count(),
    ]
    .into_iter()
    .try_fold(0_u64, |total, count| {
        let value = required_p3_count(count)?;
        total
            .checked_add(value)
            .ok_or_else(|| ReleaseCandidateReceiptError::P3Evidence(
                "operation lineage total overflow".into(),
            ))
    })?;
    let derived_lineage = ReleaseOperationLineage::new(total_operations, total_operations)?;
    if inputs.operation_lineage != derived_lineage {
        return Err(ReleaseCandidateReceiptError::P3EvidenceMismatch(
            "operation_lineage",
        ));
    }
    Ok(exact_replay_evidence)
}

#[cfg(feature = "agent-loop")]
fn required_p3_count(value: EvidenceCount) -> ReleaseCandidateReceiptResult<u64> {
    value.value().ok_or_else(|| {
        ReleaseCandidateReceiptError::P3Evidence(
            "exact evaluation receipt has unavailable operation counts".into(),
        )
    })
}

/// Immutable, policy-bound P4 promotion thresholds.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReleasePromotionPolicy {
    policy_id: ReleasePolicyId,
    max_quality_regression: f64,
    max_cost_per_task_regression: f64,
    max_p95_latency_regression: f64,
    minimum_lineage_coverage: f64,
    max_critical_findings: u64,
    max_high_findings: u64,
    require_exact_replay_evidence: bool,
    require_fixed_holdout: bool,
    require_reproduction: bool,
    require_baseline: bool,
    maximum_soak_heartbeat_gap_ms: u64,
    policy_digest: ReleaseDigest,
}

impl ReleasePromotionPolicy {
    /// Creates the frozen P4 policy defaults and computes its explicit digest.
    ///
    /// This API intentionally has no permissive threshold override. A future
    /// policy revision must receive a new schema/policy design rather than
    /// weakening a signed P4 receipt in place.
    pub fn new(policy_id: ReleasePolicyId) -> ReleaseCandidateReceiptResult<Self> {
        let max_quality_regression = 0.02;
        let max_cost_per_task_regression = 0.10;
        let max_p95_latency_regression = 0.10;
        let minimum_lineage_coverage = 1.0;
        let max_critical_findings = 0;
        let max_high_findings = 0;
        let require_exact_replay_evidence = true;
        let require_fixed_holdout = true;
        let require_reproduction = true;
        let require_baseline = true;
        let maximum_soak_heartbeat_gap_ms = 300_000;
        let digest_payload = PolicyDigestPayload {
            schema: RELEASE_CANDIDATE_RECEIPT_SCHEMA,
            schema_version: RELEASE_CANDIDATE_RECEIPT_SCHEMA_VERSION,
            policy_id: &policy_id,
            max_quality_regression,
            max_cost_per_task_regression,
            max_p95_latency_regression,
            minimum_lineage_coverage,
            max_critical_findings,
            max_high_findings,
            require_exact_replay_evidence,
            require_fixed_holdout,
            require_reproduction,
            require_baseline,
            maximum_soak_heartbeat_gap_ms,
        };
        let policy_digest = ReleaseDigest::from_canonical_bytes(&canonical_json(&digest_payload)?);
        Ok(Self {
            policy_id,
            max_quality_regression,
            max_cost_per_task_regression,
            max_p95_latency_regression,
            minimum_lineage_coverage,
            max_critical_findings,
            max_high_findings,
            require_exact_replay_evidence,
            require_fixed_holdout,
            require_reproduction,
            require_baseline,
            maximum_soak_heartbeat_gap_ms,
            policy_digest,
        })
    }

    /// Frozen policy identifier.
    pub fn policy_id(&self) -> &ReleasePolicyId {
        &self.policy_id
    }

    /// Canonical digest of all frozen policy settings.
    pub fn policy_digest(&self) -> &ReleaseDigest {
        &self.policy_digest
    }

    /// Maximum allowed quality regression in normalized points.
    pub fn max_quality_regression(&self) -> f64 {
        self.max_quality_regression
    }

    /// Maximum allowed cost-per-task regression as a fraction.
    pub fn max_cost_per_task_regression(&self) -> f64 {
        self.max_cost_per_task_regression
    }

    /// Maximum allowed p95 latency regression as a fraction.
    pub fn max_p95_latency_regression(&self) -> f64 {
        self.max_p95_latency_regression
    }

    /// Required operation-lineage coverage.
    pub fn minimum_lineage_coverage(&self) -> f64 {
        self.minimum_lineage_coverage
    }

    /// Maximum critical security findings.
    pub fn max_critical_findings(&self) -> u64 {
        self.max_critical_findings
    }

    /// Maximum high-severity security findings.
    pub fn max_high_findings(&self) -> u64 {
        self.max_high_findings
    }

    /// Whether exact replay evidence is required.
    pub fn requires_exact_replay_evidence(&self) -> bool {
        self.require_exact_replay_evidence
    }

    /// Whether a fixed holdout is required.
    pub fn requires_fixed_holdout(&self) -> bool {
        self.require_fixed_holdout
    }

    /// Whether a reproduction artifact is required.
    pub fn requires_reproduction(&self) -> bool {
        self.require_reproduction
    }

    /// Whether an incumbent baseline is required.
    pub fn requires_baseline(&self) -> bool {
        self.require_baseline
    }

    /// Maximum permitted interval between authenticated soak heartbeats.
    pub fn maximum_soak_heartbeat_gap_ms(&self) -> u64 {
        self.maximum_soak_heartbeat_gap_ms
    }

    fn from_raw(raw: RawPolicy) -> ReleaseCandidateReceiptResult<Self> {
        let policy_id = ReleasePolicyId::new(raw.policy_id)?;
        let policy = Self::new(policy_id)?;
        let claimed_digest = ReleaseDigest::new(raw.policy_digest)?;
        if policy.max_quality_regression.to_bits() != raw.max_quality_regression.to_bits()
            || policy.max_cost_per_task_regression.to_bits()
                != raw.max_cost_per_task_regression.to_bits()
            || policy.max_p95_latency_regression.to_bits()
                != raw.max_p95_latency_regression.to_bits()
            || policy.minimum_lineage_coverage.to_bits() != raw.minimum_lineage_coverage.to_bits()
            || policy.max_critical_findings != raw.max_critical_findings
            || policy.max_high_findings != raw.max_high_findings
            || policy.require_exact_replay_evidence != raw.require_exact_replay_evidence
            || policy.require_fixed_holdout != raw.require_fixed_holdout
            || policy.require_reproduction != raw.require_reproduction
            || policy.require_baseline != raw.require_baseline
            || policy.maximum_soak_heartbeat_gap_ms != raw.maximum_soak_heartbeat_gap_ms
        {
            return Err(invalid(
                "policy",
                "must exactly match the immutable P4 policy defaults",
            ));
        }
        if policy.policy_digest != claimed_digest {
            return Err(invalid("policy_digest", "does not match frozen policy settings"));
        }
        Ok(policy)
    }
}

/// All required evidence bound into a release-candidate receipt.
#[derive(Debug, Clone)]
pub struct ReleaseCandidateReceiptInputs {
    /// Immutable receipt identity.
    pub receipt_id: ReleaseReceiptId,
    /// Tenant that authorized the release evaluation.
    pub tenant_id: ReleaseTenantId,
    /// Authenticated subject that requested the release evaluation.
    pub authenticated_subject: AuthenticatedSubject,
    /// Candidate repository/artifact identity.
    pub candidate_source: ReleaseSource,
    /// Incumbent repository/artifact identity. This is the mandatory baseline.
    pub incumbent_source: ReleaseSource,
    /// Candidate metrics recomputed from typed evidence.
    pub candidate_metrics: ReleaseMetrics,
    /// Incumbent metrics recomputed from typed evidence.
    pub incumbent_metrics: ReleaseMetrics,
    /// Deterministic fixed holdout evidence.
    pub holdout: ReleaseHoldoutEvidence,
    /// Exact replay operation lineage totals.
    pub operation_lineage: ReleaseOperationLineage,
    /// Security report evidence.
    pub security_evidence: ReleaseSecurityEvidence,
    /// Reproduction artifact evidence.
    pub reproduction: ReleaseReproductionEvidence,
    /// Frozen P4 promotion policy. Exact P3 replay/export/evaluation evidence
    /// is supplied separately as nonconstructible arguments to
    /// [`ReleaseCandidateReceipt::new`].
    pub policy: ReleasePromotionPolicy,
    /// Receipt creation time.
    pub created_at: DateTime<Utc>,
}

/// Canonical, self-digesting but unsigned release-candidate evidence.
///
/// This type alone has no promotion authority. It must be signed and verified
/// against a caller-supplied scoped trust policy before a promotion gate can
/// accept it.
#[derive(Debug, Clone, PartialEq)]
pub struct ReleaseCandidateReceipt {
    receipt_id: ReleaseReceiptId,
    tenant_id: ReleaseTenantId,
    authenticated_subject: AuthenticatedSubject,
    candidate_source: ReleaseSource,
    incumbent_source: ReleaseSource,
    candidate_metrics: ReleaseMetrics,
    incumbent_metrics: ReleaseMetrics,
    holdout: ReleaseHoldoutEvidence,
    operation_lineage: ReleaseOperationLineage,
    security_evidence: ReleaseSecurityEvidence,
    reproduction: ReleaseReproductionEvidence,
    exact_replay_evidence: ExactReplayEvidence,
    policy: ReleasePromotionPolicy,
    created_at: DateTime<Utc>,
    receipt_digest: ReleaseDigest,
}

impl ReleaseCandidateReceipt {
    /// Builds a complete receipt after exact P3 evidence has been independently
    /// verified and bound by the public production constructor.
    fn from_bound_inputs(
        inputs: ReleaseCandidateReceiptInputs,
        exact_replay_evidence: ExactReplayEvidence,
    ) -> ReleaseCandidateReceiptResult<Self> {
        let payload = ReceiptDigestPayload {
            schema: RELEASE_CANDIDATE_RECEIPT_SCHEMA,
            schema_version: RELEASE_CANDIDATE_RECEIPT_SCHEMA_VERSION,
            receipt_id: &inputs.receipt_id,
            tenant_id: &inputs.tenant_id,
            authenticated_subject: &inputs.authenticated_subject,
            candidate_source: &inputs.candidate_source,
            incumbent_source: &inputs.incumbent_source,
            candidate_metrics: &inputs.candidate_metrics,
            incumbent_metrics: &inputs.incumbent_metrics,
            holdout: &inputs.holdout,
            operation_lineage: &inputs.operation_lineage,
            security_evidence: &inputs.security_evidence,
            reproduction: &inputs.reproduction,
            exact_replay_evidence: &exact_replay_evidence,
            policy: &inputs.policy,
            created_at: inputs.created_at,
        };
        let receipt_digest = ReleaseDigest::from_canonical_bytes(&canonical_json(&payload)?);
        Ok(Self {
            receipt_id: inputs.receipt_id,
            tenant_id: inputs.tenant_id,
            authenticated_subject: inputs.authenticated_subject,
            candidate_source: inputs.candidate_source,
            incumbent_source: inputs.incumbent_source,
            candidate_metrics: inputs.candidate_metrics,
            incumbent_metrics: inputs.incumbent_metrics,
            holdout: inputs.holdout,
            operation_lineage: inputs.operation_lineage,
            security_evidence: inputs.security_evidence,
            reproduction: inputs.reproduction,
            exact_replay_evidence,
            policy: inputs.policy,
            created_at: inputs.created_at,
            receipt_digest,
        })
    }

    /// Creates an unsigned receipt only by cross-binding nonconstructible P3
    /// exact replay and evaluation evidence to every P3-owned receipt field.
    ///
    /// The result still requires [`sign_release_candidate_receipt`] and a
    /// caller-supplied scoped trust-policy verification before it can reach a
    /// promotion gate.
    #[cfg(feature = "agent-loop")]
    pub fn new(
        inputs: ReleaseCandidateReceiptInputs,
        exact_run_evidence: &ExactVerifiedRunEvidence,
        evaluation_evidence: &ExactEvaluationReceiptEvidence,
    ) -> ReleaseCandidateReceiptResult<Self> {
        let exact_replay_evidence =
            bind_inputs_to_verified_p3(&inputs, exact_run_evidence, evaluation_evidence)?;
        Self::from_bound_inputs(inputs, exact_replay_evidence)
    }

    /// Immutable receipt ID.
    pub fn receipt_id(&self) -> &ReleaseReceiptId {
        &self.receipt_id
    }

    /// Receipt tenant scope.
    pub fn tenant_id(&self) -> &ReleaseTenantId {
        &self.tenant_id
    }

    /// Receipt authenticated-subject scope.
    pub fn authenticated_subject(&self) -> &AuthenticatedSubject {
        &self.authenticated_subject
    }

    /// Clones the tenant/subject trust scope.
    pub fn scope(&self) -> ReleaseReceiptScope {
        ReleaseReceiptScope::new(self.tenant_id.clone(), self.authenticated_subject.clone())
    }

    /// Candidate source identity.
    pub fn candidate_source(&self) -> &ReleaseSource {
        &self.candidate_source
    }

    /// Incumbent source identity, which is the required baseline.
    pub fn incumbent_source(&self) -> &ReleaseSource {
        &self.incumbent_source
    }

    /// Candidate metrics.
    pub fn candidate_metrics(&self) -> &ReleaseMetrics {
        &self.candidate_metrics
    }

    /// Incumbent metrics, which are the required baseline metrics.
    pub fn incumbent_metrics(&self) -> &ReleaseMetrics {
        &self.incumbent_metrics
    }

    /// Fixed holdout evidence.
    pub fn holdout(&self) -> &ReleaseHoldoutEvidence {
        &self.holdout
    }

    /// Exact replay operation lineage.
    pub fn operation_lineage(&self) -> &ReleaseOperationLineage {
        &self.operation_lineage
    }

    /// Security-report evidence.
    pub fn security_evidence(&self) -> &ReleaseSecurityEvidence {
        &self.security_evidence
    }

    /// Reproduction artifact evidence.
    pub fn reproduction(&self) -> &ReleaseReproductionEvidence {
        &self.reproduction
    }

    /// Exact P3 replay evidence.
    pub fn exact_replay_evidence(&self) -> &ExactReplayEvidence {
        &self.exact_replay_evidence
    }

    /// Frozen P4 policy and digest.
    pub fn policy(&self) -> &ReleasePromotionPolicy {
        &self.policy
    }

    /// Receipt creation time.
    pub fn created_at(&self) -> DateTime<Utc> {
        self.created_at
    }

    /// SHA-256 digest of the canonical, digest-excluded receipt payload.
    pub fn receipt_digest(&self) -> &ReleaseDigest {
        &self.receipt_digest
    }

    /// RFC 8785 canonical receipt bytes, including the self-digest.
    pub fn canonical_bytes(&self) -> ReleaseCandidateReceiptResult<Vec<u8>> {
        canonical_json(&ReceiptWire::from(self))
    }

    fn inputs_clone(&self) -> ReleaseCandidateReceiptInputs {
        ReleaseCandidateReceiptInputs {
            receipt_id: self.receipt_id.clone(),
            tenant_id: self.tenant_id.clone(),
            authenticated_subject: self.authenticated_subject.clone(),
            candidate_source: self.candidate_source.clone(),
            incumbent_source: self.incumbent_source.clone(),
            candidate_metrics: self.candidate_metrics.clone(),
            incumbent_metrics: self.incumbent_metrics.clone(),
            holdout: self.holdout.clone(),
            operation_lineage: self.operation_lineage.clone(),
            security_evidence: self.security_evidence.clone(),
            reproduction: self.reproduction.clone(),
            policy: self.policy.clone(),
            created_at: self.created_at,
        }
    }


    /// Parses a signed-wire candidate into an untrusted, private intermediate.
    /// It cannot create a verified wrapper without a fresh P3 binding.
    fn parse_untrusted_canonical_bytes(bytes: &[u8]) -> ReleaseCandidateReceiptResult<Self> {
        if bytes.is_empty() || bytes.len() > MAX_CANONICAL_RECEIPT_BYTES {
            return Err(invalid(
                "receipt_bytes",
                "must be nonempty and no larger than one mebibyte",
            ));
        }
        let raw: RawReceipt = serde_json::from_slice(bytes).map_err(|error| {
            invalid("receipt_bytes", format!("must be a valid receipt object: {error}"))
        })?;
        if raw.schema != RELEASE_CANDIDATE_RECEIPT_SCHEMA {
            return Err(invalid("schema", "is not the release-candidate receipt schema"));
        }
        if raw.schema_version != RELEASE_CANDIDATE_RECEIPT_SCHEMA_VERSION {
            return Err(invalid(
                "schema_version",
                "is not the supported release-candidate receipt version",
            ));
        }
        let claimed_digest = ReleaseDigest::new(raw.receipt_digest)?;
        let created_at = DateTime::parse_from_rfc3339(&raw.created_at)
            .map_err(|error| invalid("created_at", format!("must be RFC 3339: {error}")))?
            .with_timezone(&Utc);
        let exact_replay_evidence = ExactReplayEvidence::from_raw(raw.exact_replay_evidence)?;
        let receipt = Self::from_bound_inputs(
            ReleaseCandidateReceiptInputs {
                receipt_id: ReleaseReceiptId::new(raw.receipt_id)?,
                tenant_id: ReleaseTenantId::new(raw.tenant_id)?,
                authenticated_subject: AuthenticatedSubject::new(raw.authenticated_subject)?,
                candidate_source: ReleaseSource::from_raw(raw.candidate_source)?,
                incumbent_source: ReleaseSource::from_raw(raw.incumbent_source)?,
                candidate_metrics: ReleaseMetrics::from_raw(raw.candidate_metrics)?,
                incumbent_metrics: ReleaseMetrics::from_raw(raw.incumbent_metrics)?,
                holdout: ReleaseHoldoutEvidence::from_raw(raw.holdout)?,
                operation_lineage: ReleaseOperationLineage::from_raw(raw.operation_lineage)?,
                security_evidence: ReleaseSecurityEvidence::from_raw(raw.security_evidence)?,
                reproduction: ReleaseReproductionEvidence::from_raw(raw.reproduction)?,
                policy: ReleasePromotionPolicy::from_raw(raw.policy)?,
                created_at,
            },
            exact_replay_evidence,
        )?;
        if receipt.receipt_digest != claimed_digest {
            return Err(ReleaseCandidateReceiptError::ReceiptDigestMismatch);
        }
        if receipt.canonical_bytes()? != bytes {
            return Err(ReleaseCandidateReceiptError::NonCanonicalReceipt);
        }
        Ok(receipt)
    }
}
#[cfg(test)]
/// Crate-private fixture-only exact evidence. Production code must use P3's
/// nonconstructible exact-evidence wrappers instead.
pub(crate) fn exact_replay_evidence_fixture(
    proof_digest: ReleaseDigest,
    export_digest: ReleaseDigest,
    evaluation_receipt_digest: ReleaseDigest,
) -> ExactReplayEvidence {
    ExactReplayEvidence::for_test(proof_digest, export_digest, evaluation_receipt_digest)
}

#[cfg(test)]
/// Crate-private fixture-only receipt builder. It is unavailable to production
/// callers so test digest values cannot become release authority.
pub(crate) fn release_candidate_receipt_fixture(
    inputs: ReleaseCandidateReceiptInputs,
    exact_replay_evidence: ExactReplayEvidence,
) -> ReleaseCandidateReceiptResult<ReleaseCandidateReceipt> {
    ReleaseCandidateReceipt::from_bound_inputs(inputs, exact_replay_evidence)
}


/// Detached Ed25519 signature metadata for a canonical receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseCandidateReceiptEnvelope {
    key_id: ReleaseSigningKeyId,
    receipt_digest: ReleaseDigest,
    signature: [u8; 64],
}

impl ReleaseCandidateReceiptEnvelope {
    /// Signing key identity selected by the caller-supplied trust policy.
    pub fn key_id(&self) -> &ReleaseSigningKeyId {
        &self.key_id
    }

    /// Canonical receipt digest to which this detached signature is bound.
    pub fn receipt_digest(&self) -> &ReleaseDigest {
        &self.receipt_digest
    }

    /// RFC 8785 canonical detached-envelope bytes for durable storage.
    pub fn canonical_bytes(&self) -> ReleaseCandidateReceiptResult<Vec<u8>> {
        canonical_json(&EnvelopeWire::from(self))
    }

    fn from_canonical_bytes(bytes: &[u8]) -> ReleaseCandidateReceiptResult<Self> {
        if bytes.is_empty() || bytes.len() > MAX_CANONICAL_RECEIPT_BYTES {
            return Err(invalid(
                "envelope_bytes",
                "must be nonempty and no larger than one mebibyte",
            ));
        }
        let raw: RawEnvelope = serde_json::from_slice(bytes).map_err(|error| {
            invalid("envelope_bytes", format!("must be a valid envelope object: {error}"))
        })?;
        if raw.schema != RELEASE_CANDIDATE_RECEIPT_ENVELOPE_SCHEMA
            || raw.schema_version != RELEASE_CANDIDATE_RECEIPT_ENVELOPE_SCHEMA_VERSION
            || raw.algorithm != "ed25519"
            || raw.domain != RELEASE_CANDIDATE_SIGNATURE_DOMAIN
        {
            return Err(invalid(
                "envelope",
                "uses an unsupported schema, algorithm, or signing domain",
            ));
        }
        let signature = BASE64_STANDARD
            .decode(raw.signature)
            .map_err(|_| ReleaseCandidateReceiptError::MalformedSignature)?;
        let signature: [u8; 64] = signature
            .try_into()
            .map_err(|_| ReleaseCandidateReceiptError::MalformedSignature)?;
        let envelope = Self {
            key_id: ReleaseSigningKeyId::new(raw.key_id)?,
            receipt_digest: ReleaseDigest::new(raw.receipt_digest)?,
            signature,
        };
        if envelope.canonical_bytes()? != bytes {
            return Err(ReleaseCandidateReceiptError::NonCanonicalEnvelope);
        }
        Ok(envelope)
    }
}

/// Caller-supplied outcome for a tenant/subject/key trust lookup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseTrustDecision {
    /// A scoped policy authorizes this exact Ed25519 public key.
    Trusted { verifying_key: [u8; 32] },
    /// Trust is absent or explicitly denied.
    Denied,
}

/// Trust policy required to verify a release receipt.
///
/// There is deliberately no blanket/default implementation. The verifier first
/// requires an expected frozen policy for the exact tenant/subject scope, then
/// resolves the key for that same scope. A policy may therefore never be
/// supplied only by a signed receipt itself.
pub trait ReleaseCandidateReceiptTrustPolicy {
    /// Returns the only policy allowed for this tenant/subject scope.
    fn expected_policy(&self, scope: &ReleaseReceiptScope) -> Option<&ReleasePromotionPolicy>;

    /// Resolves a trusted public key for this exact tenant/subject scope.
    fn resolve_verifying_key(
        &self,
        scope: &ReleaseReceiptScope,
        key_id: &ReleaseSigningKeyId,
    ) -> ReleaseTrustDecision;
}

/// Non-forgeable capability returned only after receipt verification succeeds.
#[derive(Debug, Clone)]
pub struct VerifiedReleaseCandidateReceipt {
    receipt: ReleaseCandidateReceipt,
    envelope: ReleaseCandidateReceiptEnvelope,
    canonical_receipt_bytes: Vec<u8>,
    canonical_envelope_bytes: Vec<u8>,
}

impl VerifiedReleaseCandidateReceipt {
    /// Borrows the fully verified typed receipt.
    pub fn receipt(&self) -> &ReleaseCandidateReceipt {
        &self.receipt
    }

    /// Borrows detached signature metadata that was verified for this receipt.
    pub fn envelope(&self) -> &ReleaseCandidateReceiptEnvelope {
        &self.envelope
    }
}

/// Signs a canonical receipt under the release-candidate-specific Ed25519 domain.
pub fn sign_release_candidate_receipt(
    receipt: &ReleaseCandidateReceipt,
    signing_key: &SigningKey,
    key_id: ReleaseSigningKeyId,
) -> ReleaseCandidateReceiptResult<ReleaseCandidateReceiptEnvelope> {
    let canonical_receipt_bytes = receipt.canonical_bytes()?;
    let signature = signing_key
        .sign(&signature_message(
            &key_id,
            receipt.receipt_digest(),
            &canonical_receipt_bytes,
        )?)
        .to_bytes();
    Ok(ReleaseCandidateReceiptEnvelope {
        key_id,
        receipt_digest: receipt.receipt_digest.clone(),
        signature,
    })
}

/// Verifies canonical receipt bytes, nonconstructible P3 exact evidence,
/// frozen scoped policy, trusted key, and detached Ed25519 signature before
/// yielding promotion-capable evidence.
///
/// The P3 inputs are mandatory: a signed JSON document with caller-entered
/// digest strings cannot mint this wrapper.
#[cfg(feature = "agent-loop")]
pub fn verify_release_candidate_receipt(
    receipt_bytes: &[u8],
    envelope: &ReleaseCandidateReceiptEnvelope,
    exact_run_evidence: &ExactVerifiedRunEvidence,
    evaluation_evidence: &ExactEvaluationReceiptEvidence,
    trust_policy: &impl ReleaseCandidateReceiptTrustPolicy,
) -> ReleaseCandidateReceiptResult<VerifiedReleaseCandidateReceipt> {
    let untrusted = ReleaseCandidateReceipt::parse_untrusted_canonical_bytes(receipt_bytes)?;
    let receipt = ReleaseCandidateReceipt::new(
        untrusted.inputs_clone(),
        exact_run_evidence,
        evaluation_evidence,
    )?;
    if receipt.canonical_bytes()? != receipt_bytes {
        return Err(ReleaseCandidateReceiptError::P3EvidenceMismatch(
            "canonical receipt/P3 binding",
        ));
    }
    verify_signed_bound_receipt(receipt_bytes, envelope, receipt, trust_policy)
}

fn verify_signed_bound_receipt(
    receipt_bytes: &[u8],
    envelope: &ReleaseCandidateReceiptEnvelope,
    receipt: ReleaseCandidateReceipt,
    trust_policy: &impl ReleaseCandidateReceiptTrustPolicy,
) -> ReleaseCandidateReceiptResult<VerifiedReleaseCandidateReceipt> {
    if envelope.receipt_digest != receipt.receipt_digest {
        return Err(ReleaseCandidateReceiptError::EnvelopeDigestMismatch);
    }
    let scope = receipt.scope();
    let expected_policy = trust_policy
        .expected_policy(&scope)
        .ok_or(ReleaseCandidateReceiptError::UntrustedPolicy)?;
    if expected_policy != &receipt.policy {
        return Err(ReleaseCandidateReceiptError::UntrustedPolicy);
    }
    let verifying_key = match trust_policy.resolve_verifying_key(&scope, &envelope.key_id) {
        ReleaseTrustDecision::Trusted { verifying_key } => verifying_key,
        ReleaseTrustDecision::Denied => return Err(ReleaseCandidateReceiptError::UntrustedKey),
    };
    let verifying_key = VerifyingKey::from_bytes(&verifying_key)
        .map_err(|_| ReleaseCandidateReceiptError::MalformedTrustedKey)?;
    let signature = Signature::from_slice(&envelope.signature)
        .map_err(|_| ReleaseCandidateReceiptError::MalformedSignature)?;
    verifying_key
        .verify_strict(
            &signature_message(&envelope.key_id, &envelope.receipt_digest, receipt_bytes)?,
            &signature,
        )
        .map_err(|_| ReleaseCandidateReceiptError::InvalidSignature)?;
    let canonical_envelope_bytes = envelope.canonical_bytes()?;
    Ok(VerifiedReleaseCandidateReceipt {
        receipt,
        envelope: envelope.clone(),
        canonical_receipt_bytes: receipt_bytes.to_vec(),
        canonical_envelope_bytes,
    })
}

#[cfg(test)]
pub(crate) fn verify_release_candidate_receipt_fixture(
    receipt_bytes: &[u8],
    envelope: &ReleaseCandidateReceiptEnvelope,
    trust_policy: &impl ReleaseCandidateReceiptTrustPolicy,
) -> ReleaseCandidateReceiptResult<VerifiedReleaseCandidateReceipt> {
    let receipt = ReleaseCandidateReceipt::parse_untrusted_canonical_bytes(receipt_bytes)?;
    verify_signed_bound_receipt(receipt_bytes, envelope, receipt, trust_policy)
}

/// Result of appending a verified receipt to an immutable ledger.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReleaseCandidateReceiptAppendResult {
    /// Immutable bytes were written for the first time.
    Inserted,
    /// An existing row had byte-identical receipt and envelope data.
    AlreadyPresent,
}

/// Append-only SQLite ledger for verified release-candidate receipts.
///
/// The ledger accepts only [`VerifiedReleaseCandidateReceipt`] values and
/// returns only re-verified wrappers. It intentionally exposes no raw receipt
/// retrieval or promotion-authority shortcut.
#[derive(Debug)]
pub struct ReleaseCandidateReceiptLedger {
    connection: Connection,
}

impl ReleaseCandidateReceiptLedger {
    /// Opens (or initializes) an append-only SQLite receipt ledger.
    pub fn open(path: impl AsRef<Path>) -> ReleaseCandidateReceiptResult<Self> {
        let connection = Connection::open(path)
            .map_err(|error| ReleaseCandidateReceiptError::Sqlite(error.to_string()))?;
        Self::from_connection(connection)
    }

    /// Creates an isolated in-memory ledger.
    pub fn open_in_memory() -> ReleaseCandidateReceiptResult<Self> {
        let connection = Connection::open_in_memory()
            .map_err(|error| ReleaseCandidateReceiptError::Sqlite(error.to_string()))?;
        Self::from_connection(connection)
    }

    fn from_connection(connection: Connection) -> ReleaseCandidateReceiptResult<Self> {
        initialize_ledger(&connection)?;
        Ok(Self { connection })
    }

    /// Appends verified canonical receipt bytes exactly once.
    pub fn append_verified(
        &self,
        verified: &VerifiedReleaseCandidateReceipt,
    ) -> ReleaseCandidateReceiptResult<ReleaseCandidateReceiptAppendResult> {
        let receipt = verified.receipt();
        let changed = self
            .connection
            .execute(
                "INSERT INTO release_candidate_receipts \
                 (receipt_id, receipt_digest, receipt_bytes, envelope_bytes) \
                 VALUES (?1, ?2, ?3, ?4) \
                 ON CONFLICT(receipt_id) DO NOTHING",
                params![
                    receipt.receipt_id.as_str(),
                    receipt.receipt_digest.as_str(),
                    &verified.canonical_receipt_bytes,
                    &verified.canonical_envelope_bytes,
                ],
            )
            .map_err(|error| ReleaseCandidateReceiptError::Sqlite(error.to_string()))?;
        if changed == 1 {
            return Ok(ReleaseCandidateReceiptAppendResult::Inserted);
        }

        let existing = self
            .connection
            .query_row(
                "SELECT receipt_digest, receipt_bytes, envelope_bytes \
                 FROM release_candidate_receipts WHERE receipt_id = ?1",
                params![receipt.receipt_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| ReleaseCandidateReceiptError::Sqlite(error.to_string()))?
            .ok_or_else(|| ReleaseCandidateReceiptError::Sqlite("conflicting row disappeared".into()))?;
        if existing.0 == receipt.receipt_digest.as_str()
            && existing.1 == verified.canonical_receipt_bytes
            && existing.2 == verified.canonical_envelope_bytes
        {
            Ok(ReleaseCandidateReceiptAppendResult::AlreadyPresent)
        } else {
            Err(ReleaseCandidateReceiptError::LedgerConflict {
                receipt_id: receipt.receipt_id.to_string(),
            })
        }
    }

    /// Loads one row and repeats canonical, P3, policy, key, and signature
    /// verification before returning the capability wrapper.
    #[cfg(feature = "agent-loop")]
    pub fn load_verified(
        &self,
        receipt_id: &ReleaseReceiptId,
        exact_run_evidence: &ExactVerifiedRunEvidence,
        evaluation_evidence: &ExactEvaluationReceiptEvidence,
        trust_policy: &impl ReleaseCandidateReceiptTrustPolicy,
    ) -> ReleaseCandidateReceiptResult<Option<VerifiedReleaseCandidateReceipt>> {
        self.load_verified_with(receipt_id, |receipt_bytes, envelope| {
            verify_release_candidate_receipt(
                receipt_bytes,
                envelope,
                exact_run_evidence,
                evaluation_evidence,
                trust_policy,
            )
        })
    }

    #[cfg(test)]
    pub(crate) fn load_verified_fixture(
        &self,
        receipt_id: &ReleaseReceiptId,
        trust_policy: &impl ReleaseCandidateReceiptTrustPolicy,
    ) -> ReleaseCandidateReceiptResult<Option<VerifiedReleaseCandidateReceipt>> {
        self.load_verified_with(receipt_id, |receipt_bytes, envelope| {
            verify_release_candidate_receipt_fixture(receipt_bytes, envelope, trust_policy)
        })
    }

    fn load_verified_with<F>(
        &self,
        receipt_id: &ReleaseReceiptId,
        verifier: F,
    ) -> ReleaseCandidateReceiptResult<Option<VerifiedReleaseCandidateReceipt>>
    where
        F: FnOnce(
            &[u8],
            &ReleaseCandidateReceiptEnvelope,
        ) -> ReleaseCandidateReceiptResult<VerifiedReleaseCandidateReceipt>,
    {
        let row = self
            .connection
            .query_row(
                "SELECT receipt_digest, receipt_bytes, envelope_bytes \
                 FROM release_candidate_receipts WHERE receipt_id = ?1",
                params![receipt_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, Vec<u8>>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| ReleaseCandidateReceiptError::Sqlite(error.to_string()))?;
        let Some((stored_digest, receipt_bytes, envelope_bytes)) = row else {
            return Ok(None);
        };
        let stored_digest = ReleaseDigest::new(stored_digest)?;
        let receipt = ReleaseCandidateReceipt::parse_untrusted_canonical_bytes(&receipt_bytes)?;
        if &stored_digest != receipt.receipt_digest() {
            return Err(ReleaseCandidateReceiptError::LedgerDigestMismatch);
        }
        if receipt.receipt_id() != receipt_id {
            return Err(ReleaseCandidateReceiptError::LedgerReceiptIdMismatch);
        }
        let envelope = ReleaseCandidateReceiptEnvelope::from_canonical_bytes(&envelope_bytes)?;
        let verified = verifier(&receipt_bytes, &envelope)?;
        if verified.canonical_envelope_bytes != envelope_bytes {
            return Err(ReleaseCandidateReceiptError::NonCanonicalEnvelope);
        }
        Ok(Some(verified))
    }
}

/// Returns the expected immutable SQLite schema fingerprint for diagnostics and
/// marker validation.
pub fn release_candidate_ledger_schema_fingerprint() -> ReleaseDigest {
    ReleaseDigest::from_canonical_bytes(LEDGER_SCHEMA_DESCRIPTOR.as_bytes())
}

fn initialize_ledger(connection: &Connection) -> ReleaseCandidateReceiptResult<()> {
    connection
        .execute_batch(
            "CREATE TABLE IF NOT EXISTS release_candidate_ledger_meta (
                 singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                 schema_version INTEGER NOT NULL,
                 schema_fingerprint TEXT NOT NULL
             ) STRICT;
             CREATE TABLE IF NOT EXISTS release_candidate_receipts (
                 receipt_id TEXT PRIMARY KEY NOT NULL,
                 receipt_digest TEXT NOT NULL,
                 receipt_bytes BLOB NOT NULL,
                 envelope_bytes BLOB NOT NULL,
                 UNIQUE (receipt_id, receipt_digest)
             ) STRICT;",
        )
        .map_err(|error| ReleaseCandidateReceiptError::Sqlite(error.to_string()))?;
    let expected_fingerprint = release_candidate_ledger_schema_fingerprint().to_string();
    let marker = connection
        .query_row(
            "SELECT schema_version, schema_fingerprint \
             FROM release_candidate_ledger_meta WHERE singleton = 1",
            [],
            |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(|error| ReleaseCandidateReceiptError::Sqlite(error.to_string()))?;
    match marker {
        Some((version, fingerprint))
            if version == i64::from(RELEASE_CANDIDATE_LEDGER_SCHEMA_VERSION)
                && fingerprint == expected_fingerprint =>
        {
            Ok(())
        }
        Some((version, fingerprint)) => Err(ReleaseCandidateReceiptError::LedgerSchemaMismatch {
            expected_version: RELEASE_CANDIDATE_LEDGER_SCHEMA_VERSION,
            expected_fingerprint,
            actual_version: version,
            actual_fingerprint: fingerprint,
        }),
        None => connection
            .execute(
                "INSERT INTO release_candidate_ledger_meta \
                 (singleton, schema_version, schema_fingerprint) VALUES (1, ?1, ?2)",
                params![
                    i64::from(RELEASE_CANDIDATE_LEDGER_SCHEMA_VERSION),
                    expected_fingerprint,
                ],
            )
            .map(|_| ())
            .map_err(|error| ReleaseCandidateReceiptError::Sqlite(error.to_string())),
    }
}

fn checked_label(field: &'static str, value: String) -> ReleaseCandidateReceiptResult<String> {
    if value.is_empty() || value.len() > MAX_LABEL_LENGTH {
        return Err(invalid(
            field,
            format!("must contain 1 through {MAX_LABEL_LENGTH} bytes"),
        ));
    }
    if value.trim() != value || value.chars().any(char::is_control) {
        return Err(invalid(
            field,
            "must not have surrounding whitespace or control characters",
        ));
    }
    Ok(value)
}

fn checked_metric(
    field: &'static str,
    value: f64,
    normalized: bool,
) -> ReleaseCandidateReceiptResult<()> {
    if !value.is_finite() || value < 0.0 || (normalized && value > 1.0) {
        return Err(invalid(
            field,
            if normalized {
                "must be a finite number in [0, 1]"
            } else {
                "must be a finite nonnegative number"
            },
        ));
    }
    Ok(())
}

fn invalid(field: &'static str, reason: impl Into<String>) -> ReleaseCandidateReceiptError {
    ReleaseCandidateReceiptError::InvalidField {
        field,
        reason: reason.into(),
    }
}

fn canonical_json(value: &impl Serialize) -> ReleaseCandidateReceiptResult<Vec<u8>> {
    serde_json_canonicalizer::to_vec(value)
        .map_err(|error| ReleaseCandidateReceiptError::Canonicalization(error.to_string()))
}

fn signature_message(
    key_id: &ReleaseSigningKeyId,
    receipt_digest: &ReleaseDigest,
    receipt_bytes: &[u8],
) -> ReleaseCandidateReceiptResult<Vec<u8>> {
    let key_id = key_id.as_str().as_bytes();
    let receipt_digest = receipt_digest.as_str().as_bytes();
    let lengths = [key_id.len(), receipt_digest.len(), receipt_bytes.len()];
    let encoded_lengths = lengths.into_iter().try_fold(0_usize, |total, length| {
        total
            .checked_add(std::mem::size_of::<u64>())
            .and_then(|total| total.checked_add(length))
            .ok_or_else(|| invalid("signature preimage", "length overflow"))
    })?;
    let mut message = Vec::with_capacity(
        RELEASE_CANDIDATE_SIGNATURE_DOMAIN.len() + 1 + encoded_lengths,
    );
    message.extend_from_slice(RELEASE_CANDIDATE_SIGNATURE_DOMAIN.as_bytes());
    message.push(0);
    for value in [key_id, receipt_digest, receipt_bytes] {
        let length = u64::try_from(value.len())
            .map_err(|_| invalid("signature preimage", "length does not fit u64"))?;
        message.extend_from_slice(&length.to_be_bytes());
        message.extend_from_slice(value);
    }
    Ok(message)
}

#[derive(Serialize)]
struct PolicyDigestPayload<'a> {
    schema: &'static str,
    schema_version: u32,
    policy_id: &'a ReleasePolicyId,
    max_quality_regression: f64,
    max_cost_per_task_regression: f64,
    max_p95_latency_regression: f64,
    minimum_lineage_coverage: f64,
    max_critical_findings: u64,
    max_high_findings: u64,
    require_exact_replay_evidence: bool,
    require_fixed_holdout: bool,
    require_reproduction: bool,
    require_baseline: bool,
    maximum_soak_heartbeat_gap_ms: u64,
}

#[derive(Serialize)]
struct ReceiptDigestPayload<'a> {
    schema: &'static str,
    schema_version: u32,
    receipt_id: &'a ReleaseReceiptId,
    tenant_id: &'a ReleaseTenantId,
    authenticated_subject: &'a AuthenticatedSubject,
    candidate_source: &'a ReleaseSource,
    incumbent_source: &'a ReleaseSource,
    candidate_metrics: &'a ReleaseMetrics,
    incumbent_metrics: &'a ReleaseMetrics,
    holdout: &'a ReleaseHoldoutEvidence,
    operation_lineage: &'a ReleaseOperationLineage,
    security_evidence: &'a ReleaseSecurityEvidence,
    reproduction: &'a ReleaseReproductionEvidence,
    exact_replay_evidence: &'a ExactReplayEvidence,
    policy: &'a ReleasePromotionPolicy,
    created_at: DateTime<Utc>,
}

#[derive(Serialize)]
struct ReceiptWire<'a> {
    schema: &'static str,
    schema_version: u32,
    receipt_id: &'a ReleaseReceiptId,
    tenant_id: &'a ReleaseTenantId,
    authenticated_subject: &'a AuthenticatedSubject,
    candidate_source: &'a ReleaseSource,
    incumbent_source: &'a ReleaseSource,
    candidate_metrics: &'a ReleaseMetrics,
    incumbent_metrics: &'a ReleaseMetrics,
    holdout: &'a ReleaseHoldoutEvidence,
    operation_lineage: &'a ReleaseOperationLineage,
    security_evidence: &'a ReleaseSecurityEvidence,
    reproduction: &'a ReleaseReproductionEvidence,
    exact_replay_evidence: &'a ExactReplayEvidence,
    policy: &'a ReleasePromotionPolicy,
    created_at: DateTime<Utc>,
    receipt_digest: &'a ReleaseDigest,
}

impl<'a> From<&'a ReleaseCandidateReceipt> for ReceiptWire<'a> {
    fn from(receipt: &'a ReleaseCandidateReceipt) -> Self {
        Self {
            schema: RELEASE_CANDIDATE_RECEIPT_SCHEMA,
            schema_version: RELEASE_CANDIDATE_RECEIPT_SCHEMA_VERSION,
            receipt_id: &receipt.receipt_id,
            tenant_id: &receipt.tenant_id,
            authenticated_subject: &receipt.authenticated_subject,
            candidate_source: &receipt.candidate_source,
            incumbent_source: &receipt.incumbent_source,
            candidate_metrics: &receipt.candidate_metrics,
            incumbent_metrics: &receipt.incumbent_metrics,
            holdout: &receipt.holdout,
            operation_lineage: &receipt.operation_lineage,
            security_evidence: &receipt.security_evidence,
            reproduction: &receipt.reproduction,
            exact_replay_evidence: &receipt.exact_replay_evidence,
            policy: &receipt.policy,
            created_at: receipt.created_at,
            receipt_digest: &receipt.receipt_digest,
        }
    }
}

#[derive(Serialize)]
struct EnvelopeWire<'a> {
    schema: &'static str,
    schema_version: u32,
    algorithm: &'static str,
    domain: &'static str,
    key_id: &'a ReleaseSigningKeyId,
    receipt_digest: &'a ReleaseDigest,
    signature: String,
}

impl<'a> From<&'a ReleaseCandidateReceiptEnvelope> for EnvelopeWire<'a> {
    fn from(envelope: &'a ReleaseCandidateReceiptEnvelope) -> Self {
        Self {
            schema: RELEASE_CANDIDATE_RECEIPT_ENVELOPE_SCHEMA,
            schema_version: RELEASE_CANDIDATE_RECEIPT_ENVELOPE_SCHEMA_VERSION,
            algorithm: "ed25519",
            domain: RELEASE_CANDIDATE_SIGNATURE_DOMAIN,
            key_id: &envelope.key_id,
            receipt_digest: &envelope.receipt_digest,
            signature: BASE64_STANDARD.encode(envelope.signature),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSource {
    repository: String,
    revision_algorithm: String,
    full_sha: String,
    tree_digest: String,
    artifact_digest: String,
}

impl ReleaseSource {
    fn from_raw(raw: RawSource) -> ReleaseCandidateReceiptResult<Self> {
        Self::new(
            raw.repository,
            ReleaseRevisionAlgorithm::parse_wire(raw.revision_algorithm)?,
            raw.full_sha,
            ReleaseDigest::new(raw.tree_digest)?,
            ReleaseDigest::new(raw.artifact_digest)?,
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawHoldoutEvidence {
    holdout_id: String,
    holdout_digest: String,
    seed: u64,
    sample_count: u64,
}

impl ReleaseHoldoutEvidence {
    fn from_raw(raw: RawHoldoutEvidence) -> ReleaseCandidateReceiptResult<Self> {
        Self::new(
            raw.holdout_id,
            ReleaseDigest::new(raw.holdout_digest)?,
            raw.seed,
            raw.sample_count,
        )
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawMetrics {
    quality: f64,
    cost_per_task: f64,
    p95_latency: f64,
}

impl ReleaseMetrics {
    fn from_raw(raw: RawMetrics) -> ReleaseCandidateReceiptResult<Self> {
        Self::new(raw.quality, raw.cost_per_task, raw.p95_latency)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawOperationLineage {
    total_operations: u64,
    lineaged_operations: u64,
}

impl ReleaseOperationLineage {
    fn from_raw(raw: RawOperationLineage) -> ReleaseCandidateReceiptResult<Self> {
        Self::new(raw.total_operations, raw.lineaged_operations)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawSecurityEvidence {
    critical_findings: u64,
    high_findings: u64,
    report_digest: String,
}

impl ReleaseSecurityEvidence {
    fn from_raw(raw: RawSecurityEvidence) -> ReleaseCandidateReceiptResult<Self> {
        Ok(Self::new(
            raw.critical_findings,
            raw.high_findings,
            ReleaseDigest::new(raw.report_digest)?,
        ))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReproductionEvidence {
    artifact_id: String,
    artifact_digest: String,
}

impl ReleaseReproductionEvidence {
    fn from_raw(raw: RawReproductionEvidence) -> ReleaseCandidateReceiptResult<Self> {
        Self::new(raw.artifact_id, ReleaseDigest::new(raw.artifact_digest)?)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawExactReplayEvidence {
    proof_digest: String,
    export_digest: String,
    reconstruction_digest: String,
    evaluation_receipt_digest: String,
}

impl ExactReplayEvidence {
    fn from_raw(raw: RawExactReplayEvidence) -> ReleaseCandidateReceiptResult<Self> {
        Ok(Self::from_untrusted_wire(
            ReleaseDigest::new(raw.proof_digest)?,
            ReleaseDigest::new(raw.export_digest)?,
            ReleaseDigest::new(raw.reconstruction_digest)?,
            ReleaseDigest::new(raw.evaluation_receipt_digest)?,
        ))
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPolicy {
    policy_id: String,
    max_quality_regression: f64,
    max_cost_per_task_regression: f64,
    max_p95_latency_regression: f64,
    minimum_lineage_coverage: f64,
    max_critical_findings: u64,
    max_high_findings: u64,
    require_exact_replay_evidence: bool,
    require_fixed_holdout: bool,
    require_reproduction: bool,
    require_baseline: bool,
    maximum_soak_heartbeat_gap_ms: u64,
    policy_digest: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReceipt {
    schema: String,
    schema_version: u32,
    receipt_id: String,
    tenant_id: String,
    authenticated_subject: String,
    candidate_source: RawSource,
    incumbent_source: RawSource,
    candidate_metrics: RawMetrics,
    incumbent_metrics: RawMetrics,
    holdout: RawHoldoutEvidence,
    operation_lineage: RawOperationLineage,
    security_evidence: RawSecurityEvidence,
    reproduction: RawReproductionEvidence,
    exact_replay_evidence: RawExactReplayEvidence,
    policy: RawPolicy,
    created_at: String,
    receipt_digest: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawEnvelope {
    schema: String,
    schema_version: u32,
    algorithm: String,
    domain: String,
    key_id: String,
    receipt_digest: String,
    signature: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use tempfile::TempDir;

    #[derive(Clone)]
    struct TestTrust {
        scope: ReleaseReceiptScope,
        policy: ReleasePromotionPolicy,
        key_id: ReleaseSigningKeyId,
        verifying_key: [u8; 32],
    }

    impl ReleaseCandidateReceiptTrustPolicy for TestTrust {
        fn expected_policy(&self, scope: &ReleaseReceiptScope) -> Option<&ReleasePromotionPolicy> {
            (scope == &self.scope).then_some(&self.policy)
        }

        fn resolve_verifying_key(
            &self,
            scope: &ReleaseReceiptScope,
            key_id: &ReleaseSigningKeyId,
        ) -> ReleaseTrustDecision {
            if scope == &self.scope && key_id == &self.key_id {
                ReleaseTrustDecision::Trusted {
                    verifying_key: self.verifying_key,
                }
            } else {
                ReleaseTrustDecision::Denied
            }
        }
    }

    fn digest(byte: char) -> ReleaseDigest {
        ReleaseDigest::new(byte.to_string().repeat(64)).expect("valid test digest")
    }

    fn policy(name: &str) -> ReleasePromotionPolicy {
        ReleasePromotionPolicy::new(ReleasePolicyId::new(name).expect("policy ID"))
            .expect("policy")
    }

    fn source(name: &str, byte: char) -> ReleaseSource {
        ReleaseSource::new(
            format!("acme/{name}"),
            ReleaseRevisionAlgorithm::GitSha1,
            byte.to_string().repeat(40),
            digest(byte),
            digest(if byte == 'a' { 'b' } else { 'a' }),
        )
        .expect("source")
    }

    fn receipt_inputs(receipt_id: &str, policy: ReleasePromotionPolicy) -> ReleaseCandidateReceiptInputs {
        ReleaseCandidateReceiptInputs {
            receipt_id: ReleaseReceiptId::new(receipt_id).expect("receipt ID"),
            tenant_id: ReleaseTenantId::new("tenant-a").expect("tenant"),
            authenticated_subject: AuthenticatedSubject::new("oidc:subject-a").expect("subject"),
            candidate_source: source("candidate", 'a'),
            incumbent_source: source("incumbent", 'c'),
            candidate_metrics: ReleaseMetrics::new(0.81, 1.0, 10.0).expect("candidate metrics"),
            incumbent_metrics: ReleaseMetrics::new(0.82, 0.95, 9.5).expect("incumbent metrics"),
            holdout: ReleaseHoldoutEvidence::new("holdout-v1", digest('d'), 42, 100)
                .expect("holdout"),
            operation_lineage: ReleaseOperationLineage::new(100, 100).expect("lineage"),
            security_evidence: ReleaseSecurityEvidence::new(0, 0, digest('e')),
            reproduction: ReleaseReproductionEvidence::new("repro-v1", digest('f')).expect("repro"),
            policy,
            created_at: DateTime::parse_from_rfc3339("2026-07-20T12:00:00Z")
                .expect("timestamp")
                .with_timezone(&Utc),
        }
    }

    fn signed_fixture(
        receipt_id: &str,
        policy: ReleasePromotionPolicy,
    ) -> (
        ReleaseCandidateReceipt,
        ReleaseCandidateReceiptEnvelope,
        Vec<u8>,
        TestTrust,
        SigningKey,
    ) {
        let receipt = release_candidate_receipt_fixture(
            receipt_inputs(receipt_id, policy.clone()),
            exact_replay_evidence_fixture(digest('1'), digest('2'), digest('3')),
        )
        .expect("receipt");
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let key_id = ReleaseSigningKeyId::new("release-key-a").expect("key ID");
        let envelope = sign_release_candidate_receipt(&receipt, &signing_key, key_id.clone())
            .expect("signed envelope");
        let bytes = receipt.canonical_bytes().expect("canonical bytes");
        let trust = TestTrust {
            scope: receipt.scope(),
            policy,
            key_id,
            verifying_key: signing_key.verifying_key().to_bytes(),
        };
        (receipt, envelope, bytes, trust, signing_key)
    }

    #[test]
    fn valid_sign_verify_persist_and_reopen() {
        let (receipt, envelope, bytes, trust, _) = signed_fixture("receipt-valid", policy("p4"));
        let verified = verify_release_candidate_receipt_fixture(&bytes, &envelope, &trust)
            .expect("verified receipt");
        assert_eq!(verified.receipt().receipt_digest(), receipt.receipt_digest());

        let temporary = TempDir::new().expect("temporary DB directory");
        let database = temporary.path().join("receipts.sqlite");
        let ledger = ReleaseCandidateReceiptLedger::open(&database).expect("ledger");
        assert_eq!(
            ledger.append_verified(&verified).expect("append"),
            ReleaseCandidateReceiptAppendResult::Inserted
        );
        drop(ledger);

        let reopened = ReleaseCandidateReceiptLedger::open(&database).expect("reopen ledger");
        let loaded = reopened
            .load_verified_fixture(receipt.receipt_id(), &trust)
            .expect("load")
            .expect("stored receipt");
        assert_eq!(loaded.receipt().receipt_digest(), receipt.receipt_digest());
    }

    #[test]
    fn wrong_key_tenant_subject_and_policy_fail_closed() {
        let (receipt, envelope, bytes, trust, _) = signed_fixture("receipt-scope", policy("p4"));
        let wrong_key = TestTrust {
            verifying_key: SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes(),
            ..trust.clone()
        };
        assert!(verify_release_candidate_receipt_fixture(&bytes, &envelope, &wrong_key).is_err());

        let wrong_tenant = TestTrust {
            scope: ReleaseReceiptScope::new(
                ReleaseTenantId::new("tenant-b").expect("tenant"),
                receipt.authenticated_subject().clone(),
            ),
            ..trust.clone()
        };
        assert!(verify_release_candidate_receipt_fixture(&bytes, &envelope, &wrong_tenant).is_err());

        let wrong_subject = TestTrust {
            scope: ReleaseReceiptScope::new(
                receipt.tenant_id().clone(),
                AuthenticatedSubject::new("oidc:subject-b").expect("subject"),
            ),
            ..trust.clone()
        };
        assert!(verify_release_candidate_receipt_fixture(&bytes, &envelope, &wrong_subject).is_err());

        let wrong_policy = TestTrust {
            policy: policy("p4-other"),
            ..trust
        };
        assert!(verify_release_candidate_receipt_fixture(&bytes, &envelope, &wrong_policy).is_err());
    }

    #[test]
    fn altered_canonical_bytes_and_signature_fail_closed() {
        let (_, envelope, bytes, trust, _) = signed_fixture("receipt-altered", policy("p4"));
        let mut noncanonical = bytes.clone();
        noncanonical.push(b' ');
        assert!(verify_release_candidate_receipt_fixture(&noncanonical, &envelope, &trust).is_err());

        let mut altered_signature = envelope;
        altered_signature.signature[0] ^= 0x80;
        assert!(verify_release_candidate_receipt_fixture(&bytes, &altered_signature, &trust).is_err());
    }

    #[test]
    fn short_sha_digest_and_nonfinite_metrics_are_rejected() {
        assert!(ReleaseDigest::new("a".repeat(63)).is_err());
        assert!(ReleaseSource::new(
            "acme/repo",
            ReleaseRevisionAlgorithm::GitSha1,
            "a".repeat(39),
            digest('a'),
            digest('b'),
        )
        .is_err());
        assert!(ReleaseSource::new(
            "acme/repo",
            ReleaseRevisionAlgorithm::GitSha1,
            "a".repeat(64),
            digest('a'),
            digest('b'),
        )
        .is_err());
        assert!(ReleaseRevisionAlgorithm::parse_wire("git_sha256".into()).is_err());
        assert!(ReleaseMetrics::new(f64::NAN, 1.0, 1.0).is_err());
        assert!(ReleaseMetrics::new(0.5, f64::INFINITY, 1.0).is_err());
        assert!(ReleaseMetrics::new(1.01, 1.0, 1.0).is_err());
    }

    #[test]
    fn revision_algorithm_is_canonical_and_algorithm_length_mismatches_fail_closed() {
        let (receipt, _, _, _, _) = signed_fixture("receipt-revision-algorithm", policy("p4"));
        let canonical = receipt.canonical_bytes().expect("canonical receipt");
        let canonical_text = std::str::from_utf8(&canonical).expect("UTF-8 JSON");
        assert!(canonical_text.contains("\"revision_algorithm\":\"git_sha1\""));

        let mut unsupported_algorithm: Value =
            serde_json::from_slice(&canonical).expect("receipt JSON");
        unsupported_algorithm["candidate_source"]["revision_algorithm"] =
            Value::String("git_sha256".into());
        let unsupported_algorithm =
            canonical_json(&unsupported_algorithm).expect("canonical unsupported algorithm");
        assert!(ReleaseCandidateReceipt::parse_untrusted_canonical_bytes(&unsupported_algorithm).is_err());

        let mut algorithm_length_mismatch: Value =
            serde_json::from_slice(&canonical).expect("receipt JSON");
        algorithm_length_mismatch["candidate_source"]["full_sha"] =
            Value::String("a".repeat(64));
        let algorithm_length_mismatch =
            canonical_json(&algorithm_length_mismatch).expect("canonical mismatched source");
        assert!(
            ReleaseCandidateReceipt::parse_untrusted_canonical_bytes(&algorithm_length_mismatch)
                .is_err()
        );
    }

    #[test]
    fn missing_baseline_holdout_reproduction_and_exact_evidence_are_rejected() {
        let (receipt, _, _, _, _) = signed_fixture("receipt-missing", policy("p4"));
        let canonical = receipt.canonical_bytes().expect("canonical receipt");
        for field in [
            "incumbent_source",
            "holdout",
            "reproduction",
            "exact_replay_evidence",
        ] {
            let mut value: Value = serde_json::from_slice(&canonical).expect("receipt JSON");
            value
                .as_object_mut()
                .expect("receipt object")
                .remove(field)
                .expect("field exists");
            let tampered = serde_json_canonicalizer::to_vec(&value).expect("canonical tamper");
            assert!(
                ReleaseCandidateReceipt::parse_untrusted_canonical_bytes(&tampered).is_err(),
                "missing {field} unexpectedly parsed"
            );
        }
    }

    #[test]
    fn duplicate_identical_is_idempotent_and_conflict_is_rejected() {
        let (_, envelope, bytes, trust, signing_key) =
            signed_fixture("receipt-duplicate", policy("p4"));
        let verified =
            verify_release_candidate_receipt_fixture(&bytes, &envelope, &trust).expect("verified");
        let ledger = ReleaseCandidateReceiptLedger::open_in_memory().expect("ledger");
        assert_eq!(
            ledger.append_verified(&verified).expect("insert"),
            ReleaseCandidateReceiptAppendResult::Inserted
        );
        assert_eq!(
            ledger.append_verified(&verified).expect("idempotent insert"),
            ReleaseCandidateReceiptAppendResult::AlreadyPresent
        );

        let mut conflicting_inputs = receipt_inputs("receipt-duplicate", policy("p4"));
        conflicting_inputs.candidate_metrics = ReleaseMetrics::new(0.80, 1.0, 10.0).expect("metrics");
        let conflicting = release_candidate_receipt_fixture(
            conflicting_inputs,
            exact_replay_evidence_fixture(digest('1'), digest('2'), digest('3')),
        )
        .expect("conflicting receipt");
        let conflicting_envelope = sign_release_candidate_receipt(
            &conflicting,
            &signing_key,
            ReleaseSigningKeyId::new("release-key-a").expect("key"),
        )
        .expect("conflicting signature");
        let conflicting_verified = verify_release_candidate_receipt_fixture(
            &conflicting.canonical_bytes().expect("canonical"),
            &conflicting_envelope,
            &trust,
        )
        .expect("conflicting verifies before persistence");
        assert!(matches!(
            ledger.append_verified(&conflicting_verified),
            Err(ReleaseCandidateReceiptError::LedgerConflict { .. })
        ));
    }

    #[test]
    fn ledger_data_tamper_and_schema_fingerprint_tamper_are_rejected() {
        let (receipt, envelope, bytes, trust, _) = signed_fixture("receipt-tamper", policy("p4"));
        let verified =
            verify_release_candidate_receipt_fixture(&bytes, &envelope, &trust).expect("verified");
        let temporary = TempDir::new().expect("temporary DB directory");
        let database = temporary.path().join("receipts.sqlite");
        let ledger = ReleaseCandidateReceiptLedger::open(&database).expect("ledger");
        ledger.append_verified(&verified).expect("append");
        drop(ledger);

        let connection = Connection::open(&database).expect("open for tamper");
        connection
            .execute(
                "UPDATE release_candidate_receipts SET receipt_bytes = ?1 WHERE receipt_id = ?2",
                params![b"{}".to_vec(), receipt.receipt_id().as_str()],
            )
            .expect("tamper receipt bytes");
        drop(connection);
        let reopened = ReleaseCandidateReceiptLedger::open(&database).expect("schema still valid");
        assert!(reopened
            .load_verified_fixture(receipt.receipt_id(), &trust)
            .is_err());
        drop(reopened);

        let connection = Connection::open(&database).expect("open for marker tamper");
        connection
            .execute(
                "UPDATE release_candidate_ledger_meta SET schema_fingerprint = '0' WHERE singleton = 1",
                [],
            )
            .expect("tamper marker");
        drop(connection);
        assert!(matches!(
            ReleaseCandidateReceiptLedger::open(&database),
            Err(ReleaseCandidateReceiptError::LedgerSchemaMismatch { .. })
        ));
    }
}
