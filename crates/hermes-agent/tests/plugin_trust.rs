use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use hermes_agent::plugins::{PluginManager, PluginManifest, PluginQuarantineReason};
use hermes_skills::{
    canonical_bundle_digest, full_skill_scan, load_lock, store_lock, store_revocations,
    LocalOwnerSigningKey, PendingOwnerAttestation, SignedSkillRevocationList,
    SkillCompatibility, SkillLockEntryV2, SkillLockV2, SkillPackageDependency,
    SkillPackageIdentityV2, SkillPackageManifestV2, SkillPackageProof, SkillPackageSource,
    SkillPermission, SkillPermissions, SkillRevocation, SkillSignature, SkillSigningKey,
    SkillTrustAuthorityPaths, SkillTrustKeyring, SkillTrustPolicy, SkillTrustVerifier,
    SKILL_LOCK_SCHEMA_VERSION, SKILL_REVOCATION_SCHEMA_VERSION,
};
use tempfile::TempDir;

const KEY_ID: &str = "plugin-fixture-key";
const PUBLISHER: &str = "plugin-fixture-publisher";

struct TestTrust {
    signer: SkillSigningKey,
    local_signer: LocalOwnerSigningKey,
    verifier: SkillTrustVerifier,
}

fn all_permissions() -> BTreeSet<SkillPermission> {
    [
        SkillPermission::Prompt,
        SkillPermission::Tool,
        SkillPermission::Network,
        SkillPermission::FilesystemRead,
        SkillPermission::FilesystemWrite,
        SkillPermission::Process,
        SkillPermission::PluginRegistration,
    ]
    .into_iter()
    .collect()
}

fn permissions(values: &[SkillPermission]) -> BTreeSet<SkillPermission> {
    values.iter().cloned().collect()
}

fn effect_permissions() -> BTreeSet<SkillPermission> {
    permissions(&[
        SkillPermission::PluginRegistration,
        SkillPermission::Prompt,
        SkillPermission::Tool,
    ])
}

fn test_trust() -> TestTrust {
    let signer = SkillSigningKey::from_seed([7; 32]);
    let local_signer = LocalOwnerSigningKey::from_seed([8; 32]);
    let keyring = SkillTrustKeyring::new_with_local_owner_keys(
        vec![signer.trusted_key(KEY_ID, true)],
        vec![local_signer.trusted_key("fixture-owner", "fixture-local-key", true)],
    )
    .unwrap();
    let policy = SkillTrustPolicy::default()
        .with_runtime_version("0.22.0")
        .with_permitted_permissions(all_permissions());
    let verifier = SkillTrustVerifier::new(keyring, policy, BTreeMap::new()).unwrap();
    TestTrust {
        signer,
        local_signer,
        verifier,
    }
}

fn remote_root(home: &TempDir, directory_name: &str) -> PathBuf {
    home.path().join("plugins").join(directory_name)
}

fn write_remote_bundle(
    root: &Path,
    name: &str,
    version: &str,
    plugin_permissions: &[&str],
    package_permissions: BTreeSet<SkillPermission>,
    dependencies: &[(&str, &str)],
    compatibility: SkillCompatibility,
    signed: bool,
    trust: &TestTrust,
) -> SkillPackageManifestV2 {
    std::fs::create_dir_all(root).unwrap();
    let plugin_manifest = PluginManifest {
        name: name.to_string(),
        version: version.to_string(),
        description: "Fixture plugin".to_string(),
        kind: Some("backend".to_string()),
        author: Some("Fixture".to_string()),
        homepage: None,
        permissions: plugin_permissions
            .iter()
            .map(|permission| (*permission).to_string())
            .collect(),
        dependencies: dependencies
            .iter()
            .map(|(dependency_name, dependency_version)| {
                hermes_agent::plugins::PluginDependency::Exact {
                    name: (*dependency_name).to_string(),
                    version: (*dependency_version).to_string(),
                }
            })
            .collect(),
    };
    std::fs::write(
        root.join("plugin.yaml"),
        serde_yaml::to_string(&plugin_manifest).unwrap(),
    )
    .unwrap();
    // If discovery imported this file, the test process would fail. Discovery
    // must only parse metadata and verify bytes.
    std::fs::write(
        root.join("__init__.py"),
        "raise AssertionError('plugin discovery must not execute Python payloads')\n",
    )
    .unwrap();
    std::fs::write(root.join("payload.txt"), "signed fixture payload\n").unwrap();

    let content_digest = canonical_bundle_digest(root).unwrap();
    let identity = SkillPackageIdentityV2::new(
        name,
        version,
        format!("https://example.invalid/plugins/{name}"),
        PUBLISHER,
        "git:0123456789abcdef",
        content_digest,
        "2026-07-20T00:00:00Z",
    );
    let mut package_dependencies = dependencies
        .iter()
        .map(|(dependency_name, dependency_version)| SkillPackageDependency {
            name: (*dependency_name).to_string(),
            version: (*dependency_version).to_string(),
        })
        .collect::<Vec<_>>();
    package_dependencies.sort();
    let mut package_manifest = SkillPackageManifestV2::new(
        SkillPackageSource::Remote,
        identity,
        SkillPackageProof::RemoteSignature(SkillSignature::ed25519(KEY_ID, "")),
        full_skill_scan(root),
        compatibility,
        SkillPermissions {
            requested: package_permissions,
        },
        package_dependencies,
    );
    if signed {
        trust
            .signer
            .sign_manifest(&mut package_manifest, KEY_ID)
            .unwrap();
    } else {
        package_manifest.refresh_manifest_digest().unwrap();
    }
    store_lock(
        root,
        &SkillLockV2 {
            schema_version: SKILL_LOCK_SCHEMA_VERSION,
            packages: vec![SkillLockEntryV2 {
                install_path: ".".to_string(),
                manifest: package_manifest.clone(),
                quarantine: None,
            }],
        },
    )
    .unwrap();
    package_manifest
}

fn admitted_bundle(
    home: &TempDir,
    trust: &TestTrust,
    name: &str,
) -> hermes_agent::plugins::DiscoveredPlugin {
    let root = remote_root(home, name);
    write_remote_bundle(
        &root,
        name,
        "1.0.0",
        &["plugin_registration", "prompt", "tool"],
        effect_permissions(),
        &[],
        SkillCompatibility::default(),
        true,
        trust,
    );
    let mut report = PluginManager::discover_plugins(home.path(), &trust.verifier);
    assert!(report.quarantined.is_empty(), "{:#?}", report.quarantined);
    report.admitted.remove(0)
}

fn assert_effects_unavailable(manager: &mut PluginManager) {
    assert!(manager
        .invoke_hook(
            hermes_agent::plugins::HookType::PreLlmCall,
            &serde_json::json!({})
        )
        .is_empty());
    let middleware = manager.apply_tool_request_middleware(
        "fixture_tool",
        serde_json::json!({"value": "original"}),
        1,
    );
    assert!(!middleware.changed);
    assert_eq!(middleware.args, middleware.original_args);
    assert!(middleware.trace.is_empty());
}

fn assert_quarantined_without_effects(
    report: &hermes_agent::plugins::PluginDiscoveryReport,
    reason: PluginQuarantineReason,
) {
    assert!(report.admitted.is_empty(), "{:#?}", report.admitted);
    assert!(
        report
            .quarantined
            .iter()
            .any(|quarantine| quarantine.reason == reason),
        "{:#?}",
        report.quarantined
    );
    assert_effects_unavailable(&mut PluginManager::new());
}

#[test]
fn signed_remote_bundle_is_verified_but_remains_diagnostic_only() {
    let home = tempfile::tempdir().unwrap();
    let trust = test_trust();
    let bundle = admitted_bundle(&home, &trust, "signed-plugin");

    assert_eq!(bundle.manifest.name, "signed-plugin");
    assert_eq!(bundle.manifest.version, "1.0.0");
    assert_eq!(bundle.identity().publisher_id, PUBLISHER);
    // `write_remote_bundle` contains an import-failing Python payload. Success
    // here establishes that discovery only verifies metadata and file bytes.
    assert!(bundle.path.join("__init__.py").is_file());
    assert_effects_unavailable(&mut PluginManager::new());
}

#[test]
fn unsigned_bundle_is_quarantined_without_effects() {
    let home = tempfile::tempdir().unwrap();
    let trust = test_trust();
    write_remote_bundle(
        &remote_root(&home, "unsigned-plugin"),
        "unsigned-plugin",
        "1.0.0",
        &["plugin_registration", "prompt", "tool"],
        effect_permissions(),
        &[],
        SkillCompatibility::default(),
        false,
        &trust,
    );

    let report = PluginManager::discover_plugins(home.path(), &trust.verifier);
    assert_quarantined_without_effects(
        &report,
        PluginQuarantineReason::Core(hermes_skills::SkillQuarantineReason::InvalidSignature),
    );
}

#[test]
fn tampered_bundle_is_quarantined_without_effects() {
    let home = tempfile::tempdir().unwrap();
    let trust = test_trust();
    admitted_bundle(&home, &trust, "tampered-plugin");
    std::fs::write(
        remote_root(&home, "tampered-plugin").join("payload.txt"),
        "tampered payload\n",
    )
    .unwrap();

    let report = PluginManager::discover_plugins(home.path(), &trust.verifier);
    assert_quarantined_without_effects(
        &report,
        PluginQuarantineReason::Core(hermes_skills::SkillQuarantineReason::ContentDigestMismatch),
    );
}

#[test]
fn revocation_is_consulted_on_fresh_discovery_without_effects() {
    let home = tempfile::tempdir().unwrap();
    let trust = test_trust();
    admitted_bundle(&home, &trust, "revoked-plugin");
    let root = remote_root(&home, "revoked-plugin");
    let mut revocations = SignedSkillRevocationList {
        schema_version: SKILL_REVOCATION_SCHEMA_VERSION,
        issued_at: "2026-07-20T00:00:00Z".to_string(),
        signature: None,
        revoked: vec![SkillRevocation {
            name: None,
            publisher_id: Some(PUBLISHER.to_string()),
            immutable_source_revision: None,
            content_digest: None,
            key_id: None,
            reason: "fixture revocation".to_string(),
            revoked_at: "2026-07-20T00:00:00Z".to_string(),
        }],
    };
    trust.signer.sign_revocations(&mut revocations, KEY_ID).unwrap();
    store_revocations(&root, &revocations).unwrap();

    let report = PluginManager::discover_plugins(home.path(), &trust.verifier);
    assert_quarantined_without_effects(
        &report,
        PluginQuarantineReason::Core(hermes_skills::SkillQuarantineReason::RevokedPackage),
    );
}

#[test]
fn incompatible_bundle_is_quarantined_without_effects() {
    let home = tempfile::tempdir().unwrap();
    let trust = test_trust();
    write_remote_bundle(
        &remote_root(&home, "incompatible-plugin"),
        "incompatible-plugin",
        "1.0.0",
        &["plugin_registration", "prompt", "tool"],
        effect_permissions(),
        &[],
        SkillCompatibility {
            minimum_runtime_version: Some("99.0.0".to_string()),
            ..SkillCompatibility::default()
        },
        true,
        &trust,
    );

    let report = PluginManager::discover_plugins(home.path(), &trust.verifier);
    assert_quarantined_without_effects(
        &report,
        PluginQuarantineReason::Core(hermes_skills::SkillQuarantineReason::Incompatible),
    );
}

#[test]
fn overpermissioned_plugin_yaml_is_quarantined_without_effects() {
    let home = tempfile::tempdir().unwrap();
    let trust = test_trust();
    write_remote_bundle(
        &remote_root(&home, "overpermissioned-plugin"),
        "overpermissioned-plugin",
        "1.0.0",
        &["plugin_registration", "prompt", "tool", "process"],
        effect_permissions(),
        &[],
        SkillCompatibility::default(),
        true,
        &trust,
    );

    let report = PluginManager::discover_plugins(home.path(), &trust.verifier);
    assert_quarantined_without_effects(&report, PluginQuarantineReason::PermissionMismatch);
}

#[test]
fn plugin_yaml_cannot_escalate_beyond_signed_permission_manifest() {
    let home = tempfile::tempdir().unwrap();
    let trust = test_trust();
    write_remote_bundle(
        &remote_root(&home, "limited-plugin"),
        "limited-plugin",
        "1.0.0",
        &["plugin_registration", "prompt", "tool"],
        permissions(&[SkillPermission::PluginRegistration, SkillPermission::Prompt]),
        &[],
        SkillCompatibility::default(),
        true,
        &trust,
    );

    let report = PluginManager::discover_plugins(home.path(), &trust.verifier);
    assert_quarantined_without_effects(&report, PluginQuarantineReason::PermissionMismatch);
}

#[test]
fn dependency_cycles_and_duplicate_identities_are_quarantined() {
    let home = tempfile::tempdir().unwrap();
    let trust = test_trust();
    write_remote_bundle(
        &remote_root(&home, "cycle-a"),
        "cycle-a",
        "1.0.0",
        &["plugin_registration", "prompt", "tool"],
        effect_permissions(),
        &[("cycle-b", "1.0.0")],
        SkillCompatibility::default(),
        true,
        &trust,
    );
    write_remote_bundle(
        &remote_root(&home, "cycle-b"),
        "cycle-b",
        "1.0.0",
        &["plugin_registration", "prompt", "tool"],
        effect_permissions(),
        &[("cycle-a", "1.0.0")],
        SkillCompatibility::default(),
        true,
        &trust,
    );

    let report = PluginManager::discover_plugins(home.path(), &trust.verifier);
    assert!(report.admitted.is_empty());
    assert_eq!(
        report
            .quarantined
            .iter()
            .filter(|quarantine| quarantine.reason == PluginQuarantineReason::DependencyCycle)
            .count(),
        2
    );

    let duplicate_home = tempfile::tempdir().unwrap();
    write_remote_bundle(
        &remote_root(&duplicate_home, "duplicate-a"),
        "same-plugin",
        "1.0.0",
        &["plugin_registration", "prompt", "tool"],
        effect_permissions(),
        &[],
        SkillCompatibility::default(),
        true,
        &trust,
    );
    write_remote_bundle(
        &remote_root(&duplicate_home, "duplicate-b"),
        "same-plugin",
        "1.0.0",
        &["plugin_registration", "prompt", "tool"],
        effect_permissions(),
        &[],
        SkillCompatibility::default(),
        true,
        &trust,
    );
    let duplicates = PluginManager::discover_plugins(duplicate_home.path(), &trust.verifier);
    assert!(duplicates.admitted.is_empty());
    assert_eq!(
        duplicates
            .quarantined
            .iter()
            .filter(|quarantine| {
                quarantine.reason == PluginQuarantineReason::DuplicatePackageIdentity
            })
            .count(),
        2
    );
}

#[test]
fn missing_exact_dependency_is_quarantined_without_registering_effects() {
    let home = tempfile::tempdir().unwrap();
    let trust = test_trust();
    write_remote_bundle(
        &remote_root(&home, "dependent-plugin"),
        "dependent-plugin",
        "1.0.0",
        &["plugin_registration", "prompt", "tool"],
        effect_permissions(),
        &[("absent-plugin", "9.9.9")],
        SkillCompatibility::default(),
        true,
        &trust,
    );

    let report = PluginManager::discover_plugins(home.path(), &trust.verifier);
    assert_quarantined_without_effects(&report, PluginQuarantineReason::MissingDependency);
}

#[test]
fn local_authored_and_missing_lock_plugins_are_diagnostic_only() {
    let home = tempfile::tempdir().unwrap();
    let trust = test_trust();
    let local_root = remote_root(&home, "local-plugin");
    write_remote_bundle(
        &local_root,
        "local-plugin",
        "1.0.0",
        &["plugin_registration", "prompt", "tool"],
        effect_permissions(),
        &[],
        SkillCompatibility::default(),
        true,
        &trust,
    );
    let mut lock = load_lock(&local_root).unwrap();
    let package = &mut lock.packages[0].manifest;
    package.source = SkillPackageSource::LocalAuthored;
    package.identity.origin = "local://fixture-owner".to_string();
    package.identity.publisher_id = "local:fixture-owner".to_string();
    package.proof = SkillPackageProof::PendingOwnerAttestation(PendingOwnerAttestation {
        requested_at: "2026-07-20T00:00:00Z".to_string(),
    });
    trust
        .local_signer
        .sign_local_manifest(
            package,
            "fixture-owner",
            "fixture-local-key",
            "attestation-1",
            "2026-07-20T00:00:00Z",
        )
        .unwrap();
    store_lock(&local_root, &lock).unwrap();

    let missing_root = remote_root(&home, "missing-lock-plugin");
    std::fs::create_dir_all(&missing_root).unwrap();
    std::fs::write(
        missing_root.join("plugin.yaml"),
        "name: missing-lock-plugin\nversion: \"1.0.0\"\ndescription: Missing lock\npermissions:\n  - plugin_registration\n",
    )
    .unwrap();

    let report = PluginManager::discover_plugins(home.path(), &trust.verifier);
    assert!(report.admitted.is_empty());
    assert!(report
        .quarantined
        .iter()
        .any(|quarantine| quarantine.reason == PluginQuarantineReason::EffectsNotAllowed));
    assert!(report.quarantined.iter().any(|quarantine| {
        quarantine.reason
            == PluginQuarantineReason::Core(hermes_skills::SkillQuarantineReason::MissingLock)
    }));
}

#[test]
fn unprovisioned_external_authority_quarantines_plugin_discovery() {
    let temporary = tempfile::tempdir().unwrap();
    let hermes_dir = temporary.path().join("hermes");
    let authority_dir = temporary.path().join("authority");
    std::fs::create_dir_all(&hermes_dir).unwrap();
    std::fs::create_dir_all(&authority_dir).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        std::fs::set_permissions(&authority_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let authority = SkillTrustAuthorityPaths::new(
        hermes_dir.clone(),
        authority_dir.join("remote-root.json"),
        authority_dir.join("remote-audit.json"),
        authority_dir.join("local-owner-keys.json"),
    )
    .unwrap();

    let report = PluginManager::discover_plugins_with_authority_paths(
        &hermes_dir,
        None,
        false,
        &authority,
        SkillTrustPolicy::default(),
        BTreeMap::new(),
    );

    assert!(report.admitted.is_empty());
    assert_eq!(report.quarantined.len(), 1);
    assert_eq!(
        report.quarantined[0].reason,
        PluginQuarantineReason::Core(
            hermes_skills::SkillQuarantineReason::UnprovisionedAuthority
        )
    );
}

#[cfg(unix)]
#[test]
fn symlink_payload_is_quarantined_before_effect_registration() {
    use std::os::unix::fs::symlink;

    let home = tempfile::tempdir().unwrap();
    let trust = test_trust();
    let root = remote_root(&home, "symlink-plugin");
    write_remote_bundle(
        &root,
        "symlink-plugin",
        "1.0.0",
        &["plugin_registration", "prompt", "tool"],
        effect_permissions(),
        &[],
        SkillCompatibility::default(),
        true,
        &trust,
    );
    let outside = tempfile::NamedTempFile::new().unwrap();
    symlink(outside.path(), root.join("escape.txt")).unwrap();

    let report = PluginManager::discover_plugins(home.path(), &trust.verifier);
    assert!(report.admitted.is_empty());
    assert!(report.quarantined.iter().any(|quarantine| {
        quarantine.reason
            == PluginQuarantineReason::Core(hermes_skills::SkillQuarantineReason::FilesystemViolation)
    }));
}

#[test]
fn legacy_dependency_syntax_is_retained_only_for_diagnostic_quarantine() {
    let manifest: PluginManifest = serde_yaml::from_str(
        "name: legacy\nversion: \"1.0.0\"\ndescription: legacy\ndependencies:\n  - dep-without-version\n",
    )
    .unwrap();
    assert!(matches!(
        manifest.dependencies[0],
        hermes_agent::plugins::PluginDependency::Legacy(_)
    ));
}

#[test]
fn unscoped_dependency_bundle_is_quarantined_before_effect_registration() {
    let home = tempfile::tempdir().unwrap();
    let trust = test_trust();
    let root = remote_root(&home, "legacy-dependency-plugin");
    write_remote_bundle(
        &root,
        "legacy-dependency-plugin",
        "1.0.0",
        &["plugin_registration", "prompt", "tool"],
        effect_permissions(),
        &[],
        SkillCompatibility::default(),
        true,
        &trust,
    );
    let mut plugin_manifest: PluginManifest = serde_yaml::from_str(
        "name: legacy-dependency-plugin\nversion: \"1.0.0\"\ndescription: fixture\npermissions:\n  - plugin_registration\n  - prompt\n  - tool\ndependencies:\n  - missing-version\n",
    )
    .unwrap();
    plugin_manifest.description = "Fixture plugin".to_string();
    std::fs::write(
        root.join("plugin.yaml"),
        serde_yaml::to_string(&plugin_manifest).unwrap(),
    )
    .unwrap();
    let mut lock = load_lock(&root).unwrap();
    lock.packages[0].manifest.identity.content_digest = canonical_bundle_digest(&root).unwrap();
    trust
        .signer
        .sign_manifest(&mut lock.packages[0].manifest, KEY_ID)
        .unwrap();
    store_lock(&root, &lock).unwrap();

    let report = PluginManager::discover_plugins(home.path(), &trust.verifier);
    assert!(report.admitted.is_empty());
    assert!(report
        .quarantined
        .iter()
        .any(|quarantine| quarantine.reason == PluginQuarantineReason::UnscopedDependency));
}
