#[derive(Debug, Clone)]
struct SkillInstallMaterial {
    install_name: String,
    manifest: hermes_skills::SkillPackageManifestV2,
    files: Vec<(String, Bytes)>,
}

#[derive(Debug)]
struct SkillUninstallResult {
    removed: Option<hermes_skills::SkillLockEntryV2>,
    cleanup_error: Option<String>,
}

fn trust_error(context: &str, error: impl std::fmt::Display) -> AgentError {
    AgentError::Config(format!("{context}: {error}"))
}

fn ensure_private_skill_root(path: &Path) -> Result<(), AgentError> {
    std::fs::create_dir_all(path).map_err(|error| {
        AgentError::Io(format!(
            "Failed to create skill root '{}': {error}",
            path.display()
        ))
    })?;
    let metadata = std::fs::symlink_metadata(path).map_err(|error| {
        AgentError::Io(format!(
            "Failed to inspect skill root '{}': {error}",
            path.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AgentError::Config(format!(
            "Skill root '{}' must be a regular non-symlink directory",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700)).map_err(
            |error| {
                AgentError::Io(format!(
                    "Failed to restrict skill root '{}': {error}",
                    path.display()
                ))
            },
        )?;
    }
    Ok(())
}
const REMOTE_TRUST_ROOT_FILE_NAME: &str = "remote-trust-root.json";
const REMOTE_TRUST_AUDIT_FILE_NAME: &str = "remote-keyring-audit.json";
const LOCAL_OWNER_KEYRING_FILE_NAME: &str = "local-owner-keyring.json";

#[derive(Debug, Clone)]
struct CliSkillTrustAuthorityFiles {
    directory: PathBuf,
    remote_root_file: PathBuf,
    remote_audit_file: PathBuf,
    local_owner_keyring_file: PathBuf,
}

fn cli_skill_trust_authority_files(
    skills_root: &Path,
    authority_dir: Option<&Path>,
) -> Result<CliSkillTrustAuthorityFiles, AgentError> {
    let authority_dir = authority_dir.ok_or_else(|| {
        AgentError::Config(
            "Verified skill operations require --authority-dir <absolute-owner-only-directory>"
                .to_string(),
        )
    })?;
    if !authority_dir.is_absolute() {
        return Err(AgentError::Config(
            "Skill authority directory must be an absolute path".to_string(),
        ));
    }
    let metadata = std::fs::symlink_metadata(authority_dir).map_err(|error| {
        AgentError::Config(format!(
            "Skill authority directory '{}' is unavailable: {error}",
            authority_dir.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AgentError::Config(format!(
            "Skill authority directory '{}' must be a regular non-symlink directory",
            authority_dir.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(AgentError::Config(format!(
                "Skill authority directory '{}' must be owner-only",
                authority_dir.display()
            )));
        }
    }
    let directory = std::fs::canonicalize(authority_dir).map_err(|error| {
        AgentError::Config(format!(
            "Failed to canonicalize skill authority directory '{}': {error}",
            authority_dir.display()
        ))
    })?;
    let canonical_skills_root = std::fs::canonicalize(skills_root).map_err(|error| {
        AgentError::Config(format!(
            "Skills root '{}' must exist before loading authority paths: {error}",
            skills_root.display()
        ))
    })?;
    if directory.starts_with(&canonical_skills_root) {
        return Err(AgentError::Config(
            "Skill authority directory must live outside the mutable skills root".to_string(),
        ));
    }
    Ok(CliSkillTrustAuthorityFiles {
        remote_root_file: directory.join(REMOTE_TRUST_ROOT_FILE_NAME),
        remote_audit_file: directory.join(REMOTE_TRUST_AUDIT_FILE_NAME),
        local_owner_keyring_file: directory.join(LOCAL_OWNER_KEYRING_FILE_NAME),
        directory,
    })
}

fn ensure_cli_skill_authority_directory(
    skills_root: &Path,
    authority_dir: Option<&Path>,
) -> Result<CliSkillTrustAuthorityFiles, AgentError> {
    ensure_private_skill_root(skills_root)?;
    let authority_dir = authority_dir.ok_or_else(|| {
        AgentError::Config(
            "Trust provisioning requires --authority-dir <absolute-owner-only-directory>"
                .to_string(),
        )
    })?;
    if !authority_dir.is_absolute() {
        return Err(AgentError::Config(
            "Skill authority directory must be an absolute path".to_string(),
        ));
    }
    match std::fs::symlink_metadata(authority_dir) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(AgentError::Config(format!(
                    "Skill authority directory '{}' must be a regular non-symlink directory",
                    authority_dir.display()
                )));
            }
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let parent = authority_dir.parent().ok_or_else(|| {
                AgentError::Config("Skill authority directory has no parent".to_string())
            })?;
            let parent_metadata = std::fs::symlink_metadata(parent).map_err(|error| {
                AgentError::Config(format!(
                    "Skill authority parent '{}' is unavailable: {error}",
                    parent.display()
                ))
            })?;
            if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
                return Err(AgentError::Config(format!(
                    "Skill authority parent '{}' must be a regular directory",
                    parent.display()
                )));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                let mut builder = std::fs::DirBuilder::new();
                builder.mode(0o700);
                builder.create(authority_dir).map_err(|error| {
                    AgentError::Io(format!(
                        "Failed to create owner-only skill authority directory '{}': {error}",
                        authority_dir.display()
                    ))
                })?;
            }
            #[cfg(not(unix))]
            {
                std::fs::create_dir(authority_dir).map_err(|error| {
                    AgentError::Io(format!(
                        "Failed to create skill authority directory '{}': {error}",
                        authority_dir.display()
                    ))
                })?;
            }
        }
        Err(error) => {
            return Err(AgentError::Io(format!(
                "Failed to inspect skill authority directory '{}': {error}",
                authority_dir.display()
            )));
        }
    }
    cli_skill_trust_authority_files(skills_root, Some(authority_dir))
}

fn initialize_empty_local_owner_keyring(
    skills_root: &Path,
    authority: &CliSkillTrustAuthorityFiles,
) -> Result<(), AgentError> {
    match std::fs::symlink_metadata(&authority.local_owner_keyring_file) {
        Ok(_) => {
            let existing = hermes_skills::load_local_owner_keyring_file(
                skills_root,
                &authority.local_owner_keyring_file,
            )
            .map_err(|error| trust_error("Invalid existing local owner keyring", error))?;
            if !existing.keys.is_empty() {
                return Err(AgentError::Config(
                    "Refusing to overwrite a non-empty local owner keyring".to_string(),
                ));
            }
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let empty = hermes_skills::LocalOwnerKeyring::new(Vec::new())
                .map_err(|error| trust_error("Failed to initialize local owner keyring", error))?;
            hermes_skills::store_local_owner_keyring_file(
                skills_root,
                &authority.local_owner_keyring_file,
                &empty,
            )
            .map_err(|error| trust_error("Failed to store local owner keyring", error))
        }
        Err(error) => Err(AgentError::Io(format!(
            "Failed to inspect local owner keyring '{}': {error}",
            authority.local_owner_keyring_file.display()
        ))),
    }
}

fn ensure_local_owner_keyring_exists(
    skills_root: &Path,
    authority: &CliSkillTrustAuthorityFiles,
) -> Result<(), AgentError> {
    match std::fs::symlink_metadata(&authority.local_owner_keyring_file) {
        Ok(_) => hermes_skills::load_local_owner_keyring_file(
            skills_root,
            &authority.local_owner_keyring_file,
        )
        .map(|_| ())
        .map_err(|error| trust_error("Invalid existing local owner keyring", error)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let empty = hermes_skills::LocalOwnerKeyring::new(Vec::new())
                .map_err(|error| trust_error("Failed to initialize local owner keyring", error))?;
            hermes_skills::store_local_owner_keyring_file(
                skills_root,
                &authority.local_owner_keyring_file,
                &empty,
            )
            .map_err(|error| trust_error("Failed to store local owner keyring", error))
        }
        Err(error) => Err(AgentError::Io(format!(
            "Failed to inspect local owner keyring '{}': {error}",
            authority.local_owner_keyring_file.display()
        ))),
    }
}

fn load_cli_skill_trust_authority_paths(
    skills_root: &Path,
    authority_dir: Option<&Path>,
) -> Result<hermes_skills::SkillTrustAuthorityPaths, AgentError> {
    let authority = cli_skill_trust_authority_files(skills_root, authority_dir)?;
    hermes_skills::SkillTrustAuthorityPaths::new(
        skills_root.to_path_buf(),
        authority.remote_root_file,
        authority.remote_audit_file,
        authority.local_owner_keyring_file,
    )
    .map_err(|error| trust_error("Invalid external skill trust authority", error))
}

fn load_cli_skill_trust_verifier(
    skills_root: &Path,
    authority_dir: Option<&Path>,
    requires_remote_trust: bool,
) -> Result<hermes_skills::SkillTrustVerifier, AgentError> {
    let authority = cli_skill_trust_authority_files(skills_root, authority_dir)?;
    let local_owner_keys =
        hermes_skills::load_local_owner_keyring_file(skills_root, &authority.local_owner_keyring_file)
            .map_err(|error| trust_error("Invalid protected local owner keyring", error))?
            .keys;
    if requires_remote_trust {
        hermes_skills::SkillTrustAuthorityPaths::new(
            skills_root.to_path_buf(),
            authority.remote_root_file,
            authority.remote_audit_file,
            authority.local_owner_keyring_file,
        )
        .map_err(|error| trust_error("Invalid external skill trust authority", error))?
        .build_verifier(
            hermes_skills::SkillTrustPolicy::default(),
            std::collections::BTreeMap::new(),
        )
        .map_err(|error| trust_error("Invalid anchored remote skill trust configuration", error))
    } else {
        hermes_skills::SkillTrustVerifier::for_skill_root_with_local_owner_keys(
            skills_root,
            hermes_skills::SkillTrustPolicy::default(),
            std::collections::BTreeMap::new(),
            local_owner_keys,
        )
        .map_err(|error| trust_error("Invalid local skill trust configuration", error))
    }
}


fn ensure_safe_install_name(install_name: &str) -> Result<(), AgentError> {
    let normalized = hermes_skills::normalize_relative_package_path(Path::new(install_name))
        .map_err(|error| trust_error("Invalid skill install path", error))?;
    if normalized != install_name || install_name.contains('/') {
        return Err(AgentError::Config(format!(
            "Skill install path must be one safe directory name: {install_name}"
        )));
    }
    Ok(())
}

fn load_v2_skill_lock(skills_root: &Path) -> Result<hermes_skills::SkillLockV2, AgentError> {
    hermes_skills::load_lock(skills_root)
        .map_err(|error| trust_error("Skill lock is not a valid v2 lock", error))
}

fn bootstrap_requested(extra: Option<&str>) -> bool {
    skills_extra_has_flag(extra, "--bootstrap")
}

fn refuse_unsafe_bootstrap(requested: bool) -> Result<(), AgentError> {
    if requested {
        return Err(AgentError::Config(
            "Bootstrap was refused: no policy/sandbox handoff is available to the CLI; no command was executed"
                .to_string(),
        ));
    }
    Ok(())
}

fn install_verified_skill_material(
    skills_root: &Path,
    material: SkillInstallMaterial,
    authority_paths: &hermes_skills::SkillTrustAuthorityPaths,
    bootstrap: bool,
) -> Result<hermes_skills::SkillAdmission, AgentError> {
    if material.install_name != material.manifest.identity.name {
        return Err(AgentError::Config(format!(
            "Resolved skill name '{}' does not match signed package identity '{}'",
            material.install_name, material.manifest.identity.name
        )));
    }
    refuse_unsafe_bootstrap(bootstrap)?;
    let files = material
        .files
        .into_iter()
        .map(|(relative_path, bytes)| {
            hermes_skills::LocalPackageFile::new(relative_path, bytes.to_vec())
        })
        .collect::<Vec<_>>();
    hermes_skills::FileSkillStore::new(skills_root.to_path_buf())
        .with_authority_paths(authority_paths.clone())
        .install_signed_remote_package(material.manifest, &files, false)
        .map_err(|error| trust_error("Verified remote skill install was rejected", error))
}

fn uninstall_verified_skill(
    skills_root: &Path,
    install_name: &str,
) -> Result<SkillUninstallResult, AgentError> {
    ensure_safe_install_name(install_name)?;
    let mut lock = load_v2_skill_lock(skills_root)?;
    let mut removed = None;
    lock.packages.retain(|entry| {
        if entry.manifest.identity.name == install_name || entry.install_path == install_name {
            removed = Some(entry.clone());
            false
        } else {
            true
        }
    });
    if removed.is_none() {
        return Ok(SkillUninstallResult {
            removed: None,
            cleanup_error: None,
        });
    }
    hermes_skills::store_lock(skills_root, &lock)
        .map_err(|error| trust_error("Failed to remove skill from v2 lock", error))?;

    let target = hermes_skills::normalize_relative_package_path(Path::new(&removed
        .as_ref()
        .expect("removed entry checked above")
        .install_path))
    .map(|relative| skills_root.join(relative))
    .map_err(|error| trust_error("Invalid v2 skill install path", error))?;
    let cleanup_error = match std::fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_symlink() => std::fs::remove_file(&target)
            .err()
            .map(|error| error.to_string()),
        Ok(_) => std::fs::remove_dir_all(&target)
            .err()
            .map(|error| error.to_string()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => Some(error.to_string()),
    };
    Ok(SkillUninstallResult {
        removed,
        cleanup_error,
    })
}

fn skill_package_trust_label(manifest: &hermes_skills::SkillPackageManifestV2) -> String {
    match &manifest.proof {
        hermes_skills::SkillPackageProof::RemoteSignature(signature) => {
            format!("remote-signed:key={}", signature.key_id)
        }
        hermes_skills::SkillPackageProof::EmbeddedManifest { .. } => {
            "embedded-manifest".to_string()
        }
        hermes_skills::SkillPackageProof::LocalOwnerAttestation(attestation) => {
            format!(
                "local-attested:owner={}:key={}",
                attestation.owner_id, attestation.key_id
            )
        }
        hermes_skills::SkillPackageProof::PendingOwnerAttestation(_) => {
            "local-attestation-pending".to_string()
        }
    }
}

fn skill_extra_value(extra: Option<&str>, flag: &str) -> Result<Option<String>, AgentError> {
    let Some(extra) = extra else {
        return Ok(None);
    };
    let tokens = shlex::split(extra).ok_or_else(|| {
        AgentError::Config("Unable to parse skill command --extra arguments".to_string())
    })?;
    let mut index = 0usize;
    while index < tokens.len() {
        let token = &tokens[index];
        if let Some(value) = token.strip_prefix(&format!("{flag}=")) {
            if value.trim().is_empty() {
                return Err(AgentError::Config(format!("{flag} requires a value")));
            }
            return Ok(Some(value.to_string()));
        }
        if token == flag {
            let value = tokens.get(index + 1).ok_or_else(|| {
                AgentError::Config(format!("{flag} requires a value"))
            })?;
            if value.trim().is_empty() || value.starts_with("--") {
                return Err(AgentError::Config(format!("{flag} requires a value")));
            }
            return Ok(Some(value.to_string()));
        }
        index += 1;
    }
    Ok(None)
}

fn load_remote_skill_manifest(
    extra: Option<&str>,
) -> Result<hermes_skills::SkillPackageManifestV2, AgentError> {
    let manifest_path = skill_extra_value(extra, "--manifest")?.ok_or_else(|| {
        AgentError::Config(
            "Remote installs require --extra '--manifest <signed-manifest.json>'".to_string(),
        )
    })?;
    let path = PathBuf::from(manifest_path);
    let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
        AgentError::Io(format!(
            "Failed to inspect signed remote manifest '{}': {error}",
            path.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(AgentError::Config(format!(
            "Signed remote manifest '{}' must be a regular file",
            path.display()
        )));
    }
    let bytes = std::fs::read(&path).map_err(|error| {
        AgentError::Io(format!(
            "Failed to read signed remote manifest '{}': {error}",
            path.display()
        ))
    })?;
    let manifest = hermes_skills::parse_package_manifest(&bytes).map_err(|error| {
        AgentError::Config(format!(
            "Invalid signed remote manifest '{}': {error}",
            path.display()
        ))
    })?;
    if manifest.source != hermes_skills::SkillPackageSource::Remote
        || !matches!(
            &manifest.proof,
            hermes_skills::SkillPackageProof::RemoteSignature(_)
        )
    {
        return Err(AgentError::Config(
            "Remote installs require a manifest with a detached remote signature".to_string(),
        ));
    }
    Ok(manifest)
}

fn validate_remote_manifest_source(
    manifest: &hermes_skills::SkillPackageManifestV2,
    origin: &str,
    immutable_revision: &str,
) -> Result<(), AgentError> {
    if manifest.source != hermes_skills::SkillPackageSource::Remote {
        return Err(AgentError::Config(
            "A remote source cannot be relabeled as local-authored or embedded".to_string(),
        ));
    }
    if manifest.identity.origin != origin {
        return Err(AgentError::Config(format!(
            "Signed manifest origin mismatch: expected '{origin}', got '{}'",
            manifest.identity.origin
        )));
    }
    if manifest.identity.immutable_source_revision != immutable_revision {
        return Err(AgentError::Config(format!(
            "Signed manifest immutable revision mismatch: expected '{immutable_revision}', got '{}'",
            manifest.identity.immutable_source_revision
        )));
    }
    Ok(())
}


fn load_cli_skill_revocations(
    skills_root: &Path,
) -> Result<hermes_skills::SignedSkillRevocationList, AgentError> {
    hermes_skills::load_revocations(skills_root)
        .map_err(|error| trust_error("Invalid signed skill revocation list", error))
}

fn parse_local_skill_source(spec: &str) -> Result<Option<(PathBuf, String)>, AgentError> {
    let Some(path_and_query) = spec.strip_prefix("file://") else {
        return Ok(None);
    };
    let (path, query) = path_and_query.split_once('?').ok_or_else(|| {
        AgentError::Config(
            "Local skill sources require ?revision=sha256:<content-digest>".to_string(),
        )
    })?;
    let revision = query
        .strip_prefix("revision=")
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            AgentError::Config(
                "Local skill sources require exactly ?revision=sha256:<content-digest>"
                    .to_string(),
            )
        })?
        .to_string();
    if !revision.starts_with("sha256:")
        || revision.len() != "sha256:".len() + 64
        || !revision["sha256:".len()..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(AgentError::Config(
            "Local skill source revision must be a full sha256 content digest".to_string(),
        ));
    }
    let path = PathBuf::from(path);
    if !path.is_absolute() {
        return Err(AgentError::Config(
            "Local skill source must use an absolute file:// path".to_string(),
        ));
    }
    Ok(Some((path, revision.to_ascii_lowercase())))
}

fn collect_local_skill_files(
    root: &Path,
) -> Result<Vec<(String, Bytes)>, AgentError> {
    let metadata = std::fs::symlink_metadata(root).map_err(|error| {
        AgentError::Io(format!(
            "Failed to inspect local skill source '{}': {error}",
            root.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(AgentError::Config(format!(
            "Local skill source '{}' must be a regular directory",
            root.display()
        )));
    }

    fn collect(
        root: &Path,
        directory: &Path,
        files: &mut Vec<(String, Bytes)>,
    ) -> Result<(), AgentError> {
        for entry in std::fs::read_dir(directory).map_err(|error| {
            AgentError::Io(format!(
                "Failed to read local skill directory '{}': {error}",
                directory.display()
            ))
        })? {
            let entry = entry.map_err(|error| {
                AgentError::Io(format!(
                    "Failed to inspect local skill directory entry '{}': {error}",
                    directory.display()
                ))
            })?;
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
                AgentError::Io(format!(
                    "Failed to inspect local skill path '{}': {error}",
                    path.display()
                ))
            })?;
            if metadata.file_type().is_symlink() {
                return Err(AgentError::Config(format!(
                    "Local skill source contains a symlink: {}",
                    path.display()
                )));
            }
            if metadata.is_dir() {
                collect(root, &path, files)?;
                continue;
            }
            if !metadata.is_file() {
                return Err(AgentError::Config(format!(
                    "Local skill source contains a non-regular file: {}",
                    path.display()
                )));
            }
            let relative = path.strip_prefix(root).map_err(|error| {
                AgentError::Config(format!(
                    "Failed to calculate local skill relative path: {error}"
                ))
            })?;
            let normalized = hermes_skills::normalize_relative_package_path(relative)
                .map_err(|error| trust_error("Invalid local skill package path", error))?;
            let bytes = std::fs::read(&path).map_err(|error| {
                AgentError::Io(format!(
                    "Failed to read local skill file '{}': {error}",
                    path.display()
                ))
            })?;
            files.push((normalized, Bytes::from(bytes)));
        }
        Ok(())
    }

    let mut files = Vec::new();
    collect(root, root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

fn install_local_authored_skill(
    skills_root: &Path,
    install_name: &str,
    declared_revision: &str,
    owner_id: &str,
    files: Vec<(String, Bytes)>,
    owner_key_file: &Path,
    owner_public_keyring_file: &Path,
    owner_key_id: &str,
    bootstrap: bool,
) -> Result<hermes_skills::SkillPackageManifestV2, AgentError> {
    if owner_id.trim().is_empty() {
        return Err(AgentError::Config(
            "Local author attestation requires a non-empty owner ID".to_string(),
        ));
    }
    if bootstrap {
        return Err(AgentError::Config(
            "Bootstrap is unavailable for local-authored packages".to_string(),
        ));
    }
    ensure_safe_install_name(install_name)?;

    let package_files = files
        .into_iter()
        .map(|(relative_path, bytes)| {
            hermes_skills::LocalPackageFile::new(relative_path, bytes.to_vec())
        })
        .collect::<Vec<_>>();
    let staging_store = hermes_skills::FileSkillStore::new(skills_root.to_path_buf());
    staging_store
        .stage_local_package(install_name, Path::new(install_name), &package_files)
        .map_err(|error| trust_error("Failed to stage local skill package", error))?;
    let mut manifest = staging_store
        .pending_local_manifest(install_name)
        .map_err(|error| trust_error("Failed to load staged local skill manifest", error))?
        .ok_or_else(|| {
            AgentError::Config("Local skill staging did not produce a pending manifest".to_string())
        })?;
    if manifest.identity.content_digest != declared_revision {
        return Err(AgentError::Config(format!(
            "Local skill source revision mismatch: expected {declared_revision}, got {}",
            manifest.identity.content_digest
        )));
    }

    let owner_keyring =
        hermes_skills::load_local_owner_keyring_file(skills_root, owner_public_keyring_file)
            .map_err(|error| trust_error("Failed to load protected local owner keyring", error))?;
    if !owner_keyring
        .keys
        .iter()
        .any(|key| key.owner_id == owner_id && key.key_id == owner_key_id)
    {
        return Err(AgentError::Config(
            "Local owner/key ID is not present in the protected local owner keyring".to_string(),
        ));
    }
    let owner_signer = hermes_skills::LocalOwnerSigningKey::load_owner_private_key_file(
        skills_root,
        owner_key_file,
    )
    .map_err(|error| trust_error("Failed to load local owner signing key", error))?;
    let attestation = owner_signer
        .sign_local_manifest(
            &mut manifest,
            owner_id,
            owner_key_id,
            format!("manual-{}", uuid::Uuid::new_v4()),
            chrono::Utc::now().to_rfc3339(),
        )
        .map_err(|error| trust_error("Failed to sign local owner attestation", error))?;
    let promotion_store = hermes_skills::FileSkillStore::new(skills_root.to_path_buf())
        .with_trusted_local_owner_keys(owner_keyring.keys);
    promotion_store
        .attest_local_skill(install_name, attestation)
        .map_err(|error| trust_error("Failed to promote local owner-attested package", error))?;
    hermes_skills::load_lock(skills_root)
        .map_err(|error| trust_error("Failed to load promoted local package lock", error))?
        .packages
        .into_iter()
        .find(|entry| entry.manifest.identity.name == install_name)
        .map(|entry| entry.manifest)
        .ok_or_else(|| {
            AgentError::Config("Promoted local package is absent from the v2 lock".to_string())
        })
}
