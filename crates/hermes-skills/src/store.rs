//! Local skill storage with mandatory v2 trust admission on every read.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use async_trait::async_trait;
use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{instrument, warn};
use uuid::Uuid;

use hermes_core::types::{Skill, SkillMeta};

use crate::guard::SkillGuard;
use crate::skill::SkillError;
use crate::trust::{
    canonical_bundle_digest, full_skill_scan, load_lock_for_migration, load_revocations,
    normalize_relative_package_path, skill_lock_path, store_lock, LocalOwnerAttestation,
    PendingOwnerAttestation, SkillAdmission, SkillCompatibility, SkillLockEntryV2, SkillLockLoad,
    SkillLockV2, SkillPackageIdentityV2, SkillPackageManifestV2, SkillPackageProof,
    SkillPackageSource, SkillPermissions, SkillQuarantineReason, SkillScanVerdictV2,
    SkillTrustAuthorityPaths, SkillTrustError, SkillTrustLoad, SkillTrustPolicy, SkillTrustRead,
    SkillTrustStore, SkillTrustVerifier, TrustedLocalOwnerKey, SKILL_HUB_DIR,
};

pub const MAX_SKILL_NAME_LENGTH: usize = 128;

#[async_trait]
pub trait SkillStore: Send + Sync {
    async fn save(&self, skill: &Skill) -> Result<(), SkillError>;
    async fn load(&self, name: &str) -> Result<Option<Skill>, SkillError>;
    /// Read a locally staged package only for owner-authorized mutation; this
    /// is never an execution/prompt-loading path.
    async fn load_for_mutation(&self, name: &str) -> Result<Option<Skill>, SkillError> {
        self.load(name).await
    }
    async fn list(&self) -> Result<Vec<SkillMeta>, SkillError>;
    async fn delete(&self, name: &str) -> Result<(), SkillError>;
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct SkillFrontmatter {
    name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    category: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    version: Option<String>,
}

const PENDING_LOCAL_SCHEMA_VERSION: u32 = 1;
const PENDING_LOCAL_DIR: &str = "pending";
const PENDING_PAYLOAD_DIR: &str = "payload";
const PENDING_METADATA_FILE: &str = "candidate.json";
const INCOMING_REMOTE_DIR: &str = "incoming-remote";
const LOCAL_PACKAGE_DIR: &str = ".packages";

/// Owner-only, non-executable local candidate metadata. It is deliberately
/// outside the active v2 lock until a configured owner key signs it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PendingLocalSkillCandidate {
    schema_version: u32,
    install_path: String,
    manifest: SkillPackageManifestV2,
}

/// One regular UTF-8 file in a locally staged package. Paths are normalized
/// relative package paths; symlinks and detached trust metadata are never
/// accepted through this API.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalPackageFile {
    pub relative_path: PathBuf,
    pub bytes: Vec<u8>,
}

impl LocalPackageFile {
    #[must_use]
    pub fn new(relative_path: impl Into<PathBuf>, bytes: impl Into<Vec<u8>>) -> Self {
        Self {
            relative_path: relative_path.into(),
            bytes: bytes.into(),
        }
    }
}

/// File-backed store rooted at the actual skill root. Raw files are never
/// returned directly: a v2 lock, current digest, current full scan,
/// revocations, signature/attestation, compatibility, and permissions are
/// rechecked by [`SkillTrustStore`] before a `Skill` is materialized.
pub struct FileSkillStore {
    skills_dir: PathBuf,
    trust_policy: SkillTrustPolicy,
    embedded_manifest_digests: BTreeMap<String, String>,
    /// Production remote authority outside the mutable skills root.
    authority_paths: Option<SkillTrustAuthorityPaths>,
    /// Caller-owned public-key allowlist for local owner attestations. This
    /// intentionally never loads from mutable skills-root metadata.
    local_owner_keys: Vec<TrustedLocalOwnerKey>,
}

impl FileSkillStore {
    #[must_use]
    pub fn new(skills_dir: PathBuf) -> Self {
        Self {
            skills_dir,
            trust_policy: SkillTrustPolicy::default(),
            embedded_manifest_digests: BTreeMap::new(),
            authority_paths: None,
            local_owner_keys: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_trust_policy(mut self, trust_policy: SkillTrustPolicy) -> Self {
        self.trust_policy = trust_policy;
        self
    }

    /// Supply local-owner public keys from protected application
    /// configuration. Values persisted under `.hub` are intentionally ignored
    /// for local-attestation verification.
    #[must_use]
    pub fn with_trusted_local_owner_keys(
        mut self,
        local_owner_keys: Vec<TrustedLocalOwnerKey>,
    ) -> Self {
        self.local_owner_keys = local_owner_keys;
        self
    }

    #[must_use]
    pub fn with_embedded_manifest_digests(
        mut self,
        embedded_manifest_digests: BTreeMap<String, String>,
    ) -> Self {
        self.embedded_manifest_digests = embedded_manifest_digests;
        self
    }

    /// Bind this store to externally protected remote and local-owner trust
    /// authorities. This is the production configuration for any store that
    /// may admit remote packages; `.hub/keyring.json` alone is never an anchor.
    #[must_use]
    pub fn with_authority_paths(mut self, authority_paths: SkillTrustAuthorityPaths) -> Self {
        self.authority_paths = Some(authority_paths);
        self
    }

    #[must_use]
    pub fn default_dir() -> PathBuf {
        directories::ProjectDirs::from("com", "hermes", "hermes")
            .map(|dirs| dirs.data_dir().join("skills"))
            .unwrap_or_else(|| PathBuf::from(".hermes/skills"))
    }

    fn verifier(&self) -> Result<SkillTrustVerifier, SkillError> {
        match &self.authority_paths {
            Some(authority_paths) => {
                if !self.local_owner_keys.is_empty() {
                    return Err(SkillError::GuardViolation(
                        "external authority paths and injected local-owner keys cannot be combined"
                            .to_string(),
                    ));
                }
                authority_paths
                    .build_verifier(
                        self.trust_policy.clone(),
                        self.embedded_manifest_digests.clone(),
                    )
                    .map_err(authority_trust_error)
            }
            None => SkillTrustVerifier::for_skill_root_with_local_owner_keys(
                &self.skills_dir,
                self.trust_policy.clone(),
                self.embedded_manifest_digests.clone(),
                self.local_owner_keys.clone(),
            )
            .map_err(trust_error),
        }
    }

    fn trust_store(&self) -> Result<SkillTrustStore, SkillError> {
        Ok(SkillTrustStore::new(self.skills_dir.clone(), self.verifier()?))
    }

    fn validate_segment(value: &str, field: &str) -> Result<String, SkillError> {
        let trimmed = value.trim();
        if value != trimmed || trimmed.is_empty() || trimmed.len() > MAX_SKILL_NAME_LENGTH {
            return Err(SkillError::GuardViolation(format!(
                "Invalid skill {field}: value must be non-empty and no longer than {MAX_SKILL_NAME_LENGTH} chars"
            )));
        }
        let path = Path::new(trimmed);
        let mut components = path.components();
        let segment = match (components.next(), components.next()) {
            (Some(Component::Normal(segment)), None) => segment
                .to_str()
                .ok_or_else(|| SkillError::GuardViolation(format!("Invalid skill {field}: non-UTF-8")))?
                .to_string(),
            _ => {
                return Err(SkillError::GuardViolation(format!(
                    "Invalid skill {field}: path traversal or separators are not allowed"
                )))
            }
        };
        if segment.starts_with('.')
            || segment.starts_with('-')
            || !segment
                .chars()
                .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit() || matches!(character, '-' | '_' | '.'))
        {
            return Err(SkillError::GuardViolation(format!(
                "Invalid skill {field}: use lowercase letters, digits, '-', '_' or '.' without hidden/option prefixes"
            )));
        }
        Ok(segment)
    }

    fn root(&self) -> Result<PathBuf, SkillError> {
        if !self.skills_dir.exists() {
            fs::create_dir_all(&self.skills_dir)?;
        }
        let metadata = fs::symlink_metadata(&self.skills_dir)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(SkillError::Quarantined {
                reason: SkillQuarantineReason::FilesystemViolation,
                detail: "skill root must be a non-symlink directory".to_string(),
            });
        }
        fs::canonicalize(&self.skills_dir).map_err(Into::into)
    }

    /// Resolve/create a payload directory one component at a time without
    /// following a pre-existing symlink. `create_dir_all` is deliberately not
    /// used here because it can traverse a swapped category directory.
    fn ensure_contained_directory(
        &self,
        root: &Path,
        relative: &Path,
    ) -> Result<PathBuf, SkillError> {
        let mut current = root.to_path_buf();
        for component in relative.components() {
            let Component::Normal(segment) = component else {
                return Err(SkillError::GuardViolation(
                    "skill path contains traversal or root component".to_string(),
                ));
            };
            let next = current.join(segment);
            match fs::symlink_metadata(&next) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() || !metadata.is_dir() {
                        return Err(SkillError::Quarantined {
                            reason: SkillQuarantineReason::FilesystemViolation,
                            detail: "skill payload path contains a non-directory or symlink"
                                .to_string(),
                        });
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    match fs::create_dir(&next) {
                        Ok(()) => {}
                        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                        Err(error) => return Err(error.into()),
                    }
                    let metadata = fs::symlink_metadata(&next)?;
                    if metadata.file_type().is_symlink() || !metadata.is_dir() {
                        return Err(SkillError::Quarantined {
                            reason: SkillQuarantineReason::FilesystemViolation,
                            detail: "skill payload path changed while being created".to_string(),
                        });
                    }
                }
                Err(error) => return Err(error.into()),
            }
            let canonical = fs::canonicalize(&next)?;
            if !canonical.starts_with(root) {
                return Err(SkillError::Quarantined {
                    reason: SkillQuarantineReason::FilesystemViolation,
                    detail: "skill payload path escapes the skill root".to_string(),
                });
            }
            current = next;
        }
        Ok(current)
    }


    fn render_skill_file(frontmatter: &SkillFrontmatter, content: &str) -> String {
        let yaml = serde_yaml::to_string(frontmatter).unwrap_or_default();
        let yaml = yaml.trim_start_matches("---\n").trim_end();
        format!("---\n{yaml}\n---\n{content}")
    }

    fn parse_skill_file(raw: &str) -> Result<(SkillFrontmatter, String), SkillError> {
        if !raw.starts_with("---") {
            return Err(SkillError::Parse(
                "Skill file must start with YAML frontmatter".to_string(),
            ));
        }
        let rest = &raw[3..];
        let end = rest
            .find("\n---")
            .ok_or_else(|| SkillError::Parse("Missing closing --- in frontmatter".to_string()))?;
        let frontmatter = serde_yaml::from_str(&rest[..end]).map_err(|error| SkillError::Parse(error.to_string()))?;
        let content = rest[end + 4..].trim_start_matches('\n').to_string();
        Ok((frontmatter, content))
    }

    fn local_lock(&self) -> Result<SkillLockV2, SkillError> {
        match load_lock_for_migration(&self.skills_dir).map_err(trust_error)? {
            SkillLockLoad::V2(lock) => Ok(lock),
            SkillLockLoad::Legacy { .. } => Err(SkillError::Quarantined {
                reason: SkillQuarantineReason::LegacyLock,
                detail: "legacy lock must be explicitly reverified before local mutation".to_string(),
            }),
        }
    }

    fn set_owner_only_directory(path: &Path) -> Result<(), SkillError> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(SkillError::Quarantined {
                reason: SkillQuarantineReason::FilesystemViolation,
                detail: "owner-only skill state directory is not a regular directory".to_string(),
            });
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
        Ok(())
    }

    fn set_owner_only_file(path: &Path) -> Result<(), SkillError> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SkillError::Quarantined {
                reason: SkillQuarantineReason::FilesystemViolation,
                detail: "owner-only skill state file is not a regular file".to_string(),
            });
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }

    fn reject_nonprivate_file(path: &Path) -> Result<(), SkillError> {
        let metadata = fs::symlink_metadata(path)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(SkillError::Quarantined {
                reason: SkillQuarantineReason::FilesystemViolation,
                detail: "pending candidate metadata is not a regular non-symlink file".to_string(),
            });
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(SkillError::Quarantined {
                    reason: SkillQuarantineReason::FilesystemViolation,
                    detail: "pending candidate metadata is not owner-only".to_string(),
                });
            }
        }
        Ok(())
    }

    fn existing_contained_directory(
        &self,
        root: &Path,
        relative: &Path,
    ) -> Result<PathBuf, SkillError> {
        let mut current = root.to_path_buf();
        for component in relative.components() {
            let Component::Normal(segment) = component else {
                return Err(SkillError::GuardViolation(
                    "skill path contains traversal or root component".to_string(),
                ));
            };
            let next = current.join(segment);
            let metadata = fs::symlink_metadata(&next)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(SkillError::Quarantined {
                    reason: SkillQuarantineReason::FilesystemViolation,
                    detail: "skill payload path contains a non-directory or symlink".to_string(),
                });
            }
            if !fs::canonicalize(&next)?.starts_with(root) {
                return Err(SkillError::Quarantined {
                    reason: SkillQuarantineReason::FilesystemViolation,
                    detail: "skill payload path escapes the skill root".to_string(),
                });
            }
            current = next;
        }
        Ok(current)
    }

    fn pending_relative(name: &str) -> PathBuf {
        PathBuf::from(SKILL_HUB_DIR)
            .join(PENDING_LOCAL_DIR)
            .join(name)
    }

    fn pending_paths(
        &self,
        root: &Path,
        name: &str,
    ) -> (PathBuf, PathBuf, PathBuf) {
        let pending = root.join(Self::pending_relative(name));
        (
            pending.clone(),
            pending.join(PENDING_PAYLOAD_DIR),
            pending.join(PENDING_METADATA_FILE),
        )
    }

    fn load_pending_candidate(
        &self,
        name: &str,
    ) -> Result<Option<(PendingLocalSkillCandidate, PathBuf)>, SkillError> {
        let root = self.root()?;
        let relative = Self::pending_relative(name);
        let expected_pending = root.join(&relative);
        match fs::symlink_metadata(&expected_pending) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        let pending = self.existing_contained_directory(&root, &relative)?;
        Self::set_owner_only_directory(&pending)?;
        let (_, _, metadata_path) = self.pending_paths(&root, name);
        Self::reject_nonprivate_file(&metadata_path)?;
        let candidate = serde_json::from_slice::<PendingLocalSkillCandidate>(&fs::read(&metadata_path)?)
            .map_err(|error| {
                Self::quarantined(
                    name.to_string(),
                    SkillQuarantineReason::MalformedLock,
                    format!("invalid pending candidate metadata: {error}"),
                )
            })?;
        if candidate.schema_version != PENDING_LOCAL_SCHEMA_VERSION
            || candidate.manifest.identity.name != name
            || candidate.manifest.source != SkillPackageSource::LocalAuthored
            || !matches!(
                candidate.manifest.proof,
                SkillPackageProof::PendingOwnerAttestation(_)
            )
            || normalize_relative_package_path(Path::new(&candidate.install_path))
                .map_err(trust_error)?
                != candidate.install_path
        {
            return Err(Self::quarantined(
                name.to_string(),
                SkillQuarantineReason::MalformedLock,
                "pending candidate metadata is not a local pending package".to_string(),
            ));
        }
        let payload_relative = relative.join(PENDING_PAYLOAD_DIR);
        let payload = self.existing_contained_directory(&root, &payload_relative)?;
        Ok(Some((candidate, payload)))
    }


    fn checked_pending_skill(
        &self,
        candidate: &PendingLocalSkillCandidate,
        directory: &Path,
    ) -> Result<Skill, SkillError> {
        if candidate.manifest.identity.manifest_digest
            != candidate.manifest.computed_manifest_digest().map_err(trust_error)?
        {
            return Err(Self::quarantined(
                candidate.manifest.identity.name.clone(),
                SkillQuarantineReason::ManifestDigestMismatch,
                "pending candidate manifest digest changed before owner attestation".to_string(),
            ));
        }
        let content_digest = canonical_bundle_digest(directory).map_err(trust_error)?;
        if content_digest != candidate.manifest.identity.content_digest {
            return Err(Self::quarantined(
                candidate.manifest.identity.name.clone(),
                SkillQuarantineReason::ContentDigestMismatch,
                "pending candidate digest changed before owner attestation".to_string(),
            ));
        }
        let scan = full_skill_scan(directory);
        if scan != candidate.manifest.scan {
            return Err(Self::quarantined(
                candidate.manifest.identity.name.clone(),
                SkillQuarantineReason::ScanMismatch,
                "pending candidate scan evidence changed before owner attestation".to_string(),
            ));
        }
        if matches!(scan.verdict, SkillScanVerdictV2::Dangerous | SkillScanVerdictV2::Invalid) {
            return Err(Self::quarantined(
                candidate.manifest.identity.name.clone(),
                if scan.verdict == SkillScanVerdictV2::Dangerous {
                    SkillQuarantineReason::DangerousScan
                } else {
                    SkillQuarantineReason::InvalidScan
                },
                "unsafe pending candidate cannot be promoted".to_string(),
            ));
        }
        let skill_file = directory.join("SKILL.md");
        let metadata = fs::symlink_metadata(&skill_file)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(Self::quarantined(
                candidate.manifest.identity.name.clone(),
                SkillQuarantineReason::FilesystemViolation,
                "pending SKILL.md is not a regular file".to_string(),
            ));
        }
        let raw = fs::read_to_string(skill_file).map_err(|_| {
            Self::quarantined(
                candidate.manifest.identity.name.clone(),
                SkillQuarantineReason::FilesystemViolation,
                "pending SKILL.md is not UTF-8".to_string(),
            )
        })?;
        let (frontmatter, content) = Self::parse_skill_file(&raw)?;
        if frontmatter.name != candidate.manifest.identity.name {
            return Err(Self::quarantined(
                candidate.manifest.identity.name.clone(),
                SkillQuarantineReason::ContentDigestMismatch,
                "pending SKILL.md identity differs from candidate metadata".to_string(),
            ));
        }
        Ok(Skill {
            name: frontmatter.name,
            content,
            category: frontmatter.category,
            description: frontmatter.description,
        })
    }

    fn copy_regular_bundle(source: &Path, destination: &Path) -> Result<(), SkillError> {
        let mut entries = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
        entries.sort_by(|left, right| left.file_name().cmp(&right.file_name()));
        for entry in entries {
            let source_path = entry.path();
            let destination_path = destination.join(entry.file_name());
            let metadata = fs::symlink_metadata(&source_path)?;
            if metadata.file_type().is_symlink() {
                return Err(SkillError::Quarantined {
                    reason: SkillQuarantineReason::FilesystemViolation,
                    detail: "cannot promote a symlinked pending payload".to_string(),
                });
            }
            if metadata.is_dir() {
                fs::create_dir(&destination_path)?;
                Self::set_owner_only_directory(&destination_path)?;
                Self::copy_regular_bundle(&source_path, &destination_path)?;
            } else if metadata.is_file() {
                let bytes = fs::read(&source_path)?;
                if std::str::from_utf8(&bytes).is_err() {
                    return Err(SkillError::Quarantined {
                        reason: SkillQuarantineReason::FilesystemViolation,
                        detail: "cannot promote a non-UTF-8 pending payload".to_string(),
                    });
                }
                Self::atomic_write(&destination_path, &bytes)?;
            } else {
                return Err(SkillError::Quarantined {
                    reason: SkillQuarantineReason::FilesystemViolation,
                    detail: "cannot promote a non-regular pending payload".to_string(),
                });
            }
        }
        Ok(())
    }

    fn prepare_immutable_payload(
        &self,
        root: &Path,
        package_kind: &str,
        name: &str,
        content_digest: &str,
        source: &Path,
    ) -> Result<(String, PathBuf), SkillError> {
        let digest = content_digest.strip_prefix("sha256:").ok_or_else(|| {
            Self::quarantined(
                name.to_string(),
                SkillQuarantineReason::ContentDigestMismatch,
                "payload digest lacks a canonical prefix".to_string(),
            )
        })?;
        if digest.len() != 64
            || !digest
                .bytes()
                .all(|character| character.is_ascii_hexdigit() && !character.is_ascii_uppercase())
        {
            return Err(Self::quarantined(
                name.to_string(),
                SkillQuarantineReason::ContentDigestMismatch,
                "payload digest is not canonical lowercase SHA-256".to_string(),
            ));
        }
        let relative = PathBuf::from(LOCAL_PACKAGE_DIR)
            .join(package_kind)
            .join(name)
            .join(digest);
        let install_path = normalize_relative_package_path(&relative).map_err(trust_error)?;
        let destination = root.join(&relative);
        match fs::symlink_metadata(&destination) {
            Ok(metadata) => {
                if metadata.file_type().is_symlink() || !metadata.is_dir() {
                    return Err(Self::quarantined(
                        name.to_string(),
                        SkillQuarantineReason::FilesystemViolation,
                        "immutable package destination is not a regular directory".to_string(),
                    ));
                }
                if !fs::canonicalize(&destination)?.starts_with(root) {
                    return Err(Self::quarantined(
                        name.to_string(),
                        SkillQuarantineReason::FilesystemViolation,
                        "immutable package destination escapes the skill root".to_string(),
                    ));
                }
                if canonical_bundle_digest(&destination).map_err(trust_error)? != content_digest {
                    return Err(Self::quarantined(
                        name.to_string(),
                        SkillQuarantineReason::ContentDigestMismatch,
                        "immutable package destination has a conflicting digest".to_string(),
                    ));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let destination = self.ensure_contained_directory(root, &relative)?;
                Self::set_owner_only_directory(&destination)?;
                let copy_result = Self::copy_regular_bundle(source, &destination).and_then(|()| {
                    if canonical_bundle_digest(&destination).map_err(trust_error)? != content_digest {
                        return Err(Self::quarantined(
                            name.to_string(),
                            SkillQuarantineReason::ContentDigestMismatch,
                            "copied immutable payload does not match its staged digest".to_string(),
                        ));
                    }
                    Ok(())
                });
                if let Err(error) = copy_result {
                    let _ = fs::remove_dir_all(&destination);
                    return Err(error);
                }
            }
            Err(error) => return Err(error.into()),
        }
        Ok((install_path, destination))
    }

    fn remove_pending_candidate(&self, name: &str) -> Result<(), SkillError> {
        let root = self.root()?;
        let relative = Self::pending_relative(name);
        let pending = root.join(&relative);
        match fs::symlink_metadata(&pending) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error.into()),
            Ok(_) => {}
        }
        let pending = self.existing_contained_directory(&root, &relative)?;
        fs::remove_dir_all(pending)?;
        Ok(())
    }

    /// Exposes a pending manifest only to the explicit owner-attestation
    /// workflow. It is never an execution/prompt-loading API.
    pub fn pending_local_manifest(
        &self,
        name: &str,
    ) -> Result<Option<SkillPackageManifestV2>, SkillError> {
        let name = Self::validate_segment(name, "name")?;
        Ok(self
            .load_pending_candidate(&name)?
            .map(|(candidate, _)| candidate.manifest))
    }

    /// Stage a complete local package outside the active payload/lock. This
    /// accepts only normalized regular UTF-8 files and is intentionally not an
    /// execution admission API; callers must subsequently obtain a signed
    /// manifest through [`Self::pending_local_manifest`] and call
    /// [`Self::attest_local_skill`].
    pub fn stage_local_package(
        &self,
        name: &str,
        requested_install_path: &Path,
        files: &[LocalPackageFile],
    ) -> Result<(), SkillError> {
        let name = Self::validate_segment(name, "name")?;
        let install_path =
            normalize_relative_package_path(requested_install_path).map_err(trust_error)?;
        let mut normalized_files = Vec::with_capacity(files.len());
        let mut seen = BTreeSet::new();
        let mut skill_file = None;
        for file in files {
            let normalized =
                normalize_relative_package_path(&file.relative_path).map_err(trust_error)?;
            if normalized == SKILL_HUB_DIR
                || normalized.starts_with(&format!("{SKILL_HUB_DIR}/"))
                || normalized == LOCAL_PACKAGE_DIR
                || normalized.starts_with(&format!("{LOCAL_PACKAGE_DIR}/"))
                || !seen.insert(normalized.clone())
            {
                return Err(SkillError::GuardViolation(
                    "local package contains duplicate or reserved payload path".to_string(),
                ));
            }
            if std::str::from_utf8(&file.bytes).is_err() {
                return Err(SkillError::GuardViolation(
                    "local package payload files must be UTF-8".to_string(),
                ));
            }
            if normalized == "SKILL.md" {
                skill_file = Some(file.bytes.as_slice());
            }
            normalized_files.push((PathBuf::from(normalized), file.bytes.as_slice()));
        }
        let skill_file = skill_file.ok_or_else(|| {
            SkillError::GuardViolation("local package must contain SKILL.md".to_string())
        })?;
        let raw_skill = std::str::from_utf8(skill_file)
            .map_err(|_| SkillError::GuardViolation("SKILL.md must be UTF-8".to_string()))?;
        let (frontmatter, content) = Self::parse_skill_file(raw_skill)?;
        if frontmatter.name != name {
            return Err(Self::quarantined(
                name,
                SkillQuarantineReason::ContentDigestMismatch,
                "local package SKILL.md name differs from staged package name".to_string(),
            ));
        }

        let lock = self.local_lock()?;
        if let Some(existing) = lock
            .packages
            .iter()
            .find(|entry| entry.manifest.identity.name == frontmatter.name)
        {
            if existing.manifest.source != SkillPackageSource::LocalAuthored {
                return Err(Self::quarantined(
                    frontmatter.name,
                    SkillQuarantineReason::InvalidSignature,
                    "remote or embedded packages may not be replaced by a local candidate".to_string(),
                ));
            }
            let _ = self.entry_root(existing)?;
        }

        let root = self.root()?;
        let pending_relative = Self::pending_relative(&frontmatter.name);
        let pending = self.ensure_contained_directory(&root, &pending_relative)?;
        let payload_path = root.join(pending_relative.join(PENDING_PAYLOAD_DIR));
        if let Ok(metadata) = fs::symlink_metadata(&payload_path) {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(Self::quarantined(
                    frontmatter.name,
                    SkillQuarantineReason::FilesystemViolation,
                    "pending payload is not a regular directory".to_string(),
                ));
            }
            let payload_relative = Self::pending_relative(&name).join(PENDING_PAYLOAD_DIR);
            let existing = self.existing_contained_directory(&root, &payload_relative)?;
            fs::remove_dir_all(existing)?;
        }
        let payload =
            self.ensure_contained_directory(&root, &Self::pending_relative(&name).join(PENDING_PAYLOAD_DIR))?;
        let hub = root.join(SKILL_HUB_DIR);
        Self::set_owner_only_directory(&hub)?;
        Self::set_owner_only_directory(&hub.join(PENDING_LOCAL_DIR))?;
        Self::set_owner_only_directory(&pending)?;
        Self::set_owner_only_directory(&payload)?;
        for (relative_path, bytes) in normalized_files {
            if let Some(parent) = relative_path.parent().filter(|path| !path.as_os_str().is_empty()) {
                let parent = self.ensure_contained_directory(
                    &root,
                    &Self::pending_relative(&name)
                        .join(PENDING_PAYLOAD_DIR)
                        .join(parent),
                )?;
                Self::set_owner_only_directory(&parent)?;
            }
            Self::atomic_write(&payload.join(relative_path), bytes)?;
        }
        let content_digest = canonical_bundle_digest(&payload).map_err(trust_error)?;
        let scan = full_skill_scan(&payload);
        let identity = SkillPackageIdentityV2::new(
            name.clone(),
            frontmatter
                .version
                .clone()
                .unwrap_or_else(|| crate::version::compute_version(&content)),
            "local://pending-owner",
            "local:pending-owner",
            content_digest.clone(),
            content_digest,
            Utc::now().to_rfc3339(),
        );
        let mut manifest = SkillPackageManifestV2::new(
            SkillPackageSource::LocalAuthored,
            identity,
            SkillPackageProof::PendingOwnerAttestation(PendingOwnerAttestation {
                requested_at: Utc::now().to_rfc3339(),
            }),
            scan,
            SkillCompatibility::default(),
            SkillPermissions::default(),
            Vec::new(),
        );
        manifest.refresh_manifest_digest().map_err(trust_error)?;
        let candidate = PendingLocalSkillCandidate {
            schema_version: PENDING_LOCAL_SCHEMA_VERSION,
            install_path,
            manifest,
        };
        let (_, _, metadata_path) = self.pending_paths(&root, &name);
        let metadata = serde_json::to_vec_pretty(&candidate)
            .map_err(|error| SkillError::Parse(format!("failed to encode pending candidate: {error}")))?;
        Self::atomic_write(&metadata_path, &metadata)
    }

    fn stage_incoming_remote_payload(
        &self,
        root: &Path,
        name: &str,
        files: &[LocalPackageFile],
    ) -> Result<(PathBuf, PathBuf), SkillError> {
        let mut normalized_files = Vec::with_capacity(files.len());
        let mut seen = BTreeSet::new();
        let mut has_skill_file = false;
        for file in files {
            let normalized =
                normalize_relative_package_path(&file.relative_path).map_err(trust_error)?;
            if normalized == SKILL_HUB_DIR
                || normalized.starts_with(&format!("{SKILL_HUB_DIR}/"))
                || normalized == LOCAL_PACKAGE_DIR
                || normalized.starts_with(&format!("{LOCAL_PACKAGE_DIR}/"))
                || !seen.insert(normalized.clone())
            {
                return Err(SkillError::GuardViolation(
                    "remote package contains duplicate or reserved payload path".to_string(),
                ));
            }
            if std::str::from_utf8(&file.bytes).is_err() {
                return Err(SkillError::GuardViolation(
                    "remote package payload files must be UTF-8".to_string(),
                ));
            }
            has_skill_file |= normalized == "SKILL.md";
            normalized_files.push((PathBuf::from(normalized), file.bytes.as_slice()));
        }
        if !has_skill_file {
            return Err(SkillError::GuardViolation(
                "remote package must contain SKILL.md".to_string(),
            ));
        }

        let stage_relative = PathBuf::from(SKILL_HUB_DIR)
            .join(INCOMING_REMOTE_DIR)
            .join(name)
            .join(Uuid::new_v4().to_string());
        let stage = self.ensure_contained_directory(root, &stage_relative)?;
        let payload_relative = stage_relative.join(PENDING_PAYLOAD_DIR);
        let payload = self.ensure_contained_directory(root, &payload_relative)?;
        Self::set_owner_only_directory(&root.join(SKILL_HUB_DIR))?;
        Self::set_owner_only_directory(&root.join(SKILL_HUB_DIR).join(INCOMING_REMOTE_DIR))?;
        Self::set_owner_only_directory(&stage)?;
        Self::set_owner_only_directory(&payload)?;
        for (relative_path, bytes) in normalized_files {
            if let Some(parent) = relative_path.parent().filter(|path| !path.as_os_str().is_empty()) {
                let parent = self.ensure_contained_directory(
                    root,
                    &payload_relative.join(parent),
                )?;
                Self::set_owner_only_directory(&parent)?;
            }
            Self::atomic_write(&payload.join(relative_path), bytes)?;
        }
        Ok((stage, payload))
    }

    fn require_remote_admission(
        manifest: &SkillPackageManifestV2,
        verifier: &SkillTrustVerifier,
        revocations: &crate::SignedSkillRevocationList,
        root: &Path,
        force: bool,
    ) -> Result<SkillAdmission, SkillError> {
        let admission = verifier
            .verify_bundle(root, manifest, revocations, force)
            .map_err(trust_error)?;
        if let Some(quarantine) = admission.quarantine {
            return Err(Self::quarantined(
                manifest.identity.name.clone(),
                quarantine.reason,
                quarantine.detail,
            ));
        }
        if !admission.is_allowed() {
            return Err(Self::quarantined(
                manifest.identity.name.clone(),
                SkillQuarantineReason::InvalidSignature,
                "remote package did not receive an executable admission".to_string(),
            ));
        }
        let skill_file = root.join("SKILL.md");
        let metadata = fs::symlink_metadata(&skill_file)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(Self::quarantined(
                manifest.identity.name.clone(),
                SkillQuarantineReason::FilesystemViolation,
                "remote SKILL.md is not a regular file".to_string(),
            ));
        }
        let raw = fs::read_to_string(&skill_file).map_err(|_| {
            Self::quarantined(
                manifest.identity.name.clone(),
                SkillQuarantineReason::FilesystemViolation,
                "remote SKILL.md is not UTF-8".to_string(),
            )
        })?;
        let (frontmatter, _) = Self::parse_skill_file(&raw).map_err(|error| {
            Self::quarantined(
                manifest.identity.name.clone(),
                SkillQuarantineReason::MalformedLock,
                format!("remote SKILL.md frontmatter is invalid: {error}"),
            )
        })?;
        if frontmatter.name != manifest.identity.name {
            return Err(Self::quarantined(
                manifest.identity.name.clone(),
                SkillQuarantineReason::ContentDigestMismatch,
                "remote SKILL.md name differs from signed package identity".to_string(),
            ));
        }
        Ok(admission)
    }

    /// Atomically install a fully materialized, signed remote package through
    /// a fresh staging directory. The supplied manifest is reverified against
    /// an externally anchored verifier before and after the immutable payload
    /// copy; only the final v2-lock switch makes it executable.
    ///
    /// `force` is passed only to the typed verifier: it can admit a genuine
    /// `Caution` scan as `AllowWithForce`, and cannot override any other
    /// quarantine reason.
    pub fn install_signed_remote_package(
        &self,
        manifest: SkillPackageManifestV2,
        files: &[LocalPackageFile],
        force: bool,
    ) -> Result<SkillAdmission, SkillError> {
        if manifest.source != SkillPackageSource::Remote {
            return Err(Self::quarantined(
                manifest.identity.name.clone(),
                SkillQuarantineReason::InvalidSignature,
                "remote installer requires a remote signed package manifest".to_string(),
            ));
        }
        let name = Self::validate_segment(&manifest.identity.name, "name")?;
        if name != manifest.identity.name {
            return Err(Self::quarantined(
                manifest.identity.name.clone(),
                SkillQuarantineReason::MalformedLock,
                "remote package identity name is not a canonical segment".to_string(),
            ));
        }
        let mut lock = self.local_lock()?;
        if let Some(existing) = lock
            .packages
            .iter()
            .find(|entry| entry.manifest.identity.name == name)
        {
            if existing.manifest.source != SkillPackageSource::Remote {
                return Err(Self::quarantined(
                    name,
                    SkillQuarantineReason::InvalidSignature,
                    "a remote package cannot replace a local or embedded package".to_string(),
                ));
            }
        }
        let root = self.root()?;
        let (stage, staged_payload) = self.stage_incoming_remote_payload(&root, &name, files)?;
        let result = (|| -> Result<SkillAdmission, SkillError> {
            let verifier = self.verifier()?;
            let revocations = load_revocations(&self.skills_dir).map_err(trust_error)?;
            let _staged_admission =
                Self::require_remote_admission(&manifest, &verifier, &revocations, &staged_payload, force)?;
            let (install_path, promoted_payload) = self.prepare_immutable_payload(
                &root,
                "remote",
                &name,
                &manifest.identity.content_digest,
                &staged_payload,
            )?;
            let promoted_admission =
                Self::require_remote_admission(&manifest, &verifier, &revocations, &promoted_payload, force)?;
            let entry = SkillLockEntryV2 {
                install_path,
                manifest: manifest.clone(),
                quarantine: None,
            };
            if let Some(index) = lock
                .packages
                .iter()
                .position(|existing| existing.manifest.identity.name == name)
            {
                lock.packages[index] = entry;
            } else {
                lock.packages.push(entry);
                lock.packages.sort_by(|left, right| {
                    left.manifest
                        .identity
                        .stable_key()
                        .cmp(&right.manifest.identity.stable_key())
                });
            }
            store_lock(&self.skills_dir, &lock).map_err(trust_error)?;
            Ok(promoted_admission)
        })();
        if let Err(error) = fs::remove_dir_all(&stage) {
            warn!(skill = %name, error = %error, "unreachable remote staging directory retained for cleanup");
        }
        result
    }


    fn atomic_write(path: &Path, content: &[u8]) -> Result<(), SkillError> {
        let parent = path
            .parent()
            .ok_or_else(|| SkillError::Io("skill payload has no parent".to_string()))?;
        Self::set_owner_only_directory(parent)?;
        let temporary = parent.join(format!(
            ".{}.tmp-{}-{}",
            path.file_name().and_then(|name| name.to_str()).unwrap_or("SKILL.md"),
            std::process::id(),
            Uuid::new_v4()
        ));
        let result = (|| -> Result<(), SkillError> {
            let mut file = OpenOptions::new().write(true).create_new(true).open(&temporary)?;
            Self::set_owner_only_file(&temporary)?;
            file.write_all(content)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, path)?;
            Self::set_owner_only_file(path)?;
            File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        result
    }

    fn raw_skill_named_exists(&self, requested_name: &str) -> bool {
        let Ok(root) = self.root() else {
            return false;
        };
        let mut stack = vec![root];
        while let Some(directory) = stack.pop() {
            let Ok(entries) = fs::read_dir(directory) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(metadata) = fs::symlink_metadata(&path) else {
                    continue;
                };
                if metadata.file_type().is_symlink() {
                    continue;
                }
                if metadata.is_dir() {
                    if matches!(
                        path.file_name().and_then(|name| name.to_str()),
                        Some(".hub") | Some(LOCAL_PACKAGE_DIR)
                    ) {
                        continue;
                    }
                    let skill_file = path.join("SKILL.md");
                    if let Ok(file_metadata) = fs::symlink_metadata(&skill_file) {
                        if file_metadata.is_file() && !file_metadata.file_type().is_symlink() {
                            if let Ok(raw) = fs::read_to_string(&skill_file) {
                                if let Ok((frontmatter, _)) = Self::parse_skill_file(&raw) {
                                    if frontmatter.name == requested_name {
                                        return true;
                                    }
                                }
                            }
                        }
                    }
                    stack.push(path);
                }
            }
        }
        false
    }

    fn entry_root(&self, entry: &SkillLockEntryV2) -> Result<PathBuf, SkillError> {
        let normalized = normalize_relative_package_path(Path::new(&entry.install_path))
            .map_err(trust_error)?;
        let root = self.root()?;
        let directory = root.join(normalized);
        let metadata = fs::symlink_metadata(&directory)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(SkillError::Quarantined {
                reason: SkillQuarantineReason::FilesystemViolation,
                detail: "locked package directory is not a regular contained directory".to_string(),
            });
        }
        let canonical = fs::canonicalize(&directory)?;
        if !canonical.starts_with(root) {
            return Err(SkillError::Quarantined {
                reason: SkillQuarantineReason::FilesystemViolation,
                detail: "locked package directory escapes the skill root".to_string(),
            });
        }
        Ok(directory)
    }

    fn checked_staged_skill(&self, entry: &SkillLockEntryV2) -> Result<Skill, SkillError> {
        let directory = self.entry_root(entry)?;
        let content_digest = canonical_bundle_digest(&directory).map_err(trust_error)?;
        if content_digest != entry.manifest.identity.content_digest {
            return Err(Self::quarantined(
                entry.manifest.identity.name.clone(),
                SkillQuarantineReason::ContentDigestMismatch,
                "staged package digest changed before owner attestation".to_string(),
            ));
        }
        let scan = full_skill_scan(&directory);
        if scan != entry.manifest.scan {
            return Err(Self::quarantined(
                entry.manifest.identity.name.clone(),
                SkillQuarantineReason::ScanMismatch,
                "staged package scan evidence changed before owner attestation".to_string(),
            ));
        }
        if matches!(scan.verdict, SkillScanVerdictV2::Dangerous | SkillScanVerdictV2::Invalid) {
            return Err(Self::quarantined(
                entry.manifest.identity.name.clone(),
                if scan.verdict == SkillScanVerdictV2::Dangerous {
                    SkillQuarantineReason::DangerousScan
                } else {
                    SkillQuarantineReason::InvalidScan
                },
                "unsafe staged package cannot be mutated through the owner workflow".to_string(),
            ));
        }
        let skill_file = directory.join("SKILL.md");
        let metadata = fs::symlink_metadata(&skill_file)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(Self::quarantined(
                entry.manifest.identity.name.clone(),
                SkillQuarantineReason::FilesystemViolation,
                "staged SKILL.md is not a regular file".to_string(),
            ));
        }
        let raw = fs::read_to_string(skill_file)
            .map_err(|_| Self::quarantined(
                entry.manifest.identity.name.clone(),
                SkillQuarantineReason::FilesystemViolation,
                "staged SKILL.md is not UTF-8".to_string(),
            ))?;
        let (frontmatter, content) = Self::parse_skill_file(&raw)?;
        if frontmatter.name != entry.manifest.identity.name {
            return Err(Self::quarantined(
                entry.manifest.identity.name.clone(),
                SkillQuarantineReason::ContentDigestMismatch,
                "staged SKILL.md identity differs from lock identity".to_string(),
            ));
        }
        Ok(Skill {
            name: frontmatter.name,
            content,
            category: frontmatter.category,
            description: frontmatter.description,
        })
    }

    /// Promote a signed, owner-only pending local candidate. Candidate bytes
    /// are copied to a fresh immutable path and reverified before the one
    /// atomic active-lock switch, so a failed or interrupted promotion leaves
    /// an incumbent package reachable through its prior lock entry.
    pub fn attest_local_skill(
        &self,
        name: &str,
        attestation: LocalOwnerAttestation,
    ) -> Result<(), SkillError> {
        let name = Self::validate_segment(name, "name")?;
        let Some((candidate, staged_root)) = self.load_pending_candidate(&name)? else {
            return Err(SkillError::NotFound(name));
        };
        let staged = self.checked_pending_skill(&candidate, &staged_root)?;
        let content_digest = canonical_bundle_digest(&staged_root).map_err(trust_error)?;
        if attestation.content_digest != content_digest {
            return Err(Self::quarantined(
                staged.name,
                SkillQuarantineReason::InvalidLocalAttestation,
                "owner attestation digest does not match the staged package".to_string(),
            ));
        }

        let mut manifest = candidate.manifest;
        manifest.identity.origin = format!("local://{}", attestation.owner_id);
        manifest.identity.publisher_id = format!("local:{}", attestation.owner_id);
        manifest.identity.immutable_source_revision = content_digest.clone();
        manifest.identity.content_digest = content_digest.clone();
        manifest.proof = SkillPackageProof::LocalOwnerAttestation(attestation);
        manifest.scan = full_skill_scan(&staged_root);
        manifest.refresh_manifest_digest().map_err(trust_error)?;

        let verifier = self.verifier()?;
        let revocations = load_revocations(&self.skills_dir).map_err(trust_error)?;
        let staged_admission = verifier
            .verify_bundle(&staged_root, &manifest, &revocations, false)
            .map_err(trust_error)?;
        if !staged_admission.is_allowed() {
            let quarantine = staged_admission.quarantine.unwrap_or(crate::SkillQuarantine {
                reason: SkillQuarantineReason::InvalidLocalAttestation,
                detail: "local owner attestation was not admitted".to_string(),
            });
            return Err(Self::quarantined(name, quarantine.reason, quarantine.detail));
        }

        let root = self.root()?;
        let (install_path, promoted_root) = self.prepare_immutable_payload(
            &root,
            "local",
            &manifest.identity.name,
            &content_digest,
            &staged_root,
        )?;
        let promoted_admission = verifier
            .verify_bundle(&promoted_root, &manifest, &revocations, false)
            .map_err(trust_error)?;
        if !promoted_admission.is_allowed() {
            let quarantine = promoted_admission.quarantine.unwrap_or(crate::SkillQuarantine {
                reason: SkillQuarantineReason::InvalidLocalAttestation,
                detail: "promoted local owner attestation was not admitted".to_string(),
            });
            return Err(Self::quarantined(
                manifest.identity.name.clone(),
                quarantine.reason,
                quarantine.detail,
            ));
        }

        let mut lock = self.local_lock()?;
        let existing_index = lock
            .packages
            .iter()
            .position(|entry| entry.manifest.identity.name == manifest.identity.name);
        if let Some(index) = existing_index {
            if lock.packages[index].manifest.source != SkillPackageSource::LocalAuthored {
                return Err(Self::quarantined(
                    manifest.identity.name.clone(),
                    SkillQuarantineReason::InvalidSignature,
                    "remote or embedded packages may not be replaced by a local candidate".to_string(),
                ));
            }
            lock.packages[index] = SkillLockEntryV2 {
                install_path,
                manifest,
                quarantine: None,
            };
        } else {
            lock.packages.push(SkillLockEntryV2 {
                install_path,
                manifest,
                quarantine: None,
            });
            lock.packages.sort_by(|left, right| {
                left.manifest
                    .identity
                    .stable_key()
                    .cmp(&right.manifest.identity.stable_key())
            });
        }
        // `store_lock` is atomic+fsync. Before this point only the candidate
        // and an unreachable immutable copy exist; after it the new path is
        // fully materialized and verified.
        store_lock(&self.skills_dir, &lock).map_err(trust_error)?;
        if let Err(error) = self.remove_pending_candidate(&name) {
            warn!(skill = %name, error = %error, "promoted local candidate retained for later cleanup");
        }
        Ok(())
    }

    fn quarantined(name: String, reason: SkillQuarantineReason, detail: String) -> SkillError {
        SkillError::Quarantined { reason, detail: format!("{name}: {detail}") }
    }
}

#[async_trait]
impl SkillStore for FileSkillStore {
    #[instrument(skip(self, skill), fields(name = %skill.name))]
    async fn save(&self, skill: &Skill) -> Result<(), SkillError> {
        let name = Self::validate_segment(&skill.name, "name")?;
        let category = skill
            .category
            .as_deref()
            .map(|category| Self::validate_segment(category, "category"))
            .transpose()?;
        let mut requested_install_path = PathBuf::new();
        if let Some(category) = category {
            requested_install_path.push(category);
        }
        requested_install_path.push(&name);
        SkillGuard::default().scan_security_only(skill)?;
        let frontmatter = SkillFrontmatter {
            name,
            category: skill.category.clone(),
            description: skill.description.clone(),
            version: Some(crate::version::compute_version(&skill.content)),
        };
        let rendered = Self::render_skill_file(&frontmatter, &skill.content);
        self.stage_local_package(
            &frontmatter.name,
            &requested_install_path,
            &[LocalPackageFile::new("SKILL.md", rendered.into_bytes())],
        )
    }

    #[instrument(skip(self), fields(name = %name))]
    async fn load(&self, name: &str) -> Result<Option<Skill>, SkillError> {
        let name = Self::validate_segment(name, "name")?;
        if !skill_lock_path(&self.skills_dir).exists() {
            return if self.load_pending_candidate(&name)?.is_some() {
                Err(Self::quarantined(
                    name,
                    SkillQuarantineReason::PendingOwnerAttestation,
                    "local candidate is staged pending explicit owner attestation".to_string(),
                ))
            } else if self.raw_skill_named_exists(&name) {
                Err(Self::quarantined(
                    name,
                    SkillQuarantineReason::MissingLock,
                    "drop-in skill has no v2 lock".to_string(),
                ))
            } else {
                Ok(None)
            };
        }
        let trust_store = self.trust_store()?;
        match trust_store.load_named(&name, false).map_err(trust_error)? {
            SkillTrustLoad::NotFound => {
                if self.load_pending_candidate(&name)?.is_some() {
                    Err(Self::quarantined(
                        name,
                        SkillQuarantineReason::PendingOwnerAttestation,
                        "local candidate is staged pending explicit owner attestation".to_string(),
                    ))
                } else if self.raw_skill_named_exists(&name) {
                    Err(Self::quarantined(
                        name,
                        SkillQuarantineReason::MissingLock,
                        "drop-in skill has no matching v2 lock entry".to_string(),
                    ))
                } else {
                    Ok(None)
                }
            }
            SkillTrustLoad::Quarantined { name, quarantine } => {
                Err(Self::quarantined(name, quarantine.reason, quarantine.detail))
            }
            SkillTrustLoad::Admitted(_) => match trust_store
                .read_contained(&name, Path::new("SKILL.md"), false)
                .map_err(trust_error)?
            {
                SkillTrustRead::Read { package, bytes } => {
                    let raw = String::from_utf8(bytes).map_err(|_| Self::quarantined(
                        name.clone(),
                        SkillQuarantineReason::FilesystemViolation,
                        "SKILL.md is not UTF-8".to_string(),
                    ))?;
                    let (frontmatter, content) = Self::parse_skill_file(&raw)?;
                    if frontmatter.name != name || package.entry.manifest.identity.name != name {
                        return Err(Self::quarantined(
                            name,
                            SkillQuarantineReason::ContentDigestMismatch,
                            "SKILL.md identity differs from its locked package identity".to_string(),
                        ));
                    }
                    Ok(Some(Skill {
                        name: frontmatter.name,
                        content,
                        category: frontmatter.category,
                        description: frontmatter.description,
                    }))
                }
                SkillTrustRead::NotFound => Ok(None),
                SkillTrustRead::Quarantined { name, quarantine } => {
                    Err(Self::quarantined(name, quarantine.reason, quarantine.detail))
                }
            },
        }
    }

    #[instrument(skip(self), fields(name = %name))]
    async fn load_for_mutation(&self, name: &str) -> Result<Option<Skill>, SkillError> {
        let name = Self::validate_segment(name, "name")?;
        if let Some((candidate, directory)) = self.load_pending_candidate(&name)? {
            return self.checked_pending_skill(&candidate, &directory).map(Some);
        }
        let lock = self.local_lock()?;
        if let Some(entry) = lock
            .packages
            .iter()
            .find(|entry| entry.manifest.identity.name == name)
        {
            if entry.manifest.source == SkillPackageSource::LocalAuthored
                && matches!(
                    entry.manifest.proof,
                    SkillPackageProof::PendingOwnerAttestation(_)
                )
            {
                return self.checked_staged_skill(entry).map(Some);
            }
        }
        self.load(&name).await
    }

    #[instrument(skip(self))]
    async fn list(&self) -> Result<Vec<SkillMeta>, SkillError> {
        if !skill_lock_path(&self.skills_dir).exists() {
            return Ok(Vec::new());
        }
        let trust_store = self.trust_store()?;
        let mut metas = Vec::new();
        for result in trust_store.list(false).map_err(trust_error)? {
            let SkillTrustLoad::Admitted(package) = result else {
                if let SkillTrustLoad::Quarantined { name, quarantine } = result {
                    warn!(skill = %name, reason = ?quarantine.reason, "excluding quarantined skill from list");
                }
                continue;
            };
            match trust_store
                .read_contained(&package.entry.manifest.identity.name, Path::new("SKILL.md"), false)
                .map_err(trust_error)?
            {
                SkillTrustRead::Read { package, bytes } => {
                    let raw = String::from_utf8(bytes).map_err(|_| Self::quarantined(
                        package.entry.manifest.identity.name.clone(),
                        SkillQuarantineReason::FilesystemViolation,
                        "SKILL.md is not UTF-8".to_string(),
                    ))?;
                    let (frontmatter, _) = Self::parse_skill_file(&raw)?;
                    if frontmatter.name != package.entry.manifest.identity.name {
                        warn!(skill = %package.entry.manifest.identity.name, "excluding identity-mismatched skill from list");
                        continue;
                    }
                    metas.push(SkillMeta {
                        name: frontmatter.name,
                        category: frontmatter.category,
                        description: frontmatter.description,
                    });
                }
                SkillTrustRead::NotFound | SkillTrustRead::Quarantined { .. } => {}
            }
        }
        metas.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(metas)
    }

    #[instrument(skip(self), fields(name = %name))]
    async fn delete(&self, name: &str) -> Result<(), SkillError> {
        let name = Self::validate_segment(name, "name")?;
        let pending_exists = self.load_pending_candidate(&name)?.is_some();
        let mut lock = self.local_lock()?;
        let Some(index) = lock
            .packages
            .iter()
            .position(|entry| entry.manifest.identity.name == name)
        else {
            return if pending_exists {
                self.remove_pending_candidate(&name)
            } else if self.raw_skill_named_exists(&name) {
                Err(Self::quarantined(
                    name,
                    SkillQuarantineReason::MissingLock,
                    "refusing to delete untracked drop-in skill".to_string(),
                ))
            } else {
                Ok(())
            };
        };
        let entry = lock.packages.remove(index);
        let directory = self.entry_root(&entry)?;
        // Commit unreachability first. If filesystem deletion fails, the
        // residual directory is an untracked drop-in and cannot load.
        store_lock(&self.skills_dir, &lock).map_err(trust_error)?;
        fs::remove_dir_all(directory)?;
        if pending_exists {
            if let Err(error) = self.remove_pending_candidate(&name) {
                warn!(skill = %name, error = %error, "deleted active skill left pending candidate for later cleanup");
            }
        }
        Ok(())
    }
}

fn trust_error(error: impl std::fmt::Display) -> SkillError {
    SkillError::Io(format!("skill trust: {error}"))
}

fn authority_trust_error(error: SkillTrustError) -> SkillError {
    let reason = match error {
        SkillTrustError::UnprovisionedAuthority(_) => SkillQuarantineReason::UnprovisionedAuthority,
        SkillTrustError::KeyringRollback(_) => SkillQuarantineReason::KeyringRollback,
        SkillTrustError::InvalidTrustRoot(_) | SkillTrustError::MalformedKeyring(_) => {
            SkillQuarantineReason::InvalidTrustRoot
        }
        SkillTrustError::LegacyLock => SkillQuarantineReason::LegacyLock,
        SkillTrustError::Io(_) | SkillTrustError::Serialization(_) | SkillTrustError::Malformed(_) => {
            SkillQuarantineReason::InvalidTrustRoot
        }
    };
    SkillError::Quarantined {
        reason,
        detail: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn skill(name: &str, content: &str) -> Skill {
        Skill {
            name: name.to_string(),
            content: content.to_string(),
            category: None,
            description: None,
        }
    }

    fn local_store(
        directory: &Path,
        seed: [u8; 32],
    ) -> (FileSkillStore, crate::LocalOwnerSigningKey) {
        let signer = crate::LocalOwnerSigningKey::from_seed(seed);
        let store = FileSkillStore::new(directory.to_path_buf()).with_trusted_local_owner_keys(vec![
            signer.trusted_key("owner", "owner-key", true),
        ]);
        (store, signer)
    }

    fn attest(
        store: &FileSkillStore,
        signer: &crate::LocalOwnerSigningKey,
        name: &str,
        attestation_id: &str,
    ) -> Result<(), SkillError> {
        let mut manifest = store
            .pending_local_manifest(name)?
            .ok_or_else(|| SkillError::NotFound(name.to_string()))?;
        let attestation = signer
            .sign_local_manifest(
                &mut manifest,
                "owner",
                "owner-key",
                attestation_id,
                "2026-07-20T00:00:00Z",
            )
            .map_err(trust_error)?;
        store.attest_local_skill(name, attestation)
    }

    #[test]
    fn frontmatter_round_trip() {
        let frontmatter = SkillFrontmatter {
            name: "example".to_string(),
            category: Some("general".to_string()),
            description: Some("example".to_string()),
            version: Some("1".to_string()),
        };
        let rendered = FileSkillStore::render_skill_file(&frontmatter, "# Example");
        let (parsed, body) = FileSkillStore::parse_skill_file(&rendered).unwrap();
        assert_eq!(parsed, frontmatter);
        assert_eq!(body, "# Example");
    }

    #[tokio::test]
    async fn local_save_stays_staged_until_explicit_owner_attestation() {
        let directory = tempdir().unwrap();
        let signer = crate::LocalOwnerSigningKey::from_seed([3; 32]);
        let store = FileSkillStore::new(directory.path().to_path_buf())
            .with_trusted_local_owner_keys(vec![
                signer.trusted_key("owner", "owner-key", true),
            ]);
        let original = Skill {
            name: "greet".to_string(),
            content: "# Greet\n- Say hello".to_string(),
            category: Some("social".to_string()),
            description: Some("Greeting".to_string()),
        };
        store.save(&original).await.unwrap();
        let lock = crate::load_lock(directory.path()).unwrap();
        assert!(lock.packages.is_empty());
        let mut pending = store
            .pending_local_manifest("greet")
            .unwrap()
            .expect("pending candidate");
        assert!(matches!(
            pending.proof,
            SkillPackageProof::PendingOwnerAttestation(_)
        ));
        assert!(matches!(store.load("greet").await, Err(SkillError::Quarantined { .. })));
        assert_eq!(store.load_for_mutation("greet").await.unwrap(), Some(original.clone()));

        let attestation = signer
            .sign_local_manifest(
                &mut pending,
                "owner",
                "owner-key",
                "owner-approved-1",
                "2026-07-20T00:00:00Z",
            )
            .unwrap();
        store.attest_local_skill("greet", attestation).unwrap();
        assert_eq!(store.load("greet").await.unwrap(), Some(original));
    }

    #[tokio::test]
    async fn tampered_locked_file_is_quarantined() {
        let directory = tempdir().unwrap();
        let signer = crate::LocalOwnerSigningKey::from_seed([4; 32]);
        let store = FileSkillStore::new(directory.path().to_path_buf())
            .with_trusted_local_owner_keys(vec![
                signer.trusted_key("owner", "owner-key", true),
            ]);
        store.save(&skill("greet", "# Greet\n- Say hello")).await.unwrap();
        let mut pending = store
            .pending_local_manifest("greet")
            .unwrap()
            .expect("pending candidate");
        let attestation = signer
            .sign_local_manifest(
                &mut pending,
                "owner",
                "owner-key",
                "owner-approved-2",
                "2026-07-20T00:00:00Z",
            )
            .unwrap();
        store.attest_local_skill("greet", attestation).unwrap();
        let lock = crate::load_lock(directory.path()).unwrap();
        fs::write(
            directory
                .path()
                .join(&lock.packages[0].install_path)
                .join("SKILL.md"),
            "---\nname: greet\n---\n# altered",
        )
        .unwrap();
        assert!(matches!(store.load("greet").await, Err(SkillError::Quarantined { .. })));
    }

    #[tokio::test]
    async fn drop_in_skill_is_not_loadable_without_v2_lock() {
        let directory = tempdir().unwrap();
        let store = FileSkillStore::new(directory.path().to_path_buf());
        fs::create_dir_all(directory.path().join("dropin")).unwrap();
        fs::write(
            directory.path().join("dropin/SKILL.md"),
            "---\nname: dropin\n---\n# Dropin",
        )
        .unwrap();
        assert!(matches!(store.load("dropin").await, Err(SkillError::Quarantined { .. })));
    }

    #[tokio::test]
    async fn path_traversal_is_rejected() {
        let directory = tempdir().unwrap();
        let store = FileSkillStore::new(directory.path().to_path_buf());
        assert!(store.save(&skill("../escape", "# no")).await.is_err());
        assert!(store.load("../escape").await.is_err());
    }

    #[tokio::test]
    async fn local_candidate_failures_preserve_incumbent_and_attestation_switches_once() {
        let directory = tempdir().unwrap();
        let (store, signer) = local_store(directory.path(), [11; 32]);
        let original = skill("greet", "# Original\n- safe");
        store.save(&original).await.unwrap();
        attest(&store, &signer, "greet", "initial").unwrap();
        let incumbent_lock = crate::load_lock(directory.path()).unwrap();
        let incumbent_path = incumbent_lock.packages[0].install_path.clone();
        assert_eq!(store.load("greet").await.unwrap(), Some(original.clone()));

        let updated = skill("greet", "# Updated\n- safe");
        store.save(&updated).await.unwrap();
        assert_eq!(store.load("greet").await.unwrap(), Some(original.clone()));
        assert_eq!(crate::load_lock(directory.path()).unwrap(), incumbent_lock);

        let mut signed_candidate = store
            .pending_local_manifest("greet")
            .unwrap()
            .expect("updated candidate");
        let tampered_attestation = signer
            .sign_local_manifest(
                &mut signed_candidate,
                "owner",
                "owner-key",
                "tampered",
                "2026-07-20T00:00:00Z",
            )
            .unwrap();
        fs::write(
            directory
                .path()
                .join(".hub/pending/greet/payload/SKILL.md"),
            "---\nname: greet\n---\n# Tampered",
        )
        .unwrap();
        assert!(store
            .attest_local_skill("greet", tampered_attestation)
            .is_err());
        assert_eq!(store.load("greet").await.unwrap(), Some(original.clone()));
        assert_eq!(crate::load_lock(directory.path()).unwrap(), incumbent_lock);

        store.save(&updated).await.unwrap();
        let pending = store
            .pending_local_manifest("greet")
            .unwrap()
            .expect("replacement candidate");
        let digest = pending
            .identity
            .content_digest
            .strip_prefix("sha256:")
            .unwrap()
            .to_string();
        let collision = directory
            .path()
            .join(".packages/local/greet")
            .join(digest);
        fs::create_dir_all(&collision).unwrap();
        fs::write(collision.join("SKILL.md"), "conflicting payload").unwrap();
        let mut collision_candidate = pending;
        let collision_attestation = signer
            .sign_local_manifest(
                &mut collision_candidate,
                "owner",
                "owner-key",
                "collision",
                "2026-07-20T00:00:00Z",
            )
            .unwrap();
        assert!(store
            .attest_local_skill("greet", collision_attestation)
            .is_err());
        assert_eq!(store.load("greet").await.unwrap(), Some(original.clone()));
        assert_eq!(crate::load_lock(directory.path()).unwrap(), incumbent_lock);

        fs::remove_dir_all(collision).unwrap();
        attest(&store, &signer, "greet", "promoted").unwrap();
        let switched_lock = crate::load_lock(directory.path()).unwrap();
        assert_eq!(switched_lock.packages.len(), 1);
        assert_ne!(switched_lock.packages[0].install_path, incumbent_path);
        assert_eq!(store.load("greet").await.unwrap(), Some(updated));
    }

    #[tokio::test]
    async fn full_local_bundle_is_staged_and_promoted_without_loss() {
        let directory = tempdir().unwrap();
        let (store, signer) = local_store(directory.path(), [12; 32]);
        store
            .stage_local_package(
                "bundle",
                Path::new("catalog/bundle"),
                &[
                    LocalPackageFile::new(
                        "SKILL.md",
                        "---\nname: bundle\ndescription: full\n---\n# Bundle\n",
                    ),
                    LocalPackageFile::new("templates/message.md", "hello {{name}}\n"),
                ],
            )
            .unwrap();
        attest(&store, &signer, "bundle", "full-bundle").unwrap();
        let lock = crate::load_lock(directory.path()).unwrap();
        let root = directory.path().join(&lock.packages[0].install_path);
        assert_eq!(
            fs::read_to_string(root.join("templates/message.md")).unwrap(),
            "hello {{name}}\n"
        );
        assert_eq!(
            canonical_bundle_digest(&root).unwrap(),
            lock.packages[0].manifest.identity.content_digest
        );
    }

    #[tokio::test]
    async fn signed_remote_install_stages_reverifies_and_preserves_incumbent_on_failure() {
        let temporary = tempdir().unwrap();
        let skills_root = temporary.path().join("skills");
        fs::create_dir_all(&skills_root).unwrap();
        let authority = temporary.path().join("authority");
        fs::create_dir_all(&authority).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&authority, fs::Permissions::from_mode(0o700)).unwrap();
        }
        let root_signer = crate::SkillSigningKey::from_seed([61; 32]);
        let publisher = crate::SkillSigningKey::from_seed([62; 32]);
        let root_file = authority.join("remote-root.json");
        let audit_file = authority.join("remote-audit.json");
        let local_owner_file = authority.join("local-owner.json");
        crate::store_remote_trust_root_file(
            &skills_root,
            &root_file,
            &root_signer.remote_trust_root("root-key", 1),
        )
        .unwrap();
        crate::store_local_owner_keyring_file(
            &skills_root,
            &local_owner_file,
            &crate::LocalOwnerKeyring::new(Vec::new()).unwrap(),
        )
        .unwrap();
        let mut keyring = crate::SignedRemoteSkillKeyring::new(
            1,
            vec![publisher.trusted_publisher_key(
                "publisher.example",
                "publisher-key",
                true,
            )],
        )
        .unwrap();
        root_signer
            .sign_remote_keyring(&mut keyring, "root-key")
            .unwrap();
        crate::store_signed_remote_keyring(&skills_root, &keyring).unwrap();
        let authority_paths = crate::SkillTrustAuthorityPaths::new(
            skills_root.clone(),
            root_file,
            audit_file,
            local_owner_file,
        )
        .unwrap();
        let store = FileSkillStore::new(skills_root.clone()).with_authority_paths(authority_paths);

        let payload = "---\nname: remote\ndescription: signed\n---\n# Remote\n- safe\n";
        let source = temporary.path().join("source");
        fs::create_dir_all(&source).unwrap();
        fs::write(source.join("SKILL.md"), payload).unwrap();
        let digest = canonical_bundle_digest(&source).unwrap();
        let identity = crate::SkillPackageIdentityV2::new(
            "remote",
            "1.0.0",
            "https://registry.example/remote",
            "publisher.example",
            "0123456789abcdef0123456789abcdef01234567",
            digest,
            "2026-07-20T00:00:00Z",
        );
        let mut manifest = crate::SkillPackageManifestV2::new(
            crate::SkillPackageSource::Remote,
            identity,
            crate::SkillPackageProof::RemoteSignature(crate::SkillSignature::ed25519(
                "publisher-key",
                "",
            )),
            full_skill_scan(&source),
            crate::SkillCompatibility::default(),
            crate::SkillPermissions::default(),
            Vec::new(),
        );
        publisher.sign_manifest(&mut manifest, "publisher-key").unwrap();
        let admission = store
            .install_signed_remote_package(
                manifest.clone(),
                &[LocalPackageFile::new("SKILL.md", payload)],
                false,
            )
            .unwrap();
        assert_eq!(admission.decision, crate::SkillAdmissionDecision::Allow);
        assert!(store.load("remote").await.unwrap().is_some());
        let incumbent = crate::load_lock(&skills_root).unwrap();
        assert!(incumbent.packages[0]
            .install_path
            .starts_with(".packages/remote/remote/"));

        let mut bad_manifest = manifest.clone();
        bad_manifest.identity.content_digest = format!("sha256:{}", "0".repeat(64));
        bad_manifest.refresh_manifest_digest().unwrap();
        publisher
            .sign_manifest(&mut bad_manifest, "publisher-key")
            .unwrap();
        assert!(store
            .install_signed_remote_package(
                bad_manifest,
                &[LocalPackageFile::new("SKILL.md", payload)],
                false,
            )
            .is_err());
        assert_eq!(crate::load_lock(&skills_root).unwrap(), incumbent);

        fs::write(
            skills_root
                .join(&incumbent.packages[0].install_path)
                .join("SKILL.md"),
            "---\nname: remote\n---\n# Tampered",
        )
        .unwrap();
        assert!(matches!(
            store.load("remote").await,
            Err(SkillError::Quarantined { .. })
        ));
    }

    #[tokio::test]
    async fn unprovisioned_external_authority_is_a_typed_load_quarantine() {
        let temporary = tempdir().unwrap();
        let skills_root = temporary.path().join("skills");
        let authority = temporary.path().join("authority");
        fs::create_dir_all(&skills_root).unwrap();
        fs::create_dir_all(&authority).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(&authority, fs::Permissions::from_mode(0o700)).unwrap();
        }
        crate::store_lock(&skills_root, &crate::SkillLockV2::default()).unwrap();
        let authority_paths = crate::SkillTrustAuthorityPaths::new(
            skills_root.clone(),
            authority.join("remote-root.json"),
            authority.join("remote-audit.json"),
            authority.join("local-owner.json"),
        )
        .unwrap();
        let store = FileSkillStore::new(skills_root).with_authority_paths(authority_paths);
        assert!(matches!(
            store.load("missing").await,
            Err(SkillError::Quarantined {
                reason: SkillQuarantineReason::UnprovisionedAuthority,
                ..
            })
        ));
    }
}
