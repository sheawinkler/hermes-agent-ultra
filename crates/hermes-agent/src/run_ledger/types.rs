use std::fmt;
use std::str::FromStr;

use hermes_core::execution_mode::{ExecutionMode, ExecutionModeSource, ResolvedExecutionMode};
use hermes_environments::{
    ExecutionContainmentContext, VerifiedIsolatedBackendKind, VerifiedIsolatedBackendScope,
};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::sealed_artifact;

use super::error::{LedgerError, LedgerResult};

pub const CANONICALIZATION_ALGORITHM: &str = "rfc8785-jcs";
pub const CANONICALIZATION_VERSION: u8 = 1;
pub const PRIVATE_FINGERPRINT_ALGORITHM: &str = "hmac-sha256";
pub const PRIVATE_FINGERPRINT_VERSION: u8 = 1;

macro_rules! uuid_id {
    ($name:ident, $label:literal) => {
        #[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            pub const fn from_uuid(value: Uuid) -> Self {
                Self(value)
            }

            pub const fn as_uuid(self) -> Uuid {
                self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.debug_tuple($label).field(&self.0).finish()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                value.parse::<Uuid>().map(Self)
            }
        }
    };
}

macro_rules! opaque_id {
    ($name:ident, $label:literal, $maximum_length:expr) => {
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
        #[serde(transparent)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> LedgerResult<Self> {
                let value = value.into();
                validate_opaque_identifier($label, &value, $maximum_length)?;
                Ok(Self(value))
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter
                    .debug_tuple(stringify!($name))
                    .field(&self.0)
                    .finish()
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(&self.0)
            }
        }

        impl FromStr for $name {
            type Err = LedgerError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                Self::new(value)
            }
        }
    };
}

uuid_id!(RunId, "RunId");
uuid_id!(EventId, "EventId");
uuid_id!(OutboxRowId, "OutboxRowId");

opaque_id!(AppendKey, "append key", 256);
opaque_id!(LedgerIdentityKeyId, "ledger identity key id", 128);
opaque_id!(SessionId, "session id", 256);
opaque_id!(BranchId, "branch id", 256);
opaque_id!(TenantId, "tenant id", 256);
opaque_id!(ActionId, "action id", 256);
opaque_id!(ObjectiveId, "objective id", 256);
opaque_id!(TaskId, "task id", 256);
opaque_id!(ModelId, "model id", 256);
opaque_id!(ProviderId, "provider id", 256);
opaque_id!(ArtifactKeyId, "sealed artifact key id", 256);
opaque_id!(RedactorVersion, "redactor version", 256);
opaque_id!(RunOwnerId, "run owner id", 256);

#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct DurableDeliveryReceipt(String);

impl DurableDeliveryReceipt {
    pub fn new(value: impl Into<String>) -> LedgerResult<Self> {
        let value = value.into();
        validate_nonempty_text("durable delivery receipt", &value, 1024)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for DurableDeliveryReceipt {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("DurableDeliveryReceipt")
            .field(&self.0)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct Sha256Digest(String);

impl Sha256Digest {
    pub fn new(value: impl Into<String>) -> LedgerResult<Self> {
        let value = value.into();
        validate_lowercase_sha256("SHA-256 digest", &value)?;
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Sha256Digest")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct DownstreamIdempotencyKey(Sha256Digest);

impl DownstreamIdempotencyKey {
    pub(crate) fn from_sha256_hex(value: String) -> LedgerResult<Self> {
        Ok(Self(Sha256Digest::new(value)?))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for DownstreamIdempotencyKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("DownstreamIdempotencyKey")
            .field(&self.0)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct NonResumableIdempotencyKey(Sha256Digest);

impl NonResumableIdempotencyKey {
    pub(crate) fn from_sha256_hex(value: String) -> LedgerResult<Self> {
        Ok(Self(Sha256Digest::new(value)?))
    }

    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for NonResumableIdempotencyKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("NonResumableIdempotencyKey")
            .field(&self.0)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct CanonicalJson(String);

impl CanonicalJson {
    pub fn from_value<T: Serialize>(value: &T) -> LedgerResult<Self> {
        let bytes = serde_json_canonicalizer::to_vec(value)
            .map_err(|error| LedgerError::CanonicalJson(error.to_string()))?;
        let text = String::from_utf8(bytes)
            .map_err(|error| LedgerError::CanonicalJson(error.to_string()))?;
        Ok(Self(text))
    }

    pub fn from_jcs_text(value: impl Into<String>) -> LedgerResult<Self> {
        let value = value.into();
        let parsed: Value = serde_json::from_str(&value)
            .map_err(|error| LedgerError::CanonicalJson(error.to_string()))?;
        let canonical = Self::from_value(&parsed)?;
        if canonical.0 != value {
            return Err(LedgerError::CanonicalJson(
                "input is not RFC 8785 JCS-v1 canonical JSON".to_string(),
            ));
        }
        Ok(canonical)
    }

    pub fn empty_object() -> Self {
        Self("{}".to_string())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn as_value(&self) -> LedgerResult<Value> {
        serde_json::from_str(&self.0).map_err(|error| LedgerError::CanonicalJson(error.to_string()))
    }
}

impl fmt::Debug for CanonicalJson {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("CanonicalJson")
            .field(&self.0)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct LeaseToken(Uuid);

impl LeaseToken {
    pub(crate) fn random() -> Self {
        Self(Uuid::new_v4())
    }

    pub(crate) fn as_bytes(&self) -> &[u8; 16] {
        self.0.as_bytes()
    }

    pub(crate) fn to_blob(&self) -> Vec<u8> {
        self.0.as_bytes().to_vec()
    }

    pub(crate) fn from_blob(value: Vec<u8>) -> LedgerResult<Self> {
        let bytes: [u8; 16] = value
            .try_into()
            .map_err(|_| LedgerError::InvalidStoredState {
                kind: "lease token",
                value: "wrong byte length".to_string(),
            })?;
        Ok(Self(Uuid::from_bytes(bytes)))
    }
}

impl fmt::Debug for LeaseToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("LeaseToken([redacted])")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PrivateFingerprintAlgorithm {
    HmacSha256,
}

impl PrivateFingerprintAlgorithm {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::HmacSha256 => PRIVATE_FINGERPRINT_ALGORITHM,
        }
    }

    pub(crate) fn from_db(value: &str) -> LedgerResult<Self> {
        match value {
            PRIVATE_FINGERPRINT_ALGORITHM => Ok(Self::HmacSha256),
            _ => Err(LedgerError::InvalidStoredState {
                kind: "private fingerprint algorithm",
                value: value.to_string(),
            }),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
pub struct PrivateFingerprint {
    algorithm: PrivateFingerprintAlgorithm,
    algorithm_version: u8,
    key_id: LedgerIdentityKeyId,
    key_version: u32,
    bytes: [u8; 32],
}

impl PrivateFingerprint {
    pub fn hmac_sha256_v1(
        key_id: LedgerIdentityKeyId,
        key_version: u32,
        bytes: [u8; 32],
    ) -> LedgerResult<Self> {
        if key_version == 0 {
            return Err(LedgerError::InvalidStoredState {
                kind: "private fingerprint key version",
                value: "0".to_string(),
            });
        }
        Ok(Self {
            algorithm: PrivateFingerprintAlgorithm::HmacSha256,
            algorithm_version: PRIVATE_FINGERPRINT_VERSION,
            key_id,
            key_version,
            bytes,
        })
    }

    pub fn algorithm(&self) -> PrivateFingerprintAlgorithm {
        self.algorithm
    }

    pub fn algorithm_version(&self) -> u8 {
        self.algorithm_version
    }

    pub fn key_id(&self) -> &LedgerIdentityKeyId {
        &self.key_id
    }

    pub fn key_version(&self) -> u32 {
        self.key_version
    }

    pub(crate) fn as_hex(&self) -> String {
        hex::encode(self.bytes)
    }

    pub(crate) fn from_db(
        algorithm: &str,
        algorithm_version: i64,
        key_id: String,
        key_version: i64,
        fingerprint_hex: String,
    ) -> LedgerResult<Self> {
        let algorithm = PrivateFingerprintAlgorithm::from_db(algorithm)?;
        let algorithm_version =
            u8::try_from(algorithm_version).map_err(|_| LedgerError::InvalidStoredState {
                kind: "private fingerprint algorithm version",
                value: algorithm_version.to_string(),
            })?;
        if algorithm_version != PRIVATE_FINGERPRINT_VERSION {
            return Err(LedgerError::InvalidStoredState {
                kind: "private fingerprint algorithm version",
                value: algorithm_version.to_string(),
            });
        }
        let key_version =
            u32::try_from(key_version).map_err(|_| LedgerError::InvalidStoredState {
                kind: "private fingerprint key version",
                value: key_version.to_string(),
            })?;
        let bytes = decode_lowercase_32_hex("private fingerprint", &fingerprint_hex)?;
        let key_id = LedgerIdentityKeyId::new(key_id)?;
        Ok(Self {
            algorithm,
            algorithm_version,
            key_id,
            key_version: nonzero_key_version(key_version)?,
            bytes,
        })
    }

    fn from_artifact_wire(
        algorithm: &str,
        algorithm_version: u8,
        key_id: String,
        key_version: u32,
        fingerprint_hex: &str,
    ) -> LedgerResult<Self> {
        if algorithm != PRIVATE_FINGERPRINT_ALGORITHM {
            return Err(LedgerError::SealedArtifactIdentity(format!(
                "unsupported fingerprint algorithm {algorithm:?}"
            )));
        }
        if algorithm_version != PRIVATE_FINGERPRINT_VERSION {
            return Err(LedgerError::SealedArtifactIdentity(format!(
                "unsupported fingerprint algorithm version {algorithm_version}"
            )));
        }
        let bytes = decode_lowercase_32_hex("sealed artifact private fingerprint", fingerprint_hex)
            .map_err(|_| {
                LedgerError::SealedArtifactIdentity(
                    "private fingerprint must be exactly 32 lowercase hexadecimal bytes"
                        .to_string(),
                )
            })?;
        let key_id = LedgerIdentityKeyId::new(key_id).map_err(|_| {
            LedgerError::SealedArtifactIdentity("invalid fingerprint key id".to_string())
        })?;
        let key_version = nonzero_key_version(key_version).map_err(|_| {
            LedgerError::SealedArtifactIdentity("invalid fingerprint key version".to_string())
        })?;
        Ok(Self {
            algorithm: PrivateFingerprintAlgorithm::HmacSha256,
            algorithm_version,
            key_id,
            key_version,
            bytes,
        })
    }
}

impl fmt::Debug for PrivateFingerprint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PrivateFingerprint")
            .field("algorithm", &self.algorithm)
            .field("algorithm_version", &self.algorithm_version)
            .field("key_id", &self.key_id)
            .field("key_version", &self.key_version)
            .field("bytes", &"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendIdentity {
    pub public_identity: CanonicalJson,
    pub private_fingerprint: PrivateFingerprint,
}

impl AppendIdentity {
    pub fn new(public_identity: CanonicalJson, private_fingerprint: PrivateFingerprint) -> Self {
        Self {
            public_identity,
            private_fingerprint,
        }
    }
}

impl TryFrom<&sealed_artifact::AppendIdentity> for AppendIdentity {
    type Error = LedgerError;

    fn try_from(value: &sealed_artifact::AppendIdentity) -> Result<Self, Self::Error> {
        if value.canonicalization_algorithm != CANONICALIZATION_ALGORITHM
            || value.canonicalization_version != CANONICALIZATION_VERSION
        {
            return Err(LedgerError::SealedArtifactIdentity(
                "append identity canonicalization must be RFC 8785 JCS-v1".to_string(),
            ));
        }
        RedactorVersion::new(value.redactor_version.clone()).map_err(|_| {
            LedgerError::SealedArtifactIdentity(
                "append identity redactor version is not a valid opaque identifier".to_string(),
            )
        })?;
        let public_identity_jcs =
            std::str::from_utf8(&value.public_identity_jcs).map_err(|_| {
                LedgerError::SealedArtifactIdentity(
                    "public identity JCS bytes are not UTF-8".to_string(),
                )
            })?;
        let public_identity = CanonicalJson::from_jcs_text(public_identity_jcs.to_string())
            .map_err(|_| {
                LedgerError::SealedArtifactIdentity(
                    "public identity bytes are not exact RFC 8785 JCS-v1".to_string(),
                )
            })?;
        let private_fingerprint = PrivateFingerprint::from_artifact_wire(
            &value.fingerprint_algorithm,
            value.fingerprint_version,
            value.fingerprint_key_id.clone(),
            value.fingerprint_key_version,
            &value.private_fingerprint,
        )?;
        Ok(Self {
            public_identity,
            private_fingerprint,
        })
    }
}

impl TryFrom<sealed_artifact::AppendIdentity> for AppendIdentity {
    type Error = LedgerError;

    fn try_from(value: sealed_artifact::AppendIdentity) -> Result<Self, Self::Error> {
        Self::try_from(&value)
    }
}

/// A validated public reference to a sealed artifact. It contains no private
/// bytes or plaintext digest, only the envelope identity that the artifact
/// sealer has already verified before publication.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
pub struct SealedArtifactRef {
    envelope_digest: Sha256Digest,
    key_id: ArtifactKeyId,
    schema_version: u32,
    encryption_algorithm: String,
    canonicalization_algorithm: String,
    canonicalization_version: u8,
    redactor_version: RedactorVersion,
}

impl SealedArtifactRef {
    pub fn envelope_digest(&self) -> &Sha256Digest {
        &self.envelope_digest
    }

    pub fn key_id(&self) -> &ArtifactKeyId {
        &self.key_id
    }

    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn encryption_algorithm(&self) -> &str {
        &self.encryption_algorithm
    }

    pub fn canonicalization_algorithm(&self) -> &str {
        &self.canonicalization_algorithm
    }

    pub fn canonicalization_version(&self) -> u8 {
        self.canonicalization_version
    }

    pub fn redactor_version(&self) -> &RedactorVersion {
        &self.redactor_version
    }

    pub(crate) fn from_db(
        envelope_digest: String,
        key_id: String,
        schema_version: i64,
        encryption_algorithm: String,
        canonicalization_algorithm: String,
        canonicalization_version: i64,
        redactor_version: String,
    ) -> LedgerResult<Self> {
        let schema_version =
            u32::try_from(schema_version).map_err(|_| LedgerError::InvalidStoredState {
                kind: "sealed artifact schema version",
                value: schema_version.to_string(),
            })?;
        let canonicalization_version = u8::try_from(canonicalization_version).map_err(|_| {
            LedgerError::InvalidStoredState {
                kind: "sealed artifact canonicalization version",
                value: canonicalization_version.to_string(),
            }
        })?;
        Self::from_parts(
            envelope_digest,
            key_id,
            schema_version,
            encryption_algorithm,
            canonicalization_algorithm,
            canonicalization_version,
            redactor_version,
        )
        .map_err(|error| LedgerError::InvalidStoredState {
            kind: "sealed artifact reference",
            value: error.to_string(),
        })
    }

    fn from_parts(
        envelope_digest: String,
        key_id: String,
        schema_version: u32,
        encryption_algorithm: String,
        canonicalization_algorithm: String,
        canonicalization_version: u8,
        redactor_version: String,
    ) -> LedgerResult<Self> {
        if schema_version != sealed_artifact::SEALED_ARTIFACT_SCHEMA_VERSION {
            return Err(LedgerError::SealedArtifactReference(format!(
                "unsupported sealed artifact schema version {schema_version}"
            )));
        }
        if encryption_algorithm != sealed_artifact::ARTIFACT_ENCRYPTION_ALGORITHM {
            return Err(LedgerError::SealedArtifactReference(format!(
                "unsupported sealed artifact encryption algorithm {encryption_algorithm:?}"
            )));
        }
        if canonicalization_algorithm != CANONICALIZATION_ALGORITHM
            || canonicalization_version != CANONICALIZATION_VERSION
        {
            return Err(LedgerError::SealedArtifactReference(
                "sealed artifact canonicalization must be RFC 8785 JCS-v1".to_string(),
            ));
        }
        Ok(Self {
            envelope_digest: Sha256Digest::new(envelope_digest)?,
            key_id: ArtifactKeyId::new(key_id)?,
            schema_version,
            encryption_algorithm,
            canonicalization_algorithm,
            canonicalization_version,
            redactor_version: RedactorVersion::new(redactor_version)?,
        })
    }
}

impl TryFrom<&sealed_artifact::SealedArtifactRef> for SealedArtifactRef {
    type Error = LedgerError;

    fn try_from(value: &sealed_artifact::SealedArtifactRef) -> Result<Self, Self::Error> {
        Self::from_parts(
            value.envelope_digest.clone(),
            value.key_id.clone(),
            value.schema_version,
            value.encryption_algorithm.clone(),
            value.canonicalization_algorithm.clone(),
            value.canonicalization_version,
            value.redactor_version.clone(),
        )
    }
}

impl TryFrom<sealed_artifact::SealedArtifactRef> for SealedArtifactRef {
    type Error = LedgerError;

    fn try_from(value: sealed_artifact::SealedArtifactRef) -> Result<Self, Self::Error> {
        Self::try_from(&value)
    }
}
/// Physical availability of one immutable sealed envelope in the private CAS.
///
/// This is deliberately distinct from [`ArtifactReferenceState`]: a digest can
/// be shared by several event references, while a transition of one logical
/// reference must never imply that another still-live reference is retired.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactPhysicalState {
    Unverified,
    Available,
    UnavailableMissing,
    UnavailableCorrupt,
    UnavailableKeyRevoked,
    Retiring,
    Retired,
}

impl ArtifactPhysicalState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::Available => "available",
            Self::UnavailableMissing => "unavailable_missing",
            Self::UnavailableCorrupt => "unavailable_corrupt",
            Self::UnavailableKeyRevoked => "unavailable_key_revoked",
            Self::Retiring => "retiring",
            Self::Retired => "retired",
        }
    }

    pub(crate) fn from_db(value: &str) -> LedgerResult<Self> {
        match value {
            "unverified" => Ok(Self::Unverified),
            "available" => Ok(Self::Available),
            "unavailable_missing" => Ok(Self::UnavailableMissing),
            "unavailable_corrupt" => Ok(Self::UnavailableCorrupt),
            "unavailable_key_revoked" => Ok(Self::UnavailableKeyRevoked),
            "retiring" => Ok(Self::Retiring),
            "retired" => Ok(Self::Retired),
            _ => Err(LedgerError::InvalidStoredState {
                kind: "artifact physical state",
                value: value.to_string(),
            }),
        }
    }
}

/// Durable state of a logical event-to-artifact reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactReferenceState {
    Unverified,
    Available,
    UnavailableMissing,
    UnavailableCorrupt,
    UnavailableKeyRevoked,
    Retiring,
    Retired,
}

impl ArtifactReferenceState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::Available => "available",
            Self::UnavailableMissing => "unavailable_missing",
            Self::UnavailableCorrupt => "unavailable_corrupt",
            Self::UnavailableKeyRevoked => "unavailable_key_revoked",
            Self::Retiring => "retiring",
            Self::Retired => "retired",
        }
    }

    pub const fn protects_physical_artifact(self) -> bool {
        matches!(self, Self::Unverified | Self::Available | Self::Retiring)
    }

    pub(crate) fn from_db(value: &str) -> LedgerResult<Self> {
        match value {
            "unverified" => Ok(Self::Unverified),
            "available" => Ok(Self::Available),
            "unavailable_missing" => Ok(Self::UnavailableMissing),
            "unavailable_corrupt" => Ok(Self::UnavailableCorrupt),
            "unavailable_key_revoked" => Ok(Self::UnavailableKeyRevoked),
            "retiring" => Ok(Self::Retiring),
            "retired" => Ok(Self::Retired),
            _ => Err(LedgerError::InvalidStoredState {
                kind: "artifact reference state",
                value: value.to_string(),
            }),
        }
    }
}

/// Closed reason attached to a non-available lifecycle transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactStateReason {
    Missing,
    Corrupt,
    KeyRevoked,
    RetentionDeleted,
    /// A replay-critical payload was only captured as metadata/redacted
    /// evidence, or could not be captured at all.
    Sanitized,
}

impl ArtifactStateReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Corrupt => "corrupt",
            Self::KeyRevoked => "key_revoked",
            Self::RetentionDeleted => "retention_deleted",
            Self::Sanitized => "sanitized",
        }
    }

    pub(crate) fn from_db(value: &str) -> LedgerResult<Self> {
        match value {
            "missing" => Ok(Self::Missing),
            "corrupt" => Ok(Self::Corrupt),
            "key_revoked" => Ok(Self::KeyRevoked),
            "retention_deleted" => Ok(Self::RetentionDeleted),
            "sanitized" => Ok(Self::Sanitized),
            _ => Err(LedgerError::InvalidStoredState {
                kind: "artifact state reason",
                value: value.to_string(),
            }),
        }
    }
}

/// Proof-visible exact replay state for one run.
///
/// `Unverified` is provisional compatibility state for historical references:
/// it is never replayable until the coordinator verifies the private CAS.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExactReplayState {
    Unverified,
    Available,
    UnavailableMissing,
    UnavailableCorrupt,
    UnavailableKeyRevoked,
    /// At least one replay-critical artifact was metadata-only, redacted, or
    /// could not be captured. This latch never returns to exact.
    UnavailableSanitized,
    Retiring,
    Retired,
}

impl ExactReplayState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unverified => "unverified",
            Self::Available => "available",
            Self::UnavailableMissing => "unavailable_missing",
            Self::UnavailableCorrupt => "unavailable_corrupt",
            Self::UnavailableKeyRevoked => "unavailable_key_revoked",
            Self::UnavailableSanitized => "unavailable_sanitized",
            Self::Retiring => "retiring",
            Self::Retired => "retired",
        }
    }

    pub const fn exact_replay(self) -> bool {
        matches!(self, Self::Available)
    }

    pub(crate) fn from_db(value: &str) -> LedgerResult<Self> {
        match value {
            "unverified" => Ok(Self::Unverified),
            "available" => Ok(Self::Available),
            "unavailable_missing" => Ok(Self::UnavailableMissing),
            "unavailable_corrupt" => Ok(Self::UnavailableCorrupt),
            "unavailable_key_revoked" => Ok(Self::UnavailableKeyRevoked),
            "unavailable_sanitized" => Ok(Self::UnavailableSanitized),
            "retiring" => Ok(Self::Retiring),
            "retired" => Ok(Self::Retired),
            _ => Err(LedgerError::InvalidStoredState {
                kind: "exact replay state",
                value: value.to_string(),
            }),
        }
}
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ExactReplayStatus {
    pub state: ExactReplayState,
    pub reason: Option<ArtifactStateReason>,
    pub exact_replay: bool,
    pub transition_sequence: u64,
    pub latched_at_ms: Option<i64>,
    pub latched_by_event_id: Option<EventId>,
}

/// How a replay-critical artifact was durably captured. Exact private payloads
/// are the only form that can contribute to an exact run replay.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayArtifactFidelity {
    ExactPrivate,
    MetadataOnly,
}

impl ReplayArtifactFidelity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ExactPrivate => "exact_private",
            Self::MetadataOnly => "metadata_only",
        }
    }

    pub(crate) fn from_db(value: &str) -> LedgerResult<Self> {
        match value {
            "exact_private" => Ok(Self::ExactPrivate),
            "metadata_only" => Ok(Self::MetadataOnly),
            _ => Err(LedgerError::InvalidStoredState {
                kind: "replay artifact fidelity",
                value: value.to_string(),
            }),
        }
    }
}

/// The operation category captured by a replay obligation. This taxonomy is
/// authoritative evidence for replay/eval counters; consumers must not infer
/// it from effect intent, public labels, or provider-specific payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayOperationKind {
    Run,
    Model,
    Tool,
    Delegation,
    Oauth,
    Delivery,
    AgentOutput,
}

impl ReplayOperationKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Run => "run",
            Self::Model => "model",
            Self::Tool => "tool",
            Self::Delegation => "delegation",
            Self::Oauth => "oauth",
            Self::Delivery => "delivery",
            Self::AgentOutput => "agent_output",
        }
    }

    pub(crate) fn from_db(value: &str) -> LedgerResult<Self> {
        match value {
            "run" => Ok(Self::Run),
            "model" => Ok(Self::Model),
            "tool" => Ok(Self::Tool),
            "delegation" => Ok(Self::Delegation),
            "oauth" => Ok(Self::Oauth),
            "delivery" => Ok(Self::Delivery),
            "agent_output" => Ok(Self::AgentOutput),
            _ => Err(LedgerError::InvalidStoredState {
                kind: "replay operation kind",
                value: value.to_string(),
            }),
        }
    }
}

/// The transcript phase within a typed replay operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayOperationPhase {
    Request,
    Result,
}

impl ReplayOperationPhase {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Request => "request",
            Self::Result => "result",
        }
    }

    pub(crate) fn from_db(value: &str) -> LedgerResult<Self> {
        match value {
            "request" => Ok(Self::Request),
            "result" => Ok(Self::Result),
            _ => Err(LedgerError::InvalidStoredState {
                kind: "replay operation phase",
                value: value.to_string(),
            }),
        }
    }
}

/// Durable resolution state for one required replay obligation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayObligationFidelity {
    Pending,
    ExactPrivate,
    MetadataOnly,
    Unavailable,
}

impl ReplayObligationFidelity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::ExactPrivate => "exact_private",
            Self::MetadataOnly => "metadata_only",
            Self::Unavailable => "unavailable",
        }
    }

    pub(crate) fn from_db(value: &str) -> LedgerResult<Self> {
        match value {
            "pending" => Ok(Self::Pending),
            "exact_private" => Ok(Self::ExactPrivate),
            "metadata_only" => Ok(Self::MetadataOnly),
            "unavailable" => Ok(Self::Unavailable),
            _ => Err(LedgerError::InvalidStoredState {
                kind: "replay obligation fidelity",
                value: value.to_string(),
            }),
        }
    }
}

/// Bounded outcome classification bound to a replay-obligation resolution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayOperationOutcome {
    NotApplicable,
    Returned,
    HandlerError,
    EffectNonResumableUnknown,
    Cancelled,
    TimedOut,
    Unavailable,
}

impl ReplayOperationOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::NotApplicable => "not_applicable",
            Self::Returned => "returned",
            Self::HandlerError => "handler_error",
            Self::EffectNonResumableUnknown => "effect_non_resumable_unknown",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Unavailable => "unavailable",
        }
    }

    pub(crate) fn from_db(value: &str) -> LedgerResult<Self> {
        match value {
            "not_applicable" => Ok(Self::NotApplicable),
            "returned" => Ok(Self::Returned),
            "handler_error" => Ok(Self::HandlerError),
            "effect_non_resumable_unknown" => Ok(Self::EffectNonResumableUnknown),
            "cancelled" => Ok(Self::Cancelled),
            "timed_out" => Ok(Self::TimedOut),
            "unavailable" => Ok(Self::Unavailable),
            _ => Err(LedgerError::InvalidStoredState {
                kind: "replay operation outcome",
                value: value.to_string(),
            }),
        }
    }
}

/// Ordered, exportable replay-obligation evidence. Every row is required; the
/// event-chain transition history is validated before this snapshot is exposed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReplayObligationSnapshot {
    pub source_event_id: EventId,
    pub source_event_sequence: u64,
    pub operation: ReplayOperationKind,
    pub phase: ReplayOperationPhase,
    /// Uniquely identifies one operation within its source event. Runtime
    /// operations use the source event UUID; delivery uses the outbox UUID.
    pub operation_instance_id: String,
    pub required: bool,
    pub fidelity: ReplayObligationFidelity,
    pub outcome: ReplayOperationOutcome,
    pub resolved_event_id: Option<EventId>,
    pub artifact: Option<SealedArtifactRef>,
    pub reason: Option<ArtifactStateReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactReferenceStatus {
    pub run_id: RunId,
    pub event_id: EventId,
    pub artifact: SealedArtifactRef,
    pub physical_state: ArtifactPhysicalState,
    pub physical_reason: Option<ArtifactStateReason>,
    pub reference_state: ArtifactReferenceState,
    pub reference_reason: Option<ArtifactStateReason>,
    pub exact_replay: ExactReplayStatus,
}
/// One source artifact whose ledger state must remain unchanged while an
/// effect-free branch child is admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BranchReplayArtifactBinding {
    source_event_id: EventId,
    artifact: SealedArtifactRef,
    physical_state: ArtifactPhysicalState,
    reference_state: ArtifactReferenceState,
}

impl BranchReplayArtifactBinding {
    pub(crate) fn new(
        source_event_id: EventId,
        artifact: SealedArtifactRef,
        physical_state: ArtifactPhysicalState,
        reference_state: ArtifactReferenceState,
    ) -> Self {
        Self {
            source_event_id,
            artifact,
            physical_state,
            reference_state,
        }
    }

    pub(crate) const fn source_event_id(&self) -> EventId {
        self.source_event_id
    }

    pub(crate) fn artifact(&self) -> &SealedArtifactRef {
        &self.artifact
    }

    pub(crate) const fn physical_state(&self) -> ArtifactPhysicalState {
        self.physical_state
    }

    pub(crate) const fn reference_state(&self) -> ArtifactReferenceState {
        self.reference_state
    }
}

/// Immutable source facts that the atomic branch-mint transaction rechecks
/// before it inserts a child run. This is intentionally crate-visible only:
/// callers must derive it from a verified replay plan, never arbitrary input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BranchReplaySourceSnapshot {
    source_run_id: RunId,
    source_session_id: SessionId,
    source_tenant_id: TenantId,
    source_branch_id: BranchId,
    source_export_digest: Sha256Digest,
    boundary_event_id: EventId,
    boundary_event_sequence: u64,
    boundary_event_hash: Sha256Digest,
    source_head_event_id: EventId,
    /// The sequence of the source's currently committed head event.
    ///
    /// The historical SQLite column is named `next_event_sequence`, but stores
    /// this committed-head sequence; do not reinterpret it as a successor.
    source_head_sequence: u64,
    source_head_hash: Sha256Digest,
    source_exact_replay_state: ExactReplayState,
    source_exact_replay_transition_sequence: u64,
    artifacts: Vec<BranchReplayArtifactBinding>,
}

impl BranchReplaySourceSnapshot {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        source_run_id: RunId,
        source_session_id: SessionId,
        source_tenant_id: TenantId,
        source_branch_id: BranchId,
        source_export_digest: Sha256Digest,
        boundary_event_id: EventId,
        boundary_event_sequence: u64,
        boundary_event_hash: Sha256Digest,
        source_head_event_id: EventId,
        source_head_sequence: u64,
        source_head_hash: Sha256Digest,
        source_exact_replay_state: ExactReplayState,
        source_exact_replay_transition_sequence: u64,
        artifacts: Vec<BranchReplayArtifactBinding>,
    ) -> LedgerResult<Self> {
        if boundary_event_sequence > source_head_sequence {
            return Err(LedgerError::IntegrityViolation(
                "branch replay boundary is after the verified source head".to_string(),
            ));
        }
        for (index, binding) in artifacts.iter().enumerate() {
            if artifacts[..index].iter().any(|previous| {
                previous.source_event_id == binding.source_event_id
                    && previous.artifact.envelope_digest() == binding.artifact.envelope_digest()
            }) {
                return Err(LedgerError::IntegrityViolation(
                    "branch replay source artifact bindings contain a duplicate reference"
                        .to_string(),
                ));
            }
        }
        Ok(Self {
            source_run_id,
            source_session_id,
            source_tenant_id,
            source_branch_id,
            source_export_digest,
            boundary_event_id,
            boundary_event_sequence,
            boundary_event_hash,
            source_head_event_id,
            source_head_sequence,
            source_head_hash,
            source_exact_replay_state,
            source_exact_replay_transition_sequence,
            artifacts,
        })
    }

    pub(crate) const fn source_run_id(&self) -> RunId {
        self.source_run_id
    }

    pub(crate) fn source_session_id(&self) -> &SessionId {
        &self.source_session_id
    }

    pub(crate) fn source_tenant_id(&self) -> &TenantId {
        &self.source_tenant_id
    }

    pub(crate) fn source_branch_id(&self) -> &BranchId {
        &self.source_branch_id
    }

    pub(crate) fn source_export_digest(&self) -> &Sha256Digest {
        &self.source_export_digest
    }

    pub(crate) const fn boundary_event_id(&self) -> EventId {
        self.boundary_event_id
    }

    pub(crate) const fn boundary_event_sequence(&self) -> u64 {
        self.boundary_event_sequence
    }

    pub(crate) fn boundary_event_hash(&self) -> &Sha256Digest {
        &self.boundary_event_hash
    }

    pub(crate) const fn source_head_event_id(&self) -> EventId {
        self.source_head_event_id
    }

    pub(crate) const fn source_head_sequence(&self) -> u64 {
        self.source_head_sequence
    }

    pub(crate) fn source_head_hash(&self) -> &Sha256Digest {
        &self.source_head_hash
    }

    pub(crate) const fn source_exact_replay_state(&self) -> ExactReplayState {
        self.source_exact_replay_state
    }

    pub(crate) const fn source_exact_replay_transition_sequence(&self) -> u64 {
        self.source_exact_replay_transition_sequence
    }

    pub(crate) fn artifacts(&self) -> &[BranchReplayArtifactBinding] {
        &self.artifacts
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactStateTransition {
    pub run_id: RunId,
    pub event_id: EventId,
    pub artifact: SealedArtifactRef,
    pub transition_sequence: u64,
    pub from_state: ArtifactReferenceState,
    pub to_state: ArtifactReferenceState,
    pub reason: Option<ArtifactStateReason>,
    pub audit_event_id: EventId,
    pub created_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DigestAbsence {
    Unavailable,
    NotApplicable,
}

impl DigestAbsence {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::NotApplicable => "not_applicable",
        }
    }

    pub(crate) fn from_db(value: &str) -> LedgerResult<Self> {
        match value {
            "unavailable" => Ok(Self::Unavailable),
            "not_applicable" => Ok(Self::NotApplicable),
            _ => Err(LedgerError::InvalidStoredState {
                kind: "provenance digest absence",
                value: value.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "availability", content = "value", rename_all = "snake_case")]
pub enum ProvenanceDigest {
    Present(Sha256Digest),
    Absent(DigestAbsence),
}

impl ProvenanceDigest {
    pub fn present(digest: Sha256Digest) -> Self {
        Self::Present(digest)
    }

    pub fn absent(reason: DigestAbsence) -> Self {
        Self::Absent(reason)
    }

    pub const fn is_present(&self) -> bool {
        matches!(self, Self::Present(_))
    }

    pub(crate) fn to_db(&self) -> (Option<&str>, Option<&'static str>) {
        match self {
            Self::Present(digest) => (Some(digest.as_str()), None),
            Self::Absent(reason) => (None, Some(reason.as_str())),
        }
    }

    pub(crate) fn from_db(
        kind: &'static str,
        digest: Option<String>,
        absence: Option<String>,
    ) -> LedgerResult<Self> {
        match (digest, absence) {
            (Some(digest), None) => Ok(Self::Present(Sha256Digest::new(digest)?)),
            (None, Some(absence)) => Ok(Self::Absent(DigestAbsence::from_db(&absence)?)),
            _ => Err(LedgerError::InvalidStoredState {
                kind,
                value: "digest and typed absence must contain exactly one value".to_string(),
            }),
        }
    }
}

/// Immutable, typed provenance that is written once with a run header. The
/// digest fields explicitly distinguish a recorded full SHA-256 from a closed
/// reason why that material was genuinely unavailable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunProvenance {
    canonicalization_algorithm: String,
    canonicalization_version: u8,
    session_id: SessionId,
    parent_run_id: Option<RunId>,
    branch_id: BranchId,
    tenant_id: TenantId,
    action_id: ActionId,
    objective_id: ObjectiveId,
    task_id: TaskId,
    model_id: ModelId,
    provider_id: ProviderId,
    config_digest: Sha256Digest,
    prompt_digest: ProvenanceDigest,
    input_digest: ProvenanceDigest,
    output_digest: ProvenanceDigest,
    tool_digest: ProvenanceDigest,
    skill_digest: ProvenanceDigest,
    policy_digest: ProvenanceDigest,
    sandbox_digest: ProvenanceDigest,
}

impl RunProvenance {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        session_id: SessionId,
        parent_run_id: Option<RunId>,
        branch_id: BranchId,
        tenant_id: TenantId,
        action_id: ActionId,
        objective_id: ObjectiveId,
        task_id: TaskId,
        model_id: ModelId,
        provider_id: ProviderId,
        config_digest: Sha256Digest,
        prompt_digest: ProvenanceDigest,
        input_digest: ProvenanceDigest,
        output_digest: ProvenanceDigest,
        tool_digest: ProvenanceDigest,
        skill_digest: ProvenanceDigest,
        policy_digest: ProvenanceDigest,
        sandbox_digest: ProvenanceDigest,
    ) -> Self {
        Self {
            canonicalization_algorithm: CANONICALIZATION_ALGORITHM.to_string(),
            canonicalization_version: CANONICALIZATION_VERSION,
            session_id,
            parent_run_id,
            branch_id,
            tenant_id,
            action_id,
            objective_id,
            task_id,
            model_id,
            provider_id,
            config_digest,
            prompt_digest,
            input_digest,
            output_digest,
            tool_digest,
            skill_digest,
            policy_digest,
            sandbox_digest,
        }
    }

    pub fn session_id(&self) -> &SessionId {
        &self.session_id
    }

    pub fn parent_run_id(&self) -> Option<RunId> {
        self.parent_run_id
    }

    pub fn branch_id(&self) -> &BranchId {
        &self.branch_id
    }

    pub fn tenant_id(&self) -> &TenantId {
        &self.tenant_id
    }

    pub fn action_id(&self) -> &ActionId {
        &self.action_id
    }

    pub fn objective_id(&self) -> &ObjectiveId {
        &self.objective_id
    }

    pub fn task_id(&self) -> &TaskId {
        &self.task_id
    }

    pub fn model_id(&self) -> &ModelId {
        &self.model_id
    }

    pub fn provider_id(&self) -> &ProviderId {
        &self.provider_id
    }

    pub fn config_digest(&self) -> &Sha256Digest {
        &self.config_digest
    }

    pub fn prompt_digest(&self) -> &ProvenanceDigest {
        &self.prompt_digest
    }

    pub fn input_digest(&self) -> &ProvenanceDigest {
        &self.input_digest
    }

    pub fn output_digest(&self) -> &ProvenanceDigest {
        &self.output_digest
    }

    pub fn tool_digest(&self) -> &ProvenanceDigest {
        &self.tool_digest
    }

    pub fn skill_digest(&self) -> &ProvenanceDigest {
        &self.skill_digest
    }

    pub fn policy_digest(&self) -> &ProvenanceDigest {
        &self.policy_digest
    }

    pub fn sandbox_digest(&self) -> &ProvenanceDigest {
        &self.sandbox_digest
    }

    pub fn canonicalization_algorithm(&self) -> &str {
        &self.canonicalization_algorithm
    }

    pub fn canonicalization_version(&self) -> u8 {
        self.canonicalization_version
    }

    pub(crate) fn canonical_json(&self) -> LedgerResult<CanonicalJson> {
        CanonicalJson::from_value(self)
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn from_db(
        canonicalization_algorithm: String,
        canonicalization_version: i64,
        session_id: String,
        parent_run_id: Option<String>,
        branch_id: String,
        tenant_id: String,
        action_id: String,
        objective_id: String,
        task_id: String,
        model_id: String,
        provider_id: String,
        config_digest: String,
        prompt_digest: Option<String>,
        prompt_digest_absence: Option<String>,
        input_digest: Option<String>,
        input_digest_absence: Option<String>,
        output_digest: Option<String>,
        output_digest_absence: Option<String>,
        tool_digest: Option<String>,
        tool_digest_absence: Option<String>,
        skill_digest: Option<String>,
        skill_digest_absence: Option<String>,
        policy_digest: Option<String>,
        policy_digest_absence: Option<String>,
        sandbox_digest: Option<String>,
        sandbox_digest_absence: Option<String>,
    ) -> LedgerResult<Self> {
        let canonicalization_version = u8::try_from(canonicalization_version).map_err(|_| {
            LedgerError::InvalidStoredState {
                kind: "run provenance canonicalization version",
                value: canonicalization_version.to_string(),
            }
        })?;
        if canonicalization_algorithm != CANONICALIZATION_ALGORITHM
            || canonicalization_version != CANONICALIZATION_VERSION
        {
            return Err(LedgerError::InvalidStoredState {
                kind: "run provenance canonicalization",
                value: format!(
                    "{canonicalization_algorithm}/{canonicalization_version} is not RFC 8785 JCS-v1"
                ),
            });
        }
        Ok(Self {
            canonicalization_algorithm,
            canonicalization_version,
            session_id: SessionId::new(session_id)?,
            parent_run_id: parent_run_id.map(parse_run_id).transpose()?,
            branch_id: BranchId::new(branch_id)?,
            tenant_id: TenantId::new(tenant_id)?,
            action_id: ActionId::new(action_id)?,
            objective_id: ObjectiveId::new(objective_id)?,
            task_id: TaskId::new(task_id)?,
            model_id: ModelId::new(model_id)?,
            provider_id: ProviderId::new(provider_id)?,
            config_digest: Sha256Digest::new(config_digest)?,
            prompt_digest: ProvenanceDigest::from_db(
                "prompt provenance digest",
                prompt_digest,
                prompt_digest_absence,
            )?,
            input_digest: ProvenanceDigest::from_db(
                "input provenance digest",
                input_digest,
                input_digest_absence,
            )?,
            output_digest: ProvenanceDigest::from_db(
                "output provenance digest",
                output_digest,
                output_digest_absence,
            )?,
            tool_digest: ProvenanceDigest::from_db(
                "tool provenance digest",
                tool_digest,
                tool_digest_absence,
            )?,
            skill_digest: ProvenanceDigest::from_db(
                "skill provenance digest",
                skill_digest,
                skill_digest_absence,
            )?,
            policy_digest: ProvenanceDigest::from_db(
                "policy provenance digest",
                policy_digest,
                policy_digest_absence,
            )?,
            sandbox_digest: ProvenanceDigest::from_db(
                "sandbox provenance digest",
                sandbox_digest,
                sandbox_digest_absence,
            )?,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunOutcome {
    Success,
    Failure,
    Timeout,
    Cancellation,
    Blocked,
    DeadLettered,
}

impl RunOutcome {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Timeout => "timeout",
            Self::Cancellation => "cancellation",
            Self::Blocked => "blocked",
            Self::DeadLettered => "dead_lettered",
        }
    }

    pub(crate) fn from_db(value: &str) -> LedgerResult<Self> {
        match value {
            "success" => Ok(Self::Success),
            "failure" => Ok(Self::Failure),
            "timeout" => Ok(Self::Timeout),
            "cancellation" => Ok(Self::Cancellation),
            "blocked" => Ok(Self::Blocked),
            "dead_lettered" => Ok(Self::DeadLettered),
            _ => Err(LedgerError::InvalidStoredState {
                kind: "run outcome",
                value: value.to_string(),
            }),
        }
    }
}

impl fmt::Display for RunOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalReason {
    Completed,
    ExecutionFailure,
    Timeout,
    Cancellation,
    PolicyBlocked,
    RequiredDeliveryDeadLettered,
    ProcessDeath,
    EffectNonResumableUnknown,
}

impl TerminalReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::ExecutionFailure => "execution_failure",
            Self::Timeout => "timeout",
            Self::Cancellation => "cancellation",
            Self::PolicyBlocked => "policy_blocked",
            Self::RequiredDeliveryDeadLettered => "required_delivery_dead_lettered",
            Self::ProcessDeath => "process_death",
            Self::EffectNonResumableUnknown => "effect_non_resumable_unknown",
        }
    }

    pub const fn outcome(self) -> RunOutcome {
        match self {
            Self::Completed => RunOutcome::Success,
            Self::ExecutionFailure | Self::ProcessDeath | Self::EffectNonResumableUnknown => {
                RunOutcome::Failure
            }
            Self::Timeout => RunOutcome::Timeout,
            Self::Cancellation => RunOutcome::Cancellation,
            Self::PolicyBlocked => RunOutcome::Blocked,
            Self::RequiredDeliveryDeadLettered => RunOutcome::DeadLettered,
        }
    }

    pub(crate) fn from_db(value: &str) -> LedgerResult<Self> {
        match value {
            "completed" => Ok(Self::Completed),
            "execution_failure" => Ok(Self::ExecutionFailure),
            "timeout" => Ok(Self::Timeout),
            "cancellation" => Ok(Self::Cancellation),
            "policy_blocked" => Ok(Self::PolicyBlocked),
            "required_delivery_dead_lettered" => Ok(Self::RequiredDeliveryDeadLettered),
            "process_death" => Ok(Self::ProcessDeath),
            "effect_non_resumable_unknown" => Ok(Self::EffectNonResumableUnknown),
            _ => Err(LedgerError::InvalidStoredState {
                kind: "terminal reason",
                value: value.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactDegradationReason {
    SealingKeyUnavailable,
    ArtifactStoreUnavailable,
}

impl ArtifactDegradationReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SealingKeyUnavailable => "sealing_key_unavailable",
            Self::ArtifactStoreUnavailable => "artifact_store_unavailable",
        }
    }

    pub(crate) fn from_db(value: &str) -> LedgerResult<Self> {
        match value {
            "sealing_key_unavailable" => Ok(Self::SealingKeyUnavailable),
            "artifact_store_unavailable" => Ok(Self::ArtifactStoreUnavailable),
            _ => Err(LedgerError::InvalidStoredState {
                kind: "artifact degradation reason",
                value: value.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EffectIntent {
    None,
    AlphaRuntimeV1,
    /// A policy-allowlisted local operation governed by the separate
    /// non-resumable one-shot state machine, never by the outbox.
    LocalOneShotV1,
}

impl EffectIntent {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::AlphaRuntimeV1 => "alpha_runtime_v1",
            Self::LocalOneShotV1 => "local_one_shot_v1",
        }
    }

    pub const fn requires_sealed_artifact(self) -> bool {
        !matches!(self, Self::None)
    }

    pub(crate) fn from_db(value: &str) -> LedgerResult<Self> {
        match value {
            "none" => Ok(Self::None),
            "alpha_runtime_v1" => Ok(Self::AlphaRuntimeV1),
            "local_one_shot_v1" => Ok(Self::LocalOneShotV1),
            _ => Err(LedgerError::InvalidStoredState {
                kind: "effect intent",
                value: value.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Destination {
    AlphaRuntimeV1,
}

impl Destination {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AlphaRuntimeV1 => "alpha_runtime_v1",
        }
    }

    pub(crate) fn from_db(value: &str) -> LedgerResult<Self> {
        match value {
            "alpha_runtime_v1" => Ok(Self::AlphaRuntimeV1),
            _ => Err(LedgerError::InvalidStoredState {
                kind: "outbox destination",
                value: value.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionRunEventKind {
    RunStart,
    Append,
    DeliveryResult,
    RunExit,
    ManualAppendDegradedAudit,
    ArtifactStateAudit,
}

impl SessionRunEventKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RunStart => "run_start",
            Self::Append => "append",
            Self::DeliveryResult => "delivery_result",
            Self::RunExit => "run_exit",
            Self::ManualAppendDegradedAudit => "manual_append_degraded_audit",
            Self::ArtifactStateAudit => "artifact_state_audit",
        }
    }

    pub(crate) fn from_db(value: &str) -> LedgerResult<Self> {
        match value {
            "run_start" => Ok(Self::RunStart),
            "append" => Ok(Self::Append),
            "delivery_result" => Ok(Self::DeliveryResult),
            "run_exit" => Ok(Self::RunExit),
            "manual_append_degraded_audit" => Ok(Self::ManualAppendDegradedAudit),
            "artifact_state_audit" => Ok(Self::ArtifactStateAudit),
            _ => Err(LedgerError::InvalidStoredState {
                kind: "session run event kind",
                value: value.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OutboxState {
    Ready,
    Leased,
    Acknowledged,
    DeadLettered,
}

impl OutboxState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Leased => "leased",
            Self::Acknowledged => "acknowledged",
            Self::DeadLettered => "dead_lettered",
        }
    }

    pub(crate) fn from_db(value: &str) -> LedgerResult<Self> {
        match value {
            "ready" => Ok(Self::Ready),
            "leased" => Ok(Self::Leased),
            "acknowledged" => Ok(Self::Acknowledged),
            "dead_lettered" => Ok(Self::DeadLettered),
            _ => Err(LedgerError::InvalidStoredState {
                kind: "outbox state",
                value: value.to_string(),
            }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DeliveryFailureReason {
    MissingRoute,
    InvalidPayload,
    RouteDecodeFailed,
    InvalidVariant,
    SealedTaskMismatch,
    DurableStoreFailure,
    HandlerRejected,
    AttemptsExhausted,
}

impl DeliveryFailureReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MissingRoute => "missing_route",
            Self::InvalidPayload => "invalid_payload",
            Self::RouteDecodeFailed => "route_decode_failed",
            Self::InvalidVariant => "invalid_variant",
            Self::SealedTaskMismatch => "sealed_task_mismatch",
            Self::DurableStoreFailure => "durable_store_failure",
            Self::HandlerRejected => "handler_rejected",
            Self::AttemptsExhausted => "attempts_exhausted",
        }
    }

    pub(crate) fn from_db(value: &str) -> LedgerResult<Self> {
        match value {
            "missing_route" => Ok(Self::MissingRoute),
            "invalid_payload" => Ok(Self::InvalidPayload),
            "route_decode_failed" => Ok(Self::RouteDecodeFailed),
            "invalid_variant" => Ok(Self::InvalidVariant),
            "sealed_task_mismatch" => Ok(Self::SealedTaskMismatch),
            "durable_store_failure" => Ok(Self::DurableStoreFailure),
            "handler_rejected" => Ok(Self::HandlerRejected),
            "attempts_exhausted" => Ok(Self::AttemptsExhausted),
            _ => Err(LedgerError::InvalidStoredState {
                kind: "delivery failure reason",
                value: value.to_string(),
            }),
        }
    }
}

/// Typed, non-serialized execution admission observed before a run header is
/// committed. The closed variant can only be derived from an opaque containment
/// context that owns both a verified backend configuration and its executable
/// hardened plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RunExecutionCertification {
    /// Explicit host-local manual admission. Manual provenance must not claim a
    /// sandbox configuration digest.
    ManualHost,
    /// A closed mode was admitted against a configured, certified backend.
    ///
    /// This is deliberately configuration evidence only; it does not claim
    /// that any sandbox command has executed.
    CertifiedBackendConfigured {
        mode: ResolvedExecutionMode,
        backend_kind: VerifiedIsolatedBackendKind,
        scope: VerifiedIsolatedBackendScope,
        policy_version: u16,
        configuration_digest: Sha256Digest,
    },
}

impl RunExecutionCertification {
    pub(crate) fn from_containment_context(
        context: &ExecutionContainmentContext,
    ) -> LedgerResult<Self> {
        let mode = context.mode();
        if mode.mode == ExecutionMode::Manual {
            if !context.is_manual_host() || context.certificate().is_some() {
                return Err(LedgerError::InvalidExecutionCertification {
                    reason: "manual mode requires an explicit host-local containment context",
                });
            }
            return Ok(Self::ManualHost);
        }

        let certificate = context
            .certificate()
            .filter(|_| context.is_certified_dispatch_context())
            .ok_or(LedgerError::MissingExecutionCertification {
                mode: mode.mode.as_str(),
            })?;
        Ok(Self::CertifiedBackendConfigured {
            mode,
            backend_kind: certificate.backend_kind(),
            scope: certificate.scope(),
            policy_version: certificate.policy_version(),
            configuration_digest: Sha256Digest::new(certificate.configuration_digest())?,
        })
    }

    pub(crate) fn mode(&self) -> Option<ResolvedExecutionMode> {
        match self {
            Self::ManualHost => None,
            Self::CertifiedBackendConfigured { mode, .. } => Some(*mode),
        }
    }

    pub(crate) fn configuration_digest(&self) -> Option<&Sha256Digest> {
        match self {
            Self::ManualHost => None,
            Self::CertifiedBackendConfigured {
                configuration_digest,
                ..
            } => Some(configuration_digest),
        }
    }

    pub(crate) fn backend_kind(&self) -> Option<VerifiedIsolatedBackendKind> {
        match self {
            Self::ManualHost => None,
            Self::CertifiedBackendConfigured { backend_kind, .. } => Some(*backend_kind),
        }
    }

    pub(crate) fn scope(&self) -> Option<VerifiedIsolatedBackendScope> {
        match self {
            Self::ManualHost => None,
            Self::CertifiedBackendConfigured { scope, .. } => Some(*scope),
        }
    }

    pub(crate) fn policy_version(&self) -> Option<u16> {
        match self {
            Self::ManualHost => None,
            Self::CertifiedBackendConfigured { policy_version, .. } => Some(*policy_version),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunStart {
    pub run_id: RunId,
    pub provenance: RunProvenance,
    pub public_payload: CanonicalJson,
    execution_certification: RunExecutionCertification,
}

impl RunStart {
    pub fn new(run_id: RunId, provenance: RunProvenance, public_payload: CanonicalJson) -> Self {
        Self {
            run_id,
            provenance,
            public_payload,
            execution_certification: RunExecutionCertification::ManualHost,
        }
    }

    /// Bind this start to the actual containment context admitted by the
    /// runtime. The context is not serialized; its safe configured-backend
    /// projection is already bound into the immutable provenance.
    pub(crate) fn with_execution_containment_context(
        mut self,
        context: &ExecutionContainmentContext,
    ) -> LedgerResult<Self> {
        self.execution_certification =
            RunExecutionCertification::from_containment_context(context)?;
        Ok(self)
    }

    pub(crate) fn execution_certification(&self) -> &RunExecutionCertification {
        &self.execution_certification
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunOwnerLease {
    pub run_id: RunId,
    pub owner_id: RunOwnerId,
    pub lease_generation: u64,
    pub lease_expires_at_ms: i64,
    pub(crate) token: LeaseToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedRun {
    pub header: RunHeader,
    pub owner_lease: RunOwnerLease,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StaleRunRecovery {
    Live,
    Recovered(RunStatus),
    AlreadyFinalized(RunStatus),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunExit {
    pub terminal_reason: TerminalReason,
    pub public_payload: CanonicalJson,
}

impl RunExit {
    pub fn new(terminal_reason: TerminalReason, public_payload: CanonicalJson) -> Self {
        Self {
            terminal_reason,
            public_payload,
        }
    }
}

/// Typed binding between one append and the replay obligation it resolves.
///
/// A `None` source means the append's own event creates and resolves the
/// obligation. A result append instead resolves the pending obligation bound
/// to its already-committed request/intent event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplayAppendContract {
    pub operation: ReplayOperationKind,
    pub phase: ReplayOperationPhase,
    pub resolution_source_event_id: Option<EventId>,
}

impl ReplayAppendContract {
    pub const fn own_event(
        operation: ReplayOperationKind,
        phase: ReplayOperationPhase,
    ) -> Self {
        Self {
            operation,
            phase,
            resolution_source_event_id: None,
        }
    }

    pub const fn resolve_source(
        operation: ReplayOperationKind,
        phase: ReplayOperationPhase,
        source_event_id: EventId,
    ) -> Self {
        Self {
            operation,
            phase,
            resolution_source_event_id: Some(source_event_id),
        }
    }
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendRequest {
    pub run_id: RunId,
    pub append_key: AppendKey,
    pub identity: AppendIdentity,
    pub effect_intent: EffectIntent,
    pub public_payload: CanonicalJson,
    /// `Some` makes this append replay-critical and resolves the named
    /// obligation in the same transaction that appends its event/reference.
    pub replay_contract: Option<ReplayAppendContract>,
    pub replay_fidelity: ReplayArtifactFidelity,
}

impl AppendRequest {
    pub fn new(
        run_id: RunId,
        append_key: AppendKey,
        identity: AppendIdentity,
        effect_intent: EffectIntent,
        public_payload: CanonicalJson,
    ) -> Self {
        Self {
            run_id,
            append_key,
            identity,
            effect_intent,
            public_payload,
            replay_contract: None,
            replay_fidelity: ReplayArtifactFidelity::MetadataOnly,
        }
    }

    pub fn with_replay_contract(
        mut self,
        contract: ReplayAppendContract,
        fidelity: ReplayArtifactFidelity,
    ) -> Self {
        self.replay_contract = Some(contract);
        self.replay_fidelity = fidelity;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendLease {
    pub run_id: RunId,
    pub append_key: AppendKey,
    pub event_id: EventId,
    pub frozen_event_at_ms: i64,
    pub lease_generation: u64,
    pub lease_expires_at_ms: i64,
    pub published_sealed_artifact_ref: Option<SealedArtifactRef>,
    pub(crate) token: LeaseToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppendReceipt {
    pub run_id: RunId,
    pub event_id: EventId,
    pub event_sequence: u64,
    pub event_hash: String,
    pub event_kind: SessionRunEventKind,
    pub artifact_degradation_reason: Option<ArtifactDegradationReason>,
    pub sealed_artifact_ref: Option<SealedArtifactRef>,
    pub outbox_ids: Vec<OutboxRowId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppendReservation {
    Committed(AppendReceipt),
    Reserved(AppendLease),
    InFlight {
        run_id: RunId,
        event_id: EventId,
        lease_generation: u64,
        lease_expires_at_ms: i64,
        published_sealed_artifact_ref: Option<SealedArtifactRef>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NonResumableEffectState {
    Armed,
    Executing,
    Completed,
    Abandoned,
    UnknownNonResumable,
}

impl NonResumableEffectState {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Armed => "armed",
            Self::Executing => "executing",
            Self::Completed => "completed",
            Self::Abandoned => "abandoned",
            Self::UnknownNonResumable => "unknown_non_resumable",
        }
    }

    pub(crate) fn from_db(value: &str) -> LedgerResult<Self> {
        match value {
            "armed" => Ok(Self::Armed),
            "executing" => Ok(Self::Executing),
            "completed" => Ok(Self::Completed),
            "abandoned" => Ok(Self::Abandoned),
            "unknown_non_resumable" => Ok(Self::UnknownNonResumable),
            _ => Err(LedgerError::InvalidStoredState {
                kind: "non-resumable effect state",
                value: value.to_string(),
            }),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NonResumableEffectLease {
    pub run_id: RunId,
    pub event_id: EventId,
    pub idempotency_key: NonResumableIdempotencyKey,
    pub claim_binding: Sha256Digest,
    pub result_append_key: AppendKey,
    pub claimed_at_ms: i64,
    pub token: LeaseToken,
}


#[derive(Debug)]
pub struct NonResumableEffectAttempt {
    pub(crate) lease: NonResumableEffectLease,
    pub(crate) result: CanonicalJson,
}

impl NonResumableEffectAttempt {
    pub fn run_id(&self) -> RunId {
        self.lease.run_id
    }

    pub fn event_id(&self) -> EventId {
        self.lease.event_id
    }

    pub fn idempotency_key(&self) -> &NonResumableIdempotencyKey {
        &self.lease.idempotency_key
    }

    pub fn result_append_key(&self) -> &AppendKey {
        &self.lease.result_append_key
    }

    pub fn result(&self) -> &CanonicalJson {
        &self.result
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NonResumableEffectRecord {
    pub run_id: RunId,
    pub event_id: EventId,
    pub state: NonResumableEffectState,
    pub idempotency_key: NonResumableIdempotencyKey,
    pub claimed_at_ms: Option<i64>,
    pub result_payload_digest: Option<Sha256Digest>,
    pub result_event_id: Option<EventId>,
    pub terminal_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NonResumableEffectSnapshot {
    pub record: NonResumableEffectRecord,
    pub claim_binding: Option<Sha256Digest>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxLease {
    pub outbox_id: OutboxRowId,
    pub run_id: RunId,
    pub event_id: EventId,
    pub destination: Destination,
    pub idempotency_key: DownstreamIdempotencyKey,
    pub public_payload: CanonicalJson,
    pub sealed_artifact_ref: SealedArtifactRef,
    pub attempt_count: u32,
    pub lease_generation: u64,
    pub lease_expires_at_ms: i64,
    pub(crate) token: LeaseToken,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutboxRecord {
    pub outbox_id: OutboxRowId,
    pub run_id: RunId,
    pub event_id: EventId,
    pub destination: Destination,
    pub idempotency_key: DownstreamIdempotencyKey,
    pub required: bool,
    pub public_payload: CanonicalJson,
    pub sealed_artifact_ref: SealedArtifactRef,
    pub state: OutboxState,
    pub attempt_count: u32,
    pub max_attempts: u32,
    pub lease_generation: u64,
    pub delivery_receipt: Option<DurableDeliveryReceipt>,
    pub delivery_error: Option<DeliveryFailureReason>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClaimNext {
    Empty,
    Claimed(OutboxLease),
    Rejected {
        outbox_id: OutboxRowId,
        reason: DeliveryFailureReason,
        state: OutboxState,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryCompletion {
    Acknowledged(DurableDeliveryReceipt),
    Nacked(DeliveryFailureReason),
}
/// Public, validated binding for a sealed delivery-result artifact. The
/// artifact bytes remain in the authorized store; this type carries only the
/// identity commitment and exact reference that must be recorded in the same
/// transaction as an ACK or NACK.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryEvidence {
    pub identity: AppendIdentity,
    pub sealed_artifact_ref: SealedArtifactRef,
    /// This is explicit so a caller cannot accidentally promote a generic
    /// credential-redacted delivery artifact into exact replay evidence.
    pub replay_fidelity: ReplayArtifactFidelity,
}

impl DeliveryEvidence {
    /// Compatibility construction for non-exact delivery evidence. It can
    /// preserve delivery history, but always downgrades exact replay.
    pub fn new(identity: AppendIdentity, sealed_artifact_ref: SealedArtifactRef) -> Self {
        Self {
            identity,
            sealed_artifact_ref,
            replay_fidelity: ReplayArtifactFidelity::MetadataOnly,
        }
    }

    /// Construct delivery evidence only after the caller has passed the
    /// closed, credential-free exact private artifact preparation gate.
    pub fn exact_private(identity: AppendIdentity, sealed_artifact_ref: SealedArtifactRef) -> Self {
        Self {
            identity,
            sealed_artifact_ref,
            replay_fidelity: ReplayArtifactFidelity::ExactPrivate,
        }
    }
}


#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeliveryCompletionReceipt {
    pub outbox_id: OutboxRowId,
    pub state: OutboxState,
    pub run_status: RunStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunHeader {
    pub run_id: RunId,
    pub start_event_id: EventId,
    pub execution_mode: ResolvedExecutionMode,
    pub started_at_ms: i64,
    pub owner_id: RunOwnerId,
    pub owner_lease_generation: u64,
    pub owner_lease_expires_at_ms: i64,
    pub provenance: RunProvenance,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunStatus {
    pub run_id: RunId,
    pub exit_event_id: Option<EventId>,
    pub exit_terminal_reason: Option<TerminalReason>,
    pub candidate_outcome: Option<RunOutcome>,
    pub final_outcome: Option<RunOutcome>,
    pub required_dead_lettered: bool,
    pub pending_required_delivery: bool,
    /// Durable exact replay state; this is not a public replay projection.
    pub exact_replay: ExactReplayStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SessionRunEvent {
    pub event_id: EventId,
    pub run_id: RunId,
    pub sequence: u64,
    pub kind: SessionRunEventKind,
    pub append_key: Option<AppendKey>,
    pub effect_intent: EffectIntent,
    pub artifact_degradation_reason: Option<ArtifactDegradationReason>,
    pub public_identity: Option<CanonicalJson>,
    pub private_fingerprint: Option<PrivateFingerprint>,
    pub sealed_artifact_ref: Option<SealedArtifactRef>,
    pub run_provenance: Option<CanonicalJson>,
    pub public_payload: CanonicalJson,
    pub manifest_hash: String,
    pub previous_hash: String,
    pub event_hash: String,
    pub created_at_ms: i64,
}

pub(crate) fn execution_mode_to_db(mode: ExecutionMode) -> &'static str {
    mode.as_str()
}

pub(crate) fn execution_mode_source_to_db(source: ExecutionModeSource) -> &'static str {
    source.as_str()
}

pub(crate) fn execution_mode_from_db(
    mode: &str,
    source: &str,
) -> LedgerResult<ResolvedExecutionMode> {
    let mode = mode
        .parse::<ExecutionMode>()
        .map_err(|_| LedgerError::InvalidStoredState {
            kind: "execution mode",
            value: mode.to_string(),
        })?;
    let source = match source {
        "built_in_default" => ExecutionModeSource::BuiltInDefault,
        "profile" => ExecutionModeSource::Profile,
        "config_file" => ExecutionModeSource::ConfigFile,
        "environment" => ExecutionModeSource::Environment,
        "cli" => ExecutionModeSource::Cli,
        _ => {
            return Err(LedgerError::InvalidStoredState {
                kind: "execution mode source",
                value: source.to_string(),
            });
        }
    };
    Ok(ResolvedExecutionMode { mode, source })
}

fn validate_nonempty_text(
    kind: &'static str,
    value: &str,
    maximum_length: usize,
) -> LedgerResult<()> {
    if value.trim().is_empty()
        || value.len() > maximum_length
        || value.chars().any(|character| character.is_control())
    {
        return Err(LedgerError::InvalidIdentifier {
            kind,
            value: value.to_string(),
        });
    }
    Ok(())
}

fn validate_opaque_identifier(
    kind: &'static str,
    value: &str,
    maximum_length: usize,
) -> LedgerResult<()> {
    let valid = !value.is_empty()
        && value.len() <= maximum_length
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/' | b'@' | b'+' | b'=')
        });
    if !valid {
        return Err(LedgerError::InvalidIdentifier {
            kind,
            value: value.to_string(),
        });
    }
    Ok(())
}

fn validate_lowercase_sha256(kind: &'static str, value: &str) -> LedgerResult<()> {
    if value.len() != 64
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(LedgerError::InvalidDigest {
            kind,
            value: value.to_string(),
        });
    }
    Ok(())
}

fn decode_lowercase_32_hex(kind: &'static str, value: &str) -> LedgerResult<[u8; 32]> {
    validate_lowercase_sha256(kind, value)?;
    let bytes = hex::decode(value).map_err(|_| LedgerError::InvalidDigest {
        kind,
        value: value.to_string(),
    })?;
    bytes.try_into().map_err(|_| LedgerError::InvalidDigest {
        kind,
        value: value.to_string(),
    })
}

fn nonzero_key_version(value: u32) -> LedgerResult<u32> {
    if value == 0 {
        return Err(LedgerError::InvalidStoredState {
            kind: "private fingerprint key version",
            value: "0".to_string(),
        });
    }
    Ok(value)
}

fn parse_run_id(value: String) -> LedgerResult<RunId> {
    value.parse().map_err(|_| LedgerError::InvalidStoredState {
        kind: "parent run id",
        value,
    })
}
