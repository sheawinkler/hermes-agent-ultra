//! Skill utility functions for parsing, discovering, and managing skills.
//!
//! Provides helpers for YAML frontmatter extraction, skill directory scanning,
//! condition and config variable parsing, platform matching, and disabled-skill
//! filtering.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use hermes_skills::{
    load_local_owner_keyring_file, SkillAdmission, SkillLockEntryV2, SkillPackageManifestV2,
    SkillPackageProof, SkillPackageSource, SkillPermission, SkillTrustLoad, SkillTrustPolicy,
    SkillTrustRead, SkillTrustStore, SkillTrustVerifier,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Immutable verification evidence attached to every skill that can enter a
/// prompt. This is derived from the v2 lock manifest and a fresh core verifier
/// admission; frontmatter is never an authority for package identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillTrustIdentity {
    pub package_name: String,
    pub version: String,
    pub origin: String,
    pub publisher_id: String,
    pub immutable_source_revision: String,
    pub content_digest: String,
    pub manifest_digest: String,
    pub key_id: Option<String>,
    pub local_attestation_id: Option<String>,
    pub permissions: Vec<String>,
    pub source: SkillPackageSource,
    pub scan_verdict: String,
    pub findings_digest: String,
    pub effects_allowed: bool,
}

impl SkillTrustIdentity {
    fn from_verified(
        manifest: &SkillPackageManifestV2,
        admission: &SkillAdmission,
    ) -> Result<Self, String> {
        let (key_id, local_attestation_id) = match &manifest.proof {
            SkillPackageProof::RemoteSignature(signature) => (Some(signature.key_id.clone()), None),
            SkillPackageProof::LocalOwnerAttestation(attestation) => {
                (None, Some(attestation.attestation_id.clone()))
            }
            SkillPackageProof::EmbeddedManifest { .. } => (None, None),
            // A staged record is never a verified identity, even if an
            // upstream caller mistakenly reaches this materialization path.
            SkillPackageProof::PendingOwnerAttestation(_) => {
                return Err(
                    "package is quarantined pending an explicit owner attestation".to_string(),
                )
            }
        };
        Ok(Self {
            package_name: admission.identity.name.clone(),
            version: admission.identity.version.clone(),
            origin: admission.identity.origin.clone(),
            publisher_id: admission.identity.publisher_id.clone(),
            immutable_source_revision: admission.identity.immutable_source_revision.clone(),
            content_digest: admission.identity.content_digest.clone(),
            manifest_digest: admission.identity.manifest_digest.clone(),
            key_id,
            local_attestation_id,
            permissions: manifest
                .permissions
                .requested
                .iter()
                .map(permission_label)
                .map(str::to_string)
                .collect(),
            source: manifest.source,
            scan_verdict: admission.scan.verdict.as_str().to_string(),
            findings_digest: admission.scan.findings_digest.clone(),
            // The verifier is the sole authority for effect admission. Prompt
            // consumers do not turn this metadata into a tool permission.
            effects_allowed: admission.effects_allowed,
        })
    }
}

fn permission_label(permission: &SkillPermission) -> &'static str {
    match permission {
        SkillPermission::Prompt => "prompt",
        SkillPermission::Tool => "tool",
        SkillPermission::Network => "network",
        SkillPermission::FilesystemRead => "filesystem_read",
        SkillPermission::FilesystemWrite => "filesystem_write",
        SkillPermission::Process => "process",
        SkillPermission::PluginRegistration => "plugin_registration",
    }
}

#[derive(Debug, Clone, Default)]
struct VerifiedSkillLocator {
    skills_root: PathBuf,
    stable_identity: String,
}

/// Metadata and verified content for one locked skill package.
///
/// `locator` is private so callers cannot manufacture a body-only skill and
/// register it for prompt use.
#[derive(Debug, Clone)]
pub struct SkillInfo {
    /// Canonical package name from the signed/attested manifest.
    pub name: String,
    /// Logical package install path retained only for deterministic catalog
    /// ordering. It is intentionally not exposed to prompt/tool consumers.
    path: PathBuf,
    /// Verified `SKILL.md` text.
    pub content: String,
    pub frontmatter: HashMap<String, Value>,
    pub body: String,
    pub trust: SkillTrustIdentity,
    locator: VerifiedSkillLocator,
}

impl SkillInfo {
    /// True only for an object produced by the verified lock-store adapter.
    pub fn is_verified(&self) -> bool {
        !self.locator.stable_identity.is_empty()
    }

    pub fn provenance(&self) -> &SkillTrustIdentity {
        &self.trust
    }

    /// Return the verified manifest install path's first component as display
    /// metadata, without exposing a path consumers could read directly.
    pub fn category(&self) -> Option<&str> {
        self.path
            .components()
            .next()
            .and_then(|component| component.as_os_str().to_str())
            .filter(|category| !category.trim().is_empty())
    }
}

impl SkillInfo {
    #[cfg(test)]
    pub(crate) fn test_fixture(
        name: impl Into<String>,
        path: PathBuf,
        content: impl Into<String>,
        frontmatter: HashMap<String, Value>,
        body: impl Into<String>,
    ) -> Self {
        let name = name.into();
        Self {
            trust: SkillTrustIdentity {
                package_name: name.clone(),
                version: "0.0.0-test".to_string(),
                origin: "local://test".to_string(),
                publisher_id: "local:test".to_string(),
                immutable_source_revision: "sha256:test".to_string(),
                content_digest: "sha256:test".to_string(),
                manifest_digest: "sha256:test".to_string(),
                key_id: None,
                local_attestation_id: Some("test-attestation".to_string()),
                permissions: vec!["prompt".to_string()],
                source: SkillPackageSource::LocalAuthored,
                scan_verdict: "clean".to_string(),
                findings_digest: "sha256:test".to_string(),
                effects_allowed: false,
            },
            name,
            path,
            content: content.into(),
            frontmatter,
            body: body.into(),
            locator: VerifiedSkillLocator {
                skills_root: PathBuf::from("<test-skill-root>"),
                stable_identity: "test-fixture".to_string(),
            },
        }
    }
}

/// Core verifier context supplied by the runtime owner. The default context
/// loads the owner-only keyring from each actual skill root; an absent or
/// malformed keyring therefore fails closed rather than treating a label as
/// trust. Tests and embedded callers may inject an explicit verifier.
#[derive(Clone)]
enum SkillTrustRuntimeSource {
    Explicit(Arc<SkillTrustVerifier>),
    RootConfigured {
        policy: SkillTrustPolicy,
        embedded_manifest_digests: BTreeMap<String, String>,
    },
}

#[derive(Clone)]
pub struct SkillTrustRuntime {
    source: SkillTrustRuntimeSource,
}

impl std::fmt::Debug for SkillTrustRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SkillTrustRuntime")
            .field("source", &"<core-owned>")
            .finish()
    }
}

impl SkillTrustRuntime {
    pub fn new(verifier: SkillTrustVerifier) -> Self {
        Self {
            source: SkillTrustRuntimeSource::Explicit(Arc::new(verifier)),
        }
    }

    /// Unprovisioned fallback with no external authority material. It admits
    /// no local-owner or remote package.
    pub fn fail_closed() -> Result<Self, String> {
        Ok(Self {
            source: SkillTrustRuntimeSource::RootConfigured {
                policy: SkillTrustPolicy::default(),
                embedded_manifest_digests: BTreeMap::new(),
            },
        })
    }
}

/// Build a verifier from the sole canonical authority layout for an installed
/// skill root. The root must be exactly `<resolved home>/skills`; authority
/// files are always read from the sibling external trust directory.
///
/// A complete remote authority (root, audit, and local-owner keyring) enables
/// remote and local packages. A local-owner keyring on its own enables only
/// local owner-attested packages. Any partial authority is a provisioning
/// error, while an entirely absent authority remains fail-closed.
pub fn skill_trust_runtime_for_skills_root(
    skills_root: &Path,
) -> Result<SkillTrustRuntime, String> {
    let home = skills_root
        .parent()
        .ok_or_else(|| "skill root has no parent home for its authority layout".to_string())?;
    let layout = hermes_config::SkillTrustAuthorityLayout::from_resolved_home(home)
        .map_err(|error| format!("invalid skill trust authority layout: {error}"))?;
    if layout.skills_root() != skills_root {
        return Err(
            "skill root does not match the canonical authority layout derived from its home"
                .to_string(),
        );
    }

    let remote_root = authority_file_present(layout.remote_root_file(), "remote trust root")?;
    let remote_audit = authority_file_present(layout.remote_audit_file(), "remote trust audit")?;
    let local_owner_keyring =
        authority_file_present(layout.local_owner_keyring_file(), "local owner keyring")?;

    match (remote_root, remote_audit, local_owner_keyring) {
        (true, true, true) => {
            let authority = layout
                .build_skill_trust_authority_paths()
                .map_err(|error| format!("invalid skill trust authority paths: {error}"))?;
            let verifier = authority
                .build_verifier(SkillTrustPolicy::default(), BTreeMap::new())
                .map_err(|error| format!("invalid configured skill trust authority: {error}"))?;
            Ok(SkillTrustRuntime::new(verifier))
        }
        (false, false, true) => {
            let keyring = load_local_owner_keyring_file(
                layout.skills_root(),
                layout.local_owner_keyring_file(),
            )
            .map_err(|error| format!("invalid local-owner skill authority: {error}"))?;
            let verifier = SkillTrustVerifier::for_skill_root_with_local_owner_keys(
                layout.skills_root(),
                SkillTrustPolicy::default(),
                BTreeMap::new(),
                keyring.keys,
            )
            .map_err(|error| format!("invalid local-owner skill verifier: {error}"))?;
            Ok(SkillTrustRuntime::new(verifier))
        }
        (false, false, false) => SkillTrustRuntime::fail_closed()
            .map_err(|error| format!("failed to construct fail-closed skill verifier: {error}")),
        (remote_root, remote_audit, local_owner_keyring) => Err(format!(
            "incomplete skill trust authority provisioning (remote_root={remote_root}, remote_audit={remote_audit}, local_owner_keyring={local_owner_keyring})"
        )),
    }
}

fn authority_file_present(path: &Path, label: &str) -> Result<bool, String> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(format!("could not inspect {label}: {error}")),
    }
}

/// Render deterministic, escaped package evidence into a fresh prompt block.
/// This is provenance data, not a policy grant: tool policy remains the
/// independent authority for any effect.
pub fn render_skill_provenance_tag(identity: &SkillTrustIdentity) -> String {
    let key_id = identity.key_id.as_deref().unwrap_or("");
    let local_attestation = identity.local_attestation_id.as_deref().unwrap_or("");
    format!(
        "<skill_provenance package=\"{}\" version=\"{}\" publisher=\"{}\" digest=\"{}\" key_id=\"{}\" local_attestation=\"{}\" revision=\"{}\" permissions=\"{}\" source=\"{}\" effects_allowed=\"{}\" />",
        escape_provenance_attribute(&identity.package_name),
        escape_provenance_attribute(&identity.version),
        escape_provenance_attribute(&identity.publisher_id),
        escape_provenance_attribute(&identity.content_digest),
        escape_provenance_attribute(key_id),
        escape_provenance_attribute(local_attestation),
        escape_provenance_attribute(&identity.immutable_source_revision),
        escape_provenance_attribute(&identity.permissions.join(",")),
        escape_provenance_attribute(match identity.source {
            SkillPackageSource::Remote => "remote",
            SkillPackageSource::EmbeddedBuiltin => "embedded_builtin",
            SkillPackageSource::LocalAuthored => "local_authored",
        }),
        if identity.effects_allowed { "true" } else { "false" },
    )
}

fn escape_provenance_attribute(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
}

/// A typed catalog rejection. Rejections can be rendered for diagnostics but
/// are never candidates for prompt injection, slash commands, or file reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedSkill {
    pub name: String,
    pub reason: String,
}

#[derive(Debug, Clone, Default)]
pub struct VerifiedSkillCatalog {
    pub available: Vec<SkillInfo>,
    pub blocked: Vec<BlockedSkill>,
}

/// A condition that controls when a skill is active.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillCondition {
    pub kind: ConditionKind,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ConditionKind {
    Platform,
    EnvVar,
    FileExists,
    Command,
    Custom,
}

/// A configuration variable declared by a skill.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigVar {
    pub name: String,
    pub description: String,
    pub default: Option<String>,
    pub required: bool,
    pub env_var: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RequiredEnvironmentVariable {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub required_for: Option<String>,
}

// ---------------------------------------------------------------------------
// Frontmatter parsing
// ---------------------------------------------------------------------------

/// Parse YAML frontmatter delimited by `---` from a SKILL.md file.
///
/// Returns the parsed frontmatter as a map and the remaining body content.
/// If no frontmatter is found, returns an empty map and the full content.
pub fn parse_frontmatter(content: &str) -> (HashMap<String, Value>, String) {
    let trimmed = content.trim_start();
    if !trimmed.starts_with("---") {
        return (HashMap::new(), content.to_string());
    }

    let after_first = &trimmed[3..];
    let Some(end_idx) = after_first.find("\n---") else {
        return (HashMap::new(), content.to_string());
    };

    let yaml_block = &after_first[..end_idx];
    let body_start = end_idx + 4; // skip "\n---"
    let body = after_first[body_start..]
        .trim_start_matches('\n')
        .to_string();

    let frontmatter: HashMap<String, Value> =
        serde_yaml::from_str(yaml_block).unwrap_or_else(|_| parse_frontmatter_fallback(yaml_block));

    (frontmatter, body)
}

fn parse_frontmatter_fallback(yaml_block: &str) -> HashMap<String, Value> {
    let mut frontmatter = HashMap::new();
    for line in yaml_block.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((key, value)) = trimmed.split_once(':') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty()
            || key
                .chars()
                .any(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-')))
        {
            continue;
        }
        let value = value
            .trim()
            .trim_matches(|c| matches!(c, '"' | '\''))
            .to_string();
        frontmatter.insert(key.to_string(), Value::String(value));
    }
    frontmatter
}

pub fn parse_tags(value: &Value) -> Vec<String> {
    match value {
        Value::Array(items) => items
            .iter()
            .filter_map(|v| v.as_str())
            .map(clean_tag)
            .filter(|v| !v.is_empty())
            .collect(),
        Value::String(raw) => raw
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .split(',')
            .map(clean_tag)
            .filter(|v| !v.is_empty())
            .collect(),
        _ => Vec::new(),
    }
}

fn clean_tag(raw: &str) -> String {
    raw.trim()
        .trim_matches(|c| matches!(c, '"' | '\''))
        .trim()
        .to_string()
}

// ---------------------------------------------------------------------------
// Skill discovery
// ---------------------------------------------------------------------------

/// Catalog skills exclusively through the core-owned v2 trust store.
///
/// Directories are never recursively discovered: a drop-in `SKILL.md`,
/// missing/legacy lock, or quarantined entry has no route into this catalog.
pub fn discover_verified_skills(
    dirs: &[PathBuf],
    trust: &SkillTrustRuntime,
) -> VerifiedSkillCatalog {
    let mut catalog = VerifiedSkillCatalog::default();
    let mut roots = dirs.to_vec();
    roots.sort();
    roots.dedup();

    for root in roots {
        if !root.exists() {
            continue;
        }
        let store = match trust.store(&root) {
            Ok(store) => store,
            Err(error) => {
                catalog.blocked.push(BlockedSkill {
                    name: root.display().to_string(),
                    reason: format!("skill trust runtime rejected root: {error}"),
                });
                continue;
            }
        };
        let loads = match store.list(false) {
            Ok(loads) => loads,
            Err(error) => {
                catalog.blocked.push(BlockedSkill {
                    name: root.display().to_string(),
                    reason: format!("skill trust store rejected root: {error}"),
                });
                continue;
            }
        };
        for load in loads {
            match load {
                SkillTrustLoad::Admitted(package) => {
                    let name = package.entry.manifest.identity.name.clone();
                    let stable_identity = package.entry.manifest.identity.stable_key();
                    match materialize_verified_skill(&root, &name, &stable_identity, trust) {
                        Ok(skill) => catalog.available.push(skill),
                        Err(reason) => catalog.blocked.push(BlockedSkill { name, reason }),
                    }
                }
                SkillTrustLoad::Quarantined { name, quarantine } => {
                    catalog.blocked.push(BlockedSkill {
                        name,
                        reason: format!(
                            "package quarantined ({:?}): {}",
                            quarantine.reason, quarantine.detail
                        ),
                    });
                }
                SkillTrustLoad::NotFound => {}
            }
        }
    }

    catalog.available.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.path.cmp(&right.path))
    });
    catalog.blocked.sort_by(|left, right| {
        left.name
            .cmp(&right.name)
            .then_with(|| left.reason.cmp(&right.reason))
    });
    catalog
}

/// Compatibility helper for catalog-only callers. Its fail-closed default has
/// no remote trusted keys, so remote packages require an explicit verifier
/// context via [`discover_verified_skills`] before they can appear.
pub fn discover_skills(dirs: &[PathBuf]) -> Vec<SkillInfo> {
    let Ok(trust) = SkillTrustRuntime::fail_closed() else {
        return Vec::new();
    };
    discover_verified_skills(dirs, &trust).available
}

/// Resolve one package selector from the core-owned verified store and
/// reverify it immediately before returning prompt material.
pub fn load_verified_skill_by_selector(
    dirs: &[PathBuf],
    selector: &str,
    trust: &SkillTrustRuntime,
) -> Result<Option<SkillInfo>, String> {
    let selector = normalize_package_selector(selector)
        .ok_or_else(|| "invalid skill package selector".to_string())?;
    let mut roots = dirs.to_vec();
    roots.sort();
    roots.dedup();
    let mut matches = Vec::new();

    for root in roots {
        if !root.exists() {
            continue;
        }
        let store = trust
            .store(&root)
            .map_err(|error| format!("skill trust runtime rejected root: {error}"))?;
        for load in store
            .list(false)
            .map_err(|error| format!("skill trust store rejected root: {error}"))?
        {
            match load {
                SkillTrustLoad::Admitted(package)
                    if entry_matches_selector(&package.entry, &selector) =>
                {
                    let name = package.entry.manifest.identity.name.clone();
                    let stable_identity = package.entry.manifest.identity.stable_key();
                    matches.push(materialize_verified_skill(
                        &root,
                        &name,
                        &stable_identity,
                        trust,
                    )?);
                }
                SkillTrustLoad::Quarantined { name, quarantine }
                    if normalize_package_selector(&name).as_deref() == Some(&selector) =>
                {
                    return Err(format!(
                        "package '{}' is quarantined ({:?}): {}",
                        name, quarantine.reason, quarantine.detail
                    ));
                }
                _ => {}
            }
        }
    }

    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.pop()),
        _ => Err(format!(
            "ambiguous verified skill selector '{}'; use the canonical package name",
            selector
        )),
    }
}

/// Refresh an already selected package through the core store. The core
/// rechecks lock, revocations, compatibility, signature, scan, and digest on
/// every read; this function never follows `SkillInfo::path`.
pub fn reload_verified_skill(
    skill: &SkillInfo,
    trust: &SkillTrustRuntime,
) -> Result<SkillInfo, String> {
    if !skill.is_verified() {
        return Err("skill has no verified package locator".to_string());
    }
    materialize_verified_skill(
        &skill.locator.skills_root,
        &skill.name,
        &skill.locator.stable_identity,
        trust,
    )
}

/// Read one contained package file through the core trust store. The store
/// enforces canonical containment and rechecks package admission around the
/// read; no raw path, symlink, or direct filesystem read reaches consumers.
pub fn read_verified_skill_file(
    skill: &SkillInfo,
    file_path: &str,
    trust: &SkillTrustRuntime,
) -> Result<Vec<u8>, String> {
    if !skill.is_verified() {
        return Err("skill has no verified package locator".to_string());
    }
    let store = trust.store(&skill.locator.skills_root)?;
    match store
        .read_contained(&skill.name, Path::new(file_path), false)
        .map_err(|error| format!("skill trust store file read failed: {error}"))?
    {
        SkillTrustRead::Read { package, bytes } => {
            if package.entry.manifest.identity.stable_key() != skill.locator.stable_identity {
                return Err("selected skill identity changed during verified file read".to_string());
            }
            Ok(bytes)
        }
        SkillTrustRead::NotFound => {
            Err("requested file is not present in the verified package".to_string())
        }
        SkillTrustRead::Quarantined { name, quarantine } => Err(format!(
            "package '{}' is quarantined ({:?}): {}",
            name, quarantine.reason, quarantine.detail
        )),
    }
}

impl SkillTrustRuntime {
    fn store(&self, skills_root: &Path) -> Result<SkillTrustStore, String> {
        match &self.source {
            SkillTrustRuntimeSource::Explicit(verifier) => {
                Ok(SkillTrustStore::with_shared_verifier(
                    skills_root.to_path_buf(),
                    Arc::clone(verifier),
                ))
            }
            SkillTrustRuntimeSource::RootConfigured {
                policy,
                embedded_manifest_digests,
            } => SkillTrustVerifier::for_skill_root(
                skills_root,
                policy.clone(),
                embedded_manifest_digests.clone(),
            )
            .map(|verifier| SkillTrustStore::new(skills_root.to_path_buf(), verifier))
            .map_err(|error| error.to_string()),
        }
    }
}

fn materialize_verified_skill(
    skills_root: &Path,
    name: &str,
    expected_stable_identity: &str,
    trust: &SkillTrustRuntime,
) -> Result<SkillInfo, String> {
    let store = trust.store(skills_root)?;
    match store
        .read_contained(name, Path::new("SKILL.md"), false)
        .map_err(|error| format!("skill trust store prompt read failed: {error}"))?
    {
        SkillTrustRead::Read { package, bytes } => {
            let stable_identity = package.entry.manifest.identity.stable_key();
            if stable_identity != expected_stable_identity {
                return Err("verified package identity changed during prompt read".to_string());
            }
            let content = String::from_utf8(bytes)
                .map_err(|_| "verified SKILL.md is not UTF-8".to_string())?;
            let (frontmatter, body) = parse_frontmatter(&content);
            Ok(SkillInfo {
                // Manifest identity—not frontmatter—is the selector and
                // provenance authority.
                name: package.entry.manifest.identity.name.clone(),
                // This is a logical install path only. It is never joined to
                // a filesystem root or used for content reads.
                path: PathBuf::from(&package.entry.install_path),
                content,
                frontmatter,
                body,
                trust: SkillTrustIdentity::from_verified(
                    &package.entry.manifest,
                    &package.admission,
                )?,
                locator: VerifiedSkillLocator {
                    skills_root: skills_root.to_path_buf(),
                    stable_identity,
                },
            })
        }
        SkillTrustRead::NotFound => Err("verified SKILL.md is not present".to_string()),
        SkillTrustRead::Quarantined { name, quarantine } => Err(format!(
            "package '{}' is quarantined ({:?}): {}",
            name, quarantine.reason, quarantine.detail
        )),
    }
}

fn normalize_package_selector(raw: &str) -> Option<String> {
    let raw = raw.trim().trim_start_matches('/');
    if raw.is_empty()
        || raw.contains('/')
        || raw.contains('\\')
        || raw == "."
        || raw == ".."
        || raw.contains("..")
    {
        return None;
    }
    let normalized = raw
        .chars()
        .filter_map(|character| {
            if character.is_ascii_alphanumeric() {
                Some(character.to_ascii_lowercase())
            } else if matches!(character, '_' | '-' | ' ') {
                Some('-')
            } else {
                None
            }
        })
        .collect::<String>()
        .split('-')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    (!normalized.is_empty()).then_some(normalized)
}

fn entry_matches_selector(entry: &SkillLockEntryV2, selector: &str) -> bool {
    normalize_package_selector(&entry.manifest.identity.name).as_deref() == Some(selector)
        || Path::new(&entry.install_path)
            .file_name()
            .and_then(|name| name.to_str())
            .and_then(normalize_package_selector)
            .as_deref()
            == Some(selector)
}

// ---------------------------------------------------------------------------
// Condition extraction
// ---------------------------------------------------------------------------

/// Extract activation conditions from a skill's frontmatter.
pub fn extract_skill_conditions(skill: &SkillInfo) -> Vec<SkillCondition> {
    let mut conditions = Vec::new();

    if let Some(Value::Array(arr)) = skill.frontmatter.get("conditions") {
        for item in arr {
            if let Some(obj) = item.as_object() {
                let kind = obj.get("type").and_then(|v| v.as_str()).unwrap_or("custom");
                let value = obj
                    .get("value")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default();

                let kind = match kind {
                    "platform" => ConditionKind::Platform,
                    "env" | "env_var" => ConditionKind::EnvVar,
                    "file_exists" | "file" => ConditionKind::FileExists,
                    "command" | "cmd" => ConditionKind::Command,
                    _ => ConditionKind::Custom,
                };

                conditions.push(SkillCondition {
                    kind,
                    value: value.to_string(),
                });
            }
        }
    }

    if let Some(Value::String(platform)) = skill.frontmatter.get("platform") {
        conditions.push(SkillCondition {
            kind: ConditionKind::Platform,
            value: platform.clone(),
        });
    }

    conditions
}

// ---------------------------------------------------------------------------
// Config variable extraction
// ---------------------------------------------------------------------------

/// Extract configuration variables declared in a skill's frontmatter.
pub fn extract_skill_config_vars(skill: &SkillInfo) -> Vec<ConfigVar> {
    let mut vars = Vec::new();

    let config_key = skill
        .frontmatter
        .get("config_vars")
        .or_else(|| skill.frontmatter.get("config"));

    if let Some(Value::Array(arr)) = config_key {
        for item in arr {
            if let Some(obj) = item.as_object() {
                let name = obj
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let description = obj
                    .get("description")
                    .or_else(|| obj.get("desc"))
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string();
                let default = obj
                    .get("default")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                let required = obj
                    .get("required")
                    .and_then(|v| v.as_bool())
                    .unwrap_or(false);
                let env_var = obj
                    .get("env_var")
                    .or_else(|| obj.get("env"))
                    .and_then(|v| v.as_str())
                    .map(String::from);

                if !name.is_empty() {
                    vars.push(ConfigVar {
                        name,
                        description,
                        default,
                        required,
                        env_var,
                    });
                }
            }
        }
    }

    vars
}

pub fn extract_required_environment_variables(
    frontmatter: &HashMap<String, Value>,
) -> Vec<RequiredEnvironmentVariable> {
    let mut vars = Vec::new();

    if let Some(Value::Array(items)) = frontmatter.get("required_environment_variables") {
        for item in items {
            match item {
                Value::String(name) if !name.trim().is_empty() => {
                    vars.push(RequiredEnvironmentVariable {
                        name: name.trim().to_string(),
                        prompt: Some(format!("Enter value for {}", name.trim())),
                        help: None,
                        required_for: None,
                    });
                }
                Value::Object(obj) => {
                    let Some(name) = obj.get("name").and_then(|v| v.as_str()) else {
                        continue;
                    };
                    if name.trim().is_empty() {
                        continue;
                    }
                    vars.push(RequiredEnvironmentVariable {
                        name: name.trim().to_string(),
                        prompt: obj.get("prompt").and_then(|v| v.as_str()).map(String::from),
                        help: obj.get("help").and_then(|v| v.as_str()).map(String::from),
                        required_for: obj
                            .get("required_for")
                            .and_then(|v| v.as_str())
                            .map(String::from),
                    });
                }
                _ => {}
            }
        }
    }

    if let Some(Value::Object(prereqs)) = frontmatter.get("prerequisites") {
        if let Some(Value::Array(env_vars)) = prereqs.get("env_vars") {
            for item in env_vars.iter().filter_map(|v| v.as_str()) {
                let name = item.trim();
                if name.is_empty() || vars.iter().any(|v| v.name == name) {
                    continue;
                }
                vars.push(RequiredEnvironmentVariable {
                    name: name.to_string(),
                    prompt: Some(format!("Enter value for {name}")),
                    help: None,
                    required_for: None,
                });
            }
        }
    }

    vars
}

/// Resolve config variable values from environment and defaults.
pub fn resolve_skill_config_values(
    vars: &[ConfigVar],
    env: &HashMap<String, String>,
) -> HashMap<String, String> {
    let mut resolved = HashMap::new();

    for var in vars {
        // Priority: env override -> env_var -> default
        if let Some(val) = env.get(&var.name) {
            resolved.insert(var.name.clone(), val.clone());
        } else if let Some(env_name) = &var.env_var {
            if let Some(val) = env.get(env_name) {
                resolved.insert(var.name.clone(), val.clone());
            } else if let Ok(val) = std::env::var(env_name) {
                resolved.insert(var.name.clone(), val);
            } else if let Some(def) = &var.default {
                resolved.insert(var.name.clone(), def.clone());
            }
        } else if let Some(def) = &var.default {
            resolved.insert(var.name.clone(), def.clone());
        }
    }

    resolved
}

// ---------------------------------------------------------------------------
// Index files
// ---------------------------------------------------------------------------

/// Iterate over skill index files (skills.json, index.json) in a directory.
pub fn iter_skill_index_files(dir: &Path) -> Vec<PathBuf> {
    let candidates = ["skills.json", "index.json", "skills.yaml", "index.yaml"];
    candidates
        .iter()
        .map(|name| dir.join(name))
        .filter(|p| p.exists())
        .collect()
}

// ---------------------------------------------------------------------------
// Disabled skills
// ---------------------------------------------------------------------------

/// Extract the set of disabled skill names from a config value.
///
/// Looks for `disabled_skills` as an array of strings.
pub fn get_disabled_skill_names(config: &Value) -> HashSet<String> {
    config
        .get("disabled_skills")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// Platform matching
// ---------------------------------------------------------------------------

/// Check if a skill matches the given platform (e.g. "darwin", "linux", "windows").
///
/// If the skill has no platform constraint, it matches all platforms.
pub fn match_platform(skill: &SkillInfo, platform: &str) -> bool {
    match skill.frontmatter.get("platform") {
        None => true,
        Some(Value::String(p)) => {
            let p = p.to_lowercase();
            let platform = platform.to_lowercase();
            p == platform || p == "all" || p == "*"
        }
        Some(Value::Array(arr)) => {
            let platform = platform.to_lowercase();
            arr.iter().any(|v| {
                v.as_str()
                    .map(|s| s.to_lowercase() == platform)
                    .unwrap_or(false)
            })
        }
        _ => true,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_frontmatter_with_yaml() {
        let content = "---\nname: test\nversion: \"1.0\"\n---\n# Body\nHello world";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm.get("name").and_then(|v| v.as_str()), Some("test"));
        assert!(body.starts_with("# Body"));
    }

    #[test]
    fn test_parse_frontmatter_none() {
        let content = "# No frontmatter\nJust content";
        let (fm, body) = parse_frontmatter(content);
        assert!(fm.is_empty());
        assert_eq!(body, content);
    }

    #[test]
    fn test_parse_frontmatter_malformed_yaml_fallback_keeps_simple_keys() {
        let content = "---\nname: test-skill\ndescription: desc\n: invalid\n---\n# Body";
        let (fm, body) = parse_frontmatter(content);
        assert_eq!(fm.get("name").and_then(|v| v.as_str()), Some("test-skill"));
        assert_eq!(fm.get("description").and_then(|v| v.as_str()), Some("desc"));
        assert_eq!(body, "# Body");
    }

    #[test]
    fn test_parse_tags_variants() {
        assert_eq!(
            parse_tags(&serde_json::json!(["a", "b", ""])),
            vec!["a", "b"]
        );
        assert_eq!(
            parse_tags(&Value::String("\"tag1\", 'tag2'".into())),
            vec!["tag1", "tag2"]
        );
        assert_eq!(
            parse_tags(&Value::String("[a, b, c]".into())),
            vec!["a", "b", "c"]
        );
    }

    #[test]
    fn test_match_platform_any() {
        let skill = SkillInfo::test_fixture(
            "test",
            PathBuf::from("/tmp"),
            String::new(),
            HashMap::new(),
            String::new(),
        );
        assert!(match_platform(&skill, "darwin"));
        assert!(match_platform(&skill, "linux"));
    }

    #[test]
    fn test_match_platform_specific() {
        let mut fm = HashMap::new();
        fm.insert("platform".into(), Value::String("darwin".into()));
        let skill = SkillInfo::test_fixture(
            "mac-only",
            PathBuf::from("/tmp"),
            String::new(),
            fm,
            String::new(),
        );
        assert!(match_platform(&skill, "darwin"));
        assert!(!match_platform(&skill, "linux"));
    }

    #[test]
    fn test_match_platform_array() {
        let mut fm = HashMap::new();
        fm.insert("platform".into(), serde_json::json!(["darwin", "linux"]));
        let skill = SkillInfo::test_fixture(
            "unix",
            PathBuf::from("/tmp"),
            String::new(),
            fm,
            String::new(),
        );
        assert!(match_platform(&skill, "darwin"));
        assert!(match_platform(&skill, "linux"));
        assert!(!match_platform(&skill, "windows"));
    }

    #[test]
    fn test_disabled_skill_names() {
        let config = serde_json::json!({
            "disabled_skills": ["skill-a", "skill-b"]
        });
        let disabled = get_disabled_skill_names(&config);
        assert!(disabled.contains("skill-a"));
        assert!(disabled.contains("skill-b"));
        assert!(!disabled.contains("skill-c"));
    }

    #[test]
    fn test_disabled_skill_names_empty() {
        let config = serde_json::json!({});
        let disabled = get_disabled_skill_names(&config);
        assert!(disabled.is_empty());
    }

    #[test]
    fn test_extract_config_vars() {
        let mut fm = HashMap::new();
        fm.insert(
            "config_vars".into(),
            serde_json::json!([
                {"name": "API_KEY", "description": "API key", "required": true, "env_var": "MY_API_KEY"},
                {"name": "TIMEOUT", "desc": "Timeout in seconds", "default": "30"}
            ]),
        );
        let skill = SkillInfo::test_fixture(
            "test",
            PathBuf::from("/tmp"),
            String::new(),
            fm,
            String::new(),
        );
        let vars = extract_skill_config_vars(&skill);
        assert_eq!(vars.len(), 2);
        assert!(vars[0].required);
        assert_eq!(vars[0].env_var.as_deref(), Some("MY_API_KEY"));
        assert_eq!(vars[1].default.as_deref(), Some("30"));
    }

    #[test]
    fn test_extract_required_environment_variables_new_and_legacy() {
        let mut fm = HashMap::new();
        fm.insert(
            "required_environment_variables".into(),
            serde_json::json!([
                {
                    "name": "TENOR_API_KEY",
                    "prompt": "Tenor API key",
                    "help": "Get a key",
                    "required_for": "full functionality"
                }
            ]),
        );
        fm.insert(
            "prerequisites".into(),
            serde_json::json!({"env_vars": ["LEGACY_KEY", "TENOR_API_KEY"]}),
        );

        let vars = extract_required_environment_variables(&fm);
        assert_eq!(vars.len(), 2);
        assert_eq!(vars[0].name, "TENOR_API_KEY");
        assert_eq!(vars[0].prompt.as_deref(), Some("Tenor API key"));
        assert_eq!(vars[1].name, "LEGACY_KEY");
        assert_eq!(
            vars[1].prompt.as_deref(),
            Some("Enter value for LEGACY_KEY")
        );
    }

    #[test]
    fn test_discover_skills_ignores_unlocked_dropins() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("mlops").join("axolotl")).unwrap();
        std::fs::write(
            root.join("mlops").join("axolotl").join("SKILL.md"),
            "---\nname: axolotl-skill\n---\n# Body",
        )
        .unwrap();

        assert!(discover_skills(&[root.to_path_buf()]).is_empty());
    }

    #[test]
    fn test_resolve_config_values() {
        let vars = vec![
            ConfigVar {
                name: "API_KEY".into(),
                description: "key".into(),
                default: Some("default-key".into()),
                required: true,
                env_var: None,
            },
            ConfigVar {
                name: "TIMEOUT".into(),
                description: "timeout".into(),
                default: Some("30".into()),
                required: false,
                env_var: None,
            },
        ];
        let mut env = HashMap::new();
        env.insert("API_KEY".into(), "real-key".into());
        let resolved = resolve_skill_config_values(&vars, &env);
        assert_eq!(resolved.get("API_KEY").unwrap(), "real-key");
        assert_eq!(resolved.get("TIMEOUT").unwrap(), "30");
    }

    #[test]
    fn test_extract_conditions_platform() {
        let mut fm = HashMap::new();
        fm.insert("platform".into(), Value::String("darwin".into()));
        let skill = SkillInfo::test_fixture(
            "test",
            PathBuf::from("/tmp"),
            String::new(),
            fm,
            String::new(),
        );
        let conds = extract_skill_conditions(&skill);
        assert_eq!(conds.len(), 1);
        assert_eq!(conds[0].kind, ConditionKind::Platform);
        assert_eq!(conds[0].value, "darwin");
    }

    #[test]
    fn test_iter_skill_index_files_empty() {
        let dir = PathBuf::from("/nonexistent/dir");
        let files = iter_skill_index_files(&dir);
        assert!(files.is_empty());
    }
}
