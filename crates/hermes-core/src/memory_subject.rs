//! Tenant-scoped memory identity and opaque provider partition keys.
//!
//! Raw authenticated identifiers remain in process memory only. Providers and
//! durable state receive the keyed [`MemorySubjectStorageKey`] instead.

use std::fmt;

use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

type HmacSha256 = Hmac<Sha256>;

/// Version of the canonical memory subject encoding and its opaque key format.
pub const MEMORY_SUBJECT_KEY_VERSION: u8 = 1;
const MAX_IDENTIFIER_BYTES: usize = 256;
const STORAGE_KEY_PREFIX: &str = "msp1_";
const ACTOR_KEY_PREFIX: &str = "maa1_";
const EXPORT_ARTIFACT_REF_PREFIX: &str = "mea1_";
const EXPORT_DIGEST_PREFIX: &str = "sha256:";

/// Validation failures for authenticated memory identity.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MemorySubjectError {
    #[error("memory {field} is required")]
    MissingAuthenticatedField { field: &'static str },
    #[error("memory {field} must not be empty")]
    EmptyIdentifier { field: &'static str },
    #[error("memory {field} must not have surrounding whitespace")]
    SurroundingWhitespace { field: &'static str },
    #[error("memory {field} exceeds the maximum length")]
    IdentifierTooLong { field: &'static str },
    #[error("memory {field} contains an unsafe delimiter or character")]
    UnsafeIdentifier { field: &'static str },
    #[error("team visibility requires a team id")]
    MissingTeamForTeamVisibility,
}

macro_rules! memory_identifier {
    ($name:ident, $field:literal) => {
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, MemorySubjectError> {
                let value = value.into();
                validate_identifier($field, &value)?;
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
                    .field(&"[REDACTED]")
                    .finish()
            }
        }
    };
}

memory_identifier!(MemoryTenantId, "tenant id");
memory_identifier!(MemoryPrincipalId, "principal id");
memory_identifier!(MemoryTeamId, "team id");
memory_identifier!(MemorySessionId, "session id");

/// Authenticated surface that constructed a memory subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemorySurface {
    Gateway,
    Cli,
    Http,
    Acp,
    Cron,
    Local,
}

impl MemorySurface {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Cli => "cli",
            Self::Http => "http",
            Self::Acp => "acp",
            Self::Cron => "cron",
            Self::Local => "local",
        }
    }

    const fn discriminant(self) -> u8 {
        match self {
            Self::Gateway => 1,
            Self::Cli => 2,
            Self::Http => 3,
            Self::Acp => 4,
            Self::Cron => 5,
            Self::Local => 6,
        }
    }
}

/// Visibility explicitly requested for the memory subject.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryVisibility {
    Private,
    Team,
}

impl MemoryVisibility {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Private => "private",
            Self::Team => "team",
        }
    }

    const fn discriminant(self) -> u8 {
        match self {
            Self::Private => 1,
            Self::Team => 2,
        }
    }
}

/// Closed, validated authenticated memory identity.
///
/// This type intentionally does not implement `Serialize` or `Display`: raw
/// authenticated IDs must not become a filesystem or remote-provider key by
/// accident. Use [`MemorySubjectStorageKey`] for durable/provider partitioning.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct MemorySubject {
    tenant_id: MemoryTenantId,
    principal_id: MemoryPrincipalId,
    team_id: Option<MemoryTeamId>,
    session_id: MemorySessionId,
    surface: MemorySurface,
    visibility: MemoryVisibility,
}

impl fmt::Debug for MemorySubject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemorySubject")
            .field("tenant_id", &"[REDACTED]")
            .field("principal_id", &"[REDACTED]")
            .field("has_team_id", &self.team_id.is_some())
            .field("session_id", &"[REDACTED]")
            .field("surface", &self.surface)
            .field("visibility", &self.visibility)
            .finish()
    }
}

impl MemorySubject {
    pub fn new(
        tenant_id: MemoryTenantId,
        principal_id: MemoryPrincipalId,
        team_id: Option<MemoryTeamId>,
        session_id: MemorySessionId,
        surface: MemorySurface,
        visibility: MemoryVisibility,
    ) -> Result<Self, MemorySubjectError> {
        if visibility == MemoryVisibility::Team && team_id.is_none() {
            return Err(MemorySubjectError::MissingTeamForTeamVisibility);
        }
        Ok(Self {
            tenant_id,
            principal_id,
            team_id,
            session_id,
            surface,
            visibility,
        })
    }

    /// Construct a gateway subject only from authenticated ingress fields.
    ///
    /// Gateway group traffic remains principal-private by default. A group can
    /// carry a team id for explicit later sharing, but this constructor never
    /// silently chooses team visibility.
    pub fn from_authenticated_gateway(
        tenant_id: Option<&str>,
        principal_id: Option<&str>,
        team_id: Option<&str>,
        session_id: Option<&str>,
        is_dm: bool,
    ) -> Result<Self, MemorySubjectError> {
        let tenant_id = MemoryTenantId::new(required_authenticated("tenant id", tenant_id)?)?;
        let principal_id =
            MemoryPrincipalId::new(required_authenticated("principal id", principal_id)?)?;
        let session_id = MemorySessionId::new(required_authenticated("session id", session_id)?)?;
        let team_id = team_id.map(MemoryTeamId::new).transpose()?;
        // Direct messages remain private even when the authenticated ingress
        // supplies an ambient team namespace. Team identity is never itself a
        // request to share memory.
        let _ = is_dm;
        Self::new(
            tenant_id,
            principal_id,
            team_id,
            session_id,
            MemorySurface::Gateway,
            MemoryVisibility::Private,
        )
    }

    /// Construct an explicit single-owner subject for a local execution surface.
    ///
    /// The caller must still supply a real session id; no AgentConfig/session
    /// fallback is used by this constructor.
    pub fn local_owner(
        surface: MemorySurface,
        session_id: impl Into<String>,
    ) -> Result<Self, MemorySubjectError> {
        Self::new(
            MemoryTenantId::new("local-owner")?,
            MemoryPrincipalId::new("local-owner")?,
            None,
            MemorySessionId::new(session_id)?,
            surface,
            MemoryVisibility::Private,
        )
    }

    pub fn tenant_id(&self) -> &MemoryTenantId {
        &self.tenant_id
    }

    pub fn principal_id(&self) -> &MemoryPrincipalId {
        &self.principal_id
    }

    pub fn team_id(&self) -> Option<&MemoryTeamId> {
        self.team_id.as_ref()
    }

    pub fn session_id(&self) -> &MemorySessionId {
        &self.session_id
    }

    pub const fn surface(&self) -> MemorySurface {
        self.surface
    }

    pub const fn visibility(&self) -> MemoryVisibility {
        self.visibility
    }

    /// Derive the stable, opaque versioned storage/filter key for this subject.
    pub fn storage_key(&self, key: &MemorySubjectDerivationKey) -> MemorySubjectStorageKey {
        MemorySubjectStorageKey::from_digest(
            STORAGE_KEY_PREFIX,
            key.derive(b"storage", &self.canonical_bytes()),
        )
    }

    fn canonical_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(
            self.tenant_id.as_str().len()
                + self.principal_id.as_str().len()
                + self.team_id.as_ref().map_or(0, |team| team.as_str().len())
                + self.session_id.as_str().len()
                + 32,
        );
        bytes.extend_from_slice(b"hermes-memory-subject");
        bytes.push(MEMORY_SUBJECT_KEY_VERSION);
        bytes.push(self.surface.discriminant());
        bytes.push(self.visibility.discriminant());
        append_canonical_field(&mut bytes, self.tenant_id.as_str().as_bytes());
        append_canonical_field(&mut bytes, self.principal_id.as_str().as_bytes());
        match &self.team_id {
            Some(team_id) => {
                bytes.push(1);
                append_canonical_field(&mut bytes, team_id.as_str().as_bytes());
            }
            None => bytes.push(0),
        }
        append_canonical_field(&mut bytes, self.session_id.as_str().as_bytes());
        bytes
    }
}

/// Secret material used only to derive opaque memory partition identifiers.
///
/// The manager provisions this locally in an owner-only state file. It is never
/// serialized, displayed, or sent to a provider.
pub struct MemorySubjectDerivationKey([u8; 32]);

impl MemorySubjectDerivationKey {
    pub const fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn actor_for_subject(&self, subject: &MemorySubject) -> MemoryAuditActor {
        let subject_identity = subject.canonical_bytes();
        let mut bytes = Vec::with_capacity(
            b"hermes-memory-actor".len() + 1 + std::mem::size_of::<u32>() + subject_identity.len(),
        );
        bytes.extend_from_slice(b"hermes-memory-actor");
        bytes.push(MEMORY_SUBJECT_KEY_VERSION);
        append_canonical_field(&mut bytes, &subject_identity);
        MemoryAuditActor(MemorySubjectStorageKey::from_digest(
            ACTOR_KEY_PREFIX,
            self.derive(b"actor", &bytes),
        ))
    }

    fn derive(&self, domain: &[u8], bytes: &[u8]) -> [u8; 32] {
        let mut mac = HmacSha256::new_from_slice(&self.0)
            .expect("a fixed-size HMAC-SHA256 key is always valid");
        mac.update(b"hermes-memory-subject-key");
        mac.update(&[MEMORY_SUBJECT_KEY_VERSION]);
        mac.update(&(domain.len() as u32).to_be_bytes());
        mac.update(domain);
        mac.update(bytes);
        let digest = mac.finalize().into_bytes();
        let mut out = [0_u8; 32];
        out.copy_from_slice(&digest);
        out
    }
}

impl Drop for MemorySubjectDerivationKey {
    fn drop(&mut self) {
        self.0.fill(0);
    }
}

impl fmt::Debug for MemorySubjectDerivationKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MemorySubjectDerivationKey([REDACTED])")
    }
}

/// Opaque, versioned provider storage and remote-filter key.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct MemorySubjectStorageKey(String);

impl MemorySubjectStorageKey {
    fn from_digest(prefix: &str, digest: [u8; 32]) -> Self {
        let mut value = String::with_capacity(prefix.len() + digest.len() * 2);
        value.push_str(prefix);
        for byte in digest {
            const HEX: &[u8; 16] = b"0123456789abcdef";
            value.push(HEX[(byte >> 4) as usize] as char);
            value.push(HEX[(byte & 0x0f) as usize] as char);
        }
        Self(value)
    }

    fn parse(value: String) -> Result<Self, &'static str> {
        Self::parse_with_prefix(value, STORAGE_KEY_PREFIX)
    }

    fn parse_with_prefix(value: String, prefix: &str) -> Result<Self, &'static str> {
        let suffix = value
            .strip_prefix(prefix)
            .ok_or("memory key is not a canonical opaque key")?;
        if suffix.len() != 64
            || !suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err("memory key is not a canonical opaque key");
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for MemorySubjectStorageKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(value).map_err(serde::de::Error::custom)
    }
}

impl fmt::Debug for MemorySubjectStorageKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("MemorySubjectStorageKey")
            .field(&self.0)
            .finish()
    }
}

impl fmt::Display for MemorySubjectStorageKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Opaque actor identity suitable for content-free durable audit records.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct MemoryAuditActor(MemorySubjectStorageKey);

impl MemoryAuditActor {
    pub fn opaque_id(&self) -> &MemorySubjectStorageKey {
        &self.0
    }
}

impl<'de> Deserialize<'de> for MemoryAuditActor {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let key = MemorySubjectStorageKey::parse_with_prefix(value, ACTOR_KEY_PREFIX)
            .map_err(serde::de::Error::custom)?;
        Ok(Self(key))
    }
}

impl fmt::Debug for MemoryAuditActor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("MemoryAuditActor")
            .field(&self.0)
            .finish()
    }
}

/// Provider governance operation used for typed errors and receipts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryGovernanceOperation {
    Write,
    Export,
    Forget,
    RetentionSweep,
    Readback,
}

impl MemoryGovernanceOperation {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Write => "write",
            Self::Export => "export",
            Self::Forget => "forget",
            Self::RetentionSweep => "retention_sweep",
            Self::Readback => "readback",
        }
    }
}

/// Typed provider governance failure. `Unsupported` is never an empty success.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MemoryProviderGovernanceError {
    #[error("memory provider {provider} does not support {operation:?}")]
    Unsupported {
        provider: String,
        operation: MemoryGovernanceOperation,
    },
    #[error("memory provider {provider} could not complete {operation:?}")]
    Incomplete {
        provider: String,
        operation: MemoryGovernanceOperation,
    },
    #[error("memory provider {provider} failed {operation:?}")]
    Failed {
        provider: String,
        operation: MemoryGovernanceOperation,
    },
}

impl MemoryProviderGovernanceError {
    pub fn unsupported(provider: impl Into<String>, operation: MemoryGovernanceOperation) -> Self {
        Self::Unsupported {
            provider: provider.into(),
            operation,
        }
    }

    pub fn incomplete(provider: impl Into<String>, operation: MemoryGovernanceOperation) -> Self {
        Self::Incomplete {
            provider: provider.into(),
            operation,
        }
    }

    pub fn failed(provider: impl Into<String>, operation: MemoryGovernanceOperation) -> Self {
        Self::Failed {
            provider: provider.into(),
            operation,
        }
    }
}

/// Explicit retention limits passed to each provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRetentionPolicy {
    pub max_age_days: Option<u32>,
    pub max_records: Option<u64>,
}

impl MemoryRetentionPolicy {
    pub fn new(
        max_age_days: Option<u32>,
        max_records: Option<u64>,
    ) -> Result<Self, MemorySubjectError> {
        if max_age_days.is_none() && max_records.is_none() {
            return Err(MemorySubjectError::EmptyIdentifier {
                field: "retention policy",
            });
        }
        if matches!(max_age_days, Some(0)) || matches!(max_records, Some(0)) {
            return Err(MemorySubjectError::UnsafeIdentifier {
                field: "retention policy",
            });
        }
        Ok(Self {
            max_age_days,
            max_records,
        })
    }
}

/// Content-free failure returned by a transactional export sink.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum MemoryExportSinkError {
    #[error("memory export sink rejected the stream")]
    Rejected,
    #[error("memory export transaction is incomplete")]
    Incomplete,
    #[error("memory export artifact is unavailable")]
    ArtifactUnavailable,
}

/// Canonical SHA-256 digest of opaque snapshot, cursor, record-ID, or manifest
/// evidence. The source value is never retained in receipts or audit records.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct MemoryExportEvidenceDigest(String);

impl MemoryExportEvidenceDigest {
    pub fn sha256_bytes(value: &[u8]) -> Self {
        let digest = Sha256::digest(value);
        Self(format_export_digest(&digest))
    }

    /// Wrap a precomputed SHA-256 output without hashing it again.
    pub fn from_sha256_bytes(digest: [u8; 32]) -> Self {
        Self(format_export_digest(&digest))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn parse(value: String) -> Result<Self, MemoryExportSinkError> {
        if is_canonical_export_digest(&value) {
            Ok(Self(value))
        } else {
            Err(MemoryExportSinkError::Rejected)
        }
    }
}

impl<'de> Deserialize<'de> for MemoryExportEvidenceDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::parse(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl fmt::Debug for MemoryExportEvidenceDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("MemoryExportEvidenceDigest")
            .field(&self.0)
            .finish()
    }
}

/// Opaque reference to an owner-only committed memory export manifest.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct MemoryExportArtifactRef(String);

impl MemoryExportArtifactRef {
    pub fn from_opaque(value: impl Into<String>) -> Result<Self, MemoryExportSinkError> {
        let value = value.into();
        let suffix = value
            .strip_prefix(EXPORT_ARTIFACT_REF_PREFIX)
            .ok_or(MemoryExportSinkError::Rejected)?;
        if suffix.len() != 64
            || !suffix
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
        {
            return Err(MemoryExportSinkError::Rejected);
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl<'de> Deserialize<'de> for MemoryExportArtifactRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::from_opaque(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl fmt::Debug for MemoryExportArtifactRef {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("MemoryExportArtifactRef")
            .field(&self.0)
            .finish()
    }
}

/// Content-free metadata for a fully committed export artifact. The opaque
/// reference resolves only through the manager-owned owner-only artifact store.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryExportArtifact {
    pub artifact_ref: MemoryExportArtifactRef,
    pub manifest_digest: MemoryExportEvidenceDigest,
    pub record_count: u64,
    pub page_count: u64,
    pub provider_count: u32,
}

impl MemoryExportArtifact {
    pub fn new(
        artifact_ref: MemoryExportArtifactRef,
        manifest_digest: MemoryExportEvidenceDigest,
        record_count: u64,
        page_count: u64,
        provider_count: u32,
    ) -> Self {
        Self {
            artifact_ref,
            manifest_digest,
            record_count,
            page_count,
            provider_count,
        }
    }
}

/// Stable, content-free proof of the source snapshot used by one provider.
///
/// Providers hash raw remote revision/snapshot/cursor tokens locally with
/// [`MemoryExportEvidenceDigest::sha256_bytes`]. Raw tokens never enter the
/// sink, artifact manifest, receipt, or audit log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryExportSnapshot {
    pub snapshot_digest: MemoryExportEvidenceDigest,
    pub revision_digest: MemoryExportEvidenceDigest,
    pub initial_cursor: Option<MemoryExportEvidenceDigest>,
}

impl MemoryExportSnapshot {
    pub fn new(
        snapshot_digest: MemoryExportEvidenceDigest,
        revision_digest: MemoryExportEvidenceDigest,
        initial_cursor: Option<MemoryExportEvidenceDigest>,
    ) -> Self {
        Self {
            snapshot_digest,
            revision_digest,
            initial_cursor,
        }
    }
}

/// One ordered page of actual exported records and content-free page evidence.
///
/// `record_ids` must contain exactly one stable digest for each record. The
/// sink rejects duplicate IDs, cursor loops, and pages that do not follow the
/// previous page's opaque cursor.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryExportPage {
    pub cursor: Option<MemoryExportEvidenceDigest>,
    pub next_cursor: Option<MemoryExportEvidenceDigest>,
    pub record_ids: Vec<MemoryExportEvidenceDigest>,
    pub records: Vec<Value>,
}

impl MemoryExportPage {
    pub fn new(
        cursor: Option<MemoryExportEvidenceDigest>,
        next_cursor: Option<MemoryExportEvidenceDigest>,
        record_ids: Vec<MemoryExportEvidenceDigest>,
        records: Vec<Value>,
    ) -> Result<Self, MemoryExportSinkError> {
        if record_ids.len() != records.len() {
            return Err(MemoryExportSinkError::Rejected);
        }
        Ok(Self {
            cursor,
            next_cursor,
            record_ids,
            records,
        })
    }
}

impl fmt::Debug for MemoryExportPage {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MemoryExportPage")
            .field("cursor", &self.cursor)
            .field("next_cursor", &self.next_cursor)
            .field("record_count", &self.records.len())
            .finish()
    }
}

/// Completion claim for one provider stream. A successful provider must use
/// [`Self::exhausted`]; the sink independently verifies both counters and the
/// terminal cursor before the manager may commit an artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryExportCompletion {
    pub exported_records: u64,
    pub page_count: u64,
    pub exhausted: bool,
}

impl MemoryExportCompletion {
    pub const fn exhausted(exported_records: u64, page_count: u64) -> Self {
        Self {
            exported_records,
            page_count,
            exhausted: true,
        }
    }
}

/// Transactional, provider-scoped sink for actual memory-export pages.
///
/// A manager starts one owner-only transaction with its authenticated subject
/// and full expected provider set. Each provider receives only a scoped sink,
/// calls `begin_provider`, streams ordered pages, and calls `finish_provider`.
/// Only the manager owns `commit`; a provider attempting to commit its scoped
/// view receives [`MemoryExportSinkError::Incomplete`].
pub trait MemoryExportSink: Send {
    fn begin_provider(
        &mut self,
        snapshot: MemoryExportSnapshot,
    ) -> Result<(), MemoryExportSinkError>;

    fn append_page(&mut self, page: MemoryExportPage) -> Result<(), MemoryExportSinkError>;

    fn finish_provider(
        &mut self,
        completion: MemoryExportCompletion,
    ) -> Result<(), MemoryExportSinkError>;

    fn abort(&mut self);

    fn commit(self: Box<Self>) -> Result<MemoryExportArtifact, MemoryExportSinkError>;
}

/// Receipt returned by a provider after it has exhausted a stable,
/// evidence-bound export stream. `artifact` is attached only by the manager
/// after the transaction's manifest has committed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryExportReceipt {
    pub provider: String,
    pub subject_key: MemorySubjectStorageKey,
    pub snapshot: MemoryExportSnapshot,
    pub exported_records: u64,
    pub page_count: u64,
    pub exhausted: bool,
    pub artifact: Option<MemoryExportArtifact>,
}

impl MemoryExportReceipt {
    pub fn exhausted(
        provider: impl Into<String>,
        subject_key: MemorySubjectStorageKey,
        snapshot: MemoryExportSnapshot,
        exported_records: u64,
        page_count: u64,
    ) -> Self {
        Self {
            provider: provider.into(),
            subject_key,
            snapshot,
            exported_records,
            page_count,
            exhausted: true,
            artifact: None,
        }
    }

    pub fn with_artifact(mut self, artifact: MemoryExportArtifact) -> Self {
        self.artifact = Some(artifact);
        self
    }
}

/// Receipt returned by a provider after deleting the subject's records.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryForgetReceipt {
    pub provider: String,
    pub subject_key: MemorySubjectStorageKey,
    pub deleted_records: u64,
}

impl MemoryForgetReceipt {
    pub fn new(
        provider: impl Into<String>,
        subject_key: MemorySubjectStorageKey,
        deleted_records: u64,
    ) -> Self {
        Self {
            provider: provider.into(),
            subject_key,
            deleted_records,
        }
    }
}

/// Receipt returned by a provider after applying a subject-scoped retention policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryRetentionReceipt {
    pub provider: String,
    pub subject_key: MemorySubjectStorageKey,
    pub deleted_records: u64,
}

impl MemoryRetentionReceipt {
    pub fn new(
        provider: impl Into<String>,
        subject_key: MemorySubjectStorageKey,
        deleted_records: u64,
    ) -> Self {
        Self {
            provider: provider.into(),
            subject_key,
            deleted_records,
        }
    }
}

/// Readback receipt proving whether the subject still has provider residue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryReadbackReceipt {
    pub provider: String,
    pub subject_key: MemorySubjectStorageKey,
    pub residual_records: u64,
}

impl MemoryReadbackReceipt {
    pub fn absent(provider: impl Into<String>, subject_key: MemorySubjectStorageKey) -> Self {
        Self {
            provider: provider.into(),
            subject_key,
            residual_records: 0,
        }
    }

    pub fn residual(
        provider: impl Into<String>,
        subject_key: MemorySubjectStorageKey,
        residual_records: u64,
    ) -> Self {
        Self {
            provider: provider.into(),
            subject_key,
            residual_records,
        }
    }

    pub const fn is_absent(&self) -> bool {
        self.residual_records == 0
    }
}

fn required_authenticated<'a>(
    field: &'static str,
    value: Option<&'a str>,
) -> Result<&'a str, MemorySubjectError> {
    value.ok_or(MemorySubjectError::MissingAuthenticatedField { field })
}

fn validate_identifier(field: &'static str, value: &str) -> Result<(), MemorySubjectError> {
    if value.is_empty() {
        return Err(MemorySubjectError::EmptyIdentifier { field });
    }
    if value.trim() != value {
        return Err(MemorySubjectError::SurroundingWhitespace { field });
    }
    if value.len() > MAX_IDENTIFIER_BYTES {
        return Err(MemorySubjectError::IdentifierTooLong { field });
    }
    if value.chars().any(|character| {
        character.is_control()
            || character.is_whitespace()
            || matches!(character, ':' | '/' | '\\' | '|' | ';')
    }) {
        return Err(MemorySubjectError::UnsafeIdentifier { field });
    }
    Ok(())
}

fn append_canonical_field(target: &mut Vec<u8>, value: &[u8]) {
    target.extend_from_slice(&(value.len() as u32).to_be_bytes());
    target.extend_from_slice(value);
}

fn format_export_digest(bytes: &[u8]) -> String {
    let mut value = String::with_capacity(EXPORT_DIGEST_PREFIX.len() + bytes.len() * 2);
    value.push_str(EXPORT_DIGEST_PREFIX);
    for byte in bytes {
        const HEX: &[u8; 16] = b"0123456789abcdef";
        value.push(HEX[(byte >> 4) as usize] as char);
        value.push(HEX[(byte & 0x0f) as usize] as char);
    }
    value
}

fn is_canonical_export_digest(value: &str) -> bool {
    let Some(suffix) = value.strip_prefix(EXPORT_DIGEST_PREFIX) else {
        return false;
    };
    suffix.len() == 64
        && suffix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn subject(principal: &str, team: Option<&str>, session: &str) -> MemorySubject {
        MemorySubject::new(
            MemoryTenantId::new("tenant-a").expect("tenant"),
            MemoryPrincipalId::new(principal).expect("principal"),
            team.map(MemoryTeamId::new).transpose().expect("team"),
            MemorySessionId::new(session).expect("session"),
            MemorySurface::Gateway,
            MemoryVisibility::Private,
        )
        .expect("subject")
    }

    #[test]
    fn storage_keys_are_opaque_stable_and_partition_users() {
        let key = MemorySubjectDerivationKey::from_bytes([7; 32]);
        let alice = subject("alice@example.test", None, "session-a");
        let bob = subject("bob@example.test", None, "session-a");
        let alice_key = alice.storage_key(&key);
        assert_eq!(alice_key, alice.storage_key(&key));
        assert_ne!(alice_key, bob.storage_key(&key));
        assert!(alice_key.as_str().starts_with(STORAGE_KEY_PREFIX));
        assert!(!alice_key.as_str().contains("alice"));
        assert!(!alice_key.as_str().contains("tenant-a"));
    }

    #[test]
    fn opaque_key_deserialization_accepts_only_canonical_key_forms() {
        let derivation = MemorySubjectDerivationKey::from_bytes([9; 32]);
        let subject = subject("alice", None, "session");
        let storage_key = subject.storage_key(&derivation);
        let encoded = serde_json::to_string(&storage_key).expect("serialize storage key");
        assert_eq!(
            serde_json::from_str::<MemorySubjectStorageKey>(&encoded)
                .expect("deserialize storage key"),
            storage_key
        );
        for invalid in [
            "\"../../alice\"",
            "\"msp1_deadbeef\"",
            "\"msp2_0000000000000000000000000000000000000000000000000000000000000000\"",
            "\"msp1_A000000000000000000000000000000000000000000000000000000000000000\"",
        ] {
            assert!(
                serde_json::from_str::<MemorySubjectStorageKey>(invalid).is_err(),
                "{invalid}"
            );
        }

        let actor = derivation.actor_for_subject(&subject);
        let actor_encoded = serde_json::to_string(&actor).expect("serialize actor");
        assert!(serde_json::from_str::<MemoryAuditActor>(&actor_encoded).is_ok());
        assert!(serde_json::from_str::<MemorySubjectStorageKey>(&actor_encoded).is_err());
        assert!(serde_json::from_str::<MemoryAuditActor>(&encoded).is_err());
    }

    #[test]
    fn audit_actors_are_stable_and_bound_to_full_subject_identity() {
        let derivation = MemorySubjectDerivationKey::from_bytes([11; 32]);
        let alice_session_a = subject("alice", None, "session-a");
        let alice_session_a_repeat = subject("alice", None, "session-a");
        let alice_session_b = subject("alice", None, "session-b");

        assert_eq!(
            derivation.actor_for_subject(&alice_session_a),
            derivation.actor_for_subject(&alice_session_a_repeat)
        );
        assert_ne!(
            derivation.actor_for_subject(&alice_session_a),
            derivation.actor_for_subject(&alice_session_b)
        );
    }

    #[test]
    fn gateway_subject_is_private_for_dm_and_group() {
        let dm = MemorySubject::from_authenticated_gateway(
            Some("tenant"),
            Some("alice"),
            None,
            Some("session"),
            true,
        )
        .expect("dm");
        let group = MemorySubject::from_authenticated_gateway(
            Some("tenant"),
            Some("alice"),
            Some("team"),
            Some("session"),
            false,
        )
        .expect("group");
        assert_eq!(dm.visibility(), MemoryVisibility::Private);
        assert_eq!(group.visibility(), MemoryVisibility::Private);
        assert_eq!(group.team_id().map(MemoryTeamId::as_str), Some("team"));
        let team_scoped_dm = MemorySubject::from_authenticated_gateway(
            Some("tenant"),
            Some("alice"),
            Some("team"),
            Some("session"),
            true,
        )
        .expect("team-scoped dm");
        assert_eq!(team_scoped_dm.visibility(), MemoryVisibility::Private);
        assert_eq!(
            team_scoped_dm.team_id().map(MemoryTeamId::as_str),
            Some("team")
        );
    }

    #[test]
    fn unsafe_or_empty_authenticated_ids_fail_closed() {
        for value in ["", " ", "alice:admin", "a/b", "a|b", "a\\b", "a\nb"] {
            assert!(MemoryPrincipalId::new(value).is_err(), "{value:?}");
        }
        assert!(matches!(
            MemorySubject::from_authenticated_gateway(
                Some("tenant"),
                None,
                None,
                Some("session"),
                true,
            ),
            Err(MemorySubjectError::MissingAuthenticatedField {
                field: "principal id"
            })
        ));
    }

    #[test]
    fn private_group_team_context_and_principal_remain_distinct_partitions() {
        let derivation = MemorySubjectDerivationKey::from_bytes([11; 32]);
        let alice_team_a = MemorySubject::from_authenticated_gateway(
            Some("tenant"),
            Some("alice"),
            Some("team-a"),
            Some("session"),
            false,
        )
        .expect("alice team-a");
        let bob_team_a = MemorySubject::from_authenticated_gateway(
            Some("tenant"),
            Some("bob"),
            Some("team-a"),
            Some("session"),
            false,
        )
        .expect("bob team-a");
        let alice_team_b = MemorySubject::from_authenticated_gateway(
            Some("tenant"),
            Some("alice"),
            Some("team-b"),
            Some("session"),
            false,
        )
        .expect("alice team-b");
        let alice_direct = MemorySubject::from_authenticated_gateway(
            Some("tenant"),
            Some("alice"),
            None,
            Some("session"),
            true,
        )
        .expect("alice direct");

        assert_eq!(alice_team_a.visibility(), MemoryVisibility::Private);
        assert_eq!(bob_team_a.visibility(), MemoryVisibility::Private);
        assert_ne!(
            alice_team_a.storage_key(&derivation),
            bob_team_a.storage_key(&derivation),
            "different principals in one group never share private memory"
        );
        assert_ne!(
            alice_team_a.storage_key(&derivation),
            alice_team_b.storage_key(&derivation),
            "ambient authenticated team context remains part of the partition"
        );
        assert_ne!(
            alice_team_a.storage_key(&derivation),
            alice_direct.storage_key(&derivation),
            "direct and group contexts do not collapse into one partition"
        );
    }
    #[test]
    fn team_visibility_requires_an_explicit_team() {
        assert!(matches!(
            MemorySubject::new(
                MemoryTenantId::new("tenant").unwrap(),
                MemoryPrincipalId::new("principal").unwrap(),
                None,
                MemorySessionId::new("session").unwrap(),
                MemorySurface::Gateway,
                MemoryVisibility::Team,
            ),
            Err(MemorySubjectError::MissingTeamForTeamVisibility)
        ));
    }
}
