//! Secure `plugin.yaml` bundle discovery.
//!
//! Hermes validates plugin package evidence through the v2 skill trust
//! verifier, but deliberately does not load third-party implementation code.
//! Verified bundles are diagnostic metadata until a loader can bind executable
//! bytes to that evidence and hand execution to the policy sandbox.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use hermes_core::ToolResult;
use hermes_skills::{
    load_lock_for_migration, load_revocations, skill_lock_path, SkillAdmission,
    SkillAdmissionDecision, SkillLockLoad, SkillPackageIdentityV2, SkillPackageManifestV2,
    SkillPackageSource, SkillPermission, SkillQuarantine, SkillQuarantineReason,
    SkillTrustAuthorityPaths, SkillTrustError, SkillTrustPolicy, SkillTrustVerifier,
};

// ---------------------------------------------------------------------------
// HookType
// ---------------------------------------------------------------------------

/// Lifecycle hook names reserved for a future loader-bound plugin system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HookType {
    PreToolCall,
    PostToolCall,
    PreLlmCall,
    PostLlmCall,
    PreApiRequest,
    PostApiRequest,
    OnSessionStart,
    OnSessionEnd,
    OnSessionFinalize,
    OnSessionReset,
}

impl HookType {
    pub fn all() -> &'static [HookType] {
        &[
            HookType::PreToolCall,
            HookType::PostToolCall,
            HookType::PreLlmCall,
            HookType::PostLlmCall,
            HookType::PreApiRequest,
            HookType::PostApiRequest,
            HookType::OnSessionStart,
            HookType::OnSessionEnd,
            HookType::OnSessionFinalize,
            HookType::OnSessionReset,
        ]
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            HookType::PreToolCall => "pre_tool_call",
            HookType::PostToolCall => "post_tool_call",
            HookType::PreLlmCall => "pre_llm_call",
            HookType::PostLlmCall => "post_llm_call",
            HookType::PreApiRequest => "pre_api_request",
            HookType::PostApiRequest => "post_api_request",
            HookType::OnSessionStart => "on_session_start",
            HookType::OnSessionEnd => "on_session_end",
            HookType::OnSessionFinalize => "on_session_finalize",
            HookType::OnSessionReset => "on_session_reset",
        }
    }
}

// ---------------------------------------------------------------------------
// HookResult
// ---------------------------------------------------------------------------

/// Hook callback result — allows hooks to inject context or signal errors.
#[derive(Debug, Clone)]
pub enum HookResult {
    /// Hook executed successfully with no side effects.
    Ok,
    /// Hook wants to inject additional context into the message stream.
    InjectContext(String),
    /// Hook wants to rewrite final assistant text before delivery.
    TransformLlmOutput(String),
    /// Hook encountered an error.
    Error(String),
}

#[derive(Debug, Clone)]
pub struct ToolRequestMiddlewareResult {
    pub args: Value,
    pub original_args: Value,
    pub changed: bool,
    pub trace: Vec<Value>,
}

#[derive(Debug, Clone)]
pub struct ToolExecutionMiddlewareContext {
    pub tool_name: String,
    pub tool_call_id: String,
    pub args: Value,
    pub original_args: Value,
    pub turn: u32,
}

// ---------------------------------------------------------------------------
// PluginManifest
// ---------------------------------------------------------------------------

/// Plugin manifest loaded from `plugin.yaml`.
///
/// This is descriptive metadata only until the bundle is admitted through the
/// v2 package verifier. In particular, a manifest's name, version, source
/// label, or permissions must never be treated as a trust decision.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginManifest {
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    /// Requested plugin capabilities. They must exactly match the signed
    /// package manifest; no current discovery result can activate them.
    #[serde(default)]
    pub permissions: Vec<String>,
    /// Dependencies declared by this plugin bundle. Only exact dependency
    /// declarations are accepted for verified diagnostic metadata; legacy
    /// strings are retained solely so they can be reported diagnostically.
    #[serde(default)]
    pub dependencies: Vec<PluginDependency>,
}

/// A dependency declaration from `plugin.yaml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PluginDependency {
    /// A dependency pinned to an exact package version.
    Exact { name: String, version: String },
    /// Historical unscoped syntax. It is never effect-capable.
    Legacy(String),
}

impl PluginDependency {
    fn exact(&self) -> Option<(&str, &str)> {
        match self {
            Self::Exact { name, version } => Some((name, version)),
            Self::Legacy(_) => None,
        }
    }
}

/// Plugin discovery location. This is diagnostic provenance, never trust.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PluginDiscoverySource {
    User,
    Project,
}

impl PluginDiscoverySource {
    pub fn as_str(&self) -> &'static str {
        match self {
            PluginDiscoverySource::User => "user",
            PluginDiscoverySource::Project => "project",
        }
    }
}

/// Plugin-specific reason that an on-disk bundle cannot become effect-capable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginQuarantineReason {
    Core(SkillQuarantineReason),
    InvalidPluginManifest,
    MissingPackageManifest,
    UnsupportedPackageSource,
    ForceOnlyAdmission,
    EffectsNotAllowed,
    ManifestIdentityMismatch,
    PermissionMismatch,
    UnscopedDependency,
    DependencyDeclarationMismatch,
    MissingDependency,
    AmbiguousDependency,
    DependencyCycle,
    DuplicatePackageIdentity,
    Disabled,
}

/// Typed diagnostic evidence for a plugin bundle which is not executable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginQuarantine {
    pub path: PathBuf,
    pub source: PluginDiscoverySource,
    pub identity: Option<SkillPackageIdentityV2>,
    pub reason: PluginQuarantineReason,
    pub detail: String,
}

/// Result of a no-execution plugin discovery pass.
#[derive(Debug, Clone, Default)]
pub struct PluginDiscoveryReport {
    /// Bundles whose package evidence passed strict verification. They are
    /// diagnostic metadata only and cannot execute without a loader-bound
    /// implementation artifact.
    pub admitted: Vec<DiscoveredPlugin>,
    /// Every rejected candidate is retained as typed diagnostic evidence.
    pub quarantined: Vec<PluginQuarantine>,
}

/// A strictly verified plugin bundle used for diagnostics and future loader
/// input. It is not executable and cannot create a registration binding.
#[derive(Debug, Clone)]
pub struct DiscoveredPlugin {
    pub manifest: PluginManifest,
    pub path: PathBuf,
    pub source: PluginDiscoverySource,
    package_manifest: SkillPackageManifestV2,
    admission: SkillAdmission,
}

impl DiscoveredPlugin {
    pub fn identity(&self) -> &SkillPackageIdentityV2 {
        &self.admission.identity
    }

    pub fn admission(&self) -> &SkillAdmission {
        &self.admission
    }
}

/// Plugin manager — a diagnostic bundle-discovery surface.
///
/// Third-party effect execution is unavailable until an implementation loader
/// can cryptographically bind executable bytes to a verified package identity.
pub struct PluginManager;

impl PluginManager {
    pub fn new() -> Self {
        Self
    }

    pub(crate) fn has_effect_capable_callbacks(&mut self) -> bool {
        false
    }

    pub(crate) fn has_tool_execution_middleware(&mut self) -> bool {
        false
    }

    /// No plugin hooks can run without a loader-bound implementation.
    pub fn invoke_hook(&mut self, _hook: HookType, _context: &Value) -> Vec<HookResult> {
        Vec::new()
    }

    /// No plugin middleware can change tool request arguments.
    pub fn apply_tool_request_middleware(
        &mut self,
        _tool_name: &str,
        args: Value,
        _turn: u32,
    ) -> ToolRequestMiddlewareResult {
        ToolRequestMiddlewareResult {
            original_args: args.clone(),
            args,
            changed: false,
            trace: Vec::new(),
        }
    }

    /// No plugin middleware can wrap tool execution.
    pub fn run_tool_execution_middleware<F>(
        &mut self,
        context: ToolExecutionMiddlewareContext,
        mut terminal_call: F,
    ) -> ToolResult
    where
        F: FnMut(Value) -> ToolResult,
    {
        terminal_call(context.args)
    }

    /// Verify bundle metadata under the supplied core verifier. Verification
    /// does not load or execute a plugin implementation.
    pub fn discover_plugins(
        hermes_dir: &Path,
        verifier: &SkillTrustVerifier,
    ) -> PluginDiscoveryReport {
        Self::discover_plugins_with_options(hermes_dir, None, false, verifier)
    }

    /// Build the anchored verifier used for diagnostic discovery. Failure to
    /// load authority becomes typed quarantine; this API never falls back to a
    /// local keyring or trust-on-first-use.
    pub fn discover_plugins_with_authority_paths(
        hermes_dir: &Path,
        cwd: Option<&Path>,
        enable_project_plugins: bool,
        authority: &SkillTrustAuthorityPaths,
        policy: SkillTrustPolicy,
        embedded_manifest_digests: BTreeMap<String, String>,
    ) -> PluginDiscoveryReport {
        let verifier = match authority.build_verifier(policy, embedded_manifest_digests) {
            Ok(verifier) => verifier,
            Err(error) => {
                return PluginDiscoveryReport {
                    admitted: Vec::new(),
                    quarantined: vec![quarantine_from_trust_error(
                        &hermes_dir.join("plugins"),
                        PluginDiscoverySource::User,
                        error,
                    )],
                };
            }
        };
        Self::discover_plugins_with_options(hermes_dir, cwd, enable_project_plugins, &verifier)
    }

    /// Discover user and (when explicitly selected) project bundle metadata
    /// without loading or executing any plugin code.
    pub fn discover_plugins_with_options(
        hermes_dir: &Path,
        cwd: Option<&Path>,
        enable_project_plugins: bool,
        verifier: &SkillTrustVerifier,
    ) -> PluginDiscoveryReport {
        let mut report = PluginDiscoveryReport::default();
        let user_plugins_dir = hermes_dir.join("plugins");
        scan_plugin_root(
            &user_plugins_dir,
            PluginDiscoverySource::User,
            verifier,
            &mut report,
        );

        if enable_project_plugins {
            if let Some(workdir) = cwd {
                let project_plugins_dir = workdir.join(".hermes").join("plugins");
                scan_plugin_root(
                    &project_plugins_dir,
                    PluginDiscoverySource::Project,
                    verifier,
                    &mut report,
                );
            }
        }

        enforce_plugin_dependency_graph(&mut report.admitted, &mut report.quarantined);
        report
    }

    /// Discover using runtime path selection and an externally anchored
    /// authority. The environment selects only the project root; it never
    /// grants a trust bypass or supplies trust material.
    pub fn discover_plugins_runtime_default(
        hermes_dir: &Path,
        authority: &SkillTrustAuthorityPaths,
        policy: SkillTrustPolicy,
        embedded_manifest_digests: BTreeMap<String, String>,
    ) -> PluginDiscoveryReport {
        let enable_project_plugins = std::env::var("HERMES_ENABLE_PROJECT_PLUGINS")
            .ok()
            .is_some_and(|value| {
                matches!(
                    value.trim().to_ascii_lowercase().as_str(),
                    "1" | "true" | "yes" | "on"
                )
            });
        let cwd = std::env::current_dir().ok();
        Self::discover_plugins_with_authority_paths(
            hermes_dir,
            cwd.as_deref(),
            enable_project_plugins,
            authority,
            policy,
            embedded_manifest_digests,
        )
    }
}

fn scan_plugin_root(
    plugins_dir: &Path,
    source: PluginDiscoverySource,
    verifier: &SkillTrustVerifier,
    report: &mut PluginDiscoveryReport,
) {
    let entries = match std::fs::read_dir(plugins_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            report.quarantined.push(plugin_quarantine(
                plugins_dir,
                source,
                None,
                PluginQuarantineReason::Core(SkillQuarantineReason::FilesystemViolation),
                error.to_string(),
            ));
            return;
        }
    };

    for entry_result in entries {
        let entry = match entry_result {
            Ok(entry) => entry,
            Err(error) => {
                report.quarantined.push(plugin_quarantine(
                    plugins_dir,
                    source,
                    None,
                    PluginQuarantineReason::Core(SkillQuarantineReason::FilesystemViolation),
                    error.to_string(),
                ));
                continue;
            }
        };
        let path = entry.path();
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                report.quarantined.push(plugin_quarantine(
                    &path,
                    source,
                    None,
                    PluginQuarantineReason::Core(SkillQuarantineReason::FilesystemViolation),
                    error.to_string(),
                ));
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            report.quarantined.push(plugin_quarantine(
                &path,
                source,
                None,
                PluginQuarantineReason::Core(SkillQuarantineReason::FilesystemViolation),
                "plugin root must be a non-symlink directory",
            ));
            continue;
        }
        if !metadata.is_dir() {
            continue;
        }
        let manifest_path = path.join("plugin.yaml");
        if let Err(error) = std::fs::symlink_metadata(&manifest_path) {
            if error.kind() == std::io::ErrorKind::NotFound {
                continue;
            }
            report.quarantined.push(plugin_quarantine(
                &path,
                source,
                None,
                PluginQuarantineReason::Core(SkillQuarantineReason::FilesystemViolation),
                error.to_string(),
            ));
            continue;
        }

        match verify_plugin_bundle(&path, source, verifier) {
            Ok(bundle) => report.admitted.push(bundle),
            Err(quarantine) => report.quarantined.push(quarantine),
        }
    }
}

fn verify_plugin_bundle(
    root: &Path,
    source: PluginDiscoverySource,
    verifier: &SkillTrustVerifier,
) -> Result<DiscoveredPlugin, PluginQuarantine> {
    let manifest = read_plugin_manifest(root, source)?;
    if root.join(".disabled").exists() {
        return Err(plugin_quarantine(
            root,
            source,
            None,
            PluginQuarantineReason::Disabled,
            "plugin bundle is disabled by its local marker",
        ));
    }

    let lock_path = skill_lock_path(root);
    let lock = match load_lock_for_migration(root) {
        Ok(SkillLockLoad::V2(lock)) if lock_path.exists() => lock,
        Ok(SkillLockLoad::V2(_)) => {
            return Err(plugin_quarantine(
                root,
                source,
                None,
                PluginQuarantineReason::Core(SkillQuarantineReason::MissingLock),
                "plugin bundle has no v2 lock under its actual root",
            ))
        }
        Ok(SkillLockLoad::Legacy { .. }) => {
            return Err(plugin_quarantine(
                root,
                source,
                None,
                PluginQuarantineReason::Core(SkillQuarantineReason::LegacyLock),
                "legacy plugin lock cannot be upgraded into trust",
            ))
        }
        Err(error) => {
            return Err(plugin_quarantine(
                root,
                source,
                None,
                PluginQuarantineReason::Core(SkillQuarantineReason::MalformedLock),
                error.to_string(),
            ))
        }
    };
    if lock.packages.len() != 1 {
        return Err(plugin_quarantine(
            root,
            source,
            None,
            PluginQuarantineReason::MissingPackageManifest,
            "a plugin root must contain exactly one v2 package manifest",
        ));
    }
    let entry = &lock.packages[0];
    if entry.install_path != "." {
        return Err(plugin_quarantine(
            root,
            source,
            Some(entry.manifest.identity.clone()),
            PluginQuarantineReason::MissingPackageManifest,
            "plugin package must be locked at the actual bundle root",
        ));
    }
    let revocations = match load_revocations(root) {
        Ok(revocations) => revocations,
        Err(error) => {
            return Err(plugin_quarantine(
                root,
                source,
                Some(entry.manifest.identity.clone()),
                PluginQuarantineReason::Core(SkillQuarantineReason::InvalidRevocationList),
                error.to_string(),
            ))
        }
    };
    let admission = match verifier.verify_lock(root, &lock, &revocations, false) {
        Ok(mut admissions) if admissions.len() == 1 => admissions.remove(0),
        Ok(_) => {
            return Err(plugin_quarantine(
                root,
                source,
                Some(entry.manifest.identity.clone()),
                PluginQuarantineReason::Core(SkillQuarantineReason::MalformedLock),
                "verifier did not return exactly one package admission",
            ))
        }
        Err(error) => return Err(quarantine_from_trust_error(root, source, error)),
    };
    if admission.decision == SkillAdmissionDecision::AllowWithForce {
        return Err(plugin_quarantine(
            root,
            source,
            Some(admission.identity),
            PluginQuarantineReason::ForceOnlyAdmission,
            "plugin discovery never enables force-only package admission",
        ));
    }
    if admission.decision != SkillAdmissionDecision::Allow {
        let quarantine = admission.quarantine.unwrap_or(SkillQuarantine {
            reason: SkillQuarantineReason::ScanMismatch,
            detail: "package was not strictly admitted".to_string(),
        });
        return Err(plugin_quarantine(
            root,
            source,
            Some(admission.identity),
            PluginQuarantineReason::Core(quarantine.reason),
            quarantine.detail,
        ));
    }
    if !admission.effects_allowed {
        return Err(plugin_quarantine(
            root,
            source,
            Some(admission.identity),
            PluginQuarantineReason::EffectsNotAllowed,
            "the package admission does not permit effects",
        ));
    }
    if !matches!(
        entry.manifest.source,
        SkillPackageSource::Remote | SkillPackageSource::EmbeddedBuiltin
    ) {
        return Err(plugin_quarantine(
            root,
            source,
            Some(admission.identity),
            PluginQuarantineReason::UnsupportedPackageSource,
            "only signed remote and verified embedded plugin packages may pass v2 verification",
        ));
    }

    validate_plugin_manifest(&manifest, &entry.manifest, root, source)?;
    Ok(DiscoveredPlugin {
        manifest,
        path: root.to_path_buf(),
        source,
        package_manifest: entry.manifest.clone(),
        admission,
    })
}

fn read_plugin_manifest(
    root: &Path,
    source: PluginDiscoverySource,
) -> Result<PluginManifest, PluginQuarantine> {
    let manifest_path = root.join("plugin.yaml");
    let metadata = match std::fs::symlink_metadata(&manifest_path) {
        Ok(metadata) => metadata,
        Err(error) => {
            return Err(plugin_quarantine(
                root,
                source,
                None,
                PluginQuarantineReason::InvalidPluginManifest,
                error.to_string(),
            ))
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(plugin_quarantine(
            root,
            source,
            None,
            PluginQuarantineReason::Core(SkillQuarantineReason::FilesystemViolation),
            "plugin.yaml must be a regular non-symlink file",
        ));
    }
    let content = std::fs::read_to_string(&manifest_path).map_err(|error| {
        plugin_quarantine(
            root,
            source,
            None,
            PluginQuarantineReason::InvalidPluginManifest,
            error.to_string(),
        )
    })?;
    serde_yaml::from_str::<PluginManifest>(&content).map_err(|error| {
        plugin_quarantine(
            root,
            source,
            None,
            PluginQuarantineReason::InvalidPluginManifest,
            error.to_string(),
        )
    })
}

fn validate_plugin_manifest(
    plugin_manifest: &PluginManifest,
    package_manifest: &SkillPackageManifestV2,
    root: &Path,
    source: PluginDiscoverySource,
) -> Result<(), PluginQuarantine> {
    let identity = &package_manifest.identity;
    if plugin_manifest.name != identity.name || plugin_manifest.version != identity.version {
        return Err(plugin_quarantine(
            root,
            source,
            Some(identity.clone()),
            PluginQuarantineReason::ManifestIdentityMismatch,
            "plugin.yaml name/version does not match the immutable package identity",
        ));
    }

    let declared_permissions = plugin_permissions(plugin_manifest, root, source, identity)?;
    if declared_permissions != package_manifest.permissions.requested {
        return Err(plugin_quarantine(
            root,
            source,
            Some(identity.clone()),
            PluginQuarantineReason::PermissionMismatch,
            "plugin.yaml permissions do not exactly match the signed package manifest",
        ));
    }
    if !declared_permissions.contains(&SkillPermission::PluginRegistration) {
        return Err(plugin_quarantine(
            root,
            source,
            Some(identity.clone()),
            PluginQuarantineReason::PermissionMismatch,
            "future effect-capable plugins must explicitly declare plugin_registration",
        ));
    }

    let declared_dependencies = plugin_dependencies(plugin_manifest, root, source, identity)?;
    let trusted_dependencies = package_manifest
        .dependencies
        .iter()
        .map(|dependency| (dependency.name.clone(), dependency.version.clone()))
        .collect::<BTreeSet<_>>();
    if declared_dependencies != trusted_dependencies {
        return Err(plugin_quarantine(
            root,
            source,
            Some(identity.clone()),
            PluginQuarantineReason::DependencyDeclarationMismatch,
            "plugin.yaml dependencies do not exactly match the signed package manifest",
        ));
    }
    Ok(())
}

fn plugin_permissions(
    manifest: &PluginManifest,
    root: &Path,
    source: PluginDiscoverySource,
    identity: &SkillPackageIdentityV2,
) -> Result<BTreeSet<SkillPermission>, PluginQuarantine> {
    let mut permissions = BTreeSet::new();
    for permission in &manifest.permissions {
        let Some(permission) = parse_plugin_permission(permission) else {
            return Err(plugin_quarantine(
                root,
                source,
                Some(identity.clone()),
                PluginQuarantineReason::PermissionMismatch,
                "plugin.yaml declares an unknown permission",
            ));
        };
        if !permissions.insert(permission) {
            return Err(plugin_quarantine(
                root,
                source,
                Some(identity.clone()),
                PluginQuarantineReason::PermissionMismatch,
                "plugin.yaml declares a permission more than once",
            ));
        }
    }
    Ok(permissions)
}

fn parse_plugin_permission(value: &str) -> Option<SkillPermission> {
    match value.trim() {
        "prompt" => Some(SkillPermission::Prompt),
        "tool" => Some(SkillPermission::Tool),
        "network" => Some(SkillPermission::Network),
        "filesystem_read" => Some(SkillPermission::FilesystemRead),
        "filesystem_write" => Some(SkillPermission::FilesystemWrite),
        "process" => Some(SkillPermission::Process),
        "plugin_registration" => Some(SkillPermission::PluginRegistration),
        _ => None,
    }
}

fn plugin_dependencies(
    manifest: &PluginManifest,
    root: &Path,
    source: PluginDiscoverySource,
    identity: &SkillPackageIdentityV2,
) -> Result<BTreeSet<(String, String)>, PluginQuarantine> {
    let mut dependencies = BTreeSet::new();
    for dependency in &manifest.dependencies {
        let Some((name, version)) = dependency.exact() else {
            return Err(plugin_quarantine(
                root,
                source,
                Some(identity.clone()),
                PluginQuarantineReason::UnscopedDependency,
                "plugin.yaml dependencies must declare an exact name and version",
            ));
        };
        if name.trim().is_empty() || version.trim().is_empty() {
            return Err(plugin_quarantine(
                root,
                source,
                Some(identity.clone()),
                PluginQuarantineReason::UnscopedDependency,
                "plugin.yaml dependency name and version must be non-empty",
            ));
        }
        if !dependencies.insert((name.to_string(), version.to_string())) {
            return Err(plugin_quarantine(
                root,
                source,
                Some(identity.clone()),
                PluginQuarantineReason::DependencyDeclarationMismatch,
                "plugin.yaml declares a dependency more than once",
            ));
        }
    }
    Ok(dependencies)
}

fn enforce_plugin_dependency_graph(
    bundles: &mut Vec<DiscoveredPlugin>,
    quarantined: &mut Vec<PluginQuarantine>,
) {
    let mut reasons = vec![None::<(PluginQuarantineReason, String)>; bundles.len()];
    let mut edges = vec![Vec::<usize>::new(); bundles.len()];

    for left in 0..bundles.len() {
        for right in 0..left {
            if bundles[left].admission.identity == bundles[right].admission.identity {
                let detail =
                    "discovery contains the same immutable package identity more than once"
                        .to_string();
                reasons[left] = Some((
                    PluginQuarantineReason::DuplicatePackageIdentity,
                    detail.clone(),
                ));
                reasons[right] = Some((PluginQuarantineReason::DuplicatePackageIdentity, detail));
            }
        }
    }

    for (index, bundle) in bundles.iter().enumerate() {
        for dependency in &bundle.package_manifest.dependencies {
            let matches = bundles
                .iter()
                .enumerate()
                .filter_map(|(candidate_index, candidate)| {
                    (candidate.admission.identity.name == dependency.name
                        && candidate.admission.identity.version == dependency.version)
                        .then_some(candidate_index)
                })
                .collect::<Vec<_>>();
            match matches.as_slice() {
                [] => {
                    reasons[index] = Some((
                        PluginQuarantineReason::MissingDependency,
                        format!(
                            "missing exact plugin dependency {}@{}",
                            dependency.name, dependency.version
                        ),
                    ));
                }
                [dependency_index] => edges[index].push(*dependency_index),
                _ => {
                    reasons[index] = Some((
                        PluginQuarantineReason::AmbiguousDependency,
                        format!(
                            "multiple plugin bundles satisfy {}@{}",
                            dependency.name, dependency.version
                        ),
                    ));
                }
            }
        }
    }

    let mut states = vec![0u8; bundles.len()];
    let mut stack = Vec::new();
    let mut cyclic = BTreeSet::new();
    for index in 0..bundles.len() {
        mark_plugin_dependency_cycles(
            index,
            &edges,
            &reasons,
            &mut states,
            &mut stack,
            &mut cyclic,
        );
    }
    for index in cyclic {
        reasons[index] = Some((
            PluginQuarantineReason::DependencyCycle,
            "plugin dependency graph contains a cycle".to_string(),
        ));
    }

    loop {
        let mut changed = false;
        for index in 0..bundles.len() {
            if reasons[index].is_some() {
                continue;
            }
            if let Some(dependency_index) = edges[index]
                .iter()
                .copied()
                .find(|dependency_index| reasons[*dependency_index].is_some())
            {
                reasons[index] = Some((
                    PluginQuarantineReason::MissingDependency,
                    format!(
                        "dependency {}@{} is not admitted",
                        bundles[dependency_index].admission.identity.name,
                        bundles[dependency_index].admission.identity.version
                    ),
                ));
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    let mut admitted = Vec::with_capacity(bundles.len());
    for (index, bundle) in std::mem::take(bundles).into_iter().enumerate() {
        if let Some((reason, detail)) = reasons[index].take() {
            quarantined.push(plugin_quarantine(
                &bundle.path,
                bundle.source,
                Some(bundle.admission.identity),
                reason,
                detail,
            ));
        } else {
            admitted.push(bundle);
        }
    }
    *bundles = admitted;
}

fn mark_plugin_dependency_cycles(
    index: usize,
    edges: &[Vec<usize>],
    reasons: &[Option<(PluginQuarantineReason, String)>],
    states: &mut [u8],
    stack: &mut Vec<usize>,
    cyclic: &mut BTreeSet<usize>,
) {
    if reasons[index].is_some() || states[index] == 2 {
        return;
    }
    if states[index] == 1 {
        if let Some(start) = stack.iter().position(|item| *item == index) {
            cyclic.extend(stack[start..].iter().copied());
        }
        return;
    }
    states[index] = 1;
    stack.push(index);
    for dependency in &edges[index] {
        mark_plugin_dependency_cycles(*dependency, edges, reasons, states, stack, cyclic);
    }
    stack.pop();
    states[index] = 2;
}

fn plugin_quarantine(
    path: &Path,
    source: PluginDiscoverySource,
    identity: Option<SkillPackageIdentityV2>,
    reason: PluginQuarantineReason,
    detail: impl Into<String>,
) -> PluginQuarantine {
    PluginQuarantine {
        path: path.to_path_buf(),
        source,
        identity,
        reason,
        detail: detail.into(),
    }
}

fn quarantine_from_trust_error(
    root: &Path,
    source: PluginDiscoverySource,
    error: SkillTrustError,
) -> PluginQuarantine {
    let reason = match &error {
        SkillTrustError::LegacyLock => SkillQuarantineReason::LegacyLock,
        SkillTrustError::Io(_) => SkillQuarantineReason::FilesystemViolation,
        SkillTrustError::Serialization(_)
        | SkillTrustError::Malformed(_)
        | SkillTrustError::MalformedKeyring(_) => SkillQuarantineReason::MalformedLock,
        SkillTrustError::InvalidTrustRoot(_) => SkillQuarantineReason::InvalidTrustRoot,
        SkillTrustError::KeyringRollback(_) => SkillQuarantineReason::KeyringRollback,
        SkillTrustError::UnprovisionedAuthority(_) => SkillQuarantineReason::UnprovisionedAuthority,
    };
    plugin_quarantine(
        root,
        source,
        None,
        PluginQuarantineReason::Core(reason),
        error.to_string(),
    )
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}
