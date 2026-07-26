//! Typed, redacted locations for the skill trust authorities.
//!
//! These paths are derived only from roots that startup has already resolved.
//! They deliberately do not read environment variables and never create or
//! populate trust material.

use std::fmt;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde::ser::{SerializeStruct, Serializer};
use serde::Serialize;

use crate::loader::ConfigError;

const SKILLS_DIRECTORY: &str = "skills";
const TRUST_DIRECTORY: &str = "skill-trust";
const REMOTE_ROOT_FILE: &str = "remote-root.json";
const REMOTE_AUDIT_FILE: &str = "remote-audit.json";
const LOCAL_OWNER_KEYRING_FILE: &str = "local-owner-keyring.json";
const REDACTED: &str = "[REDACTED]";

/// Absolute locations for the skill package tree and its external authorities.
///
/// The authority directory is always outside `skills/`, so a remotely supplied
/// package cannot replace the remote trust root, audit, or local-owner public
/// keyring through ordinary skill-tree writes. Construction is passive: absent
/// authority files describe an unprovisioned fresh install and are never
/// created or filled from a fallback source.
#[derive(Clone, PartialEq, Eq)]
pub struct SkillTrustAuthorityLayout {
    state_root: PathBuf,
    skills_root: PathBuf,
    authority_root: PathBuf,
    remote_root_file: PathBuf,
    remote_audit_file: PathBuf,
    local_owner_keyring_file: PathBuf,
}

impl SkillTrustAuthorityLayout {
    /// Derive the layout when the resolved Hermes home is also the state root.
    pub fn from_resolved_home(home: impl AsRef<Path>) -> Result<Self, ConfigError> {
        Self::from_resolved_roots(home.as_ref(), home.as_ref())
    }

    /// Derive the layout when the caller has already resolved a state root.
    ///
    /// This is intentionally equivalent to [`Self::from_resolved_home`]: it
    /// exists for runtime entrypoints that call the resolved root `state_root`.
    pub fn from_resolved_state_root(state_root: impl AsRef<Path>) -> Result<Self, ConfigError> {
        Self::from_resolved_roots(state_root.as_ref(), state_root.as_ref())
    }

    /// Derive a layout from explicit, already-resolved home and state roots.
    ///
    /// Both roots must be absolute, traversal-free paths. Existing roots and
    /// derived directories must not be symlinks; absent fresh-install paths are
    /// allowed. The state root must not be nested in the skills tree, because
    /// that would place authority files under remotely writable package state.
    pub fn from_resolved_roots(
        home: impl AsRef<Path>,
        state_root: impl AsRef<Path>,
    ) -> Result<Self, ConfigError> {
        let home = validated_absolute_path(home.as_ref(), "Hermes home")?;
        let state_root = validated_absolute_path(state_root.as_ref(), "Hermes state root")?;
        validate_existing_directory_or_absent(&home, "Hermes home")?;
        validate_existing_directory_or_absent(&state_root, "Hermes state root")?;

        let skills_root = home.join(SKILLS_DIRECTORY);
        let authority_root = state_root.join(TRUST_DIRECTORY);
        validate_existing_directory_or_absent(&skills_root, "skills root")?;
        validate_existing_directory_or_absent(&authority_root, "skill trust authority directory")?;

        if paths_overlap(&skills_root, &authority_root) {
            return Err(layout_error(
                "skill trust authority directory must not overlap the skills root",
            ));
        }

        let remote_root_file = authority_root.join(REMOTE_ROOT_FILE);
        let remote_audit_file = authority_root.join(REMOTE_AUDIT_FILE);
        let local_owner_keyring_file = authority_root.join(LOCAL_OWNER_KEYRING_FILE);
        for (path, label) in [
            (&remote_root_file, "remote trust root"),
            (&remote_audit_file, "remote trust audit"),
            (&local_owner_keyring_file, "local owner public keyring"),
        ] {
            validate_existing_regular_file_or_absent(path, label)?;
            if !path.starts_with(&authority_root) {
                return Err(layout_error(
                    "derived authority path escaped its fixed directory",
                ));
            }
        }

        Ok(Self {
            state_root,
            skills_root,
            authority_root,
            remote_root_file,
            remote_audit_file,
            local_owner_keyring_file,
        })
    }

    /// The resolved runtime state root used to derive this layout.
    #[must_use]
    pub fn state_root(&self) -> &Path {
        &self.state_root
    }

    /// The absolute installed-skill root.
    #[must_use]
    pub fn skills_root(&self) -> &Path {
        &self.skills_root
    }

    /// The fixed external directory containing trust authority files.
    #[must_use]
    pub fn authority_root(&self) -> &Path {
        &self.authority_root
    }

    /// The absolute, external remote-root file location.
    #[must_use]
    pub fn remote_root_file(&self) -> &Path {
        &self.remote_root_file
    }

    /// The absolute, external remote-keyring audit location.
    #[must_use]
    pub fn remote_audit_file(&self) -> &Path {
        &self.remote_audit_file
    }

    /// The absolute, external local-owner public-keyring location.
    #[must_use]
    pub fn local_owner_keyring_file(&self) -> &Path {
        &self.local_owner_keyring_file
    }

    /// Build the skills-layer authority type without creating authority files.
    ///
    /// The skills crate performs the owner-only and provisioning checks. A
    /// fresh installation can therefore construct this layout before the
    /// verifier reports its typed unprovisioned state.
    pub fn build_skill_trust_authority_paths(
        &self,
    ) -> Result<hermes_skills::SkillTrustAuthorityPaths, ConfigError> {
        hermes_skills::SkillTrustAuthorityPaths::new(
            self.skills_root.clone(),
            self.remote_root_file.clone(),
            self.remote_audit_file.clone(),
            self.local_owner_keyring_file.clone(),
        )
        .map_err(|error| layout_error(format!("invalid skill trust authority paths: {error}")))
    }
}

impl fmt::Debug for SkillTrustAuthorityLayout {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SkillTrustAuthorityLayout")
            .field("state_root", &REDACTED)
            .field("skills_root", &REDACTED)
            .field("authority_root", &REDACTED)
            .field("remote_root_file", &REDACTED)
            .field("remote_audit_file", &REDACTED)
            .field("local_owner_keyring_file", &REDACTED)
            .finish()
    }
}

impl Serialize for SkillTrustAuthorityLayout {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut state = serializer.serialize_struct("SkillTrustAuthorityLayout", 6)?;
        state.serialize_field("state_root", REDACTED)?;
        state.serialize_field("skills_root", REDACTED)?;
        state.serialize_field("authority_root", REDACTED)?;
        state.serialize_field("remote_root_file", REDACTED)?;
        state.serialize_field("remote_audit_file", REDACTED)?;
        state.serialize_field("local_owner_keyring_file", REDACTED)?;
        state.end()
    }
}

fn layout_error(reason: impl fmt::Display) -> ConfigError {
    ConfigError::ValidationError(format!("invalid skill trust authority layout: {reason}"))
}

fn validated_absolute_path(path: &Path, label: &str) -> Result<PathBuf, ConfigError> {
    if !path.is_absolute() {
        return Err(layout_error(format!("{label} must be absolute")));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(layout_error(format!(
            "{label} must not contain path traversal"
        )));
    }
    Ok(path.to_path_buf())
}

fn validate_existing_directory_or_absent(path: &Path, label: &str) -> Result<(), ConfigError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(layout_error(format!("{label} must not be a symlink")))
        }
        Ok(metadata) if !metadata.is_dir() => {
            Err(layout_error(format!("{label} must be a directory")))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(layout_error(format!("could not inspect {label}: {error}"))),
    }
}

fn validate_existing_regular_file_or_absent(path: &Path, label: &str) -> Result<(), ConfigError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            Err(layout_error(format!("{label} must not be a symlink")))
        }
        Ok(metadata) if !metadata.is_file() => {
            Err(layout_error(format!("{label} must be a regular file")))
        }
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(layout_error(format!("could not inspect {label}: {error}"))),
    }
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left.starts_with(right) || right.starts_with(left)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_external_absolute_authority_paths_from_explicit_root() {
        let root = tempfile::tempdir().expect("create state root");
        let layout = SkillTrustAuthorityLayout::from_resolved_state_root(root.path())
            .expect("derive layout");

        assert_eq!(layout.state_root(), root.path());
        assert_eq!(layout.skills_root(), root.path().join("skills"));
        assert_eq!(layout.authority_root(), root.path().join("skill-trust"));
        assert_eq!(
            layout.remote_root_file(),
            root.path().join("skill-trust/remote-root.json")
        );
        assert_eq!(
            layout.remote_audit_file(),
            root.path().join("skill-trust/remote-audit.json")
        );
        assert_eq!(
            layout.local_owner_keyring_file(),
            root.path().join("skill-trust/local-owner-keyring.json")
        );
        assert!(layout.skills_root().is_absolute());
        assert!(layout.remote_root_file().is_absolute());
        assert!(!paths_overlap(
            layout.skills_root(),
            layout.authority_root()
        ));
    }

    #[test]
    fn permits_absent_fresh_install_authority_files() {
        let root = tempfile::tempdir().expect("create state root");
        let layout = SkillTrustAuthorityLayout::from_resolved_home(root.path())
            .expect("absent authority files must not prevent layout construction");

        assert!(!layout.authority_root().exists());
        assert!(!layout.remote_root_file().exists());
        assert!(!layout.remote_audit_file().exists());
        assert!(!layout.local_owner_keyring_file().exists());

        std::fs::create_dir_all(layout.skills_root()).expect("create skills root");
        std::fs::create_dir_all(layout.authority_root()).expect("create authority root");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(
                layout.authority_root(),
                std::fs::Permissions::from_mode(0o700),
            )
            .expect("restrict authority root");
        }

        let paths = layout
            .build_skill_trust_authority_paths()
            .expect("unprovisioned leaves must still produce typed authority paths");
        assert_eq!(
            paths.remote_trust_config().root_file,
            layout.remote_root_file()
        );
        assert_eq!(
            paths.local_owner_keyring_file(),
            layout.local_owner_keyring_file()
        );
    }

    #[test]
    fn rejects_relative_traversal_and_skills_overlap() {
        let relative = SkillTrustAuthorityLayout::from_resolved_state_root("relative/root")
            .expect_err("relative state root must reject");
        assert!(relative.to_string().contains("must be absolute"));

        let root = tempfile::tempdir().expect("create root");
        let traversal = root.path().join("nested").join("..").join("escaped");
        let traversal = SkillTrustAuthorityLayout::from_resolved_state_root(&traversal)
            .expect_err("path traversal must reject");
        assert!(traversal.to_string().contains("path traversal"));

        let overlap =
            SkillTrustAuthorityLayout::from_resolved_roots(root.path(), root.path().join("skills"))
                .expect_err("authority directory inside skills root must reject");
        assert!(overlap.to_string().contains("must not overlap"));
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_home_and_authority_directory() {
        use std::os::unix::fs::symlink;

        let homes = tempfile::tempdir().expect("create home parent");
        let real_home = tempfile::tempdir().expect("create real home");
        let linked_home = homes.path().join("linked-home");
        symlink(real_home.path(), &linked_home).expect("create home symlink");
        let error = SkillTrustAuthorityLayout::from_resolved_home(&linked_home)
            .expect_err("symlinked home must reject");
        assert!(error.to_string().contains("must not be a symlink"));
        let root = tempfile::tempdir().expect("create root");
        let external = tempfile::tempdir().expect("create external target");
        symlink(external.path(), root.path().join(TRUST_DIRECTORY))
            .expect("create authority-directory symlink");

        let error = SkillTrustAuthorityLayout::from_resolved_state_root(root.path())
            .expect_err("symlinked authority directory must reject");
        assert!(error.to_string().contains("must not be a symlink"));
    }

    #[test]
    fn debug_and_serde_redact_derived_paths() {
        let root = tempfile::tempdir().expect("create state root");
        let layout = SkillTrustAuthorityLayout::from_resolved_state_root(root.path())
            .expect("derive layout");
        let root_text = root.path().display().to_string();

        let debug = format!("{layout:?}");
        assert!(!debug.contains(&root_text));
        assert!(debug.contains(REDACTED));

        let json = serde_json::to_string(&layout).expect("serialize redacted layout");
        assert!(!json.contains(&root_text));
        assert!(!json.contains(REMOTE_ROOT_FILE));
        assert!(json.contains(REDACTED));
    }
}
