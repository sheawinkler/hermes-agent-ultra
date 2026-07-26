//! Detached Ed25519 signature verification for already verified run-export
//! replay plans.
//!
//! This module deliberately does not create keys, read files, or decide which
//! keys are trusted. Export integrity is established first by the run-export
//! verifier; a caller-supplied policy then resolves the detached proof's
//! `key_id`. It produces policy-relative signature evidence, never authentication,
//! exact-replay, effect-execution, or promotion authority.

use base64::Engine as _;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::deterministic_replay::{
    DeterministicReplayReport, ReplayEvidenceCounters, ReplayFidelity, VerifiedRunReplayPlan,
};
use crate::run_export::{verify_run_export_replay_plan_bytes, RunExportError};
use crate::run_ledger::{ExactReplayState, RunId, RunOutcome, TerminalReason};

/// Stable wire schema for a detached signed run proof.
pub const RUN_PROOF_SCHEMA: &str = "hermes.session-run.proof";
/// Current detached-proof wire schema version.
pub const RUN_PROOF_SCHEMA_VERSION: u32 = 1;
/// Only accepted detached-proof signature algorithm.
pub const RUN_PROOF_SIGNATURE_ALGORITHM: &str = "ed25519";

const RUN_PROOF_SIGNING_DOMAIN: &[u8] = b"hermes.session-run.proof.ed25519/v1\0";
const MAX_ENVELOPE_BYTES: usize = 16 * 1024;
const MAX_KEY_ID_BYTES: usize = 256;
const MAX_POLICY_ID_BYTES: usize = 256;
const ED25519_PUBLIC_KEY_BYTES: usize = 32;
const ED25519_SIGNATURE_BYTES: usize = 64;

pub type RunProofResult<T> = Result<T, RunProofError>;

/// Caller-owned decision for a proof `key_id` at verification time.
///
/// A policy must evaluate its own key validity windows before returning this
/// value. `NotCurrentlyValid` covers a key that is expired or not yet valid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunProofTrustDecision {
    /// A currently valid trusted Ed25519 public key, encoded as its 32 raw
    /// bytes. Length and point validation remain verifier responsibilities.
    Trusted { public_key: Vec<u8> },
    /// No trusted key is known for this identifier.
    Unknown,
    /// The identifier is known but revoked and must never verify a proof.
    Revoked,
    /// The identifier is known but not valid at this verification time.
    NotCurrentlyValid,
}

/// Scope supplied to the caller-owned trust policy.
///
/// Every value is extracted from the same already verified export bytes.
/// `owner_id` is deliberately absent: it is a lease owner, not an authenticated
/// principal. Current export schemas have no authenticated subject, so policies
/// receive `None` and the result cannot become authentication authority.
#[derive(Debug, Clone, Copy)]
pub struct RunProofTrustContext<'a> {
    tenant_id: &'a str,
    branch_id: Option<&'a str>,
    authenticated_subject: Option<&'a str>,
    run_id: RunId,
    export_digest: &'a str,
}

impl RunProofTrustContext<'_> {
    pub fn tenant_id(&self) -> &str {
        self.tenant_id
    }

    pub fn branch_id(&self) -> Option<&str> {
        self.branch_id
    }

    /// Current v2 exports lack a real authenticated principal, so this is
    /// always `None` until a P4 export schema provides one.
    pub fn authenticated_subject(&self) -> Option<&str> {
        self.authenticated_subject
    }

    pub const fn run_id(&self) -> RunId {
        self.run_id
    }

    pub fn export_digest(&self) -> &str {
        self.export_digest
    }
}

/// Resolves detached-proof signer identifiers through caller-controlled,
/// explicitly scoped trust.
///
/// `policy_id` and `policy_digest` are caller claims recorded as verifier
/// provenance, not an embedded trust root, authenticated authority, or
/// promotion authorization. Implementations must fail closed for unavailable
/// trust material by returning a non-trusted decision.
pub trait RunProofTrustPolicy {
    /// Stable caller-defined identity for the policy that made the decision.
    fn policy_id(&self) -> &str;

    /// Canonical lowercase SHA-256 digest of the policy that made this decision.
    fn policy_digest(&self) -> &str;

    fn resolve_key(&self, context: RunProofTrustContext<'_>, key_id: &str)
        -> RunProofTrustDecision;
}

/// Fail-closed errors for detached proof serialization, trust, and verification.
#[derive(Debug, Error)]
pub enum RunProofError {
    #[error("run export/replay verification failed: {0}")]
    Export(#[from] RunExportError),
    #[error("run proof envelope exceeds {MAX_ENVELOPE_BYTES} bytes")]
    EnvelopeTooLarge,
    #[error("run proof envelope is not valid JSON: {0}")]
    InvalidEnvelope(String),
    #[error("run proof envelope is not canonical JCS")]
    NonCanonicalEnvelope,
    #[error("run proof canonicalization failed: {0}")]
    Canonicalization(String),
    #[error("unsupported run proof schema {schema:?} version {schema_version}")]
    UnsupportedSchema { schema: String, schema_version: u32 },
    #[error("unsupported run proof signature algorithm {0:?}")]
    UnsupportedSignatureAlgorithm(String),
    #[error("invalid run proof key identifier")]
    InvalidKeyId,
    #[error("run proof policy identity is missing or malformed")]
    InvalidPolicyId,
    #[error("run proof trust policy digest is not canonical SHA-256")]
    InvalidTrustPolicyDigest,
    #[error("verified export lacks a usable tenant/branch/run verification scope")]
    InvalidVerifiedExportScope,
    #[error("run proof export digest is not canonical SHA-256")]
    InvalidExportDigest,
    #[error("run proof export digest does not match the verified export")]
    ExportDigestMismatch,
    #[error("verified replay plan did not retain the supplied export bytes")]
    ReplayPlanBytesMismatch,
    #[error("run proof key {0:?} is unknown")]
    UnknownKey(String),
    #[error("run proof key {0:?} is revoked")]
    RevokedKey(String),
    #[error("run proof key {0:?} is not currently valid")]
    KeyNotCurrentlyValid(String),
    #[error("trusted run proof public key is malformed")]
    MalformedTrustedKey,
    #[error("run proof signature is malformed")]
    MalformedSignature,
    #[error("run proof signature verification failed")]
    InvalidSignature,
}

/// A versioned detached signature envelope.
///
/// Its fields are private to keep callers on the canonical serialization path.
/// The envelope intentionally contains no public key; `key_id` is resolved only
/// through [`RunProofTrustPolicy`] during verification.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunProofEnvelope {
    schema: String,
    schema_version: u32,
    signature_algorithm: String,
    key_id: String,
    export_digest: String,
    signature: String,
}

impl RunProofEnvelope {
    /// Serialize the complete detached envelope as canonical JCS bytes.
    pub fn canonical_bytes(&self) -> RunProofResult<Vec<u8>> {
        self.validate_header()?;
        let _ = self.decode_signature()?;
        canonical_json(self)
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    pub fn export_digest(&self) -> &str {
        &self.export_digest
    }

    fn parse_canonical_bytes(bytes: &[u8]) -> RunProofResult<Self> {
        if bytes.len() > MAX_ENVELOPE_BYTES {
            return Err(RunProofError::EnvelopeTooLarge);
        }
        let envelope: Self = serde_json::from_slice(bytes)
            .map_err(|error| RunProofError::InvalidEnvelope(error.to_string()))?;
        let canonical = canonical_json(&envelope)?;
        if canonical != bytes {
            return Err(RunProofError::NonCanonicalEnvelope);
        }
        envelope.validate_header()?;
        Ok(envelope)
    }

    fn validate_header(&self) -> RunProofResult<()> {
        if self.schema != RUN_PROOF_SCHEMA || self.schema_version != RUN_PROOF_SCHEMA_VERSION {
            return Err(RunProofError::UnsupportedSchema {
                schema: self.schema.clone(),
                schema_version: self.schema_version,
            });
        }
        if self.signature_algorithm != RUN_PROOF_SIGNATURE_ALGORITHM {
            return Err(RunProofError::UnsupportedSignatureAlgorithm(
                self.signature_algorithm.clone(),
            ));
        }
        validate_key_id(&self.key_id)?;
        validate_export_digest(&self.export_digest)
    }

    fn signing_preimage_checked(&self) -> RunProofResult<Vec<u8>> {
        let canonical = canonical_json(&RunProofPreimage {
            schema: &self.schema,
            schema_version: self.schema_version,
            signature_algorithm: &self.signature_algorithm,
            key_id: &self.key_id,
            export_digest: &self.export_digest,
        })?;
        let mut preimage = Vec::with_capacity(RUN_PROOF_SIGNING_DOMAIN.len() + canonical.len());
        preimage.extend_from_slice(RUN_PROOF_SIGNING_DOMAIN);
        preimage.extend_from_slice(&canonical);
        Ok(preimage)
    }

    fn decode_signature(&self) -> RunProofResult<Signature> {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&self.signature)
            .map_err(|_| RunProofError::MalformedSignature)?;
        if base64::engine::general_purpose::STANDARD.encode(&decoded) != self.signature {
            return Err(RunProofError::MalformedSignature);
        }
        let bytes: [u8; ED25519_SIGNATURE_BYTES] = decoded
            .as_slice()
            .try_into()
            .map_err(|_| RunProofError::MalformedSignature)?;
        Ok(Signature::from_bytes(&bytes))
    }
}

/// A successful detached signature verification relative to one caller-supplied
/// scoped trust policy and the replay plan derived from the same export bytes.
///
/// There is no public constructor. Call [`verify_trust_policy_run_proof`] to
/// obtain this type. Its policy provenance is evidence of which caller policy
/// accepted the signer; it is not authentication, release, or promotion
/// authority.
#[derive(Debug)]
pub struct TrustPolicyVerifiedRunProof {
    replay_plan: VerifiedRunReplayPlan,
    export_digest: String,
    proof_digest: String,
    key_id: String,
    policy_id: String,
    policy_digest: String,
}

impl TrustPolicyVerifiedRunProof {
    /// The immutable replay plan built from the exact export bytes checked by
    /// this detached proof.
    pub fn replay_plan(&self) -> &VerifiedRunReplayPlan {
        &self.replay_plan
    }

    pub fn export_digest(&self) -> &str {
        &self.export_digest
    }

    /// Lowercase SHA-256 digest of the verified canonical detached envelope.
    ///
    /// The raw proof bytes remain private; this stable commitment is for P4
    /// receipt cross-binding only.
    pub fn proof_digest(&self) -> &str {
        &self.proof_digest
    }

    pub fn key_id(&self) -> &str {
        &self.key_id
    }

    /// Caller-supplied policy label recorded with this verification result.
    pub fn policy_id(&self) -> &str {
        &self.policy_id
    }

    /// Canonical digest of the caller-supplied policy that accepted the key.
    pub fn policy_digest(&self) -> &str {
        &self.policy_digest
    }

    /// Export JCS/SHA-256 integrity was verified independently of this proof.
    pub const fn integrity_verified(&self) -> bool {
        true
    }

    /// The Ed25519 signature was verified with a key accepted by this result's
    /// caller-supplied tenant/run-scoped policy.
    pub const fn signature_verified(&self) -> bool {
        true
    }

    /// Current exports have no real authenticated subject, so detached proof
    /// verification cannot establish authentication authority before P4.
    pub const fn authentication_authority_available(&self) -> bool {
        false
    }

    /// The exported durable latch is evidence only. It is never an exact replay
    /// result; exactness is established only by `DeterministicReplayReport`
    /// after obligation-validated private-artifact reconstruction.
    pub const fn recorded_exact_replay_state(&self) -> ExactReplayState {
        self.replay_plan.recorded_exact_replay_state()
    }

    /// A caller-provided policy cannot self-grant promotion authority.
    pub const fn promotion_authoritative(&self) -> bool {
        false
    }

    /// Detached proof verification never authorizes effects.
    pub const fn authorizes_effect_execution(&self) -> bool {
        false
    }
}

pub type ExactVerifiedRunEvidenceResult<T> = Result<T, ExactVerifiedRunEvidenceError>;

/// Fail-closed errors while binding a scoped signature proof to exact,
/// obligation-complete reconstruction evidence.
#[derive(Debug, Error)]
pub enum ExactVerifiedRunEvidenceError {
    #[error("scoped proof integrity or signature evidence is unavailable")]
    ProofNotVerified,
    #[error("scoped proof is not bound to its replay plan export digest")]
    ProofPlanDigestMismatch,
    #[error("reconstruction run ID does not match the scoped proof")]
    RunIdMismatch,
    #[error("reconstruction export digest does not match the scoped proof")]
    ExportDigestMismatch,
    #[error("reconstruction is not exact: {0:?}")]
    NonExactReplay(ReplayFidelity),
    #[error("reconstruction replay obligations are incomplete")]
    IncompleteReplayObligations,
    #[error("reconstruction digest is not canonical SHA-256")]
    InvalidReconstructionDigest,
}

/// Nonconstructible P3 handoff binding a scoped signature proof to one exact,
/// obligation-complete deterministic reconstruction.
///
/// It contains only commitments and typed summaries—never a raw proof,
/// transcript, private payload, artifact reader, effect capability, or
/// promotion grant. P4 may bind this evidence to a separate evaluation receipt.
#[derive(Debug, Clone)]
pub struct ExactVerifiedRunEvidence {
    run_id: RunId,
    export_digest: String,
    proof_digest: String,
    reconstruction_digest: String,
    event_count: u64,
    fidelity: ReplayFidelity,
    counters: ReplayEvidenceCounters,
    terminal_reason: TerminalReason,
    final_outcome: RunOutcome,
    policy_id: String,
    policy_digest: String,
}

impl ExactVerifiedRunEvidence {
    pub const fn run_id(&self) -> RunId {
        self.run_id
    }

    pub fn export_digest(&self) -> &str {
        &self.export_digest
    }

    pub fn proof_digest(&self) -> &str {
        &self.proof_digest
    }

    /// Stable digest of the exact public reconstruction/transcript projection.
    pub fn reconstruction_digest(&self) -> &str {
        &self.reconstruction_digest
    }

    pub const fn event_count(&self) -> u64 {
        self.event_count
    }

    pub const fn fidelity(&self) -> ReplayFidelity {
        self.fidelity
    }

    /// Typed operation counts from the obligation-complete reconstruction.
    pub const fn counters(&self) -> ReplayEvidenceCounters {
        self.counters
    }

    pub const fn terminal_reason(&self) -> TerminalReason {
        self.terminal_reason
    }

    pub const fn final_outcome(&self) -> RunOutcome {
        self.final_outcome
    }

    pub fn policy_id(&self) -> &str {
        &self.policy_id
    }

    pub fn policy_digest(&self) -> &str {
        &self.policy_digest
    }

    pub const fn integrity_verified(&self) -> bool {
        true
    }

    pub const fn signature_verified(&self) -> bool {
        true
    }

    /// Current exports lack a real authenticated subject; exact evidence does
    /// not turn a policy-relative signer decision into principal authority.
    pub const fn authentication_authority_available(&self) -> bool {
        false
    }

    pub const fn obligations_complete(&self) -> bool {
        true
    }

    /// Exact evidence is not a release/promotion authorization.
    pub const fn promotion_authoritative(&self) -> bool {
        false
    }

    /// Exact evidence is never a capability to execute effects.
    pub const fn authorizes_effect_execution(&self) -> bool {
        false
    }
}

/// Cross-bind a nonconstructible scoped signature proof to an exact,
/// obligation-complete deterministic replay report.
pub fn verify_exact_run_evidence(
    proof: &TrustPolicyVerifiedRunProof,
    report: &DeterministicReplayReport,
) -> ExactVerifiedRunEvidenceResult<ExactVerifiedRunEvidence> {
    if !proof.integrity_verified() || !proof.signature_verified() {
        return Err(ExactVerifiedRunEvidenceError::ProofNotVerified);
    }
    if proof.replay_plan().export_digest() != proof.export_digest()
        || !is_canonical_sha256(proof.proof_digest())
    {
        return Err(ExactVerifiedRunEvidenceError::ProofPlanDigestMismatch);
    }
    if report.run_id() != proof.replay_plan().summary().run_id {
        return Err(ExactVerifiedRunEvidenceError::RunIdMismatch);
    }
    if report.export_digest() != proof.export_digest() {
        return Err(ExactVerifiedRunEvidenceError::ExportDigestMismatch);
    }
    if report.fidelity() != ReplayFidelity::Exact {
        return Err(ExactVerifiedRunEvidenceError::NonExactReplay(
            report.fidelity(),
        ));
    }
    if !report.obligations_complete() {
        return Err(ExactVerifiedRunEvidenceError::IncompleteReplayObligations);
    }
    if !is_canonical_sha256(report.reconstruction_digest()) {
        return Err(ExactVerifiedRunEvidenceError::InvalidReconstructionDigest);
    }

    Ok(ExactVerifiedRunEvidence {
        run_id: report.run_id(),
        export_digest: report.export_digest().to_owned(),
        proof_digest: proof.proof_digest().to_owned(),
        reconstruction_digest: report.reconstruction_digest().to_owned(),
        event_count: report.event_count(),
        fidelity: report.fidelity(),
        counters: report.counters(),
        terminal_reason: report.terminal_reason(),
        final_outcome: report.final_outcome(),
        policy_id: proof.policy_id().to_owned(),
        policy_digest: proof.policy_digest().to_owned(),
    })
}

/// Sign a detached proof for an already verified replay plan.
///
/// The caller supplies both its Ed25519 signing key and its externally managed
/// key identifier. This function never generates, loads, persists, or trusts a
/// key, and it cannot sign an arbitrary raw digest.
pub fn sign_verified_run_proof(
    replay_plan: &VerifiedRunReplayPlan,
    signer: &SigningKey,
    key_id: &str,
) -> RunProofResult<RunProofEnvelope> {
    let mut envelope = RunProofEnvelope {
        schema: RUN_PROOF_SCHEMA.to_owned(),
        schema_version: RUN_PROOF_SCHEMA_VERSION,
        signature_algorithm: RUN_PROOF_SIGNATURE_ALGORITHM.to_owned(),
        key_id: key_id.to_owned(),
        export_digest: replay_plan.summary().digest.clone(),
        signature: String::new(),
    };
    envelope.validate_header()?;
    let signature = signer.sign(&envelope.signing_preimage_checked()?);
    envelope.signature = base64::engine::general_purpose::STANDARD.encode(signature.to_bytes());
    Ok(envelope)
}

/// Verify a detached proof against the exact supplied export bytes and one
/// caller-supplied, tenant/branch/run-scoped trust policy.
///
/// The export is first parsed into a verified replay plan from `export_bytes`;
/// no path is accepted or reread. The signature preimage binds the plan's
/// verified export digest, key identifier, schema, and algorithm, so neither a
/// self-rehashed export nor a signature from another export can be transplanted.
/// The successful result is relative to `trust_policy`; it is never
/// authentication or promotion authority.
pub fn verify_trust_policy_run_proof(
    export_bytes: &[u8],
    envelope_bytes: &[u8],
    trust_policy: &dyn RunProofTrustPolicy,
) -> RunProofResult<TrustPolicyVerifiedRunProof> {
    let envelope = RunProofEnvelope::parse_canonical_bytes(envelope_bytes)?;
    let replay_plan = verify_run_export_replay_plan_bytes(export_bytes)?;
    if replay_plan.canonical_bytes() != export_bytes {
        return Err(RunProofError::ReplayPlanBytesMismatch);
    }
    if envelope.export_digest != replay_plan.summary().digest {
        return Err(RunProofError::ExportDigestMismatch);
    }

    let scope = verified_export_scope(export_bytes, &replay_plan)?;
    let policy_provenance = validated_policy_provenance(trust_policy)?;
    let context = RunProofTrustContext {
        tenant_id: &scope.tenant_id,
        branch_id: scope.branch_id.as_deref(),
        authenticated_subject: scope.authenticated_subject.as_deref(),
        run_id: replay_plan.summary().run_id,
        export_digest: replay_plan.summary().digest.as_str(),
    };
    let verifying_key = resolve_verifying_key(trust_policy, context, &envelope.key_id)?;
    let signature = envelope.decode_signature()?;
    verifying_key
        .verify_strict(&envelope.signing_preimage_checked()?, &signature)
        .map_err(|_| RunProofError::InvalidSignature)?;

    Ok(TrustPolicyVerifiedRunProof {
        export_digest: replay_plan.summary().digest.clone(),
        proof_digest: sha256_hex(envelope_bytes),
        key_id: envelope.key_id,
        policy_id: policy_provenance.policy_id,
        policy_digest: policy_provenance.policy_digest,
        replay_plan,
    })
}

struct VerifiedExportScope {
    tenant_id: String,
    branch_id: Option<String>,
    authenticated_subject: Option<String>,
}

struct ValidatedTrustPolicyProvenance {
    policy_id: String,
    policy_digest: String,
}

fn verified_export_scope(
    export_bytes: &[u8],
    replay_plan: &VerifiedRunReplayPlan,
) -> RunProofResult<VerifiedExportScope> {
    let document: serde_json::Value = serde_json::from_slice(export_bytes)
        .map_err(|_| RunProofError::InvalidVerifiedExportScope)?;
    let tenant_id = document
        .pointer("/header/provenance/tenant_id")
        .and_then(serde_json::Value::as_str)
        .ok_or(RunProofError::InvalidVerifiedExportScope)?;
    let branch_id = document
        .pointer("/header/provenance/branch_id")
        .and_then(serde_json::Value::as_str);
    let header_run_id = document
        .pointer("/header/run_id")
        .and_then(serde_json::Value::as_str)
        .ok_or(RunProofError::InvalidVerifiedExportScope)?;
    if header_run_id != replay_plan.summary().run_id.to_string()
        || !valid_scope_identifier(tenant_id)
    {
        return Err(RunProofError::InvalidVerifiedExportScope);
    }
    if let Some(branch_id) = branch_id {
        if !valid_scope_identifier(branch_id) {
            return Err(RunProofError::InvalidVerifiedExportScope);
        }
    }
    Ok(VerifiedExportScope {
        tenant_id: tenant_id.to_owned(),
        branch_id: branch_id.map(str::to_owned),
        // v2 verifies no authenticated principal. Do not substitute lease
        // ownership, session, or another operational identifier as one.
        authenticated_subject: None,
    })
}

fn validated_policy_provenance(
    trust_policy: &dyn RunProofTrustPolicy,
) -> RunProofResult<ValidatedTrustPolicyProvenance> {
    let policy_id = trust_policy.policy_id();
    if !valid_policy_id(policy_id) {
        return Err(RunProofError::InvalidPolicyId);
    }
    let policy_digest = trust_policy.policy_digest();
    if !is_canonical_sha256(policy_digest) {
        return Err(RunProofError::InvalidTrustPolicyDigest);
    }
    Ok(ValidatedTrustPolicyProvenance {
        policy_id: policy_id.to_owned(),
        policy_digest: policy_digest.to_owned(),
    })
}

fn resolve_verifying_key(
    trust_policy: &dyn RunProofTrustPolicy,
    context: RunProofTrustContext<'_>,
    key_id: &str,
) -> RunProofResult<VerifyingKey> {
    let public_key = match trust_policy.resolve_key(context, key_id) {
        RunProofTrustDecision::Trusted { public_key } => public_key,
        RunProofTrustDecision::Unknown => return Err(RunProofError::UnknownKey(key_id.to_owned())),
        RunProofTrustDecision::Revoked => return Err(RunProofError::RevokedKey(key_id.to_owned())),
        RunProofTrustDecision::NotCurrentlyValid => {
            return Err(RunProofError::KeyNotCurrentlyValid(key_id.to_owned()))
        }
    };
    let bytes: [u8; ED25519_PUBLIC_KEY_BYTES] = public_key
        .as_slice()
        .try_into()
        .map_err(|_| RunProofError::MalformedTrustedKey)?;
    VerifyingKey::from_bytes(&bytes).map_err(|_| RunProofError::MalformedTrustedKey)
}

fn canonical_json(value: &impl Serialize) -> RunProofResult<Vec<u8>> {
    serde_json_canonicalizer::to_vec(value)
        .map_err(|error| RunProofError::Canonicalization(error.to_string()))
}

fn validate_key_id(key_id: &str) -> RunProofResult<()> {
    if key_id.is_empty() || key_id.len() > MAX_KEY_ID_BYTES || key_id.chars().any(char::is_control)
    {
        return Err(RunProofError::InvalidKeyId);
    }
    Ok(())
}

fn validate_export_digest(digest: &str) -> RunProofResult<()> {
    if !is_canonical_sha256(digest) {
        return Err(RunProofError::InvalidExportDigest);
    }
    Ok(())
}

fn valid_policy_id(policy_id: &str) -> bool {
    !policy_id.is_empty()
        && policy_id.len() <= MAX_POLICY_ID_BYTES
        && !policy_id.chars().any(char::is_control)
}

fn valid_scope_identifier(value: &str) -> bool {
    !value.is_empty() && value.len() <= MAX_KEY_ID_BYTES && !value.chars().any(char::is_control)
}

fn is_canonical_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

#[derive(Serialize)]
struct RunProofPreimage<'a> {
    schema: &'a str,
    schema_version: u32,
    signature_algorithm: &'a str,
    key_id: &'a str,
    export_digest: &'a str,
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    use base64::Engine as _;
    use ed25519_dalek::SigningKey;
    use hermes_core::execution_mode::{ExecutionMode, ExecutionModeSource, ResolvedExecutionMode};
    use serde_json::{json, Value};
    use tempfile::TempDir;

    use super::*;
    use crate::deterministic_replay::{
        reconstruct_exact_verified_run, reconstruct_verified_run, DeterministicReplayError,
        ExactArtifactReadError, ExactArtifactReader, ReplayArtifactReference, ReplayFidelity,
        VerifiedReplayArtifact,
    };
    use crate::run_export::{export_run, verify_run_export_replay_plan_bytes};
    use crate::run_ledger::{
        ActionId, AppendIdentity, ArtifactStateReason, BranchId, CanonicalJson, DigestAbsence,
        ExactReplayState, LedgerClock, LedgerConnectionFactory, LedgerError, LedgerIdentityKeyId,
        ModelId, ObjectiveId, PrivateFingerprint, ProvenanceDigest, ProviderId,
        ReplayArtifactFidelity, RunExit, RunId, RunLedger, RunLedgerConfig, RunOwnerId,
        RunProvenance, RunStart, SealedArtifactRef, SessionId, Sha256Digest, TaskId, TenantId,
        TerminalReason,
    };
    use crate::sealed_artifact;

    #[derive(Debug)]
    struct TestClock {
        millis: AtomicI64,
    }

    impl TestClock {
        fn new(millis: i64) -> Self {
            Self {
                millis: AtomicI64::new(millis),
            }
        }
    }

    impl LedgerClock for TestClock {
        fn now_unix_ms(&self) -> Result<i64, LedgerError> {
            Ok(self.millis.load(Ordering::SeqCst))
        }
    }

    struct TestTrust {
        policy_id: String,
        policy_digest: String,
        decisions: BTreeMap<String, RunProofTrustDecision>,
    }

    impl Default for TestTrust {
        fn default() -> Self {
            Self {
                policy_id: "fixture-release-policy".to_owned(),
                policy_digest: "a".repeat(64),
                decisions: BTreeMap::new(),
            }
        }
    }

    impl TestTrust {
        fn one(key_id: &str, decision: RunProofTrustDecision) -> Self {
            let mut trust = Self::default();
            trust.insert(key_id, decision);
            trust
        }

        fn insert(&mut self, key_id: &str, decision: RunProofTrustDecision) {
            self.decisions.insert(key_id.to_owned(), decision);
        }

        fn with_provenance(
            mut self,
            policy_id: impl Into<String>,
            policy_digest: impl Into<String>,
        ) -> Self {
            self.policy_id = policy_id.into();
            self.policy_digest = policy_digest.into();
            self
        }
    }

    impl RunProofTrustPolicy for TestTrust {
        fn policy_id(&self) -> &str {
            &self.policy_id
        }

        fn policy_digest(&self) -> &str {
            &self.policy_digest
        }

        fn resolve_key(
            &self,
            context: RunProofTrustContext<'_>,
            key_id: &str,
        ) -> RunProofTrustDecision {
            if context.tenant_id() != "run-proof-tenant"
                || context.branch_id() != Some("run-proof-branch")
                || context.authenticated_subject().is_some()
                || context.run_id().to_string().is_empty()
                || !is_canonical_sha256(context.export_digest())
            {
                return RunProofTrustDecision::Unknown;
            }
            self.decisions
                .get(key_id)
                .cloned()
                .unwrap_or(RunProofTrustDecision::Unknown)
        }
    }

    struct MetadataOnlyReader;

    impl ExactArtifactReader for MetadataOnlyReader {
        fn read_exact(
            &self,
            _plan: &VerifiedRunReplayPlan,
            _artifact: &ReplayArtifactReference,
        ) -> Result<VerifiedReplayArtifact, ExactArtifactReadError> {
            let private_jcs = serde_json_canonicalizer::to_vec(&json!({
                "payload": {"sanitized": true},
                "replay_fidelity": "metadata_only",
                "source_commitment": "b".repeat(64),
                "source_commitment_algorithm": "hmac-sha256",
                "source_commitment_version": 1,
            }))
            .expect("canonical metadata-only payload");
            Ok(VerifiedReplayArtifact::from_verified_canonical_bytes(
                private_jcs,
            ))
        }
    }

    fn mode() -> ResolvedExecutionMode {
        ResolvedExecutionMode {
            mode: ExecutionMode::Manual,
            source: ExecutionModeSource::BuiltInDefault,
        }
    }

    fn object(value: Value) -> CanonicalJson {
        CanonicalJson::from_value(&value).expect("canonical JSON")
    }

    fn digest(byte: u8) -> Sha256Digest {
        Sha256Digest::new(hex::encode([byte; 32])).expect("SHA-256 digest")
    }

    fn provenance() -> RunProvenance {
        RunProvenance::new(
            SessionId::new("run-proof-session").expect("session"),
            None,
            BranchId::new("run-proof-branch").expect("branch"),
            TenantId::new("run-proof-tenant").expect("tenant"),
            ActionId::new("run-proof-action").expect("action"),
            ObjectiveId::new("run-proof-objective").expect("objective"),
            TaskId::new("run-proof-task").expect("task"),
            ModelId::new("run-proof-model").expect("model"),
            ProviderId::new("run-proof-provider").expect("provider"),
            digest(1),
            ProvenanceDigest::present(digest(2)),
            ProvenanceDigest::present(digest(3)),
            ProvenanceDigest::absent(DigestAbsence::Unavailable),
            ProvenanceDigest::present(digest(4)),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
            ProvenanceDigest::present(digest(5)),
            ProvenanceDigest::absent(DigestAbsence::NotApplicable),
        )
    }

    fn identity(byte: u8) -> AppendIdentity {
        AppendIdentity::new(
            object(json!({
                "artifact_kind":"request",
                "evidence_id":"run-start",
                "evidence_subject":"run-provenance",
                "outcome":"not_applicable",
                "replay_fidelity":"credential_redacted",
                "schema":"hermes.runtime-evidence/v1"
            })),
            PrivateFingerprint::hmac_sha256_v1(
                LedgerIdentityKeyId::new("run-proof-ledger-key-v1").expect("key ID"),
                1,
                [byte; 32],
            )
            .expect("private fingerprint"),
        )
    }

    fn artifact_ref(byte: u8) -> SealedArtifactRef {
        SealedArtifactRef::try_from(sealed_artifact::SealedArtifactRef {
            envelope_digest: hex::encode([byte; 32]),
            key_id: "run-proof-artifact-key-v1".to_owned(),
            schema_version: sealed_artifact::SEALED_ARTIFACT_SCHEMA_VERSION,
            encryption_algorithm: sealed_artifact::ARTIFACT_ENCRYPTION_ALGORITHM.to_owned(),
            canonicalization_algorithm: sealed_artifact::ARTIFACT_CANONICALIZATION.to_owned(),
            canonicalization_version: sealed_artifact::ARTIFACT_CANONICALIZATION_VERSION,
            redactor_version: "run-proof-redactor-v1".to_owned(),
        })
        .expect("sealed artifact reference")
    }

    fn fixture() -> (TempDir, RunLedger) {
        let directory = tempfile::tempdir().expect("ledger directory");
        let factory = LedgerConnectionFactory::new(directory.path().join("sessions.db"));
        factory
            .migrate_and_cut_over_v0_to_v1()
            .expect("migrate ledger");
        let ledger = RunLedger::open_with_clock(
            factory,
            RunLedgerConfig {
                append_lease_duration: Duration::from_secs(10),
                outbox_lease_duration: Duration::from_secs(10),
                owner_lease_duration: Duration::from_secs(10),
                max_delivery_attempts: 3,
            },
            Arc::new(TestClock::new(1_700_000_000_000)),
        )
        .expect("open ledger");
        (directory, ledger)
    }

    fn export_bytes(metadata_only: bool) -> Vec<u8> {
        let (directory, ledger) = fixture();
        let run_id = RunId::new();
        let start = RunStart::new(
            run_id,
            provenance(),
            object(json!({"run_proof_fixture":"start"})),
        );
        let created = if metadata_only {
            let artifact = artifact_ref(9);
            let created = ledger
                .create_run_and_start_with_bound_artifact(
                    mode(),
                    RunOwnerId::new("run-proof-owner").expect("owner"),
                    start,
                    identity(9),
                    artifact.clone(),
                    ReplayArtifactFidelity::MetadataOnly,
                )
                .expect("create bound-artifact run");
            ledger
                .mark_sealed_artifact_unavailable(&artifact, ArtifactStateReason::Missing)
                .expect("latch unavailable artifact");
            created
        } else {
            ledger
                .create_run_and_start(
                    mode(),
                    RunOwnerId::new("run-proof-owner").expect("owner"),
                    start,
                )
                .expect("create ordinary run")
        };
        ledger
            .finalize_run_exit(
                mode(),
                &created.owner_lease,
                RunExit::new(
                    TerminalReason::Completed,
                    object(json!({"run_proof_fixture":"complete"})),
                ),
            )
            .expect("finalize run");
        let receipt = export_run(&ledger, directory.path().join("exports"), run_id)
            .expect("export terminal run");
        let bytes = std::fs::read(receipt.path).expect("read export");
        verify_run_export_replay_plan_bytes(&bytes).expect("verify replay plan");
        bytes
    }

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[23; 32])
    }

    fn trusted_decision(signer: &SigningKey) -> RunProofTrustDecision {
        RunProofTrustDecision::Trusted {
            public_key: signer.verifying_key().to_bytes().to_vec(),
        }
    }

    fn signed_envelope(export_bytes: &[u8], signer: &SigningKey, key_id: &str) -> RunProofEnvelope {
        let plan = verify_run_export_replay_plan_bytes(export_bytes).expect("verified plan");
        sign_verified_run_proof(&plan, signer, key_id).expect("sign proof")
    }

    fn envelope_value(envelope: &RunProofEnvelope) -> Value {
        serde_json::from_slice(&envelope.canonical_bytes().expect("canonical envelope"))
            .expect("parse envelope")
    }

    fn canonical_value(value: &Value) -> Vec<u8> {
        serde_json_canonicalizer::to_vec(value).expect("canonical JSON value")
    }

    #[test]
    fn verifies_a_known_key_against_the_same_verified_export_bytes() {
        let export = export_bytes(false);
        let signer = signing_key();
        let envelope = signed_envelope(&export, &signer, "known-key");
        let envelope_bytes = envelope.canonical_bytes().expect("serialize proof");
        let trust = TestTrust::one("known-key", trusted_decision(&signer));

        let verified = verify_trust_policy_run_proof(&export, &envelope_bytes, &trust)
            .expect("known scoped policy verifies proof");

        assert!(verified.integrity_verified());
        assert!(verified.signature_verified());
        assert!(!verified.authentication_authority_available());
        assert_eq!(
            verified.recorded_exact_replay_state(),
            ExactReplayState::UnavailableSanitized
        );
        assert!(!verified.promotion_authoritative());
        assert!(!verified.authorizes_effect_execution());
        assert_eq!(verified.key_id(), "known-key");
        assert_eq!(verified.policy_id(), "fixture-release-policy");
        assert_eq!(verified.policy_digest(), "a".repeat(64));
        assert_eq!(verified.proof_digest(), sha256_hex(&envelope_bytes));
        assert_eq!(
            verified.replay_plan().canonical_bytes(),
            export.as_slice(),
            "verified plan must retain the exact caller-supplied bytes"
        );
    }

    #[test]
    fn rejects_an_unknown_key_id() {
        let export = export_bytes(false);
        let signer = signing_key();
        let envelope = signed_envelope(&export, &signer, "known-key");
        let error = verify_trust_policy_run_proof(
            &export,
            &envelope.canonical_bytes().expect("serialize proof"),
            &TestTrust::default(),
        )
        .expect_err("unknown key must fail closed");

        assert!(matches!(error, RunProofError::UnknownKey(key_id) if key_id == "known-key"));
    }

    #[test]
    fn rejects_a_revoked_key_id() {
        let export = export_bytes(false);
        let signer = signing_key();
        let envelope = signed_envelope(&export, &signer, "known-key");
        let trust = TestTrust::one("known-key", RunProofTrustDecision::Revoked);
        let error = verify_trust_policy_run_proof(
            &export,
            &envelope.canonical_bytes().expect("serialize proof"),
            &trust,
        )
        .expect_err("revoked key must fail closed");

        assert!(matches!(error, RunProofError::RevokedKey(key_id) if key_id == "known-key"));
    }

    #[test]
    fn rejects_a_key_outside_its_validity_window() {
        let export = export_bytes(false);
        let signer = signing_key();
        let envelope = signed_envelope(&export, &signer, "known-key");
        let trust = TestTrust::one("known-key", RunProofTrustDecision::NotCurrentlyValid);
        let error = verify_trust_policy_run_proof(
            &export,
            &envelope.canonical_bytes().expect("serialize proof"),
            &trust,
        )
        .expect_err("not-currently-valid key must fail closed");

        assert!(
            matches!(error, RunProofError::KeyNotCurrentlyValid(key_id) if key_id == "known-key")
        );
    }

    #[test]
    fn rejects_a_malformed_trusted_public_key_length() {
        let export = export_bytes(false);
        let signer = signing_key();
        let envelope = signed_envelope(&export, &signer, "known-key");
        let trust = TestTrust::one(
            "known-key",
            RunProofTrustDecision::Trusted {
                public_key: vec![0; ED25519_PUBLIC_KEY_BYTES - 1],
            },
        );
        let error = verify_trust_policy_run_proof(
            &export,
            &envelope.canonical_bytes().expect("serialize proof"),
            &trust,
        )
        .expect_err("short trusted key must fail closed");

        assert!(matches!(error, RunProofError::MalformedTrustedKey));
    }

    #[test]
    fn rejects_a_malformed_signature_length() {
        let export = export_bytes(false);
        let signer = signing_key();
        let envelope = signed_envelope(&export, &signer, "known-key");
        let mut value = envelope_value(&envelope);
        value["signature"] =
            Value::String(base64::engine::general_purpose::STANDARD.encode(vec![0; 63]));
        let trust = TestTrust::one("known-key", trusted_decision(&signer));
        let error = verify_trust_policy_run_proof(&export, &canonical_value(&value), &trust)
            .expect_err("short signature must fail closed");

        assert!(matches!(error, RunProofError::MalformedSignature));
    }

    #[test]
    fn rejects_a_schema_mismatch_before_signature_verification() {
        let export = export_bytes(false);
        let signer = signing_key();
        let envelope = signed_envelope(&export, &signer, "known-key");
        let mut value = envelope_value(&envelope);
        value["schema"] = json!("hermes.session-run.proof.other");
        let error = verify_trust_policy_run_proof(
            &export,
            &canonical_value(&value),
            &TestTrust::one("known-key", trusted_decision(&signer)),
        )
        .expect_err("schema mismatch must fail closed");

        assert!(matches!(error, RunProofError::UnsupportedSchema { .. }));
    }

    #[test]
    fn rejects_an_algorithm_mismatch_before_signature_verification() {
        let export = export_bytes(false);
        let signer = signing_key();
        let envelope = signed_envelope(&export, &signer, "known-key");
        let mut value = envelope_value(&envelope);
        value["signature_algorithm"] = json!("ed448");
        let error = verify_trust_policy_run_proof(
            &export,
            &canonical_value(&value),
            &TestTrust::one("known-key", trusted_decision(&signer)),
        )
        .expect_err("algorithm mismatch must fail closed");

        assert!(matches!(
            error,
            RunProofError::UnsupportedSignatureAlgorithm(algorithm) if algorithm == "ed448"
        ));
    }

    #[test]
    fn rejects_a_self_rehashed_valid_export_substitution() {
        let signed_export = export_bytes(false);
        let self_rehashed_alternative = export_bytes(false);
        let signed_summary =
            verify_run_export_replay_plan_bytes(&signed_export).expect("signed export plan");
        let alternative_summary = verify_run_export_replay_plan_bytes(&self_rehashed_alternative)
            .expect("self-rehashed alternative plan");
        assert_ne!(
            signed_summary.summary().digest,
            alternative_summary.summary().digest,
            "the independently verified alternative has a new unkeyed export digest"
        );

        let signer = signing_key();
        let envelope = signed_envelope(&signed_export, &signer, "known-key");
        let error = verify_trust_policy_run_proof(
            &self_rehashed_alternative,
            &envelope.canonical_bytes().expect("serialize proof"),
            &TestTrust::one("known-key", trusted_decision(&signer)),
        )
        .expect_err("a self-rehashed replacement must not inherit signature evidence");

        assert!(matches!(error, RunProofError::ExportDigestMismatch));
    }

    #[test]
    fn rejects_a_signature_transplanted_across_export_digests() {
        let first_export = export_bytes(false);
        let second_export = export_bytes(true);
        let signer = signing_key();
        let proof_for_first = signed_envelope(&first_export, &signer, "known-key");
        let error = verify_trust_policy_run_proof(
            &second_export,
            &proof_for_first.canonical_bytes().expect("serialize proof"),
            &TestTrust::one("known-key", trusted_decision(&signer)),
        )
        .expect_err("a signature for another export digest must not transplant");

        assert!(matches!(error, RunProofError::ExportDigestMismatch));
    }

    #[test]
    fn rejects_key_id_substitution_even_when_both_ids_resolve_to_the_same_key() {
        let export = export_bytes(false);
        let signer = signing_key();
        let envelope = signed_envelope(&export, &signer, "known-key");
        let mut value = envelope_value(&envelope);
        value["key_id"] = json!("alias-key");
        let mut trust = TestTrust::one("known-key", trusted_decision(&signer));
        trust.insert("alias-key", trusted_decision(&signer));
        let error = verify_trust_policy_run_proof(&export, &canonical_value(&value), &trust)
            .expect_err("key ID is part of the signature preimage");

        assert!(matches!(error, RunProofError::InvalidSignature));
    }

    #[test]
    fn rejects_a_noncanonical_envelope() {
        let export = export_bytes(false);
        let signer = signing_key();
        let envelope = signed_envelope(&export, &signer, "known-key");
        let canonical = envelope.canonical_bytes().expect("serialize proof");
        let mut noncanonical = Vec::with_capacity(canonical.len() + 1);
        noncanonical.push(b' ');
        noncanonical.extend_from_slice(&canonical);
        let error = verify_trust_policy_run_proof(
            &export,
            &noncanonical,
            &TestTrust::one("known-key", trusted_decision(&signer)),
        )
        .expect_err("leading JSON whitespace is not canonical JCS");

        assert!(matches!(error, RunProofError::NonCanonicalEnvelope));
    }

    #[test]
    fn rejects_an_envelope_that_supplies_its_own_public_key() {
        let export = export_bytes(false);
        let signer = signing_key();
        let envelope = signed_envelope(&export, &signer, "known-key");
        let mut value = envelope_value(&envelope);
        value["public_key"] = json!(
            base64::engine::general_purpose::STANDARD.encode(signer.verifying_key().to_bytes())
        );
        let error = verify_trust_policy_run_proof(
            &export,
            &canonical_value(&value),
            &TestTrust::one("known-key", trusted_decision(&signer)),
        )
        .expect_err("the envelope must not carry a trustable public key");

        assert!(matches!(error, RunProofError::InvalidEnvelope(_)));
    }

    #[test]
    fn identical_key_and_proof_under_distinct_policy_provenance_stay_distinguishable() {
        let export = export_bytes(false);
        let signer = signing_key();
        let envelope = signed_envelope(&export, &signer, "known-key");
        let envelope_bytes = envelope.canonical_bytes().expect("serialize proof");
        let production_policy = TestTrust::one("known-key", trusted_decision(&signer))
            .with_provenance("production-release-policy", "b".repeat(64));
        let test_policy = TestTrust::one("known-key", trusted_decision(&signer))
            .with_provenance("test-release-policy", "c".repeat(64));

        let production =
            verify_trust_policy_run_proof(&export, &envelope_bytes, &production_policy)
                .expect("production policy verifies signature");
        let test = verify_trust_policy_run_proof(&export, &envelope_bytes, &test_policy)
            .expect("test policy verifies same signature");

        assert_eq!(production.key_id(), test.key_id());
        assert_ne!(production.policy_id(), test.policy_id());
        assert_ne!(production.policy_digest(), test.policy_digest());
        assert!(!production.authentication_authority_available());
        assert!(!production.promotion_authoritative());
        assert!(!test.promotion_authoritative());
    }

    #[test]
    fn rejects_missing_policy_identity() {
        let export = export_bytes(false);
        let signer = signing_key();
        let envelope = signed_envelope(&export, &signer, "known-key");
        let policy = TestTrust::one("known-key", trusted_decision(&signer))
            .with_provenance("", "b".repeat(64));
        let error = verify_trust_policy_run_proof(
            &export,
            &envelope.canonical_bytes().expect("serialize proof"),
            &policy,
        )
        .expect_err("missing policy identity must fail closed");

        assert!(matches!(error, RunProofError::InvalidPolicyId));
    }

    #[test]
    fn rejects_a_malformed_policy_digest() {
        let export = export_bytes(false);
        let signer = signing_key();
        let envelope = signed_envelope(&export, &signer, "known-key");
        let policy = TestTrust::one("known-key", trusted_decision(&signer))
            .with_provenance("fixture-release-policy", "not-a-canonical-digest");
        let error = verify_trust_policy_run_proof(
            &export,
            &envelope.canonical_bytes().expect("serialize proof"),
            &policy,
        )
        .expect_err("malformed policy digest must fail closed");

        assert!(matches!(error, RunProofError::InvalidTrustPolicyDigest));
    }

    #[test]
    fn scoped_signature_verification_cannot_upgrade_sanitized_metadata_to_exact_replay() {
        let metadata_export = export_bytes(true);
        let signer = signing_key();
        let envelope = signed_envelope(&metadata_export, &signer, "known-key");
        let verified = verify_trust_policy_run_proof(
            &metadata_export,
            &envelope.canonical_bytes().expect("serialize proof"),
            &TestTrust::one("known-key", trusted_decision(&signer)),
        )
        .expect("metadata-only export can have a scoped signature verification");

        assert!(verified.integrity_verified());
        assert!(verified.signature_verified());
        assert!(!verified.authentication_authority_available());
        assert_eq!(
            verified.recorded_exact_replay_state(),
            ExactReplayState::UnavailableSanitized
        );
        assert!(!verified.promotion_authoritative());
        assert!(!verified.authorizes_effect_execution());

        let reader = MetadataOnlyReader;
        let report = reconstruct_verified_run(verified.replay_plan(), Some(&reader))
            .expect("sanitized metadata can reconstruct only as metadata");
        assert_eq!(report.fidelity(), ReplayFidelity::MetadataOnly);
        assert!(!report.exact());
        let error = verify_exact_run_evidence(&verified, &report)
            .expect_err("metadata-only reconstruction must not mint exact evidence");
        assert!(matches!(
            error,
            ExactVerifiedRunEvidenceError::NonExactReplay(ReplayFidelity::MetadataOnly)
        ));
        let error = reconstruct_exact_verified_run(verified.replay_plan(), Some(&reader))
            .expect_err("exact replay must reject sanitized metadata");
        assert!(matches!(
            error,
            DeterministicReplayError::ExactFidelityRequired { .. }
        ));
    }
}
