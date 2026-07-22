//! Shared runtime version labeling and immutable build identity.

use serde::Serialize;
use sha2::{Digest, Sha256};
use std::sync::LazyLock;

/// Workspace package version embedded at compile time.
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Stable schema for the complete embedded build identity document.
pub const BUILD_IDENTITY_SCHEMA: &str = "hermes.embedded-build-identity.v1";

/// Stable schema for the embedded build-recipe document.
pub const BUILD_RECIPE_SCHEMA: &str = "hermes.build-recipe.v1";

const GIT_SHA1_ALGORITHM: &str = "git-sha1";
const SOURCE_STATE_CLEAN: &str = "clean";
const SOURCE_STATE_DIRTY: &str = "dirty";
const SOURCE_STATE_UNAVAILABLE: &str = "unavailable";

const EMBEDDED_IDENTITY_SCHEMA: Option<&str> = option_env!("HERMES_EMBEDDED_BUILD_IDENTITY_SCHEMA");
const EMBEDDED_GIT_SHA1: Option<&str> = option_env!("HERMES_EMBEDDED_BUILD_GIT_SHA1");
const EMBEDDED_SOURCE_STATE: Option<&str> = option_env!("HERMES_EMBEDDED_BUILD_SOURCE_STATE");
const EMBEDDED_RECIPE_SCHEMA: Option<&str> = option_env!("HERMES_EMBEDDED_BUILD_RECIPE_SCHEMA");
const EMBEDDED_TARGET: Option<&str> = option_env!("HERMES_EMBEDDED_BUILD_TARGET");
const EMBEDDED_PROFILE: Option<&str> = option_env!("HERMES_EMBEDDED_BUILD_PROFILE");
const EMBEDDED_FEATURES: Option<&str> = option_env!("HERMES_EMBEDDED_BUILD_FEATURES");

/// The only revision algorithm accepted by the embedded identity protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionAlgorithm {
    /// A lowercase, complete 40-hex Git SHA-1 commit identifier.
    GitSha1,
}

impl RevisionAlgorithm {
    const fn as_str(self) -> &'static str {
        match self {
            Self::GitSha1 => GIT_SHA1_ALGORITHM,
        }
    }
}

/// Closed source-state vocabulary embedded by the hermes-core build script.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceState {
    /// Git deterministically reported no tracked, untracked, or submodule changes.
    Clean,
    /// Git deterministically reported at least one source-tree change.
    Dirty,
    /// No complete and validated source state was available at build time.
    Unavailable,
}

impl SourceState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Clean => SOURCE_STATE_CLEAN,
            Self::Dirty => SOURCE_STATE_DIRTY,
            Self::Unavailable => SOURCE_STATE_UNAVAILABLE,
        }
    }

    fn parse(value: &str) -> Result<Self, BuildIdentityError> {
        match value {
            SOURCE_STATE_CLEAN => Ok(Self::Clean),
            SOURCE_STATE_DIRTY => Ok(Self::Dirty),
            SOURCE_STATE_UNAVAILABLE => Ok(Self::Unavailable),
            _ => Err(BuildIdentityError::MalformedEmbeddedIdentity),
        }
    }
}

/// Typed failure returned when no authority-grade embedded identity is available.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildIdentityError {
    /// The build source could not be identified completely.
    #[error("embedded build identity is unavailable")]
    SourceUnavailable,
    /// The build was made from a dirty source tree and cannot be promoted.
    #[error("embedded build identity was built from dirty source")]
    SourceDirty,
    /// A build-script value violated the closed identity protocol.
    #[error("embedded build identity is malformed")]
    MalformedEmbeddedIdentity,
}

/// Immutable, compiler-embedded source and build-recipe authority.
///
/// This type has no public constructor. Obtain it only through
/// [`embedded_build_identity`], which fails closed for unavailable, dirty, or
/// malformed source identity.
#[derive(Debug)]
pub struct EmbeddedBuildIdentity {
    git_sha1: &'static str,
    recipe: BuildRecipe,
    artifact_sha256: String,
}

impl EmbeddedBuildIdentity {
    /// Returns the closed revision algorithm used by [`Self::full_git_sha1`].
    pub const fn revision_algorithm(&self) -> RevisionAlgorithm {
        RevisionAlgorithm::GitSha1
    }

    /// Returns the full, validated lowercase 40-hex Git SHA-1.
    ///
    /// This is the authority accessor. Presentation code must use its own
    /// abbreviation rather than truncating the embedded identity.
    pub const fn full_git_sha1(&self) -> &'static str {
        self.git_sha1
    }

    /// Returns the source state bound into this authority identity.
    pub const fn source_state(&self) -> SourceState {
        SourceState::Clean
    }

    /// Returns whether this authority identity came from clean source.
    pub const fn source_clean(&self) -> bool {
        true
    }

    /// Returns the package version bound into the canonical identity digest.
    pub const fn package_version(&self) -> &'static str {
        PACKAGE_VERSION
    }

    /// Returns the immutable build-recipe inputs bound into this identity.
    pub const fn build_recipe(&self) -> &BuildRecipe {
        &self.recipe
    }

    /// Returns the RFC 8785/SHA-256 digest of the canonical build recipe.
    pub fn build_recipe_digest(&self) -> &str {
        self.recipe.sha256()
    }

    /// Returns the lowercase SHA-256 digest of the RFC 8785 canonical identity.
    ///
    /// This binds the schema, package version, Git SHA-1 algorithm and full
    /// revision, clean source state, and complete build recipe. It does not
    /// hash a mutable filesystem path or executable at runtime.
    pub fn artifact_sha256(&self) -> &str {
        &self.artifact_sha256
    }
}

/// Immutable build recipe included in an [`EmbeddedBuildIdentity`].
#[derive(Debug)]
pub struct BuildRecipe {
    target: &'static str,
    profile: &'static str,
    features: &'static str,
    sha256: String,
}

impl BuildRecipe {
    /// Returns the Cargo target triple or validated target identifier.
    pub const fn target(&self) -> &'static str {
        self.target
    }

    /// Returns the Cargo profile bound into the recipe.
    pub const fn profile(&self) -> &'static str {
        self.profile
    }

    /// Iterates over sorted Cargo feature environment keys in the recipe.
    pub fn feature_names(&self) -> impl Iterator<Item = &'static str> + '_ {
        self.features
            .split(',')
            .filter(|feature| !feature.is_empty())
    }

    /// Returns the lowercase SHA-256 digest of this RFC 8785 build recipe.
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
}

static EMBEDDED_BUILD_IDENTITY: LazyLock<Result<EmbeddedBuildIdentity, BuildIdentityError>> =
    LazyLock::new(load_embedded_build_identity);

/// Returns the immutable build identity compiled into this hermes-core binary.
///
/// Dirty, missing, and malformed identities return a typed error so release and
/// soak promotion consumers fail closed.
pub fn embedded_build_identity() -> Result<&'static EmbeddedBuildIdentity, BuildIdentityError> {
    EMBEDDED_BUILD_IDENTITY.as_ref().map_err(Clone::clone)
}

/// Returns the explicitly embedded source state, including unavailable state.
pub fn embedded_source_state() -> Result<SourceState, BuildIdentityError> {
    let state = required_embedded_value(EMBEDDED_SOURCE_STATE)?;
    SourceState::parse(state)
}

fn load_embedded_build_identity() -> Result<EmbeddedBuildIdentity, BuildIdentityError> {
    let source_state = embedded_source_state()?;
    match source_state {
        SourceState::Unavailable => Err(BuildIdentityError::SourceUnavailable),
        SourceState::Dirty => {
            if let Some(git_sha1) = EMBEDDED_GIT_SHA1 {
                validate_git_sha1(git_sha1)?;
            }
            Err(BuildIdentityError::SourceDirty)
        }
        SourceState::Clean => embedded_build_identity_from_values(
            EMBEDDED_IDENTITY_SCHEMA,
            EMBEDDED_GIT_SHA1,
            EMBEDDED_RECIPE_SCHEMA,
            EMBEDDED_TARGET,
            EMBEDDED_PROFILE,
            EMBEDDED_FEATURES,
        ),
    }
}

fn embedded_build_identity_from_values(
    identity_schema: Option<&'static str>,
    git_sha1: Option<&'static str>,
    recipe_schema: Option<&'static str>,
    target: Option<&'static str>,
    profile: Option<&'static str>,
    features: Option<&'static str>,
) -> Result<EmbeddedBuildIdentity, BuildIdentityError> {
    if required_embedded_value(identity_schema)? != BUILD_IDENTITY_SCHEMA {
        return Err(BuildIdentityError::MalformedEmbeddedIdentity);
    }
    if required_embedded_value(recipe_schema)? != BUILD_RECIPE_SCHEMA {
        return Err(BuildIdentityError::MalformedEmbeddedIdentity);
    }

    let git_sha1 = required_embedded_value(git_sha1)?;
    validate_git_sha1(git_sha1)?;

    if !is_valid_package_version(PACKAGE_VERSION) {
        return Err(BuildIdentityError::MalformedEmbeddedIdentity);
    }

    let target = required_embedded_value(target)?;
    let profile = required_embedded_value(profile)?;
    let features = required_embedded_value(features)?;
    if !is_valid_recipe_component(target) || !is_valid_recipe_component(profile) {
        return Err(BuildIdentityError::MalformedEmbeddedIdentity);
    }
    let feature_names = parse_feature_names(features)?;

    let canonical_recipe = CanonicalBuildRecipe {
        features: feature_names,
        profile,
        schema: BUILD_RECIPE_SCHEMA,
        target,
    };
    let recipe_sha256 = canonical_sha256(&canonical_recipe)?;
    let canonical_identity = CanonicalBuildIdentity {
        build_recipe: canonical_recipe,
        package_version: PACKAGE_VERSION,
        revision: CanonicalRevision {
            algorithm: RevisionAlgorithm::GitSha1.as_str(),
            value: git_sha1,
        },
        schema: BUILD_IDENTITY_SCHEMA,
        source_state: SourceState::Clean.as_str(),
    };
    let artifact_sha256 = canonical_sha256(&canonical_identity)?;

    Ok(EmbeddedBuildIdentity {
        git_sha1,
        recipe: BuildRecipe {
            target,
            profile,
            features,
            sha256: recipe_sha256,
        },
        artifact_sha256,
    })
}

fn required_embedded_value(
    value: Option<&'static str>,
) -> Result<&'static str, BuildIdentityError> {
    value.ok_or(BuildIdentityError::SourceUnavailable)
}

fn validate_git_sha1(git_sha1: &str) -> Result<(), BuildIdentityError> {
    if is_valid_git_sha1(git_sha1) {
        Ok(())
    } else {
        Err(BuildIdentityError::MalformedEmbeddedIdentity)
    }
}

fn is_valid_git_sha1(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn is_valid_package_version(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'+')
        })
}

fn is_valid_recipe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value.bytes().all(|byte| {
            byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-')
        })
}

fn parse_feature_names(value: &'static str) -> Result<Vec<&'static str>, BuildIdentityError> {
    if value.is_empty() {
        return Ok(Vec::new());
    }

    let mut features = Vec::new();
    let mut previous = None;
    for feature in value.split(',') {
        if !is_valid_feature_name(feature) || previous.is_some_and(|prior| prior >= feature) {
            return Err(BuildIdentityError::MalformedEmbeddedIdentity);
        }
        previous = Some(feature);
        features.push(feature);
    }
    Ok(features)
}

fn is_valid_feature_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn canonical_sha256<T: Serialize>(value: &T) -> Result<String, BuildIdentityError> {
    let canonical = serde_json_canonicalizer::to_vec(value)
        .map_err(|_| BuildIdentityError::MalformedEmbeddedIdentity)?;
    Ok(format!("{:x}", Sha256::digest(canonical)))
}

#[derive(Serialize)]
struct CanonicalBuildRecipe<'a> {
    features: Vec<&'a str>,
    profile: &'a str,
    schema: &'a str,
    target: &'a str,
}

#[derive(Serialize)]
struct CanonicalRevision<'a> {
    algorithm: &'a str,
    value: &'a str,
}

#[derive(Serialize)]
struct CanonicalBuildIdentity<'a> {
    build_recipe: CanonicalBuildRecipe<'a>,
    package_version: &'a str,
    revision: CanonicalRevision<'a>,
    schema: &'a str,
    source_state: &'a str,
}

fn raw_embedded_git_sha1() -> Option<&'static str> {
    EMBEDDED_GIT_SHA1.filter(|git_sha1| is_valid_git_sha1(git_sha1))
}

fn abbreviated_git_sha1(git_sha1: &str) -> &str {
    debug_assert!(is_valid_git_sha1(git_sha1));
    &git_sha1[..12]
}

fn version_label_for_identity(identity: &EmbeddedBuildIdentity) -> String {
    format!(
        "Hermes Agent Ultra v{} ({})",
        identity.package_version(),
        abbreviated_git_sha1(identity.full_git_sha1())
    )
}

fn version_label_for_dirty_revision(git_sha1: &str) -> String {
    format!(
        "Hermes Agent Ultra v{PACKAGE_VERSION} ({}, dirty)",
        abbreviated_git_sha1(git_sha1)
    )
}

fn unavailable_version_label() -> String {
    format!("Hermes Agent Ultra v{PACKAGE_VERSION} (source unavailable)")
}

/// Human-readable version line for chat and CLI slash surfaces.
///
/// The optional SHA in this label is intentionally abbreviated only for display;
/// release and soak consumers must use [`embedded_build_identity`].
pub fn version_label() -> String {
    match embedded_build_identity() {
        Ok(identity) => version_label_for_identity(identity),
        Err(BuildIdentityError::SourceDirty) => raw_embedded_git_sha1()
            .map(version_label_for_dirty_revision)
            .unwrap_or_else(unavailable_version_label),
        Err(BuildIdentityError::SourceUnavailable | BuildIdentityError::MalformedEmbeddedIdentity) => {
            unavailable_version_label()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA_A: &str = "0123456789abcdef0123456789abcdef01234567";
    const SHA_B: &str = "89abcdef0123456789abcdef0123456789abcdef";
    const TARGET_A: &str = "aarch64-apple-darwin";
    const TARGET_B: &str = "x86_64-unknown-linux-gnu";
    const PROFILE: &str = "release";
    const FEATURES: &str = "DEFAULT,RELEASE_HARDENING";

    fn clean_identity(
        git_sha1: &'static str,
        target: &'static str,
    ) -> Result<EmbeddedBuildIdentity, BuildIdentityError> {
        embedded_build_identity_from_values(
            Some(BUILD_IDENTITY_SCHEMA),
            Some(git_sha1),
            Some(BUILD_RECIPE_SCHEMA),
            Some(target),
            Some(PROFILE),
            Some(FEATURES),
        )
    }

    #[test]
    fn authority_accessor_retains_all_40_git_sha1_characters() {
        let identity = clean_identity(SHA_A, TARGET_A).expect("valid clean identity");

        assert_eq!(identity.revision_algorithm(), RevisionAlgorithm::GitSha1);
        assert_eq!(identity.full_git_sha1(), SHA_A);
        assert_eq!(identity.full_git_sha1().len(), 40);
        assert_ne!(identity.full_git_sha1().len(), 12);
        assert!(identity.source_clean());
        assert_eq!(identity.source_state(), SourceState::Clean);
    }

    #[test]
    fn version_display_abbreviates_without_changing_authority_sha1() {
        let identity = clean_identity(SHA_A, TARGET_A).expect("valid clean identity");
        let label = version_label_for_identity(&identity);

        assert_eq!(label, format!("Hermes Agent Ultra v{PACKAGE_VERSION} ({})", &SHA_A[..12]));
        assert!(!label.contains(SHA_A));
        assert_eq!(identity.full_git_sha1(), SHA_A);
    }

    #[test]
    fn dirty_and_unavailable_identity_states_fail_closed() {
        assert_eq!(
            embedded_source_state_from_value(Some(SOURCE_STATE_DIRTY)),
            Ok(SourceState::Dirty)
        );
        assert_eq!(
            embedded_source_state_from_value(Some(SOURCE_STATE_UNAVAILABLE)),
            Ok(SourceState::Unavailable)
        );
        assert_eq!(
            load_identity_for_source_state(SourceState::Dirty, Some(SHA_A)),
            Err(BuildIdentityError::SourceDirty)
        );
        assert_eq!(
            load_identity_for_source_state(SourceState::Unavailable, None),
            Err(BuildIdentityError::SourceUnavailable)
        );
    }

    #[test]
    fn canonical_digests_are_stable_and_bind_each_input() {
        let first = clean_identity(SHA_A, TARGET_A).expect("first valid identity");
        let repeated = clean_identity(SHA_A, TARGET_A).expect("repeated valid identity");
        let changed_revision = clean_identity(SHA_B, TARGET_A).expect("changed revision identity");
        let changed_recipe = clean_identity(SHA_A, TARGET_B).expect("changed recipe identity");

        assert_eq!(first.artifact_sha256(), repeated.artifact_sha256());
        assert_eq!(first.build_recipe_digest(), repeated.build_recipe_digest());
        assert_ne!(first.artifact_sha256(), changed_revision.artifact_sha256());
        assert_eq!(first.build_recipe_digest(), changed_revision.build_recipe_digest());
        assert_ne!(first.build_recipe_digest(), changed_recipe.build_recipe_digest());
        assert_ne!(first.artifact_sha256(), changed_recipe.artifact_sha256());
        assert!(is_lowercase_sha256(first.artifact_sha256()));
        assert!(is_lowercase_sha256(first.build_recipe_digest()));
    }

    #[test]
    fn malformed_embedded_values_are_not_authority() {
        assert_eq!(
            clean_identity("0123456789ABCDEF0123456789abcdef01234567", TARGET_A),
            Err(BuildIdentityError::MalformedEmbeddedIdentity)
        );
        assert_eq!(
            embedded_source_state_from_value(Some("unknown")),
            Err(BuildIdentityError::MalformedEmbeddedIdentity)
        );
    }

    #[test]
    fn version_label_contains_product_and_version() {
        let label = version_label();
        assert!(label.starts_with("Hermes Agent Ultra v"));
        assert!(label.contains(PACKAGE_VERSION));
    }

    fn embedded_source_state_from_value(
        value: Option<&str>,
    ) -> Result<SourceState, BuildIdentityError> {
        value
            .ok_or(BuildIdentityError::SourceUnavailable)
            .and_then(SourceState::parse)
    }

    fn load_identity_for_source_state(
        source_state: SourceState,
        git_sha1: Option<&'static str>,
    ) -> Result<EmbeddedBuildIdentity, BuildIdentityError> {
        match source_state {
            SourceState::Clean => clean_identity(git_sha1.ok_or(BuildIdentityError::SourceUnavailable)?, TARGET_A),
            SourceState::Dirty => {
                if let Some(git_sha1) = git_sha1 {
                    validate_git_sha1(git_sha1)?;
                }
                Err(BuildIdentityError::SourceDirty)
            }
            SourceState::Unavailable => Err(BuildIdentityError::SourceUnavailable),
        }
    }

    fn is_lowercase_sha256(value: &str) -> bool {
        value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    }
}
