//! Signed soak, rollback-drill, and content-addressed rollback operations.
//!
//! This module intentionally has no command-execution surface.  A rollback is
//! represented solely by atomically changing a single pointer inside an
//! operator-provided release root after every manifest entry has been restored
//! and verified from a content-addressed store.

use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use uuid::Uuid;

#[cfg(feature = "agent-loop")]
use hermes_agent::{
    RunRuntime,
    run_ledger::{
        EffectIntent, NonResumableEffectState, OutboxState, RunOutcome, TerminalReason,
    },
};

use crate::release_candidate::{
    AuthenticatedSubject, ReleaseDigest, ReleasePolicyId, ReleasePromotionPolicy,
    ReleaseReceiptScope, ReleaseRevisionAlgorithm, ReleaseSigningKeyId, ReleaseTenantId,
    ReleaseTrustDecision, VerifiedReleaseCandidateReceipt,
};

/// Stable schema for a signed soak receipt.
pub const SOAK_RECEIPT_SCHEMA: &str = "hermes.release.soak-receipt";
/// Stable schema for a signed rollback drill receipt.
pub const ROLLBACK_DRILL_RECEIPT_SCHEMA: &str = "hermes.release.rollback-drill-receipt";
/// Stable schema for a signed rollback manifest.
pub const ROLLBACK_MANIFEST_SCHEMA: &str = "hermes.release.rollback-manifest";
/// Current wire-schema version for all release-operation envelopes.
pub const RELEASE_OPERATION_SCHEMA_VERSION: u32 = 1;
/// The only supported release-operation signature algorithm.
pub const RELEASE_OPERATION_SIGNATURE_ALGORITHM: &str = "ed25519";
/// A promotion soak must be at least twenty-four hours.
pub const MINIMUM_SOAK_DURATION_MS: u64 = 24 * 60 * 60 * 1000;
/// A rollback drill must finish strictly before five minutes.
pub const MAXIMUM_ROLLBACK_DRILL_DURATION_MS: u64 = 5 * 60 * 1000;

const SOAK_SIGNING_DOMAIN: &[u8] = b"hermes.release.soak-receipt.ed25519/v1\0";
const ROLLBACK_DRILL_SIGNING_DOMAIN: &[u8] =
    b"hermes.release.rollback-drill-receipt.ed25519/v1\0";
const ROLLBACK_MANIFEST_SIGNING_DOMAIN: &[u8] =
    b"hermes.release.rollback-manifest.ed25519/v1\0";
const MAX_ENVELOPE_BYTES: usize = 1024 * 1024;
const MAX_ARTIFACT_BYTES: usize = 64 * 1024 * 1024;
const MAX_ARTIFACTS: usize = 4096;
const MAX_RUN_PROOFS: usize = 4096;
const RELEASE_POINTER_SCHEMA: &str = "hermes.release.current-pointer";
const RELEASE_POINTER_VERSION: u32 = 1;
const RELEASE_ROOT_LOCK_FILENAME: &str = ".release-operation.lock.sqlite";
const RELEASE_ROOT_LOCK_TIMEOUT: Duration = Duration::from_secs(30);

/// Result alias for release-operation construction, verification, and restore.
pub type ReleaseOperationsResult<T> = Result<T, ReleaseOperationsError>;

/// Fail-closed errors for release-operation authority and rollback storage.
#[derive(Debug, Error)]
pub enum ReleaseOperationsError {
    #[error("release operation canonicalization failed: {0}")]
    Canonicalization(String),
    #[error("release operation envelope is malformed: {0}")]
    InvalidEnvelope(String),
    #[error("release operation envelope is not canonical RFC 8785 JSON")]
    NonCanonicalEnvelope,
    #[error("release operation envelope exceeds {MAX_ENVELOPE_BYTES} bytes")]
    EnvelopeTooLarge,
    #[error("unsupported release operation schema {schema:?} version {version}")]
    UnsupportedSchema { schema: String, version: u32 },
    #[error("unsupported release operation signature algorithm {0:?}")]
    UnsupportedSignatureAlgorithm(String),
    #[error("release operation signature is malformed")]
    MalformedSignature,
    #[error("release operation signer is not trusted for this scope")]
    UntrustedSigner,
    #[error("release operation trusted public key is malformed")]
    MalformedTrustedKey,
    #[error("release operation signature verification failed")]
    InvalidSignature,
    #[error("release operation scope does not equal the caller-required scope")]
    ScopeMismatch,
    #[error("release operation policy is unavailable for the required scope")]
    PolicyUnavailable,
    #[error("release operation policy does not equal the caller-required policy")]
    PolicyMismatch,
    #[error("release operation evidence is invalid: {0}")]
    InvalidEvidence(&'static str),
    #[error("release operation evidence is missing: {0}")]
    MissingEvidence(&'static str),
    #[error("release operation duration is invalid")]
    InvalidDuration,
    #[error("release operation is not promotion eligible: {0}")]
    IneligibleEvidence(&'static str),
    #[error("rollback binding is inconsistent: {0}")]
    InconsistentBinding(&'static str),
    #[error("unsafe rollback artifact path {0:?}")]
    UnsafeArtifactPath(String),
    #[error("rollback artifact exceeds {MAX_ARTIFACT_BYTES} bytes")]
    ArtifactTooLarge,
    #[error("rollback artifact is missing for digest {0}")]
    MissingArtifact(ReleaseDigest),
    #[error("rollback artifact digest mismatch for {0}")]
    ArtifactDigestMismatch(ReleaseDigest),
    #[error("rollback path is a symlink or otherwise unsafe: {0}")]
    UnsafeFilesystemPath(String),
    #[error("release pointer is missing")]
    MissingReleasePointer,
    #[error("release pointer is malformed")]
    InvalidReleasePointer,
    #[error("release pointer does not identify the expected candidate manifest")]
    PointerNotCandidate,
    #[error("release pointer changed unexpectedly")]
    PointerChanged,
    #[error("rollback drill ID was already recorded with different evidence")]
    IdempotencyConflict,
    #[error("rollback receipt ledger failed: {0}")]
    Ledger(#[from] rusqlite::Error),
    #[error("soak session is already complete")]
    SoakSessionComplete,
    #[error("release operation I/O while {operation}: {source}")]
    Io {
        operation: &'static str,
        #[source]
        source: std::io::Error,
    },
}

fn io_error(operation: &'static str, source: std::io::Error) -> ReleaseOperationsError {
    ReleaseOperationsError::Io { operation, source }
}

/// Caller-supplied, tenant/subject-scoped trust for release-operation envelopes.
///
/// Implementations must fail closed: missing, expired, revoked, or otherwise
/// unavailable key material must produce [`ReleaseTrustDecision::Denied`].  The
/// policy is not embedded as a trust root; it supplies the expected frozen
/// promotion policy and resolves the signer only after the receipt scope has
/// been parsed from canonical signed bytes.
pub trait ReleaseOperationsTrustPolicy {
    /// Return the only frozen promotion policy accepted for `scope`, if any.
    fn expected_policy(&self, scope: &ReleaseReceiptScope) -> Option<&ReleasePromotionPolicy>;

    /// Resolve a currently trusted Ed25519 public key for `key_id` in `scope`.
    fn resolve_verifying_key(
        &self,
        scope: &ReleaseReceiptScope,
        key_id: &ReleaseSigningKeyId,
    ) -> ReleaseTrustDecision;
}

/// A wall-clock observation paired with an externally measured monotonic tick.
///
/// The monotonic component, rather than wall-clock subtraction, determines the
/// signed elapsed duration.  The wall clock must still be nondecreasing so a
/// receipt cannot hide a backwards timestamp jump.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonotonicWallTimestamp {
    wall_time_ms: i64,
    monotonic_ms: u64,
}

impl MonotonicWallTimestamp {
    pub fn new(wall_time_ms: i64, monotonic_ms: u64) -> ReleaseOperationsResult<Self> {
        if wall_time_ms < 0 {
            return Err(ReleaseOperationsError::InvalidEvidence("wall timestamp"));
        }
        Ok(Self {
            wall_time_ms,
            monotonic_ms,
        })
    }

    pub const fn wall_time_ms(&self) -> i64 {
        self.wall_time_ms
    }

    pub const fn monotonic_ms(&self) -> u64 {
        self.monotonic_ms
    }
}

/// Signed start/end observations and their independently recomputable elapsed
/// monotonic duration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MonotonicWallTimeWindow {
    started_at: MonotonicWallTimestamp,
    ended_at: MonotonicWallTimestamp,
    elapsed_ms: u64,
}

impl MonotonicWallTimeWindow {
    pub fn new(
        started_at: MonotonicWallTimestamp,
        ended_at: MonotonicWallTimestamp,
    ) -> ReleaseOperationsResult<Self> {
        let elapsed_ms = ended_at
            .monotonic_ms
            .checked_sub(started_at.monotonic_ms)
            .ok_or(ReleaseOperationsError::InvalidDuration)?;
        if ended_at.wall_time_ms < started_at.wall_time_ms {
            return Err(ReleaseOperationsError::InvalidDuration);
        }
        Ok(Self {
            started_at,
            ended_at,
            elapsed_ms,
        })
    }

    fn validate(&self) -> ReleaseOperationsResult<()> {
        let started_at =
            MonotonicWallTimestamp::new(self.started_at.wall_time_ms, self.started_at.monotonic_ms)?;
        let ended_at =
            MonotonicWallTimestamp::new(self.ended_at.wall_time_ms, self.ended_at.monotonic_ms)?;
        let checked = Self::new(started_at, ended_at)?;
        if checked.elapsed_ms != self.elapsed_ms {
            return Err(ReleaseOperationsError::InvalidDuration);
        }
        Ok(())
    }

    pub fn started_at(&self) -> MonotonicWallTimestamp {
        self.started_at
    }

    pub fn ended_at(&self) -> MonotonicWallTimestamp {
        self.ended_at
    }

    pub const fn elapsed_ms(&self) -> u64 {
        self.elapsed_ms
    }
}

fn heartbeat_gap_within(
    earlier: MonotonicWallTimestamp,
    later: MonotonicWallTimestamp,
    maximum_gap_ms: u64,
) -> ReleaseOperationsResult<bool> {
    let monotonic_gap = later
        .monotonic_ms()
        .checked_sub(earlier.monotonic_ms())
        .ok_or(ReleaseOperationsError::InvalidDuration)?;
    let wall_gap = later
        .wall_time_ms()
        .checked_sub(earlier.wall_time_ms())
        .ok_or(ReleaseOperationsError::InvalidDuration)?;
    let wall_gap = u64::try_from(wall_gap)
        .map_err(|_| ReleaseOperationsError::InvalidDuration)?;
    Ok(monotonic_gap <= maximum_gap_ms && wall_gap <= maximum_gap_ms)
}

/// Closed canary-health outcome recorded by a soak receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CanaryHealth {
    Healthy,
    Unhealthy,
}

/// Closed post-restore verification outcome recorded by a rollback drill.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RollbackVerificationResult {
    Verified,
    Failed,
}

/// Stable id for one idempotent rollback drill attempt.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RollbackDrillId(String);

impl RollbackDrillId {
    pub fn new(value: impl Into<String>) -> ReleaseOperationsResult<Self> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 256
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
        {
            return Err(ReleaseOperationsError::InvalidEvidence("rollback drill ID"));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A strictly relative, portable artifact destination below a staged release.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RollbackArtifactPath(String);

impl RollbackArtifactPath {
    pub fn new(value: impl Into<String>) -> ReleaseOperationsResult<Self> {
        let value = value.into();
        if value.is_empty() || value.len() > 1024 || value.contains('\\') {
            return Err(ReleaseOperationsError::UnsafeArtifactPath(value));
        }
        let mut normalized = Vec::new();
        for segment in value.split('/') {
            if segment.is_empty()
                || segment == "."
                || segment == ".."
                || !segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
            {
                return Err(ReleaseOperationsError::UnsafeArtifactPath(value));
            }
            normalized.push(segment);
        }
        if normalized.join("/") != value {
            return Err(ReleaseOperationsError::UnsafeArtifactPath(value));
        }
        let path = Path::new(&value);
        if path.is_absolute()
            || path.components().any(|component| {
                !matches!(component, Component::Normal(_))
            })
        {
            return Err(ReleaseOperationsError::UnsafeArtifactPath(value));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn join_to(&self, root: &Path) -> PathBuf {
        root.join(&self.0)
    }
}

/// Immutable source/build identity embedded in the one declared runtime
/// entrypoint.  A manifest cannot represent a dirty checkout or a shortened
/// revision identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeBuildBinding {
    revision_algorithm: ReleaseRevisionAlgorithm,
    full_git_sha1: String,
    source_clean: bool,
    build_recipe_digest: ReleaseDigest,
    artifact_sha256: ReleaseDigest,
}

impl RuntimeBuildBinding {
    pub fn new(
        revision_algorithm: ReleaseRevisionAlgorithm,
        full_git_sha1: impl Into<String>,
        source_clean: bool,
        build_recipe_digest: ReleaseDigest,
        artifact_sha256: ReleaseDigest,
    ) -> ReleaseOperationsResult<Self> {
        if revision_algorithm != ReleaseRevisionAlgorithm::GitSha1 {
            return Err(ReleaseOperationsError::InvalidEvidence(
                "runtime revision algorithm",
            ));
        }
        let full_git_sha1 = full_git_sha1.into();
        if full_git_sha1.len() != 40
            || !full_git_sha1
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(ReleaseOperationsError::InvalidEvidence(
                "runtime full Git SHA-1",
            ));
        }
        if !source_clean {
            return Err(ReleaseOperationsError::IneligibleEvidence(
                "dirty runtime source identity",
            ));
        }
        Ok(Self {
            revision_algorithm,
            full_git_sha1,
            source_clean,
            build_recipe_digest,
            artifact_sha256,
        })
    }

    pub fn revision_algorithm(&self) -> ReleaseRevisionAlgorithm {
        self.revision_algorithm
    }

    pub fn full_git_sha1(&self) -> &str {
        &self.full_git_sha1
    }

    pub const fn source_clean(&self) -> bool {
        self.source_clean
    }

    pub fn build_recipe_digest(&self) -> &ReleaseDigest {
        &self.build_recipe_digest
    }

    pub fn artifact_sha256(&self) -> &ReleaseDigest {
        &self.artifact_sha256
    }
}

/// Typed, unsigned input that can only become a receipt through
/// [`sign_soak_receipt`].
#[derive(Debug, Clone)]
pub struct SoakReceiptInput {
    candidate_receipt_digest: ReleaseDigest,
    scope: ReleaseReceiptScope,
    policy: ReleasePromotionPolicy,
    time_window: MonotonicWallTimeWindow,
    event_loss_count: u64,
    unreconciled_effect_count: u64,
    canary_health: CanaryHealth,
    exact_run_proof_digests: Vec<ReleaseDigest>,
}

impl SoakReceiptInput {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        candidate_receipt_digest: ReleaseDigest,
        scope: ReleaseReceiptScope,
        policy: ReleasePromotionPolicy,
        time_window: MonotonicWallTimeWindow,
        event_loss_count: u64,
        unreconciled_effect_count: u64,
        canary_health: CanaryHealth,
        exact_run_proof_digests: impl IntoIterator<Item = ReleaseDigest>,
    ) -> ReleaseOperationsResult<Self> {
        time_window.validate()?;
        let mut exact_run_proof_digests = exact_run_proof_digests.into_iter().collect::<Vec<_>>();
        exact_run_proof_digests.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        if exact_run_proof_digests.is_empty() {
            return Err(ReleaseOperationsError::MissingEvidence("exact run-proof digests"));
        }
        if exact_run_proof_digests.len() > MAX_RUN_PROOFS {
            return Err(ReleaseOperationsError::InvalidEvidence("exact run-proof digest count"));
        }
        if exact_run_proof_digests
            .windows(2)
            .any(|pair| pair[0] == pair[1])
        {
            return Err(ReleaseOperationsError::InvalidEvidence(
                "duplicate exact run-proof digest",
            ));
        }
        Ok(Self {
            candidate_receipt_digest,
            scope,
            policy,
            time_window,
            event_loss_count,
            unreconciled_effect_count,
            canary_health,
            exact_run_proof_digests,
        })
    }
}

/// Closed signed soak payload.  Its fields are private; construction flows only
/// through [`SoakReceiptInput`] and [`sign_soak_receipt`].
#[derive(Debug, Clone, PartialEq)]
pub struct SoakReceipt {
    candidate_receipt_digest: ReleaseDigest,
    scope: ReleaseReceiptScope,
    policy: ReleasePromotionPolicy,
    time_window: MonotonicWallTimeWindow,
    event_loss_count: u64,
    unreconciled_effect_count: u64,
    canary_health: CanaryHealth,
    exact_run_proof_digests: Vec<ReleaseDigest>,
}

impl From<SoakReceiptInput> for SoakReceipt {
    fn from(value: SoakReceiptInput) -> Self {
        Self {
            candidate_receipt_digest: value.candidate_receipt_digest,
            scope: value.scope,
            policy: value.policy,
            time_window: value.time_window,
            event_loss_count: value.event_loss_count,
            unreconciled_effect_count: value.unreconciled_effect_count,
            canary_health: value.canary_health,
            exact_run_proof_digests: value.exact_run_proof_digests,
        }
    }
}

impl SoakReceipt {
    fn validate_structure(&self) -> ReleaseOperationsResult<()> {
        self.time_window.validate()?;
        if self.exact_run_proof_digests.is_empty() {
            return Err(ReleaseOperationsError::MissingEvidence("exact run-proof digests"));
        }
        if self.exact_run_proof_digests.len() > MAX_RUN_PROOFS {
            return Err(ReleaseOperationsError::InvalidEvidence("exact run-proof digest count"));
        }
        if self
            .exact_run_proof_digests
            .windows(2)
            .any(|pair| pair[0].as_str() >= pair[1].as_str())
        {
            return Err(ReleaseOperationsError::InvalidEvidence(
                "exact run-proof digest ordering",
            ));
        }
        Ok(())
    }

    fn validate_promotion_conditions(&self) -> ReleaseOperationsResult<()> {
        self.validate_structure()?;
        if self.time_window.elapsed_ms < MINIMUM_SOAK_DURATION_MS {
            return Err(ReleaseOperationsError::IneligibleEvidence("soak duration"));
        }
        if self.event_loss_count != 0 {
            return Err(ReleaseOperationsError::IneligibleEvidence("event loss"));
        }
        if self.unreconciled_effect_count != 0 {
            return Err(ReleaseOperationsError::IneligibleEvidence(
                "unreconciled effects",
            ));
        }
        if self.canary_health != CanaryHealth::Healthy {
            return Err(ReleaseOperationsError::IneligibleEvidence("canary health"));
        }
        Ok(())
    }

    pub fn candidate_receipt_digest(&self) -> &ReleaseDigest {
        &self.candidate_receipt_digest
    }

    pub fn scope(&self) -> &ReleaseReceiptScope {
        &self.scope
    }

    pub fn policy(&self) -> &ReleasePromotionPolicy {
        &self.policy
    }

    pub fn time_window(&self) -> &MonotonicWallTimeWindow {
        &self.time_window
    }

    pub const fn event_loss_count(&self) -> u64 {
        self.event_loss_count
    }

    pub const fn unreconciled_effect_count(&self) -> u64 {
        self.unreconciled_effect_count
    }

    pub const fn canary_health(&self) -> CanaryHealth {
        self.canary_health
    }

    pub fn exact_run_proof_digests(&self) -> &[ReleaseDigest] {
        &self.exact_run_proof_digests
    }
}

/// Typed signed soak envelope.  It carries no public constructor; callers must
/// use [`sign_soak_receipt`] and verify it against a caller-owned policy.
#[derive(Debug, Clone, PartialEq)]
pub struct SoakReceiptEnvelope {
    header: EnvelopeHeader,
    receipt: SoakReceipt,
    signature: String,
}

impl SoakReceiptEnvelope {
    pub fn canonical_bytes(&self) -> ReleaseOperationsResult<Vec<u8>> {
        self.validate()?;
        canonical_json(&SignedEnvelopeWire::from_parts(
            &self.header,
            SoakReceiptWire::from(&self.receipt),
            self.signature.clone(),
        ))
    }

    pub fn receipt(&self) -> &SoakReceipt {
        &self.receipt
    }

    pub fn key_id(&self) -> &ReleaseSigningKeyId {
        &self.header.key_id
    }

    fn validate(&self) -> ReleaseOperationsResult<()> {
        self.header.validate(SOAK_RECEIPT_SCHEMA)?;
        self.receipt.validate_structure()?;
        let _ = decode_signature(&self.signature)?;
        Ok(())
    }

    fn signing_preimage(&self) -> ReleaseOperationsResult<Vec<u8>> {
        let payload = SoakReceiptWire::from(&self.receipt);
        signing_preimage(SOAK_SIGNING_DOMAIN, &self.header, &payload)
    }

    fn parse_canonical_bytes(bytes: &[u8]) -> ReleaseOperationsResult<Self> {
        let wire: SignedEnvelopeWire<SoakReceiptWire> = parse_canonical_envelope(bytes)?;
        let (header, payload, signature) = wire.into_header()?;
        let envelope = Self {
            header,
            receipt: SoakReceipt::try_from(payload)?,
            signature,
        };
        envelope.validate()?;
        Ok(envelope)
    }
}

/// Sign a structurally valid soak attestation.  This is intentionally not
/// promotion authority: a caller holding a signing key can attest arbitrary
/// timestamps and health values.  Only [`SoakSession`] can mint
/// [`VerifiedSoakReceipt`] after durable operational observation.
pub fn sign_soak_receipt(
    input: SoakReceiptInput,
    signing_key: &SigningKey,
    key_id: ReleaseSigningKeyId,
) -> ReleaseOperationsResult<SoakReceiptEnvelope> {
    let receipt = SoakReceipt::from(input);
    receipt.validate_structure()?;
    let header = EnvelopeHeader::new(SOAK_RECEIPT_SCHEMA, key_id);
    let payload = SoakReceiptWire::from(&receipt);
    let signature = encode_signature(signing_key.sign(&signing_preimage(
        SOAK_SIGNING_DOMAIN,
        &header,
        &payload,
    )?));
    let envelope = SoakReceiptEnvelope {
        header,
        receipt,
        signature,
    };
    envelope.validate()?;
    Ok(envelope)
}

/// Verify a signed soak attestation in one expected tenant/subject scope.
///
/// The return value proves only canonical encoding, detached Ed25519 signature,
/// scope, and frozen policy.  It is deliberately not accepted by the promotion
/// gate; externally supplied timestamps, health, and reconciliation counts are
/// never sufficient to mint release authority.
pub fn verify_signed_soak_receipt(
    envelope: &SoakReceiptEnvelope,
    expected_scope: &ReleaseReceiptScope,
    trust_policy: &dyn ReleaseOperationsTrustPolicy,
) -> ReleaseOperationsResult<SignatureVerifiedSoakReceipt> {
    envelope.validate()?;
    verify_scope_policy(
        envelope.receipt.scope(),
        envelope.receipt.policy(),
        expected_scope,
        trust_policy,
    )?;
    let payload = SoakReceiptWire::from(&envelope.receipt);
    verify_signature(
        SOAK_SIGNING_DOMAIN,
        &envelope.header,
        &payload,
        &envelope.signature,
        envelope.receipt.scope(),
        trust_policy,
    )?;
    Ok(SignatureVerifiedSoakReceipt {
        envelope: envelope.clone(),
    })
}

/// Verify an untrusted canonical soak-envelope byte sequence to the
/// signature-only evidence tier.
pub fn verify_signed_soak_receipt_bytes(
    bytes: &[u8],
    expected_scope: &ReleaseReceiptScope,
    trust_policy: &dyn ReleaseOperationsTrustPolicy,
) -> ReleaseOperationsResult<SignatureVerifiedSoakReceipt> {
    let envelope = SoakReceiptEnvelope::parse_canonical_bytes(bytes)?;
    verify_signed_soak_receipt(&envelope, expected_scope, trust_policy)
}

/// Nonconstructible signature-only soak evidence.  It remains unsuitable for
/// promotion because it may have been signed from externally supplied facts.
#[derive(Debug)]
pub struct SignatureVerifiedSoakReceipt {
    envelope: SoakReceiptEnvelope,
}

impl SignatureVerifiedSoakReceipt {
    fn receipt(&self) -> &SoakReceipt {
        &self.envelope.receipt
    }

    pub fn candidate_receipt_digest(&self) -> &ReleaseDigest {
        self.receipt().candidate_receipt_digest()
    }

    pub fn scope(&self) -> &ReleaseReceiptScope {
        self.receipt().scope()
    }

    pub fn policy(&self) -> &ReleasePromotionPolicy {
        self.receipt().policy()
    }
}

/// Promotion-authoritative soak evidence.  This value has no public
/// constructor; [`SoakSession`] creates it only after it has durably recorded
/// operational health, exact run-proof, and reconciliation observations and
/// captured the ending monotonic timestamp itself.
#[derive(Debug)]
pub struct VerifiedSoakReceipt {
    signature_verified: SignatureVerifiedSoakReceipt,
}

impl VerifiedSoakReceipt {
    pub fn candidate_receipt_digest(&self) -> &ReleaseDigest {
        self.signature_verified.receipt().candidate_receipt_digest()
    }

    pub fn scope(&self) -> &ReleaseReceiptScope {
        self.signature_verified.receipt().scope()
    }

    pub fn policy(&self) -> &ReleasePromotionPolicy {
        self.signature_verified.receipt().policy()
    }

    pub fn elapsed_ms(&self) -> u64 {
        self.signature_verified.receipt().time_window.elapsed_ms
    }
    pub fn receipt(&self) -> &SoakReceipt {
        self.signature_verified.receipt()
    }
}

/// A typed operational observation that only crate-internal runtime code can
/// append to a [`SoakSession`].  It has no public constructor or externally
/// implementable probe trait, preventing a caller from turning a self-reported
/// health JSON document into promotion authority.
#[derive(Debug)]
pub(crate) enum OperationalSoakObservation {
    CanaryProbe(CanaryHealth),
    ExactRunProof(ReleaseDigest),
    Reconciliation {
        event_loss_count: u64,
        unreconciled_effect_count: u64,
    },
}

trait SoakClock: Send {
    fn now(&mut self) -> ReleaseOperationsResult<MonotonicWallTimestamp>;
}

struct SystemSoakClock {
    monotonic_origin: Instant,
}

impl SystemSoakClock {
    fn new() -> Self {
        Self {
            monotonic_origin: Instant::now(),
        }
    }
}

impl SoakClock for SystemSoakClock {
    fn now(&mut self) -> ReleaseOperationsResult<MonotonicWallTimestamp> {
        let wall_time_ms = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| ReleaseOperationsError::InvalidEvidence("system wall clock"))?
            .as_millis();
        let monotonic_ms = self.monotonic_origin.elapsed().as_millis();
        MonotonicWallTimestamp::new(
            i64::try_from(wall_time_ms)
                .map_err(|_| ReleaseOperationsError::InvalidEvidence("system wall clock"))?,
            u64::try_from(monotonic_ms)
                .map_err(|_| ReleaseOperationsError::InvalidEvidence("system monotonic clock"))?,
        )
    }
}

#[derive(Debug)]
struct SoakSessionState {
    completed: bool,
    last_timestamp: MonotonicWallTimestamp,
}

/// Durable, crate-internal operational soak recorder.
///
/// Production construction captures `SystemTime` and `Instant` directly.  The
/// observation append API is crate-private and accepts only typed runtime
/// observations.  The test module may inject a clock through the private
/// constructor below, but neither a public timestamp constructor nor a public
/// prober callback can mint [`VerifiedSoakReceipt`].
pub struct SoakSession {
    session_id: String,
    candidate_receipt_digest: ReleaseDigest,
    scope: ReleaseReceiptScope,
    policy: ReleasePromotionPolicy,
    signing_key: SigningKey,
    key_id: ReleaseSigningKeyId,
    started_at: MonotonicWallTimestamp,
    state: Mutex<SoakSessionState>,
    clock: Mutex<Box<dyn SoakClock>>,
    ledger: Mutex<Connection>,
}

impl SoakSession {
    /// Start a production soak with a real system wall clock and process-local
    /// monotonic clock.  This is crate-private so ingress code, not an
    /// untrusted client, owns the operational boundary.
    pub(crate) fn start(
        ledger_path: impl Into<PathBuf>,
        candidate_receipt: &VerifiedReleaseCandidateReceipt,
        signing_key: SigningKey,
        key_id: ReleaseSigningKeyId,
    ) -> ReleaseOperationsResult<Self> {
        Self::start_with_clock(
            ledger_path.into(),
            candidate_receipt,
            signing_key,
            key_id,
            Box::new(SystemSoakClock::new()),
        )
    }

    fn start_with_clock(
        ledger_path: PathBuf,
        candidate_receipt: &VerifiedReleaseCandidateReceipt,
        signing_key: SigningKey,
        key_id: ReleaseSigningKeyId,
        mut clock: Box<dyn SoakClock>,
    ) -> ReleaseOperationsResult<Self> {
        let started_at = clock.now()?;
        let scope = candidate_receipt.receipt().scope();
        let policy = candidate_receipt.receipt().policy().clone();
        let candidate_receipt_digest = candidate_receipt.receipt().receipt_digest().clone();
        let session_id = Uuid::new_v4().to_string();
        let ledger = open_soak_ledger(&ledger_path)?;
        ledger.execute(
            "INSERT INTO release_soak_sessions (
                session_id,
                candidate_receipt_digest,
                tenant_id,
                authenticated_subject,
                policy_id,
                policy_digest,
                started_wall_time_ms,
                started_monotonic_ms,
                completed_envelope
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL)",
            params![
                session_id,
                candidate_receipt_digest.to_string(),
                scope.tenant_id().to_string(),
                scope.authenticated_subject().to_string(),
                policy.policy_id().to_string(),
                policy.policy_digest().to_string(),
                started_at.wall_time_ms(),
                u64_to_sqlite(started_at.monotonic_ms(), "started monotonic timestamp")?,
            ],
        )?;
        Ok(Self {
            session_id,
            candidate_receipt_digest,
            scope,
            policy,
            signing_key,
            key_id,
            started_at,
            state: Mutex::new(SoakSessionState {
                completed: false,
                last_timestamp: started_at,
            }),
            clock: Mutex::new(clock),
            ledger: Mutex::new(ledger),
        })
    }

    /// Append a durable runtime observation with a session-owned timestamp.
    /// This is crate-private specifically so external callers cannot fabricate
    /// canary health, proof digests, or reconciliation counts.
    pub(crate) fn append_observation(
        &self,
        observation: OperationalSoakObservation,
    ) -> ReleaseOperationsResult<MonotonicWallTimestamp> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ReleaseOperationsError::InvalidEvidence("soak session state lock"))?;
        if state.completed {
            return Err(ReleaseOperationsError::SoakSessionComplete);
        }
        let observed_at = self
            .clock
            .lock()
            .map_err(|_| ReleaseOperationsError::InvalidEvidence("soak clock lock"))?
            .now()?;
        if observed_at.monotonic_ms() < state.last_timestamp.monotonic_ms()
            || observed_at.wall_time_ms() < state.last_timestamp.wall_time_ms()
        {
            return Err(ReleaseOperationsError::InvalidDuration);
        }

        let ledger = self
            .ledger
            .lock()
            .map_err(|_| ReleaseOperationsError::InvalidEvidence("soak ledger lock"))?;
        let sequence: i64 = ledger.query_row(
            "SELECT COALESCE(MAX(sequence), -1) + 1
             FROM release_soak_observations WHERE session_id = ?1",
            params![self.session_id],
            |row| row.get(0),
        )?;
        let (kind, proof_digest, canary_healthy, event_loss_count, unreconciled_effect_count) =
            match observation {
                OperationalSoakObservation::CanaryProbe(health) => (
                    "canary_probe",
                    None,
                    Some(if health == CanaryHealth::Healthy { 1_i64 } else { 0_i64 }),
                    None,
                    None,
                ),
                OperationalSoakObservation::ExactRunProof(digest) => (
                    "exact_run_proof",
                    Some(digest.to_string()),
                    None,
                    None,
                    None,
                ),
                OperationalSoakObservation::Reconciliation {
                    event_loss_count,
                    unreconciled_effect_count,
                } => (
                    "reconciliation",
                    None,
                    None,
                    Some(u64_to_sqlite(event_loss_count, "event loss count")?),
                    Some(u64_to_sqlite(
                        unreconciled_effect_count,
                        "unreconciled effect count",
                    )?),
                ),
            };
        ledger.execute(
            "INSERT INTO release_soak_observations (
                session_id,
                sequence,
                kind,
                observed_wall_time_ms,
                observed_monotonic_ms,
                proof_digest,
                canary_healthy,
                event_loss_count,
                unreconciled_effect_count
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                self.session_id,
                sequence,
                kind,
                observed_at.wall_time_ms(),
                u64_to_sqlite(observed_at.monotonic_ms(), "observed monotonic timestamp")?,
                proof_digest,
                canary_healthy,
                event_loss_count,
                unreconciled_effect_count,
            ],
        )?;
        state.last_timestamp = observed_at;
        Ok(observed_at)
    }

    /// Reconcile all durable observations, capture the session-owned ending
    /// timestamp, sign the resulting receipt, and mint promotion authority.
    /// The method has crate visibility: no external caller can supply an end
    /// timestamp, a fake probe implementation, or raw reconciliation JSON.
    pub(crate) fn complete(
        &self,
        trust_policy: &dyn ReleaseOperationsTrustPolicy,
    ) -> ReleaseOperationsResult<VerifiedSoakReceipt> {
        self.complete_after_sample(None, None, None, trust_policy)
    }

    fn complete_after_sample(
        &self,
        required_freshness_after: Option<MonotonicWallTimestamp>,
        maximum_heartbeat_gap_ms: Option<u64>,
        recorded_sample_count: Option<u64>,
        trust_policy: &dyn ReleaseOperationsTrustPolicy,
    ) -> ReleaseOperationsResult<VerifiedSoakReceipt> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| ReleaseOperationsError::InvalidEvidence("soak session state lock"))?;
        if state.completed {
            return Err(ReleaseOperationsError::SoakSessionComplete);
        }
        let ended_at = self
            .clock
            .lock()
            .map_err(|_| ReleaseOperationsError::InvalidEvidence("soak clock lock"))?
            .now()?;
        if ended_at.monotonic_ms() < state.last_timestamp.monotonic_ms()
            || ended_at.wall_time_ms() < state.last_timestamp.wall_time_ms()
        {
            return Err(ReleaseOperationsError::InvalidDuration);
        }
        if let Some(freshness_after) = required_freshness_after {
            if ended_at.monotonic_ms() <= freshness_after.monotonic_ms()
                || ended_at.wall_time_ms() <= freshness_after.wall_time_ms()
            {
                return Err(ReleaseOperationsError::InvalidDuration);
            }
        }
        let time_window = MonotonicWallTimeWindow::new(self.started_at, ended_at)?;
        if let (Some(maximum_gap_ms), Some(sample_count)) =
            (maximum_heartbeat_gap_ms, recorded_sample_count)
        {
            if maximum_gap_ms == 0
                || !heartbeat_gap_within(
                    required_freshness_after.ok_or(ReleaseOperationsError::MissingEvidence(
                        "fresh end runtime sample",
                    ))?,
                    ended_at,
                    maximum_gap_ms,
                )?
            {
                return Err(ReleaseOperationsError::IneligibleEvidence(
                    "final soak heartbeat gap",
                ));
            }
            let required_sample_count = time_window
                .elapsed_ms()
                .checked_add(maximum_gap_ms - 1)
                .ok_or(ReleaseOperationsError::InvalidEvidence("soak sample coverage"))?
                / maximum_gap_ms;
            if sample_count < required_sample_count {
                return Err(ReleaseOperationsError::MissingEvidence(
                    "minimum soak heartbeat coverage",
                ));
            }
        }
        let (canary_health, exact_run_proof_digests, event_loss_count, unreconciled_effect_count) =
            self.reconcile_observations()?;
        let input = SoakReceiptInput::new(
            self.candidate_receipt_digest.clone(),
            self.scope.clone(),
            self.policy.clone(),
            time_window,
            event_loss_count,
            unreconciled_effect_count,
            canary_health,
            exact_run_proof_digests,
        )?;
        let receipt = SoakReceipt::from(input.clone());
        receipt.validate_promotion_conditions()?;
        let envelope = sign_soak_receipt(input, &self.signing_key, self.key_id.clone())?;
        let signature_verified =
            verify_signed_soak_receipt(&envelope, &self.scope, trust_policy)?;
        signature_verified.receipt().validate_promotion_conditions()?;
        let canonical_envelope = envelope.canonical_bytes()?;
        let ledger = self
            .ledger
            .lock()
            .map_err(|_| ReleaseOperationsError::InvalidEvidence("soak ledger lock"))?;
        let changed = ledger.execute(
            "UPDATE release_soak_sessions
             SET completed_envelope = ?1
             WHERE session_id = ?2 AND completed_envelope IS NULL",
            params![canonical_envelope, self.session_id],
        )?;
        if changed != 1 {
            return Err(ReleaseOperationsError::SoakSessionComplete);
        }
        state.completed = true;
        state.last_timestamp = ended_at;
        Ok(VerifiedSoakReceipt { signature_verified })
    }

    fn reconcile_observations(
        &self,
    ) -> ReleaseOperationsResult<(CanaryHealth, Vec<ReleaseDigest>, u64, u64)> {
        let ledger = self
            .ledger
            .lock()
            .map_err(|_| ReleaseOperationsError::InvalidEvidence("soak ledger lock"))?;
        let mut statement = ledger.prepare(
            "SELECT kind, proof_digest, canary_healthy, event_loss_count, unreconciled_effect_count
             FROM release_soak_observations
             WHERE session_id = ?1
             ORDER BY sequence ASC",
        )?;
        let rows = statement.query_map(params![self.session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<i64>>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        })?;

        let mut saw_probe = false;
        let mut canary_healthy = true;
        let mut saw_reconciliation = false;
        let mut event_loss_count = 0_u64;
        let mut unreconciled_effect_count = 0_u64;
        let mut exact_run_proof_digests = Vec::new();
        for row in rows {
            let (kind, proof_digest, probe, event_loss, unreconciled) = row?;
            match kind.as_str() {
                "canary_probe" => {
                    let probe = probe.ok_or(ReleaseOperationsError::InvalidEvidence(
                        "durable canary probe",
                    ))?;
                    if probe != 0 && probe != 1 {
                        return Err(ReleaseOperationsError::InvalidEvidence(
                            "durable canary probe",
                        ));
                    }
                    saw_probe = true;
                    canary_healthy &= probe == 1;
                }
                "exact_run_proof" => {
                    let digest = proof_digest.ok_or(ReleaseOperationsError::MissingEvidence(
                        "durable exact run-proof digest",
                    ))?;
                    exact_run_proof_digests
                        .push(parse_release_digest(digest, "durable exact run-proof digest")?);
                }
                "reconciliation" => {
                    let event_loss = sqlite_to_u64(
                        event_loss.ok_or(ReleaseOperationsError::MissingEvidence(
                            "durable event-loss count",
                        ))?,
                        "durable event-loss count",
                    )?;
                    let unreconciled = sqlite_to_u64(
                        unreconciled.ok_or(ReleaseOperationsError::MissingEvidence(
                            "durable unreconciled-effect count",
                        ))?,
                        "durable unreconciled-effect count",
                    )?;
                    saw_reconciliation = true;
                    event_loss_count = event_loss_count
                        .checked_add(event_loss)
                        .ok_or(ReleaseOperationsError::InvalidEvidence("event-loss total"))?;
                    unreconciled_effect_count = unreconciled_effect_count
                        .checked_add(unreconciled)
                        .ok_or(ReleaseOperationsError::InvalidEvidence(
                            "unreconciled-effect total",
                        ))?;
                }
                _ => return Err(ReleaseOperationsError::InvalidEvidence("soak observation kind")),
            }
        }
        if !saw_probe {
            return Err(ReleaseOperationsError::MissingEvidence("durable canary probe"));
        }
        if !saw_reconciliation {
            return Err(ReleaseOperationsError::MissingEvidence(
                "durable reconciliation",
            ));
        }
        Ok((
            if canary_healthy {
                CanaryHealth::Healthy
            } else {
                CanaryHealth::Unhealthy
            },
            exact_run_proof_digests,
            event_loss_count,
            unreconciled_effect_count,
        ))
    }
}

/// A private, normalized snapshot from an authoritative runtime/ledger probe.
/// No public or crate-visible constructor exists: callers can neither invent a
/// healthy sample nor submit self-reported reconciliation totals.
#[cfg(feature = "agent-loop")]
#[derive(Debug)]
struct RuntimeSoakSnapshot {
    proof_digest: ReleaseDigest,
    canary_health: CanaryHealth,
    event_loss_count: u64,
    unreconciled_effect_count: u64,
}

/// Internal source boundary for the concrete runtime probe.  It remains private
/// so third parties cannot implement a convenient self-certifying probe.
#[cfg(feature = "agent-loop")]
trait AuthoritativeSoakProbe: Send + Sync {
    fn capture(
        &self,
        soak_started_at: MonotonicWallTimestamp,
    ) -> ReleaseOperationsResult<RuntimeSoakSnapshot>;
}

/// Private identity verifier for the currently executing runtime process.
/// Test-only implementations may model process identity, but production always
/// hashes `current_exe`; callers never supply a boolean candidate-match claim.
#[cfg(feature = "agent-loop")]
trait RuntimeArtifactIdentity: Send + Sync {
    fn verify(
        &self,
        candidate_artifacts: &BTreeMap<RollbackArtifactPath, ReleaseDigest>,
    ) -> ReleaseOperationsResult<()>;
}

#[cfg(feature = "agent-loop")]
struct ProcessExecutableIdentity;

#[cfg(feature = "agent-loop")]
impl RuntimeArtifactIdentity for ProcessExecutableIdentity {
    fn verify(
        &self,
        candidate_artifacts: &BTreeMap<RollbackArtifactPath, ReleaseDigest>,
    ) -> ReleaseOperationsResult<()> {
        let executable = std::env::current_exe()
            .map_err(|error| io_error("resolving running executable", error))?;
        let metadata = fs::symlink_metadata(&executable)
            .map_err(|error| io_error("reading running executable metadata", error))?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ReleaseOperationsError::UnsafeFilesystemPath(
                executable.display().to_string(),
            ));
        }
        let executable_digest = digest_bytes(&read_regular_file(&executable)?)?;
        if !candidate_artifacts
            .values()
            .any(|digest| digest == &executable_digest)
        {
            return Err(ReleaseOperationsError::InconsistentBinding(
                "running executable digest",
            ));
        }
        Ok(())
    }
}

/// Concrete probe over the durable [`RunRuntime`] ledger and the active release
/// pointer.  It admits only post-soak-start, terminally successful canary runs,
/// detects event-sequence gaps, and treats every pending/dead-lettered required
/// outbox record or non-completed non-resumable effect as unreconciled.
#[cfg(feature = "agent-loop")]
struct RunRuntimeSoakProbe {
    runtime: Arc<RunRuntime>,
    release_root: PathBuf,
    candidate_manifest_digest: ReleaseDigest,
    candidate_artifacts: BTreeMap<RollbackArtifactPath, ReleaseDigest>,
    expected_proof_digest: ReleaseDigest,
    artifact_identity: Box<dyn RuntimeArtifactIdentity>,
}

#[cfg(feature = "agent-loop")]
impl RunRuntimeSoakProbe {
    fn new(
        runtime: Arc<RunRuntime>,
        release_root: impl Into<PathBuf>,
        candidate_receipt: &VerifiedReleaseCandidateReceipt,
        candidate_manifest: &VerifiedRollbackManifest,
    ) -> ReleaseOperationsResult<Self> {
        Self::new_with_identity(
            runtime,
            release_root,
            candidate_receipt,
            candidate_manifest,
            Box::new(ProcessExecutableIdentity),
        )
    }

    fn new_with_identity(
        runtime: Arc<RunRuntime>,
        release_root: impl Into<PathBuf>,
        candidate_receipt: &VerifiedReleaseCandidateReceipt,
        candidate_manifest: &VerifiedRollbackManifest,
        artifact_identity: Box<dyn RuntimeArtifactIdentity>,
    ) -> ReleaseOperationsResult<Self> {
        if candidate_manifest.release_receipt_digest()
            != candidate_receipt.receipt().receipt_digest()
        {
            return Err(ReleaseOperationsError::InconsistentBinding(
                "candidate manifest release receipt digest",
            ));
        }
        if candidate_manifest.scope() != &candidate_receipt.receipt().scope() {
            return Err(ReleaseOperationsError::ScopeMismatch);
        }
        if candidate_manifest.policy() != candidate_receipt.receipt().policy() {
            return Err(ReleaseOperationsError::PolicyMismatch);
        }
        let release_root = release_root.into();
        ensure_safe_directory(&release_root)?;
        let release_root = fs::canonicalize(&release_root)
            .map_err(|error| io_error("canonicalizing release root", error))?;
        ensure_safe_file_if_exists(&release_root.join("current.json"))?;
        Ok(Self {
            runtime,
            release_root,
            candidate_manifest_digest: candidate_manifest.manifest_digest().clone(),
            candidate_artifacts: candidate_manifest.artifacts().clone(),
            expected_proof_digest: candidate_receipt
                .receipt()
                .exact_replay_evidence()
                .proof_digest()
                .clone(),
            artifact_identity,
        })
    }

    fn ensure_candidate_is_deployed(&self) -> ReleaseOperationsResult<()> {
        let _lock = ReleaseRootLock::acquire(&release_root_lock_path(&self.release_root))?;
        if read_release_pointer_at(&self.release_root)? != self.candidate_manifest_digest {
            return Err(ReleaseOperationsError::PointerNotCandidate);
        }
        let deployed_tree = self
            .release_root
            .join("releases")
            .join(self.candidate_manifest_digest.to_string());
        verify_release_tree(&deployed_tree, &self.candidate_artifacts)?;
        self.artifact_identity.verify(&self.candidate_artifacts)
    }
}

#[cfg(feature = "agent-loop")]
impl AuthoritativeSoakProbe for RunRuntimeSoakProbe {
    fn capture(
        &self,
        soak_started_at: MonotonicWallTimestamp,
    ) -> ReleaseOperationsResult<RuntimeSoakSnapshot> {
        const MAX_SAMPLED_RUNS: usize = 4096;

        self.ensure_candidate_is_deployed()?;
        let runs = self
            .runtime
            .recent_runs(MAX_SAMPLED_RUNS)
            .map_err(|_| ReleaseOperationsError::InvalidEvidence("runtime ledger snapshot"))?;
        if runs.len() == MAX_SAMPLED_RUNS {
            return Err(ReleaseOperationsError::MissingEvidence(
                "complete post-start runtime sample",
            ));
        }

        let mut post_start_run_count = 0_u64;
        let mut canary_health = CanaryHealth::Healthy;
        let mut event_loss_count = 0_u64;
        let mut unreconciled_effect_count = 0_u64;
        for run in runs {
            if run.header.started_at_ms <= soak_started_at.wall_time_ms() {
                continue;
            }
            post_start_run_count = post_start_run_count
                .checked_add(1)
                .ok_or(ReleaseOperationsError::InvalidEvidence("post-start run count"))?;
            if run.event_count == 0
                || run.status.final_outcome != Some(RunOutcome::Success)
                || run.status.exit_terminal_reason != Some(TerminalReason::Completed)
            {
                canary_health = CanaryHealth::Unhealthy;
            }
            if run.status.pending_required_delivery || run.status.required_dead_lettered {
                unreconciled_effect_count = unreconciled_effect_count.checked_add(1).ok_or(
                    ReleaseOperationsError::InvalidEvidence("unreconciled-effect total"),
                )?;
                canary_health = CanaryHealth::Unhealthy;
            }

            // The durable ledger's first event is sequence one.  Validate the
            // whole observed prefix and both terminal anchors, rather than
            // treating only an interior discontinuity as loss.
            let mut expected_sequence = 1_u64;
            for event in &run.events {
                if event.sequence != expected_sequence {
                    let missing = if event.sequence > expected_sequence {
                        event.sequence - expected_sequence
                    } else {
                        1
                    };
                    event_loss_count = event_loss_count.checked_add(missing).ok_or(
                        ReleaseOperationsError::InvalidEvidence("event-loss total"),
                    )?;
                    canary_health = CanaryHealth::Unhealthy;
                }
                expected_sequence = event.sequence.checked_add(1).ok_or(
                    ReleaseOperationsError::InvalidEvidence("runtime event sequence"),
                )?;
                if event.effect_intent != EffectIntent::None {
                    let record = self
                        .runtime
                        .ledger()
                        .nonresumable_effect(event.event_id)
                        .map_err(|_| {
                            ReleaseOperationsError::InvalidEvidence("runtime effect ledger")
                        })?;
                    if !matches!(
                        record,
                        Some(record) if record.state == NonResumableEffectState::Completed
                    ) {
                        unreconciled_effect_count =
                            unreconciled_effect_count.checked_add(1).ok_or(
                                ReleaseOperationsError::InvalidEvidence(
                                    "unreconciled-effect total",
                                ),
                            )?;
                        canary_health = CanaryHealth::Unhealthy;
                    }
                }
            }
            if u64::try_from(run.events.len())
                .map_err(|_| ReleaseOperationsError::InvalidEvidence("runtime event count"))?
                != run.event_count
                || run
                    .events
                    .first()
                    .map(|event| event.event_id != run.header.start_event_id)
                    .unwrap_or(true)
                || run
                    .events
                    .last()
                    .map(|event| {
                        event.manifest_hash != run.chain_head
                            || run.status.exit_event_id.as_ref() != Some(&event.event_id)
                    })
                    .unwrap_or(true)
            {
                event_loss_count = event_loss_count.checked_add(1).ok_or(
                    ReleaseOperationsError::InvalidEvidence("event-loss total"),
                )?;
                canary_health = CanaryHealth::Unhealthy;
            }
            for outbox in self
                .runtime
                .ledger()
                .outbox_for_run(run.run_id)
                .map_err(|_| ReleaseOperationsError::InvalidEvidence("runtime outbox ledger"))?
            {
                if outbox.required && outbox.state != OutboxState::Acknowledged {
                    unreconciled_effect_count =
                        unreconciled_effect_count.checked_add(1).ok_or(
                            ReleaseOperationsError::InvalidEvidence("unreconciled-effect total"),
                        )?;
                    canary_health = CanaryHealth::Unhealthy;
                }
            }
        }
        if post_start_run_count == 0 {
            return Err(ReleaseOperationsError::MissingEvidence(
                "post-start canary runtime sample",
            ));
        }
        self.ensure_candidate_is_deployed()?;
        Ok(RuntimeSoakSnapshot {
            proof_digest: self.expected_proof_digest.clone(),
            canary_health,
            event_loss_count,
            unreconciled_effect_count,
        })
    }
}

#[cfg(feature = "agent-loop")]
#[derive(Debug)]
struct SoakAuthorityState {
    failed: bool,
    proof_recorded: bool,
    sample_count: u64,
    first_sample_at: Option<MonotonicWallTimestamp>,
    last_sample_at: Option<MonotonicWallTimestamp>,
}

/// Public, library-owned promotion-soak orchestrator.
///
/// The only public constructor wires a concrete [`RunRuntimeSoakProbe`] to a
/// verified candidate receipt and signed candidate manifest.  The only public
/// observation method samples that probe.  Thus timestamps, canary health,
/// event-loss totals, effect reconciliation totals, deployment identity, and
/// exact proof identity all arise from verified/durable sources rather than
/// caller JSON.
#[cfg(feature = "agent-loop")]
pub struct SoakAuthorityService {
    session: SoakSession,
    probe: Box<dyn AuthoritativeSoakProbe>,
    started_at: MonotonicWallTimestamp,
    maximum_heartbeat_gap_ms: u64,
    state: Mutex<SoakAuthorityState>,
}

#[cfg(feature = "agent-loop")]
impl SoakAuthorityService {
    /// Start a durable soak over a deployed, signed candidate manifest and the
    /// authoritative runtime ledger.  The deployment pointer is revalidated on
    /// every sample.
    pub fn start(
        ledger_path: impl Into<PathBuf>,
        runtime: Arc<RunRuntime>,
        release_root: impl Into<PathBuf>,
        candidate_receipt: &VerifiedReleaseCandidateReceipt,
        candidate_manifest: &VerifiedRollbackManifest,
        signing_key: SigningKey,
        key_id: ReleaseSigningKeyId,
    ) -> ReleaseOperationsResult<Self> {
        let probe = Box::new(RunRuntimeSoakProbe::new(
            runtime,
            release_root,
            candidate_receipt,
            candidate_manifest,
        )?);
        Self::start_with_probe(
            ledger_path.into(),
            candidate_receipt,
            signing_key,
            key_id,
            probe,
        )
    }

    fn start_with_probe(
        ledger_path: PathBuf,
        candidate_receipt: &VerifiedReleaseCandidateReceipt,
        signing_key: SigningKey,
        key_id: ReleaseSigningKeyId,
        probe: Box<dyn AuthoritativeSoakProbe>,
    ) -> ReleaseOperationsResult<Self> {
        let maximum_heartbeat_gap_ms = candidate_receipt
            .receipt()
            .policy()
            .maximum_soak_heartbeat_gap_ms();
        if maximum_heartbeat_gap_ms == 0 {
            return Err(ReleaseOperationsError::InvalidEvidence(
                "maximum soak heartbeat gap",
            ));
        }
        let session = SoakSession::start(ledger_path, candidate_receipt, signing_key, key_id)?;
        let started_at = session.started_at;
        Ok(Self {
            session,
            probe,
            started_at,
            maximum_heartbeat_gap_ms,
            state: Mutex::new(SoakAuthorityState {
                failed: false,
                proof_recorded: false,
                sample_count: 0,
                first_sample_at: None,
                last_sample_at: None,
            }),
        })
    }

    /// Capture one post-start concrete runtime/ledger sample.  It records the
    /// candidate's verified exact proof only once, but records every health and
    /// reconciliation snapshot in sequence so a later receipt cannot conceal a
    /// mid-window failure.
    pub fn record_sample(&self) -> ReleaseOperationsResult<()> {
        let snapshot = self.probe.capture(self.started_at)?;
        let mut state = self
            .state
            .lock()
            .map_err(|_| ReleaseOperationsError::InvalidEvidence("soak authority state lock"))?;
        if state.failed {
            return Err(ReleaseOperationsError::IneligibleEvidence(
                "failed authoritative soak session",
            ));
        }
        let persisted = (|| -> ReleaseOperationsResult<MonotonicWallTimestamp> {
            if !state.proof_recorded {
                self.session.append_observation(
                    OperationalSoakObservation::ExactRunProof(snapshot.proof_digest),
                )?;
                state.proof_recorded = true;
            }
            self.session.append_observation(
                OperationalSoakObservation::CanaryProbe(snapshot.canary_health),
            )?;
            self.session.append_observation(
                OperationalSoakObservation::Reconciliation {
                    event_loss_count: snapshot.event_loss_count,
                    unreconciled_effect_count: snapshot.unreconciled_effect_count,
                },
            )
        })();
        match persisted {
            Ok(sampled_at) => {
                let previous_sample_at = state.last_sample_at.unwrap_or(self.started_at);
                if !heartbeat_gap_within(
                    previous_sample_at,
                    sampled_at,
                    self.maximum_heartbeat_gap_ms,
                )? {
                    state.failed = true;
                    return Err(ReleaseOperationsError::IneligibleEvidence(
                        "soak heartbeat gap",
                    ));
                }
                state.sample_count = state
                    .sample_count
                    .checked_add(1)
                    .ok_or(ReleaseOperationsError::InvalidEvidence("soak sample count"))?;
                if state.first_sample_at.is_none() {
                    state.first_sample_at = Some(sampled_at);
                }
                state.last_sample_at = Some(sampled_at);
                Ok(())
            }
            Err(error) => {
                state.failed = true;
                Err(error)
            }
        }
    }

    /// Capture a fresh end-window sample, then mint authority only when at
    /// least two post-start samples span time and the session captures a
    /// strictly later end timestamp.
    pub fn complete(
        &self,
        trust_policy: &dyn ReleaseOperationsTrustPolicy,
    ) -> ReleaseOperationsResult<VerifiedSoakReceipt> {
        self.record_sample()?;
        let (last_sample_at, sample_count) = {
            let state = self
                .state
                .lock()
                .map_err(|_| ReleaseOperationsError::InvalidEvidence("soak authority state lock"))?;
            let first_sample_at = state
                .first_sample_at
                .ok_or(ReleaseOperationsError::MissingEvidence("first runtime sample"))?;
            let last_sample_at = state
                .last_sample_at
                .ok_or(ReleaseOperationsError::MissingEvidence("fresh end runtime sample"))?;
            if state.sample_count < 2
                || first_sample_at.monotonic_ms() <= self.started_at.monotonic_ms()
                || first_sample_at.wall_time_ms() <= self.started_at.wall_time_ms()
                || last_sample_at.monotonic_ms() <= first_sample_at.monotonic_ms()
                || last_sample_at.wall_time_ms() <= first_sample_at.wall_time_ms()
            {
                return Err(ReleaseOperationsError::MissingEvidence(
                    "multiple post-start runtime samples spanning soak",
                ));
            }
            (last_sample_at, state.sample_count)
        };
        self.session.complete_after_sample(
            Some(last_sample_at),
            Some(self.maximum_heartbeat_gap_ms),
            Some(sample_count),
            trust_policy,
        )
    }
}

fn open_soak_ledger(path: &Path) -> ReleaseOperationsResult<Connection> {
    let parent = path.parent().ok_or_else(|| {
        ReleaseOperationsError::UnsafeFilesystemPath(path.display().to_string())
    })?;
    ensure_safe_directory(parent)?;
    ensure_safe_file_if_exists(path)?;
    let connection = Connection::open(path)?;
    connection.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA synchronous = FULL;
         CREATE TABLE IF NOT EXISTS release_soak_sessions (
             session_id TEXT PRIMARY KEY NOT NULL,
             candidate_receipt_digest TEXT NOT NULL,
             tenant_id TEXT NOT NULL,
             authenticated_subject TEXT NOT NULL,
             policy_id TEXT NOT NULL,
             policy_digest TEXT NOT NULL,
             started_wall_time_ms INTEGER NOT NULL,
             started_monotonic_ms INTEGER NOT NULL,
             completed_envelope BLOB
         ) STRICT;
         CREATE TABLE IF NOT EXISTS release_soak_observations (
             session_id TEXT NOT NULL,
             sequence INTEGER NOT NULL,
             kind TEXT NOT NULL,
             observed_wall_time_ms INTEGER NOT NULL,
             observed_monotonic_ms INTEGER NOT NULL,
             proof_digest TEXT,
             canary_healthy INTEGER,
             event_loss_count INTEGER,
             unreconciled_effect_count INTEGER,
             PRIMARY KEY (session_id, sequence),
             FOREIGN KEY (session_id) REFERENCES release_soak_sessions(session_id)
         ) STRICT;",
    )?;
    Ok(connection)
}

fn u64_to_sqlite(value: u64, field: &'static str) -> ReleaseOperationsResult<i64> {
    i64::try_from(value).map_err(|_| ReleaseOperationsError::InvalidEvidence(field))
}

fn sqlite_to_u64(value: i64, field: &'static str) -> ReleaseOperationsResult<u64> {
    u64::try_from(value).map_err(|_| ReleaseOperationsError::InvalidEvidence(field))
}

/// Typed, unsigned input for a signed rollback manifest.
#[derive(Debug, Clone)]
pub struct RollbackManifestInput {
    release_receipt_digest: ReleaseDigest,
    scope: ReleaseReceiptScope,
    policy: ReleasePromotionPolicy,
    artifacts: BTreeMap<RollbackArtifactPath, ReleaseDigest>,
    runtime_entrypoint: RollbackArtifactPath,
    runtime_build: RuntimeBuildBinding,
}

impl RollbackManifestInput {
    pub fn new(
        release_receipt_digest: ReleaseDigest,
        scope: ReleaseReceiptScope,
        policy: ReleasePromotionPolicy,
        artifacts: impl IntoIterator<Item = (RollbackArtifactPath, ReleaseDigest)>,
        runtime_entrypoint: RollbackArtifactPath,
        runtime_build: RuntimeBuildBinding,
    ) -> ReleaseOperationsResult<Self> {
        let mut by_path = BTreeMap::new();
        for (path, digest) in artifacts {
            if by_path.insert(path, digest).is_some() {
                return Err(ReleaseOperationsError::InvalidEvidence(
                    "duplicate rollback artifact path",
                ));
            }
        }
        if by_path.is_empty() {
            return Err(ReleaseOperationsError::MissingEvidence("rollback artifacts"));
        }
        if by_path.len() > MAX_ARTIFACTS {
            return Err(ReleaseOperationsError::InvalidEvidence("rollback artifact count"));
        }
        if by_path.get(&runtime_entrypoint) != Some(runtime_build.artifact_sha256()) {
            return Err(ReleaseOperationsError::InconsistentBinding(
                "runtime entrypoint artifact digest",
            ));
        }
        Ok(Self {
            release_receipt_digest,
            scope,
            policy,
            artifacts: by_path,
            runtime_entrypoint,
            runtime_build,
        })
    }
}

/// Closed signed rollback manifest payload.
#[derive(Debug, Clone, PartialEq)]
pub struct RollbackManifest {
    release_receipt_digest: ReleaseDigest,
    scope: ReleaseReceiptScope,
    policy: ReleasePromotionPolicy,
    artifacts: BTreeMap<RollbackArtifactPath, ReleaseDigest>,
    runtime_entrypoint: RollbackArtifactPath,
    runtime_build: RuntimeBuildBinding,
    artifact_set_digest: ReleaseDigest,
}

impl RollbackManifest {
    fn from_input(input: RollbackManifestInput) -> ReleaseOperationsResult<Self> {
        let artifact_set_digest = artifact_set_digest(&input.artifacts)?;
        let manifest = Self {
            release_receipt_digest: input.release_receipt_digest,
            scope: input.scope,
            policy: input.policy,
            artifacts: input.artifacts,
            runtime_entrypoint: input.runtime_entrypoint,
            runtime_build: input.runtime_build,
            artifact_set_digest,
        };
        manifest.validate_structure()?;
        Ok(manifest)
    }

    fn validate_structure(&self) -> ReleaseOperationsResult<()> {
        if self.artifacts.is_empty() {
            return Err(ReleaseOperationsError::MissingEvidence("rollback artifacts"));
        }
        if self.artifacts.len() > MAX_ARTIFACTS {
            return Err(ReleaseOperationsError::InvalidEvidence("rollback artifact count"));
        }
        for path in self.artifacts.keys() {
            RollbackArtifactPath::new(path.0.clone())?;
        }
        if artifact_set_digest(&self.artifacts)? != self.artifact_set_digest {
            return Err(ReleaseOperationsError::InconsistentBinding(
                "rollback artifact-set digest",
            ));
        }
        if self.artifacts.get(&self.runtime_entrypoint) != Some(self.runtime_build.artifact_sha256()) {
            return Err(ReleaseOperationsError::InconsistentBinding(
                "runtime entrypoint artifact digest",
            ));
        }
        Ok(())
    }

    pub fn release_receipt_digest(&self) -> &ReleaseDigest {
        &self.release_receipt_digest
    }

    pub fn scope(&self) -> &ReleaseReceiptScope {
        &self.scope
    }

    pub fn policy(&self) -> &ReleasePromotionPolicy {
        &self.policy
    }

    pub fn artifacts(&self) -> &BTreeMap<RollbackArtifactPath, ReleaseDigest> {
        &self.artifacts
    }

    /// The only manifest artifact that may execute the release runtime.
    pub fn runtime_entrypoint(&self) -> &RollbackArtifactPath {
        &self.runtime_entrypoint
    }

    /// Immutable source/build identity embedded in the declared entrypoint.
    pub fn runtime_build(&self) -> &RuntimeBuildBinding {
        &self.runtime_build
    }
    pub fn artifact_set_digest(&self) -> &ReleaseDigest {
        &self.artifact_set_digest
    }

/// Typed signed rollback-manifest envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct RollbackManifestEnvelope {
    header: EnvelopeHeader,
    manifest: RollbackManifest,
    signature: String,
}

impl RollbackManifestEnvelope {
    pub fn canonical_bytes(&self) -> ReleaseOperationsResult<Vec<u8>> {
        self.validate()?;
        canonical_json(&SignedEnvelopeWire::from_parts(
            &self.header,
            RollbackManifestWire::from(&self.manifest),
            self.signature.clone(),
        ))
    }

    pub fn manifest(&self) -> &RollbackManifest {
        &self.manifest
    }

    pub fn key_id(&self) -> &ReleaseSigningKeyId {
        &self.header.key_id
    }

    pub fn manifest_digest(&self) -> ReleaseOperationsResult<ReleaseDigest> {
        digest_bytes(&self.canonical_bytes()?)
    }

    fn validate(&self) -> ReleaseOperationsResult<()> {
        self.header.validate(ROLLBACK_MANIFEST_SCHEMA)?;
        self.manifest.validate_structure()?;
        let _ = decode_signature(&self.signature)?;
        Ok(())
    }

    fn parse_canonical_bytes(bytes: &[u8]) -> ReleaseOperationsResult<Self> {
        let wire: SignedEnvelopeWire<RollbackManifestWire> = parse_canonical_envelope(bytes)?;
        let (header, payload, signature) = wire.into_header()?;
        let envelope = Self {
            header,
            manifest: RollbackManifest::try_from(payload)?,
            signature,
        };
        envelope.validate()?;
        Ok(envelope)
    }
}

/// Sign a typed rollback manifest.
pub fn sign_rollback_manifest(
    input: RollbackManifestInput,
    signing_key: &SigningKey,
    key_id: ReleaseSigningKeyId,
) -> ReleaseOperationsResult<RollbackManifestEnvelope> {
    let manifest = RollbackManifest::from_input(input)?;
    let header = EnvelopeHeader::new(ROLLBACK_MANIFEST_SCHEMA, key_id);
    let payload = RollbackManifestWire::from(&manifest);
    let signature = encode_signature(signing_key.sign(&signing_preimage(
        ROLLBACK_MANIFEST_SIGNING_DOMAIN,
        &header,
        &payload,
    )?));
    let envelope = RollbackManifestEnvelope {
        header,
        manifest,
        signature,
    };
    envelope.validate()?;
    Ok(envelope)
}

/// Verify a signed rollback manifest in one caller-required scope.
pub fn verify_rollback_manifest(
    envelope: &RollbackManifestEnvelope,
    expected_scope: &ReleaseReceiptScope,
    trust_policy: &dyn ReleaseOperationsTrustPolicy,
) -> ReleaseOperationsResult<VerifiedRollbackManifest> {
    envelope.validate()?;
    verify_scope_policy(
        envelope.manifest.scope(),
        envelope.manifest.policy(),
        expected_scope,
        trust_policy,
    )?;
    let payload = RollbackManifestWire::from(&envelope.manifest);
    verify_signature(
        ROLLBACK_MANIFEST_SIGNING_DOMAIN,
        &envelope.header,
        &payload,
        &envelope.signature,
        envelope.manifest.scope(),
        trust_policy,
    )?;
    Ok(VerifiedRollbackManifest {
        manifest_digest: envelope.manifest_digest()?,
        envelope: envelope.clone(),
    })
}

/// Verify a canonical rollback-manifest envelope from untrusted bytes.
pub fn verify_rollback_manifest_bytes(
    bytes: &[u8],
    expected_scope: &ReleaseReceiptScope,
    trust_policy: &dyn ReleaseOperationsTrustPolicy,
) -> ReleaseOperationsResult<VerifiedRollbackManifest> {
    let envelope = RollbackManifestEnvelope::parse_canonical_bytes(bytes)?;
    verify_rollback_manifest(&envelope, expected_scope, trust_policy)
}

/// Nonconstructible signed, scope-bound rollback manifest.
#[derive(Debug)]
pub struct VerifiedRollbackManifest {
    envelope: RollbackManifestEnvelope,
    manifest_digest: ReleaseDigest,
}

impl VerifiedRollbackManifest {
    pub fn manifest_digest(&self) -> &ReleaseDigest {
        &self.manifest_digest
    }

    pub fn release_receipt_digest(&self) -> &ReleaseDigest {
        self.envelope.manifest.release_receipt_digest()
    }

    pub fn scope(&self) -> &ReleaseReceiptScope {
        self.envelope.manifest.scope()
    }

    pub fn policy(&self) -> &ReleasePromotionPolicy {
        self.envelope.manifest.policy()
    }

    pub fn artifacts(&self) -> &BTreeMap<RollbackArtifactPath, ReleaseDigest> {
        self.envelope.manifest.artifacts()
    }

    pub fn artifact_set_digest(&self) -> &ReleaseDigest {
        self.envelope.manifest.artifact_set_digest()
    }
}

/// Input to an idempotent rollback drill.  The manager captures its own start
/// and end timestamps; callers can name the immutable drill but cannot claim a
/// duration.
#[derive(Debug, Clone)]
pub struct RollbackDrillRequest {
    drill_id: RollbackDrillId,
}

impl RollbackDrillRequest {
    pub fn new(drill_id: RollbackDrillId) -> Self {
        Self { drill_id }
    }

    pub fn drill_id(&self) -> &RollbackDrillId {
        &self.drill_id
    }
}

/// Typed, unsigned input for a signed rollback drill receipt.
#[derive(Debug, Clone)]
pub struct RollbackDrillReceiptInput {
    drill_id: RollbackDrillId,
    candidate_receipt_digest: ReleaseDigest,
    incumbent_receipt_digest: ReleaseDigest,
    candidate_manifest_digest: ReleaseDigest,
    incumbent_manifest_digest: ReleaseDigest,
    restored_artifact_set_digest: ReleaseDigest,
    scope: ReleaseReceiptScope,
    policy: ReleasePromotionPolicy,
    time_window: MonotonicWallTimeWindow,
    verification_result: RollbackVerificationResult,
}

impl RollbackDrillReceiptInput {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        drill_id: RollbackDrillId,
        candidate_receipt_digest: ReleaseDigest,
        incumbent_receipt_digest: ReleaseDigest,
        candidate_manifest_digest: ReleaseDigest,
        incumbent_manifest_digest: ReleaseDigest,
        restored_artifact_set_digest: ReleaseDigest,
        scope: ReleaseReceiptScope,
        policy: ReleasePromotionPolicy,
        time_window: MonotonicWallTimeWindow,
        verification_result: RollbackVerificationResult,
    ) -> ReleaseOperationsResult<Self> {
        time_window.validate()?;
        if candidate_receipt_digest == incumbent_receipt_digest {
            return Err(ReleaseOperationsError::InconsistentBinding(
                "candidate and incumbent receipt digest",
            ));
        }
        if candidate_manifest_digest == incumbent_manifest_digest {
            return Err(ReleaseOperationsError::InconsistentBinding(
                "candidate and incumbent manifest digest",
            ));
        }
        Ok(Self {
            drill_id,
            candidate_receipt_digest,
            incumbent_receipt_digest,
            candidate_manifest_digest,
            incumbent_manifest_digest,
            restored_artifact_set_digest,
            scope,
            policy,
            time_window,
            verification_result,
        })
    }
}

/// Closed signed rollback-drill receipt payload.
#[derive(Debug, Clone, PartialEq)]
pub struct RollbackDrillReceipt {
    drill_id: RollbackDrillId,
    candidate_receipt_digest: ReleaseDigest,
    incumbent_receipt_digest: ReleaseDigest,
    candidate_manifest_digest: ReleaseDigest,
    incumbent_manifest_digest: ReleaseDigest,
    restored_artifact_set_digest: ReleaseDigest,
    scope: ReleaseReceiptScope,
    policy: ReleasePromotionPolicy,
    time_window: MonotonicWallTimeWindow,
    verification_result: RollbackVerificationResult,
}

impl From<RollbackDrillReceiptInput> for RollbackDrillReceipt {
    fn from(value: RollbackDrillReceiptInput) -> Self {
        Self {
            drill_id: value.drill_id,
            candidate_receipt_digest: value.candidate_receipt_digest,
            incumbent_receipt_digest: value.incumbent_receipt_digest,
            candidate_manifest_digest: value.candidate_manifest_digest,
            incumbent_manifest_digest: value.incumbent_manifest_digest,
            restored_artifact_set_digest: value.restored_artifact_set_digest,
            scope: value.scope,
            policy: value.policy,
            time_window: value.time_window,
            verification_result: value.verification_result,
        }
    }
}

impl RollbackDrillReceipt {
    fn validate_structure(&self) -> ReleaseOperationsResult<()> {
        self.time_window.validate()?;
        RollbackDrillId::new(self.drill_id.0.clone())?;
        if self.candidate_receipt_digest == self.incumbent_receipt_digest {
            return Err(ReleaseOperationsError::InconsistentBinding(
                "candidate and incumbent receipt digest",
            ));
        }
        if self.candidate_manifest_digest == self.incumbent_manifest_digest {
            return Err(ReleaseOperationsError::InconsistentBinding(
                "candidate and incumbent manifest digest",
            ));
        }
        Ok(())
    }

    fn validate_authority_conditions(&self) -> ReleaseOperationsResult<()> {
        self.validate_structure()?;
        if self.time_window.elapsed_ms >= MAXIMUM_ROLLBACK_DRILL_DURATION_MS {
            return Err(ReleaseOperationsError::IneligibleEvidence(
                "rollback drill duration",
            ));
        }
        if self.verification_result != RollbackVerificationResult::Verified {
            return Err(ReleaseOperationsError::IneligibleEvidence(
                "rollback verification result",
            ));
        }
        Ok(())
    }

    pub fn drill_id(&self) -> &RollbackDrillId {
        &self.drill_id
    }

    pub fn candidate_receipt_digest(&self) -> &ReleaseDigest {
        &self.candidate_receipt_digest
    }

    pub fn incumbent_receipt_digest(&self) -> &ReleaseDigest {
        &self.incumbent_receipt_digest
    }

    pub fn candidate_manifest_digest(&self) -> &ReleaseDigest {
        &self.candidate_manifest_digest
    }

    pub fn incumbent_manifest_digest(&self) -> &ReleaseDigest {
        &self.incumbent_manifest_digest
    }

    pub fn restored_artifact_set_digest(&self) -> &ReleaseDigest {
        &self.restored_artifact_set_digest
    }

    pub fn scope(&self) -> &ReleaseReceiptScope {
        &self.scope
    }

    pub fn policy(&self) -> &ReleasePromotionPolicy {
        &self.policy
    }

    pub fn time_window(&self) -> &MonotonicWallTimeWindow {
        &self.time_window
    }

    pub const fn verification_result(&self) -> RollbackVerificationResult {
        self.verification_result
    }
}

/// Typed signed rollback-drill envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct RollbackDrillReceiptEnvelope {
    header: EnvelopeHeader,
    receipt: RollbackDrillReceipt,
    signature: String,
}

impl RollbackDrillReceiptEnvelope {
    pub fn canonical_bytes(&self) -> ReleaseOperationsResult<Vec<u8>> {
        self.validate()?;
        canonical_json(&SignedEnvelopeWire::from_parts(
            &self.header,
            RollbackDrillReceiptWire::from(&self.receipt),
            self.signature.clone(),
        ))
    }

    pub fn receipt(&self) -> &RollbackDrillReceipt {
        &self.receipt
    }

    pub fn key_id(&self) -> &ReleaseSigningKeyId {
        &self.header.key_id
    }

    pub fn receipt_digest(&self) -> ReleaseOperationsResult<ReleaseDigest> {
        digest_bytes(&self.canonical_bytes()?)
    }

    fn validate(&self) -> ReleaseOperationsResult<()> {
        self.header.validate(ROLLBACK_DRILL_RECEIPT_SCHEMA)?;
        self.receipt.validate_structure()?;
        let _ = decode_signature(&self.signature)?;
        Ok(())
    }

    fn parse_canonical_bytes(bytes: &[u8]) -> ReleaseOperationsResult<Self> {
        let wire: SignedEnvelopeWire<RollbackDrillReceiptWire> = parse_canonical_envelope(bytes)?;
        let (header, payload, signature) = wire.into_header()?;
        let envelope = Self {
            header,
            receipt: RollbackDrillReceipt::try_from(payload)?,
            signature,
        };
        envelope.validate()?;
        Ok(envelope)
    }
}

/// Sign a typed rollback drill attestation.  This produces only a signed
/// envelope; an externally supplied signed attestation is never promotion
/// evidence.  Only the rollback manager mints authority after an actual,
/// verified atomic pointer swap.
pub fn sign_rollback_drill_receipt(
    input: RollbackDrillReceiptInput,
    signing_key: &SigningKey,
    key_id: ReleaseSigningKeyId,
) -> ReleaseOperationsResult<RollbackDrillReceiptEnvelope> {
    let receipt = RollbackDrillReceipt::from(input);
    receipt.validate_structure()?;
    let header = EnvelopeHeader::new(ROLLBACK_DRILL_RECEIPT_SCHEMA, key_id);
    let payload = RollbackDrillReceiptWire::from(&receipt);
    let signature = encode_signature(signing_key.sign(&signing_preimage(
        ROLLBACK_DRILL_SIGNING_DOMAIN,
        &header,
        &payload,
    )?));
    let envelope = RollbackDrillReceiptEnvelope {
        header,
        receipt,
        signature,
    };
    envelope.validate()?;
    Ok(envelope)
}

/// Verify only the detached signature, scope, policy, and intrinsic drill
/// conditions.  The returned value is deliberately insufficient for promotion;
/// only the rollback manager binds a signed attestation to an executed swap.
pub fn verify_signed_rollback_drill_receipt(
    envelope: &RollbackDrillReceiptEnvelope,
    expected_scope: &ReleaseReceiptScope,
    trust_policy: &dyn ReleaseOperationsTrustPolicy,
) -> ReleaseOperationsResult<SignatureVerifiedRollbackDrillReceipt> {
    envelope.validate()?;
    verify_scope_policy(
        envelope.receipt.scope(),
        envelope.receipt.policy(),
        expected_scope,
        trust_policy,
    )?;
    let payload = RollbackDrillReceiptWire::from(&envelope.receipt);
    verify_signature(
        ROLLBACK_DRILL_SIGNING_DOMAIN,
        &envelope.header,
        &payload,
        &envelope.signature,
        envelope.receipt.scope(),
        trust_policy,
    )?;
    envelope.receipt.validate_authority_conditions()?;
    Ok(SignatureVerifiedRollbackDrillReceipt {
        envelope: envelope.clone(),
    })
}

/// Verify an untrusted canonical rollback-drill envelope to the
/// signature-only evidence tier.
pub fn verify_signed_rollback_drill_receipt_bytes(
    bytes: &[u8],
    expected_scope: &ReleaseReceiptScope,
    trust_policy: &dyn ReleaseOperationsTrustPolicy,
) -> ReleaseOperationsResult<SignatureVerifiedRollbackDrillReceipt> {
    let envelope = RollbackDrillReceiptEnvelope::parse_canonical_bytes(bytes)?;
    verify_signed_rollback_drill_receipt(&envelope, expected_scope, trust_policy)
}

/// Nonconstructible signature-only rollback evidence.  It has not yet proven
/// that its candidate/incumbent digest claims resolve to verified evidence.
#[derive(Debug)]
pub struct SignatureVerifiedRollbackDrillReceipt {
    envelope: RollbackDrillReceiptEnvelope,
}

impl SignatureVerifiedRollbackDrillReceipt {
    fn receipt(&self) -> &RollbackDrillReceipt {
        &self.envelope.receipt
    }
}

/// Bind a signature-verified rollback drill to the exact verified candidate and
/// incumbent source receipts and manifests it claims.  This private constructor
/// is called only after the rollback manager has completed its atomic pointer
/// swap.
fn verify_bound_rollback_drill_receipt(
    envelope: &RollbackDrillReceiptEnvelope,
    expected_scope: &ReleaseReceiptScope,
    trust_policy: &dyn ReleaseOperationsTrustPolicy,
    candidate_receipt: &VerifiedReleaseCandidateReceipt,
    incumbent_receipt: &VerifiedReleaseCandidateReceipt,
    candidate_manifest: &VerifiedRollbackManifest,
    incumbent_manifest: &VerifiedRollbackManifest,
) -> ReleaseOperationsResult<VerifiedRollbackDrillReceipt> {
    let signature_verified =
        verify_signed_rollback_drill_receipt(envelope, expected_scope, trust_policy)?;
    validate_drill_binding(
        signature_verified.receipt(),
        expected_scope,
        candidate_receipt,
        incumbent_receipt,
        candidate_manifest,
        incumbent_manifest,
    )?;
    Ok(VerifiedRollbackDrillReceipt { signature_verified })
}

/// Promotion-authoritative rollback drill evidence.  It is nonconstructible and
/// proves both the operation signature and every signed receipt/manifest digest
/// binding.
#[derive(Debug)]
pub struct VerifiedRollbackDrillReceipt {
    signature_verified: SignatureVerifiedRollbackDrillReceipt,
}

impl VerifiedRollbackDrillReceipt {
    pub fn candidate_receipt_digest(&self) -> &ReleaseDigest {
        self.signature_verified.receipt().candidate_receipt_digest()
    }

    pub fn incumbent_receipt_digest(&self) -> &ReleaseDigest {
        self.signature_verified.receipt().incumbent_receipt_digest()
    }

    pub fn candidate_manifest_digest(&self) -> &ReleaseDigest {
        self.signature_verified.receipt().candidate_manifest_digest()
    }

    pub fn incumbent_manifest_digest(&self) -> &ReleaseDigest {
        self.signature_verified.receipt().incumbent_manifest_digest()
    }

    pub fn restored_artifact_set_digest(&self) -> &ReleaseDigest {
        self.signature_verified.receipt().restored_artifact_set_digest()
    }

    pub fn scope(&self) -> &ReleaseReceiptScope {
        self.signature_verified.receipt().scope()
    }

    pub fn policy(&self) -> &ReleasePromotionPolicy {
        self.signature_verified.receipt().policy()
    }

    pub fn drill_duration_ms(&self) -> u64 {
        self.signature_verified.receipt().time_window.elapsed_ms
    }

    pub fn receipt(&self) -> &RollbackDrillReceipt {
        self.signature_verified.receipt()
    }
}

/// A content-addressed artifact store that never accepts a caller-provided file
/// path.  Every object is addressed solely by a validated SHA-256 digest.
#[derive(Debug, Clone)]
pub struct ContentAddressedArtifactStore {
    root: PathBuf,
}

impl ContentAddressedArtifactStore {
    /// Open an existing, non-symlink artifact directory.
    pub fn open(root: impl Into<PathBuf>) -> ReleaseOperationsResult<Self> {
        let root = root.into();
        ensure_safe_directory(&root)?;
        Ok(Self { root })
    }

    /// Persist one object under its SHA-256 content digest.  Repeating the same
    /// bytes is idempotent; a conflicting or symlinked existing object fails.
    pub fn put(&self, bytes: &[u8]) -> ReleaseOperationsResult<ReleaseDigest> {
        if bytes.len() > MAX_ARTIFACT_BYTES {
            return Err(ReleaseOperationsError::ArtifactTooLarge);
        }
        let digest = digest_bytes(bytes)?;
        let destination = self.object_path(&digest);
        match fs::symlink_metadata(&destination) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_file() {
                    return Err(ReleaseOperationsError::UnsafeFilesystemPath(
                        destination.display().to_string(),
                    ));
                }
                let existing = read_regular_file(&destination)?;
                if digest_bytes(&existing)? != digest {
                    return Err(ReleaseOperationsError::ArtifactDigestMismatch(digest));
                }
                return Ok(digest);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error("reading artifact metadata", error)),
        }

        let temporary = self.root.join(format!(".artifact-{}.tmp", Uuid::new_v4()));
        write_new_file(&temporary, bytes)?;
        match fs::rename(&temporary, &destination) {
            Ok(()) => sync_directory(&self.root)?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&temporary);
            }
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(io_error("publishing content-addressed artifact", error));
            }
        }
        let restored = self.read_verified(&digest)?;
        if restored != bytes {
            return Err(ReleaseOperationsError::ArtifactDigestMismatch(digest));
        }
        Ok(digest)
    }

    fn object_path(&self, digest: &ReleaseDigest) -> PathBuf {
        self.root.join(digest.to_string())
    }

    fn read_verified(&self, digest: &ReleaseDigest) -> ReleaseOperationsResult<Vec<u8>> {
        let path = self.object_path(digest);
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Err(ReleaseOperationsError::MissingArtifact(digest.clone()));
            }
            Err(error) => return Err(io_error("reading artifact metadata", error)),
        };
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(ReleaseOperationsError::UnsafeFilesystemPath(
                path.display().to_string(),
            ));
        }
        let bytes = read_regular_file(&path)?;
        if digest_bytes(&bytes)? != *digest {
            return Err(ReleaseOperationsError::ArtifactDigestMismatch(digest.clone()));
        }
        Ok(bytes)
    }
}

/// Per-process companions to the release-root SQLite lock.  The SQLite
/// transaction coordinates independent processes; this map prevents needless
/// lock contention and makes ordering explicit for managers in one process.
static RELEASE_ROOT_PROCESS_LOCKS: LazyLock<Mutex<BTreeMap<PathBuf, Arc<Mutex<()>>>>> =
    LazyLock::new(|| Mutex::new(BTreeMap::new()));

fn release_root_lock_path(release_root: &Path) -> PathBuf {
    release_root.join(RELEASE_ROOT_LOCK_FILENAME)
}

fn process_release_root_lock(release_root: &Path) -> ReleaseOperationsResult<Arc<Mutex<()>>> {
    let mut locks = RELEASE_ROOT_PROCESS_LOCKS
        .lock()
        .map_err(|_| ReleaseOperationsError::InvalidEvidence("release-root lock registry"))?;
    Ok(locks
        .entry(release_root.to_path_buf())
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone())
}

/// Cross-process exclusive transaction rooted beside the release pointer.
///
/// SQLite clears this operating-system lock when a process dies, so a reopened
/// manager cannot be blocked by a stale create-new lock file.
struct ReleaseRootLock {
    connection: Connection,
}

impl ReleaseRootLock {
    fn acquire(path: &Path) -> ReleaseOperationsResult<Self> {
        ensure_safe_file_if_exists(path)?;
        let connection = Connection::open(path)?;
        ensure_safe_file_if_exists(path)?;
        connection.busy_timeout(RELEASE_ROOT_LOCK_TIMEOUT)?;
        connection.execute_batch("BEGIN EXCLUSIVE;")?;
        Ok(Self { connection })
    }
}

impl Drop for ReleaseRootLock {
    fn drop(&mut self) {
        let _ = self.connection.execute_batch("ROLLBACK;");
    }
}

/// File-backed rollback executor.  It restores only verified signed manifests,
/// stages all files below its release root, and atomically swaps `current.json`.
/// It never invokes a shell, process, service manager, or arbitrary command.
pub struct RollbackManager {
    artifact_store: ContentAddressedArtifactStore,
    release_root: PathBuf,
    releases_root: PathBuf,
    pointer_path: PathBuf,
    operation_lock_path: PathBuf,
    in_process_lock: Arc<Mutex<()>>,
    clock: Mutex<Box<dyn SoakClock>>,
    ledger: Mutex<Connection>,
}

impl RollbackManager {
    /// Open a manager over existing non-symlink roots and a SQLite receipt
    /// ledger.  The ledger contains canonical signed receipt bytes only after a
    /// completed pointer swap, and it never updates or deletes prior rows.
    pub fn open(
        artifact_store: ContentAddressedArtifactStore,
        release_root: impl Into<PathBuf>,
        ledger_path: impl Into<PathBuf>,
    ) -> ReleaseOperationsResult<Self> {
        Self::open_with_clock(
            artifact_store,
            release_root.into(),
            ledger_path.into(),
            Box::new(SystemSoakClock::new()),
        )
    }

    fn open_with_clock(
        artifact_store: ContentAddressedArtifactStore,
        release_root: PathBuf,
        ledger_path: PathBuf,
        clock: Box<dyn SoakClock>,
    ) -> ReleaseOperationsResult<Self> {
        ensure_safe_directory(&release_root)?;
        let release_root = fs::canonicalize(&release_root)
            .map_err(|error| io_error("canonicalizing release root", error))?;
        ensure_safe_directory(&release_root)?;
        let releases_root = release_root.join("releases");
        ensure_safe_child_directory(&release_root, &releases_root)?;
        let pointer_path = release_root.join("current.json");
        ensure_safe_file_if_exists(&pointer_path)?;
        let operation_lock_path = release_root_lock_path(&release_root);
        ensure_safe_file_if_exists(&operation_lock_path)?;
        let in_process_lock = process_release_root_lock(&release_root)?;

        let parent = ledger_path.parent().ok_or(ReleaseOperationsError::UnsafeFilesystemPath(
            ledger_path.display().to_string(),
        ))?;
        ensure_safe_directory(parent)?;
        ensure_safe_file_if_exists(&ledger_path)?;
        let ledger = Connection::open(&ledger_path)?;
        ledger.execute_batch(
            "PRAGMA foreign_keys = ON;
             PRAGMA journal_mode = WAL;
             PRAGMA synchronous = FULL;
             CREATE TABLE IF NOT EXISTS release_rollback_drill_receipts (
                 drill_id TEXT PRIMARY KEY NOT NULL,
                 request_digest TEXT NOT NULL,
                 receipt_digest TEXT NOT NULL,
                 canonical_envelope BLOB NOT NULL
             ) STRICT;",
        )?;
        Ok(Self {
            artifact_store,
            release_root,
            releases_root,
            pointer_path,
            operation_lock_path,
            in_process_lock,
            clock: Mutex::new(clock),
            ledger: Mutex::new(ledger),
        })
    }

    /// Restore the incumbent manifest after binding every source to verified,
    /// signed evidence.  On all pre-swap failures the candidate pointer remains
    /// unchanged; receipt-ledger failure after the swap triggers a best-effort
    /// atomic restoration of the candidate pointer before returning an error.
    #[allow(clippy::too_many_arguments)]
    pub fn execute_drill(
        &self,
        candidate_receipt: &VerifiedReleaseCandidateReceipt,
        incumbent_receipt: &VerifiedReleaseCandidateReceipt,
        candidate_manifest: &VerifiedRollbackManifest,
        incumbent_manifest: &VerifiedRollbackManifest,
        request: &RollbackDrillRequest,
        signing_key: &SigningKey,
        key_id: ReleaseSigningKeyId,
        expected_scope: &ReleaseReceiptScope,
        trust_policy: &dyn ReleaseOperationsTrustPolicy,
    ) -> ReleaseOperationsResult<VerifiedRollbackDrillReceipt> {
        // Start before either lock acquisition: contention, staging, every
        // verification, the pointer CAS, and durable receipt persistence all
        // consume the strict rollback-drill budget.
        let started_at = self.operation_timestamp()?;
        let _in_process_lock = self
            .in_process_lock
            .lock()
            .map_err(|_| ReleaseOperationsError::InvalidEvidence("rollback in-process lock"))?;
        let _release_root_lock = ReleaseRootLock::acquire(&self.operation_lock_path)?;
        validate_source_binding(
            expected_scope,
            candidate_receipt,
            incumbent_receipt,
            candidate_manifest,
            incumbent_manifest,
        )?;
        let request_digest = rollback_request_digest(
            request.drill_id(),
            candidate_receipt.receipt().receipt_digest(),
            incumbent_receipt.receipt().receipt_digest(),
            candidate_manifest.manifest_digest(),
            incumbent_manifest.manifest_digest(),
            incumbent_manifest.artifact_set_digest(),
            expected_scope,
            candidate_receipt.receipt().policy(),
        )?;

        if let Some(existing) = self.load_existing(request.drill_id(), &request_digest)? {
            if self.read_current_pointer()? != *incumbent_manifest.manifest_digest() {
                return Err(ReleaseOperationsError::PointerChanged);
            }
            return verify_bound_rollback_drill_receipt(
                &existing,
                expected_scope,
                trust_policy,
                candidate_receipt,
                incumbent_receipt,
                candidate_manifest,
                incumbent_manifest,
            );
        }

        if self.read_current_pointer()? != *candidate_manifest.manifest_digest() {
            return Err(ReleaseOperationsError::PointerNotCandidate);
        }
        self.stage_verified_manifest(incumbent_manifest)?;
        self.swap_pointer_if_current(
            candidate_manifest.manifest_digest(),
            incumbent_manifest.manifest_digest(),
        )?;
        let ended_at = self.operation_timestamp()?;
        let time_window = match MonotonicWallTimeWindow::new(started_at, ended_at) {
            Ok(window) => window,
            Err(error) => {
                self.restore_candidate_pointer(candidate_manifest, incumbent_manifest)?;
                return Err(error);
            }
        };
        if time_window.elapsed_ms() >= MAXIMUM_ROLLBACK_DRILL_DURATION_MS {
            self.restore_candidate_pointer(candidate_manifest, incumbent_manifest)?;
            return Err(ReleaseOperationsError::IneligibleEvidence(
                "rollback drill duration",
            ));
        }
        let input = RollbackDrillReceiptInput::new(
            request.drill_id.clone(),
            candidate_receipt.receipt().receipt_digest().clone(),
            incumbent_receipt.receipt().receipt_digest().clone(),
            candidate_manifest.manifest_digest().clone(),
            incumbent_manifest.manifest_digest().clone(),
            incumbent_manifest.artifact_set_digest().clone(),
            expected_scope.clone(),
            candidate_receipt.receipt().policy().clone(),
            time_window,
            RollbackVerificationResult::Verified,
        )?;
        let envelope = match sign_rollback_drill_receipt(input, signing_key, key_id) {
            Ok(envelope) => envelope,
            Err(error) => {
                self.restore_candidate_pointer(candidate_manifest, incumbent_manifest)?;
                return Err(error);
            }
        };
        let signature_verified = match verify_signed_rollback_drill_receipt(
            &envelope,
            expected_scope,
            trust_policy,
        ) {
            Ok(verified) => verified,
            Err(error) => {
                self.restore_candidate_pointer(candidate_manifest, incumbent_manifest)?;
                return Err(error);
            }
        };
        if let Err(error) = validate_drill_binding(
            signature_verified.receipt(),
            expected_scope,
            candidate_receipt,
            incumbent_receipt,
            candidate_manifest,
            incumbent_manifest,
        ) {
            self.restore_candidate_pointer(candidate_manifest, incumbent_manifest)?;
            return Err(error);
        }

        let verified = match verify_bound_rollback_drill_receipt(
            &envelope,
            expected_scope,
            trust_policy,
            candidate_receipt,
            incumbent_receipt,
            candidate_manifest,
            incumbent_manifest,
        ) {
            Ok(verified) => verified,
            Err(error) => {
                self.restore_candidate_pointer(candidate_manifest, incumbent_manifest)?;
                return Err(error);
            }
        };
        if let Err(error) = self.store_completed(
            request.drill_id(),
            &request_digest,
            &envelope,
        ) {
            self.restore_candidate_pointer(candidate_manifest, incumbent_manifest)?;
            return Err(error);
        }
        let completed_at = self.operation_timestamp()?;
        let completed_window = MonotonicWallTimeWindow::new(started_at, completed_at)?;
        if completed_window.elapsed_ms() >= MAXIMUM_ROLLBACK_DRILL_DURATION_MS {
            self.restore_candidate_pointer(candidate_manifest, incumbent_manifest)?;
            return Err(ReleaseOperationsError::IneligibleEvidence(
                "rollback drill duration",
            ));
        }
        Ok(verified)
    }

    fn operation_timestamp(&self) -> ReleaseOperationsResult<MonotonicWallTimestamp> {
        self.clock
            .lock()
            .map_err(|_| ReleaseOperationsError::InvalidEvidence("rollback clock lock"))?
            .now()
    }

    fn restore_candidate_pointer(
        &self,
        candidate_manifest: &VerifiedRollbackManifest,
        incumbent_manifest: &VerifiedRollbackManifest,
    ) -> ReleaseOperationsResult<()> {
        self.swap_pointer_if_current(
            incumbent_manifest.manifest_digest(),
            candidate_manifest.manifest_digest(),
        )
        .map_err(|_| ReleaseOperationsError::PointerChanged)
    }

    fn load_existing(
        &self,
        drill_id: &RollbackDrillId,
        request_digest: &ReleaseDigest,
    ) -> ReleaseOperationsResult<Option<RollbackDrillReceiptEnvelope>> {
        let connection = self
            .ledger
            .lock()
            .map_err(|_| ReleaseOperationsError::InvalidEvidence("rollback receipt ledger lock"))?;
        let stored: Option<(String, String, Vec<u8>)> = connection
            .query_row(
                "SELECT request_digest, receipt_digest, canonical_envelope
                 FROM release_rollback_drill_receipts WHERE drill_id = ?1",
                params![drill_id.as_str()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((stored_request_digest, stored_receipt_digest, bytes)) = stored else {
            return Ok(None);
        };
        if stored_request_digest != request_digest.to_string() {
            return Err(ReleaseOperationsError::IdempotencyConflict);
        }
        let envelope = RollbackDrillReceiptEnvelope::parse_canonical_bytes(&bytes)?;
        let stored_receipt_digest = ReleaseDigest::new(stored_receipt_digest)
            .map_err(|_| ReleaseOperationsError::InvalidEvidence("stored receipt digest"))?;
        if envelope.receipt_digest()? != stored_receipt_digest {
            return Err(ReleaseOperationsError::InconsistentBinding(
                "stored rollback receipt digest",
            ));
        }
        Ok(Some(envelope))
    }


    fn store_completed(
        &self,
        drill_id: &RollbackDrillId,
        request_digest: &ReleaseDigest,
        envelope: &RollbackDrillReceiptEnvelope,
    ) -> ReleaseOperationsResult<()> {
        let bytes = envelope.canonical_bytes()?;
        let receipt_digest = envelope.receipt_digest()?;
        let connection = self
            .ledger
            .lock()
            .map_err(|_| ReleaseOperationsError::InvalidEvidence("rollback receipt ledger lock"))?;
        let inserted = connection.execute(
            "INSERT INTO release_rollback_drill_receipts
                (drill_id, request_digest, receipt_digest, canonical_envelope)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(drill_id) DO NOTHING",
            params![
                drill_id.as_str(),
                request_digest.to_string(),
                receipt_digest.to_string(),
                bytes,
            ],
        )?;
        if inserted == 0 {
            let stored_request: String = connection.query_row(
                "SELECT request_digest FROM release_rollback_drill_receipts WHERE drill_id = ?1",
                params![drill_id.as_str()],
                |row| row.get(0),
            )?;
            if stored_request != request_digest.to_string() {
                return Err(ReleaseOperationsError::IdempotencyConflict);
            }
            let stored_bytes: Vec<u8> = connection.query_row(
                "SELECT canonical_envelope FROM release_rollback_drill_receipts WHERE drill_id = ?1",
                params![drill_id.as_str()],
                |row| row.get(0),
            )?;
            if stored_bytes != envelope.canonical_bytes()? {
                return Err(ReleaseOperationsError::IdempotencyConflict);
            }
        }
        Ok(())
    }

    fn stage_verified_manifest(&self, manifest: &VerifiedRollbackManifest) -> ReleaseOperationsResult<()> {
        let final_path = self.releases_root.join(manifest.manifest_digest().to_string());
        match fs::symlink_metadata(&final_path) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(ReleaseOperationsError::UnsafeFilesystemPath(
                        final_path.display().to_string(),
                    ));
                }
                return verify_release_tree(&final_path, manifest.artifacts());
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io_error("reading staged release metadata", error)),
        }

        let staging_path = self
            .releases_root
            .join(format!(".staging-{}", Uuid::new_v4()));
        fs::create_dir(&staging_path).map_err(|error| io_error("creating rollback staging directory", error))?;
        let staged = StagedDirectory::new(staging_path.clone());
        for (path, digest) in manifest.artifacts() {
            let contents = self.artifact_store.read_verified(digest)?;
            let destination = path.join_to(&staging_path);
            let parent = destination.parent().ok_or_else(|| {
                ReleaseOperationsError::UnsafeFilesystemPath(destination.display().to_string())
            })?;
            ensure_safe_staging_parents(&staging_path, parent)?;
            write_new_file(&destination, &contents)?;
        }
        verify_release_tree(&staging_path, manifest.artifacts())?;
        match fs::rename(&staging_path, &final_path) {
            Ok(()) => {
                staged.disarm();
                sync_directory(&self.releases_root)?;
                Ok(())
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                verify_release_tree(&final_path, manifest.artifacts())
            }
            Err(error) => Err(io_error("publishing staged rollback release", error)),
        }
    }

    fn read_current_pointer(&self) -> ReleaseOperationsResult<ReleaseDigest> {
        read_release_pointer_at(&self.release_root)
    }

    /// Compare and atomically replace the single release pointer while the
    /// caller holds both release-root locks.  The re-read immediately before
    /// rename makes failed-drill restoration conditional on this operation's
    /// incumbent value rather than an unconditional overwrite.
    fn swap_pointer_if_current(
        &self,
        expected_manifest_digest: &ReleaseDigest,
        next_manifest_digest: &ReleaseDigest,
    ) -> ReleaseOperationsResult<()> {
        if self.read_current_pointer()? != *expected_manifest_digest {
            return Err(ReleaseOperationsError::PointerChanged);
        }
        self.write_pointer(next_manifest_digest)
    }

    fn write_pointer(&self, manifest_digest: &ReleaseDigest) -> ReleaseOperationsResult<()> {
        ensure_safe_file_if_exists(&self.pointer_path)?;
        let bytes = canonical_json(&ReleasePointerWire {
            schema: RELEASE_POINTER_SCHEMA.to_owned(),
            version: RELEASE_POINTER_VERSION,
            manifest_digest: manifest_digest.to_string(),
        })?;
        let temporary = self
            .release_root
            .join(format!(".current-{}.tmp", Uuid::new_v4()));
        write_new_file(&temporary, &bytes)?;
        if let Err(error) = fs::rename(&temporary, &self.pointer_path) {
            let _ = fs::remove_file(&temporary);
            return Err(io_error("atomically swapping release pointer", error));
        }
        sync_directory(&self.release_root)
    }
}

fn read_release_pointer_at(release_root: &Path) -> ReleaseOperationsResult<ReleaseDigest> {
    let pointer_path = release_root.join("current.json");
    let metadata = fs::symlink_metadata(&pointer_path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            ReleaseOperationsError::MissingReleasePointer
        } else {
            io_error("reading release pointer metadata", error)
        }
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ReleaseOperationsError::UnsafeFilesystemPath(
            pointer_path.display().to_string(),
        ));
    }
    if metadata.len() > 4096 {
        return Err(ReleaseOperationsError::InvalidReleasePointer);
    }
    let bytes = read_regular_file(&pointer_path)?;
    let wire: ReleasePointerWire =
        serde_json::from_slice(&bytes).map_err(|_| ReleaseOperationsError::InvalidReleasePointer)?;
    if canonical_json(&wire)? != bytes {
        return Err(ReleaseOperationsError::NonCanonicalEnvelope);
    }
    if wire.schema != RELEASE_POINTER_SCHEMA || wire.version != RELEASE_POINTER_VERSION {
        return Err(ReleaseOperationsError::InvalidReleasePointer);
    }
    ReleaseDigest::new(wire.manifest_digest)
        .map_err(|_| ReleaseOperationsError::InvalidReleasePointer)
}

#[derive(Debug)]
struct StagedDirectory {
    path: PathBuf,
    active: bool,
}

impl StagedDirectory {
    fn new(path: PathBuf) -> Self {
        Self { path, active: true }
    }

    fn disarm(mut self) {
        self.active = false;
    }
}

impl Drop for StagedDirectory {
    fn drop(&mut self) {
        if self.active {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EnvelopeHeader {
    schema: String,
    schema_version: u32,
    signature_algorithm: String,
    key_id: ReleaseSigningKeyId,
}

impl EnvelopeHeader {
    fn new(schema: &str, key_id: ReleaseSigningKeyId) -> Self {
        Self {
            schema: schema.to_owned(),
            schema_version: RELEASE_OPERATION_SCHEMA_VERSION,
            signature_algorithm: RELEASE_OPERATION_SIGNATURE_ALGORITHM.to_owned(),
            key_id,
        }
    }

    fn validate(&self, expected_schema: &str) -> ReleaseOperationsResult<()> {
        if self.schema != expected_schema || self.schema_version != RELEASE_OPERATION_SCHEMA_VERSION {
            return Err(ReleaseOperationsError::UnsupportedSchema {
                schema: self.schema.clone(),
                version: self.schema_version,
            });
        }
        if self.signature_algorithm != RELEASE_OPERATION_SIGNATURE_ALGORITHM {
            return Err(ReleaseOperationsError::UnsupportedSignatureAlgorithm(
                self.signature_algorithm.clone(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvelopeHeaderWire {
    schema: String,
    schema_version: u32,
    signature_algorithm: String,
    key_id: String,
}

impl From<&EnvelopeHeader> for EnvelopeHeaderWire {
    fn from(header: &EnvelopeHeader) -> Self {
        Self {
            schema: header.schema.clone(),
            schema_version: header.schema_version,
            signature_algorithm: header.signature_algorithm.clone(),
            key_id: header.key_id.to_string(),
        }
    }
}

impl TryFrom<EnvelopeHeaderWire> for EnvelopeHeader {
    type Error = ReleaseOperationsError;

    fn try_from(wire: EnvelopeHeaderWire) -> Result<Self, Self::Error> {
        Ok(Self {
            schema: wire.schema,
            schema_version: wire.schema_version,
            signature_algorithm: wire.signature_algorithm,
            key_id: ReleaseSigningKeyId::new(wire.key_id)
                .map_err(|_| ReleaseOperationsError::InvalidEvidence("signing key ID"))?,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeWire {
    tenant_id: String,
    authenticated_subject: String,
}

impl From<&ReleaseReceiptScope> for ScopeWire {
    fn from(scope: &ReleaseReceiptScope) -> Self {
        Self {
            tenant_id: scope.tenant_id().to_string(),
            authenticated_subject: scope.authenticated_subject().to_string(),
        }
    }
}

impl TryFrom<ScopeWire> for ReleaseReceiptScope {
    type Error = ReleaseOperationsError;

    fn try_from(wire: ScopeWire) -> Result<Self, Self::Error> {
        let tenant_id = ReleaseTenantId::new(wire.tenant_id)
            .map_err(|_| ReleaseOperationsError::InvalidEvidence("tenant ID"))?;
        let authenticated_subject = AuthenticatedSubject::new(wire.authenticated_subject)
            .map_err(|_| ReleaseOperationsError::InvalidEvidence("authenticated subject"))?;
        Ok(Self::new(tenant_id, authenticated_subject))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PolicyWire {
    policy_id: String,
    policy_digest: String,
}

impl From<&ReleasePromotionPolicy> for PolicyWire {
    fn from(policy: &ReleasePromotionPolicy) -> Self {
        Self {
            policy_id: policy.policy_id().to_string(),
            policy_digest: policy.policy_digest().to_string(),
        }
    }
}

impl TryFrom<PolicyWire> for ReleasePromotionPolicy {
    type Error = ReleaseOperationsError;

    fn try_from(wire: PolicyWire) -> Result<Self, Self::Error> {
        let policy_id = ReleasePolicyId::new(wire.policy_id)
            .map_err(|_| ReleaseOperationsError::InvalidEvidence("policy ID"))?;
        let policy = Self::new(policy_id)
            .map_err(|_| ReleaseOperationsError::InvalidEvidence("frozen promotion policy"))?;
        let claimed_digest = parse_release_digest(wire.policy_digest, "policy digest")?;
        if policy.policy_digest() != &claimed_digest {
            return Err(ReleaseOperationsError::InconsistentBinding("policy digest"));
        }
        Ok(policy)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SoakReceiptWire {
    candidate_receipt_digest: String,
    scope: ScopeWire,
    policy: PolicyWire,
    time_window: MonotonicWallTimeWindow,
    event_loss_count: u64,
    unreconciled_effect_count: u64,
    canary_health: CanaryHealth,
    exact_run_proof_digests: Vec<String>,
}

impl From<&SoakReceipt> for SoakReceiptWire {
    fn from(receipt: &SoakReceipt) -> Self {
        Self {
            candidate_receipt_digest: receipt.candidate_receipt_digest.to_string(),
            scope: ScopeWire::from(&receipt.scope),
            policy: PolicyWire::from(&receipt.policy),
            time_window: receipt.time_window.clone(),
            event_loss_count: receipt.event_loss_count,
            unreconciled_effect_count: receipt.unreconciled_effect_count,
            canary_health: receipt.canary_health,
            exact_run_proof_digests: receipt
                .exact_run_proof_digests
                .iter()
                .map(ToString::to_string)
                .collect(),
        }
    }
}

impl TryFrom<SoakReceiptWire> for SoakReceipt {
    type Error = ReleaseOperationsError;

    fn try_from(wire: SoakReceiptWire) -> Result<Self, Self::Error> {
        let mut exact_run_proof_digests = Vec::new();
        for digest in wire.exact_run_proof_digests {
            exact_run_proof_digests.push(parse_release_digest(
                digest,
                "exact run-proof digest",
            )?);
        }
        Ok(SoakReceiptInput::new(
            parse_release_digest(wire.candidate_receipt_digest, "candidate receipt digest")?,
            ReleaseReceiptScope::try_from(wire.scope)?,
            ReleasePromotionPolicy::try_from(wire.policy)?,
            wire.time_window,
            wire.event_loss_count,
            wire.unreconciled_effect_count,
            wire.canary_health,
            exact_run_proof_digests,
        )?
        .into())
    }
}

impl Serialize for SoakReceipt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        SoakReceiptWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SoakReceipt {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = SoakReceiptWire::deserialize(deserializer)?;
        Self::try_from(wire).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RollbackManifestWire {
    release_receipt_digest: String,
    scope: ScopeWire,
    policy: PolicyWire,
    artifacts: BTreeMap<String, String>,
    artifact_set_digest: String,
}

impl From<&RollbackManifest> for RollbackManifestWire {
    fn from(manifest: &RollbackManifest) -> Self {
        Self {
            release_receipt_digest: manifest.release_receipt_digest.to_string(),
            scope: ScopeWire::from(&manifest.scope),
            policy: PolicyWire::from(&manifest.policy),
            artifacts: artifact_map_wire(&manifest.artifacts),
            artifact_set_digest: manifest.artifact_set_digest.to_string(),
        }
    }
}

impl TryFrom<RollbackManifestWire> for RollbackManifest {
    type Error = ReleaseOperationsError;

    fn try_from(wire: RollbackManifestWire) -> Result<Self, Self::Error> {
        let mut artifacts = BTreeMap::new();
        for (path, digest) in wire.artifacts {
            let path = RollbackArtifactPath::new(path)?;
            let digest = parse_release_digest(digest, "rollback artifact digest")?;
            if artifacts.insert(path, digest).is_some() {
                return Err(ReleaseOperationsError::InvalidEvidence(
                    "duplicate rollback artifact path",
                ));
            }
        }
        let manifest = RollbackManifest::from_input(RollbackManifestInput::new(
            parse_release_digest(wire.release_receipt_digest, "release receipt digest")?,
            ReleaseReceiptScope::try_from(wire.scope)?,
            ReleasePromotionPolicy::try_from(wire.policy)?,
            artifacts,
        )?)?;
        if manifest.artifact_set_digest.to_string() != wire.artifact_set_digest {
            return Err(ReleaseOperationsError::InconsistentBinding(
                "rollback artifact-set digest",
            ));
        }
        Ok(manifest)
    }
}

impl Serialize for RollbackManifest {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        RollbackManifestWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RollbackManifest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = RollbackManifestWire::deserialize(deserializer)?;
        Self::try_from(wire).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RollbackDrillReceiptWire {
    drill_id: String,
    candidate_receipt_digest: String,
    incumbent_receipt_digest: String,
    candidate_manifest_digest: String,
    incumbent_manifest_digest: String,
    restored_artifact_set_digest: String,
    scope: ScopeWire,
    policy: PolicyWire,
    time_window: MonotonicWallTimeWindow,
    verification_result: RollbackVerificationResult,
}

impl From<&RollbackDrillReceipt> for RollbackDrillReceiptWire {
    fn from(receipt: &RollbackDrillReceipt) -> Self {
        Self {
            drill_id: receipt.drill_id.as_str().to_owned(),
            candidate_receipt_digest: receipt.candidate_receipt_digest.to_string(),
            incumbent_receipt_digest: receipt.incumbent_receipt_digest.to_string(),
            candidate_manifest_digest: receipt.candidate_manifest_digest.to_string(),
            incumbent_manifest_digest: receipt.incumbent_manifest_digest.to_string(),
            restored_artifact_set_digest: receipt.restored_artifact_set_digest.to_string(),
            scope: ScopeWire::from(&receipt.scope),
            policy: PolicyWire::from(&receipt.policy),
            time_window: receipt.time_window.clone(),
            verification_result: receipt.verification_result,
        }
    }
}

impl TryFrom<RollbackDrillReceiptWire> for RollbackDrillReceipt {
    type Error = ReleaseOperationsError;

    fn try_from(wire: RollbackDrillReceiptWire) -> Result<Self, Self::Error> {
        Ok(RollbackDrillReceiptInput::new(
            RollbackDrillId::new(wire.drill_id)?,
            parse_release_digest(wire.candidate_receipt_digest, "candidate receipt digest")?,
            parse_release_digest(wire.incumbent_receipt_digest, "incumbent receipt digest")?,
            parse_release_digest(wire.candidate_manifest_digest, "candidate manifest digest")?,
            parse_release_digest(wire.incumbent_manifest_digest, "incumbent manifest digest")?,
            parse_release_digest(
                wire.restored_artifact_set_digest,
                "restored artifact-set digest",
            )?,
            ReleaseReceiptScope::try_from(wire.scope)?,
            ReleasePromotionPolicy::try_from(wire.policy)?,
            wire.time_window,
            wire.verification_result,
        )?
        .into())
    }
}

impl Serialize for RollbackDrillReceipt {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        RollbackDrillReceiptWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for RollbackDrillReceipt {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = RollbackDrillReceiptWire::deserialize(deserializer)?;
        Self::try_from(wire).map_err(serde::de::Error::custom)
    }
}

impl Serialize for EnvelopeHeader {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        EnvelopeHeaderWire::from(self).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for EnvelopeHeader {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = EnvelopeHeaderWire::deserialize(deserializer)?;
        Self::try_from(wire).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedEnvelopeWire<T> {
    schema: String,
    schema_version: u32,
    signature_algorithm: String,
    key_id: String,
    payload: T,
    signature: String,
}

impl<T> SignedEnvelopeWire<T> {
    fn from_parts(header: &EnvelopeHeader, payload: T, signature: String) -> Self {
        Self {
            schema: header.schema.clone(),
            schema_version: header.schema_version,
            signature_algorithm: header.signature_algorithm.clone(),
            key_id: header.key_id.to_string(),
            payload,
            signature,
        }
    }

    fn into_header(self) -> ReleaseOperationsResult<(EnvelopeHeader, T, String)> {
        let header = EnvelopeHeader::try_from(EnvelopeHeaderWire {
            schema: self.schema,
            schema_version: self.schema_version,
            signature_algorithm: self.signature_algorithm,
            key_id: self.key_id,
        })?;
        Ok((header, self.payload, self.signature))
    }
}

#[derive(Serialize)]
struct SignaturePreimage<'a, T: Serialize> {
    schema: &'a str,
    schema_version: u32,
    signature_algorithm: &'a str,
    key_id: &'a str,
    payload: &'a T,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReleasePointerWire {
    schema: String,
    version: u32,
    manifest_digest: String,
}

fn canonical_json(value: &impl Serialize) -> ReleaseOperationsResult<Vec<u8>> {
    serde_json_canonicalizer::to_vec(value)
        .map_err(|error| ReleaseOperationsError::Canonicalization(error.to_string()))
}

fn parse_canonical_envelope<T>(bytes: &[u8]) -> ReleaseOperationsResult<SignedEnvelopeWire<T>>
where
    T: for<'de> Deserialize<'de> + Serialize,
{
    if bytes.len() > MAX_ENVELOPE_BYTES {
        return Err(ReleaseOperationsError::EnvelopeTooLarge);
    }
    let wire: SignedEnvelopeWire<T> = serde_json::from_slice(bytes)
        .map_err(|error| ReleaseOperationsError::InvalidEnvelope(error.to_string()))?;
    if canonical_json(&wire)? != bytes {
        return Err(ReleaseOperationsError::NonCanonicalEnvelope);
    }
    Ok(wire)
}

fn signing_preimage<T: Serialize>(
    domain: &[u8],
    header: &EnvelopeHeader,
    payload: &T,
) -> ReleaseOperationsResult<Vec<u8>> {
    let canonical = canonical_json(&SignaturePreimage {
        schema: &header.schema,
        schema_version: header.schema_version,
        signature_algorithm: &header.signature_algorithm,
        key_id: header.key_id.as_str(),
        payload,
    })?;
    let mut preimage = Vec::with_capacity(domain.len() + canonical.len());
    preimage.extend_from_slice(domain);
    preimage.extend_from_slice(&canonical);
    Ok(preimage)
}

fn encode_signature(signature: Signature) -> String {
    base64::engine::general_purpose::STANDARD.encode(signature.to_bytes())
}

fn decode_signature(value: &str) -> ReleaseOperationsResult<Signature> {
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(value)
        .map_err(|_| ReleaseOperationsError::MalformedSignature)?;
    if base64::engine::general_purpose::STANDARD.encode(&bytes) != value {
        return Err(ReleaseOperationsError::MalformedSignature);
    }
    let signature: [u8; 64] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| ReleaseOperationsError::MalformedSignature)?;
    Ok(Signature::from_bytes(&signature))
}

fn verify_scope_policy(
    signed_scope: &ReleaseReceiptScope,
    signed_policy: &ReleasePromotionPolicy,
    expected_scope: &ReleaseReceiptScope,
    trust_policy: &dyn ReleaseOperationsTrustPolicy,
) -> ReleaseOperationsResult<()> {
    if signed_scope != expected_scope {
        return Err(ReleaseOperationsError::ScopeMismatch);
    }
    let expected_policy = trust_policy
        .expected_policy(expected_scope)
        .ok_or(ReleaseOperationsError::PolicyUnavailable)?;
    if signed_policy != expected_policy {
        return Err(ReleaseOperationsError::PolicyMismatch);
    }
    Ok(())
}

fn verify_signature<T: Serialize>(
    domain: &[u8],
    header: &EnvelopeHeader,
    payload: &T,
    signature: &str,
    scope: &ReleaseReceiptScope,
    trust_policy: &dyn ReleaseOperationsTrustPolicy,
) -> ReleaseOperationsResult<()> {
    let verifying_key = match trust_policy.resolve_verifying_key(scope, &header.key_id) {
        ReleaseTrustDecision::Trusted { verifying_key } => {
            VerifyingKey::from_bytes(&verifying_key)
                .map_err(|_| ReleaseOperationsError::MalformedTrustedKey)?
        }
        ReleaseTrustDecision::Denied => return Err(ReleaseOperationsError::UntrustedSigner),
    };
    verifying_key
        .verify_strict(&signing_preimage(domain, header, payload)?, &decode_signature(signature)?)
        .map_err(|_| ReleaseOperationsError::InvalidSignature)
}

fn digest_bytes(bytes: &[u8]) -> ReleaseOperationsResult<ReleaseDigest> {
    ReleaseDigest::new(hex::encode(Sha256::digest(bytes)))
        .map_err(|_| ReleaseOperationsError::InvalidEvidence("SHA-256 digest"))
}

fn parse_release_digest(
    value: String,
    field: &'static str,
) -> ReleaseOperationsResult<ReleaseDigest> {
    ReleaseDigest::new(value).map_err(|_| ReleaseOperationsError::InvalidEvidence(field))
}

fn artifact_map_wire(
    artifacts: &BTreeMap<RollbackArtifactPath, ReleaseDigest>,
) -> BTreeMap<String, String> {
    artifacts
        .iter()
        .map(|(path, digest)| (path.as_str().to_owned(), digest.to_string()))
        .collect()
}

fn artifact_set_digest(
    artifacts: &BTreeMap<RollbackArtifactPath, ReleaseDigest>,
) -> ReleaseOperationsResult<ReleaseDigest> {
    #[derive(Serialize)]
    struct ArtifactSet {
        artifacts: BTreeMap<String, String>,
    }
    digest_bytes(&canonical_json(&ArtifactSet {
        artifacts: artifact_map_wire(artifacts),
    })?)
}

fn rollback_request_digest(
    drill_id: &RollbackDrillId,
    candidate_receipt_digest: &ReleaseDigest,
    incumbent_receipt_digest: &ReleaseDigest,
    candidate_manifest_digest: &ReleaseDigest,
    incumbent_manifest_digest: &ReleaseDigest,
    restored_artifact_set_digest: &ReleaseDigest,
    scope: &ReleaseReceiptScope,
    policy: &ReleasePromotionPolicy,
) -> ReleaseOperationsResult<ReleaseDigest> {
    #[derive(Serialize)]
    struct Request {
        drill_id: String,
        candidate_receipt_digest: String,
        incumbent_receipt_digest: String,
        candidate_manifest_digest: String,
        incumbent_manifest_digest: String,
        restored_artifact_set_digest: String,
        scope: ScopeWire,
        policy: PolicyWire,
    }
    digest_bytes(&canonical_json(&Request {
        drill_id: drill_id.as_str().to_owned(),
        candidate_receipt_digest: candidate_receipt_digest.to_string(),
        incumbent_receipt_digest: incumbent_receipt_digest.to_string(),
        candidate_manifest_digest: candidate_manifest_digest.to_string(),
        incumbent_manifest_digest: incumbent_manifest_digest.to_string(),
        restored_artifact_set_digest: restored_artifact_set_digest.to_string(),
        scope: ScopeWire::from(scope),
        policy: PolicyWire::from(policy),
    })?)
}

fn validate_source_binding(
    expected_scope: &ReleaseReceiptScope,
    candidate_receipt: &VerifiedReleaseCandidateReceipt,
    incumbent_receipt: &VerifiedReleaseCandidateReceipt,
    candidate_manifest: &VerifiedRollbackManifest,
    incumbent_manifest: &VerifiedRollbackManifest,
) -> ReleaseOperationsResult<()> {
    if &candidate_receipt.receipt().scope() != expected_scope
        || &incumbent_receipt.receipt().scope() != expected_scope
        || candidate_manifest.scope() != expected_scope
        || incumbent_manifest.scope() != expected_scope
    {
        return Err(ReleaseOperationsError::ScopeMismatch);
    }
    if candidate_receipt.receipt().policy() != incumbent_receipt.receipt().policy()
        || candidate_receipt.receipt().policy() != candidate_manifest.policy()
        || candidate_receipt.receipt().policy() != incumbent_manifest.policy()
    {
        return Err(ReleaseOperationsError::PolicyMismatch);
    }
    if candidate_manifest.release_receipt_digest() != candidate_receipt.receipt().receipt_digest() {
        return Err(ReleaseOperationsError::InconsistentBinding(
            "candidate manifest receipt digest",
        ));
    }
    if incumbent_manifest.release_receipt_digest() != incumbent_receipt.receipt().receipt_digest() {
        return Err(ReleaseOperationsError::InconsistentBinding(
            "incumbent manifest receipt digest",
        ));
    }
    if candidate_receipt.receipt().receipt_digest() == incumbent_receipt.receipt().receipt_digest()
        || candidate_manifest.manifest_digest() == incumbent_manifest.manifest_digest()
    {
        return Err(ReleaseOperationsError::InconsistentBinding(
            "candidate and incumbent identity",
        ));
    }
    Ok(())
}

fn validate_drill_binding(
    receipt: &RollbackDrillReceipt,
    expected_scope: &ReleaseReceiptScope,
    candidate_receipt: &VerifiedReleaseCandidateReceipt,
    incumbent_receipt: &VerifiedReleaseCandidateReceipt,
    candidate_manifest: &VerifiedRollbackManifest,
    incumbent_manifest: &VerifiedRollbackManifest,
) -> ReleaseOperationsResult<()> {
    validate_source_binding(
        expected_scope,
        candidate_receipt,
        incumbent_receipt,
        candidate_manifest,
        incumbent_manifest,
    )?;
    if receipt.scope() != expected_scope
        || receipt.policy() != candidate_receipt.receipt().policy()
        || receipt.candidate_receipt_digest() != candidate_receipt.receipt().receipt_digest()
        || receipt.incumbent_receipt_digest() != incumbent_receipt.receipt().receipt_digest()
        || receipt.candidate_manifest_digest() != candidate_manifest.manifest_digest()
        || receipt.incumbent_manifest_digest() != incumbent_manifest.manifest_digest()
        || receipt.restored_artifact_set_digest() != incumbent_manifest.artifact_set_digest()
    {
        return Err(ReleaseOperationsError::InconsistentBinding(
            "signed rollback drill receipt",
        ));
    }
    Ok(())
}

fn ensure_safe_directory(path: &Path) -> ReleaseOperationsResult<()> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error("reading directory metadata", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(ReleaseOperationsError::UnsafeFilesystemPath(
            path.display().to_string(),
        ));
    }
    Ok(())
}

fn ensure_safe_child_directory(parent: &Path, child: &Path) -> ReleaseOperationsResult<()> {
    if child.parent() != Some(parent) {
        return Err(ReleaseOperationsError::UnsafeFilesystemPath(
            child.display().to_string(),
        ));
    }
    match fs::symlink_metadata(child) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(ReleaseOperationsError::UnsafeFilesystemPath(
                    child.display().to_string(),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            fs::create_dir(child).map_err(|error| io_error("creating release subdirectory", error))?;
            sync_directory(parent)?;
        }
        Err(error) => return Err(io_error("reading release subdirectory metadata", error)),
    }
    Ok(())
}

fn ensure_safe_file_if_exists(path: &Path) -> ReleaseOperationsResult<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_file() {
                return Err(ReleaseOperationsError::UnsafeFilesystemPath(
                    path.display().to_string(),
                ));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error("reading file metadata", error)),
    }
    Ok(())
}

fn ensure_safe_staging_parents(root: &Path, destination_parent: &Path) -> ReleaseOperationsResult<()> {
    let relative = destination_parent.strip_prefix(root).map_err(|_| {
        ReleaseOperationsError::UnsafeFilesystemPath(destination_parent.display().to_string())
    })?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(segment) = component else {
            return Err(ReleaseOperationsError::UnsafeFilesystemPath(
                destination_parent.display().to_string(),
            ));
        };
        current.push(segment);
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(ReleaseOperationsError::UnsafeFilesystemPath(
                        current.display().to_string(),
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir(&current).map_err(|error| io_error("creating staged artifact directory", error))?;
            }
            Err(error) => return Err(io_error("reading staged artifact metadata", error)),
        }
    }
    Ok(())
}

fn write_new_file(path: &Path, bytes: &[u8]) -> ReleaseOperationsResult<()> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| io_error("creating release file", error))?;
    file.write_all(bytes)
        .map_err(|error| io_error("writing release file", error))?;
    file.sync_all()
        .map_err(|error| io_error("syncing release file", error))
}

fn read_regular_file(path: &Path) -> ReleaseOperationsResult<Vec<u8>> {
    let metadata = fs::symlink_metadata(path).map_err(|error| io_error("reading file metadata", error))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(ReleaseOperationsError::UnsafeFilesystemPath(
            path.display().to_string(),
        ));
    }
    let length = usize::try_from(metadata.len()).map_err(|_| ReleaseOperationsError::ArtifactTooLarge)?;
    if length > MAX_ARTIFACT_BYTES {
        return Err(ReleaseOperationsError::ArtifactTooLarge);
    }
    fs::read(path).map_err(|error| io_error("reading release file", error))
}

fn verify_release_tree(
    root: &Path,
    expected: &BTreeMap<RollbackArtifactPath, ReleaseDigest>,
) -> ReleaseOperationsResult<()> {
    ensure_safe_directory(root)?;
    let mut actual = BTreeMap::new();
    collect_release_files(root, root, &mut actual)?;
    if actual != *expected {
        return Err(ReleaseOperationsError::InconsistentBinding(
            "staged rollback artifact tree",
        ));
    }
    Ok(())
}

fn collect_release_files(
    root: &Path,
    directory: &Path,
    actual: &mut BTreeMap<RollbackArtifactPath, ReleaseDigest>,
) -> ReleaseOperationsResult<()> {
    for entry in fs::read_dir(directory).map_err(|error| io_error("reading staged release directory", error))? {
        let entry = entry.map_err(|error| io_error("reading staged release entry", error))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .map_err(|error| io_error("reading staged release metadata", error))?;
        if metadata.file_type().is_symlink() {
            return Err(ReleaseOperationsError::UnsafeFilesystemPath(
                path.display().to_string(),
            ));
        }
        if metadata.is_dir() {
            collect_release_files(root, &path, actual)?;
        } else if metadata.is_file() {
            let relative = path.strip_prefix(root).map_err(|_| {
                ReleaseOperationsError::UnsafeFilesystemPath(path.display().to_string())
            })?;
            let relative = relative.to_str().ok_or_else(|| {
                ReleaseOperationsError::UnsafeFilesystemPath(path.display().to_string())
            })?;
            let artifact_path = RollbackArtifactPath::new(relative)?;
            let bytes = read_regular_file(&path)?;
            let digest = digest_bytes(&bytes)?;
            if actual.insert(artifact_path, digest).is_some() {
                return Err(ReleaseOperationsError::InconsistentBinding(
                    "duplicate staged artifact path",
                ));
            }
        } else {
            return Err(ReleaseOperationsError::UnsafeFilesystemPath(
                path.display().to_string(),
            ));
        }
    }
    Ok(())
}

fn sync_directory(path: &Path) -> ReleaseOperationsResult<()> {
    let directory = File::open(path).map_err(|error| io_error("opening release directory for sync", error))?;
    directory
        .sync_all()
        .map_err(|error| io_error("syncing release directory", error))
}

#[cfg(test)]
mod tests {
    use std::fs;

    use ed25519_dalek::SigningKey;
    use tempfile::TempDir;

    use super::*;
    use crate::release_candidate::{
        AuthenticatedSubject, ReleasePolicyId, ReleaseReceiptScope, ReleaseSigningKeyId,
        ReleaseTenantId, ReleaseTrustDecision,
    };

    #[derive(Clone)]
    struct TestTrust {
        scope: ReleaseReceiptScope,
        policy: ReleasePromotionPolicy,
        key_id: ReleaseSigningKeyId,
        verifying_key: [u8; 32],
    }

    impl ReleaseOperationsTrustPolicy for TestTrust {
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

    fn digest(byte: u8) -> ReleaseDigest {
        ReleaseDigest::new(format!("{byte:02x}").repeat(32)).expect("digest")
    }

    fn scope() -> ReleaseReceiptScope {
        ReleaseReceiptScope::new(
            ReleaseTenantId::new("tenant-a").expect("tenant"),
            AuthenticatedSubject::new("oidc:subject-a").expect("subject"),
        )
    }

    fn policy() -> ReleasePromotionPolicy {
        ReleasePromotionPolicy::new(ReleasePolicyId::new("p4").expect("policy")).expect("policy")
    }

    fn signing_key() -> SigningKey {
        SigningKey::from_bytes(&[7; 32])
    }

    fn trust(scope: ReleaseReceiptScope, policy: ReleasePromotionPolicy) -> TestTrust {
        let signing_key = signing_key();
        TestTrust {
            scope,
            policy,
            key_id: ReleaseSigningKeyId::new("release-key-a").expect("key ID"),
            verifying_key: signing_key.verifying_key().to_bytes(),
        }
    }

    fn soak_input(
        duration_ms: u64,
        event_loss_count: u64,
        unreconciled_effect_count: u64,
        health: CanaryHealth,
    ) -> (SoakReceiptInput, ReleaseReceiptScope, ReleasePromotionPolicy) {
        let scope = scope();
        let policy = policy();
        let started = MonotonicWallTimestamp::new(1_000, 10).expect("start");
        let ended = MonotonicWallTimestamp::new(
            1_000 + i64::try_from(duration_ms).expect("duration"),
            10 + duration_ms,
        )
        .expect("end");
        let input = SoakReceiptInput::new(
            digest(1),
            scope.clone(),
            policy.clone(),
            MonotonicWallTimeWindow::new(started, ended).expect("window"),
            event_loss_count,
            unreconciled_effect_count,
            health,
            vec![digest(2)],
        )
        .expect("input");
        (input, scope, policy)
    }

    #[test]
    fn signed_24h_soak_verifies_but_short_and_unhealthy_evidence_fail_closed() {
        let (input, scope, policy) =
            soak_input(MINIMUM_SOAK_DURATION_MS, 0, 0, CanaryHealth::Healthy);
        let trusted_policy = trust(scope.clone(), policy);
        let envelope = sign_soak_receipt(
            input,
            &signing_key(),
            ReleaseSigningKeyId::new("release-key-a").expect("key"),
        )
        .expect("sign");
        let verified =
            verify_signed_soak_receipt(&envelope, &scope, &trusted_policy).expect("verify");
        assert_eq!(verified.receipt().time_window().elapsed_ms(), MINIMUM_SOAK_DURATION_MS);

        for (duration, losses, effects, health) in [
            (
                MINIMUM_SOAK_DURATION_MS - 1,
                0,
                0,
                CanaryHealth::Healthy,
            ),
            (MINIMUM_SOAK_DURATION_MS, 1, 0, CanaryHealth::Healthy),
            (MINIMUM_SOAK_DURATION_MS, 0, 1, CanaryHealth::Healthy),
            (MINIMUM_SOAK_DURATION_MS, 0, 0, CanaryHealth::Unhealthy),
        ] {
            let (input, scope, policy) = soak_input(duration, losses, effects, health);
            let trusted_policy = trust(scope.clone(), policy);
            let envelope = sign_soak_receipt(
                input,
                &signing_key(),
                ReleaseSigningKeyId::new("release-key-a").expect("key"),
            )
            .expect("sign");
            assert!(verify_signed_soak_receipt(&envelope, &scope, &trusted_policy).is_err());
        }
    }

    #[test]
    fn soak_scope_policy_and_signature_mismatches_fail_closed() {
        let (input, scope, policy) =
            soak_input(MINIMUM_SOAK_DURATION_MS, 0, 0, CanaryHealth::Healthy);
        let envelope = sign_soak_receipt(
            input,
            &signing_key(),
            ReleaseSigningKeyId::new("release-key-a").expect("key"),
        )
        .expect("sign");
        let good_trust = trust(scope.clone(), policy.clone());
        let wrong_scope = ReleaseReceiptScope::new(
            ReleaseTenantId::new("tenant-b").expect("tenant"),
            AuthenticatedSubject::new("oidc:subject-a").expect("subject"),
        );
        assert!(verify_signed_soak_receipt(&envelope, &wrong_scope, &good_trust).is_err());
        let wrong_policy_trust = trust(
            scope.clone(),
            ReleasePromotionPolicy::new(ReleasePolicyId::new("p4-other").expect("policy"))
                .expect("policy"),
        );
        assert!(verify_signed_soak_receipt(&envelope, &scope, &wrong_policy_trust).is_err());
        let bad_envelope = sign_soak_receipt(
            soak_input(MINIMUM_SOAK_DURATION_MS, 0, 0, CanaryHealth::Healthy).0,
            &SigningKey::from_bytes(&[8; 32]),
            ReleaseSigningKeyId::new("release-key-a").expect("key"),
        )
        .expect("sign");
        assert!(verify_signed_soak_receipt(&bad_envelope, &scope, &good_trust).is_err());
    }

    #[test]
    fn rollback_paths_and_artifacts_reject_traversal_symlink_and_absence() {
        for path in ["", "../escape", "/absolute", "nested/../../escape", "nested\\escape"] {
            assert!(RollbackArtifactPath::new(path).is_err(), "{path}");
        }

        let temporary = TempDir::new().expect("temporary directory");
        let store_root = temporary.path().join("objects");
        fs::create_dir(&store_root).expect("object directory");
        let store = ContentAddressedArtifactStore::open(&store_root).expect("store");
        assert!(matches!(
            store.read_verified(&digest(9)),
            Err(ReleaseOperationsError::MissingArtifact(_))
        ));

        #[cfg(unix)]
        {
            use std::os::unix::fs::symlink;

            let bytes = b"candidate artifact";
            let target = temporary.path().join("target");
            fs::write(&target, bytes).expect("target");
            let collision = store_root.join(digest_bytes(bytes).expect("digest").to_string());
            symlink(&target, &collision).expect("symlink");
            assert!(matches!(
                store.put(bytes),
                Err(ReleaseOperationsError::UnsafeFilesystemPath(_))
            ));
        }
    }

    #[test]
    fn heartbeat_gap_requires_bounded_monotonic_and_wall_intervals() {
        let earlier = MonotonicWallTimestamp::new(1_000, 1_000).expect("earlier");
        assert!(heartbeat_gap_within(
            earlier,
            MonotonicWallTimestamp::new(301_000, 301_000).expect("within"),
            300_000
        )
        .expect("check"));
        assert!(!heartbeat_gap_within(
            earlier,
            MonotonicWallTimestamp::new(301_001, 301_001).expect("outside"),
            300_000
        )
        .expect("check"));
    }

    #[test]
    fn release_root_lock_serializes_racers_and_reopens_after_drop() {
        use std::sync::mpsc;
        use std::thread;
        use std::time::Duration;

        let temporary = TempDir::new().expect("temporary directory");
        let lock_path = release_root_lock_path(temporary.path());
        let first = ReleaseRootLock::acquire(&lock_path).expect("first lock");
        let (ready_sender, ready_receiver) = mpsc::channel();
        let second_path = lock_path.clone();
        let second = thread::spawn(move || {
            let lock = ReleaseRootLock::acquire(&second_path).expect("second lock");
            ready_sender.send(()).expect("notify");
            drop(lock);
        });
        assert!(
            ready_receiver
                .recv_timeout(Duration::from_millis(50))
                .is_err(),
            "second manager acquired the release-root lock while first held it"
        );
        drop(first);
        ready_receiver
            .recv_timeout(Duration::from_secs(2))
            .expect("second manager acquires after release");
        second.join().expect("second manager");

        // Dropping the transaction models a crashed process: a newly opened
        // manager must acquire the OS-backed SQLite lock rather than encounter
        // a stale lock-file sentinel.
        let reopened = ReleaseRootLock::acquire(&lock_path).expect("reopen lock");
        drop(reopened);
    }
}

