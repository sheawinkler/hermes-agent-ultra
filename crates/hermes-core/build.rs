use std::fmt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

const BUILD_IDENTITY_SCHEMA: &str = "hermes.embedded-build-identity.v1";
const BUILD_RECIPE_SCHEMA: &str = "hermes.build-recipe.v1";
const SOURCE_STATE_CLEAN: &str = "clean";
const SOURCE_STATE_DIRTY: &str = "dirty";
const SOURCE_STATE_UNAVAILABLE: &str = "unavailable";

fn main() {
    println!("cargo:rerun-if-env-changed=HERMES_BUILD_GIT_SHA");
    println!("cargo:rerun-if-env-changed=GITHUB_SHA");
    println!("cargo:rerun-if-env-changed=HERMES_BUILD_SOURCE_STATE");

    let manifest_dir = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR")
            .expect("Cargo must provide CARGO_MANIFEST_DIR to hermes-core build.rs"),
    );
    let repo_root = manifest_dir.join("../..");
    emit_git_rerun_triggers(&repo_root);

    let recipe = BuildRecipeInputs::from_environment()
        .unwrap_or_else(|error| panic!("invalid hermes-core build recipe input: {error}"));
    emit_build_recipe(&recipe);

    let trusted_environment = TrustedEnvironmentIdentity::from_environment()
        .unwrap_or_else(|error| panic!("invalid hermes-core build identity environment: {error}"));
    let source_identity = resolve_source_identity(&repo_root, trusted_environment)
        .unwrap_or_else(|error| panic!("invalid hermes-core source identity: {error}"));
    emit_source_identity(source_identity);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildSourceState {
    Clean,
    Dirty,
    Unavailable,
}

impl BuildSourceState {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Clean => SOURCE_STATE_CLEAN,
            Self::Dirty => SOURCE_STATE_DIRTY,
            Self::Unavailable => SOURCE_STATE_UNAVAILABLE,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            SOURCE_STATE_CLEAN => Some(Self::Clean),
            SOURCE_STATE_DIRTY => Some(Self::Dirty),
            SOURCE_STATE_UNAVAILABLE => Some(Self::Unavailable),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TrustedEnvironmentIdentity {
    git_sha1: Option<String>,
    source_state: Option<BuildSourceState>,
}

impl TrustedEnvironmentIdentity {
    fn from_environment() -> Result<Self, BuildScriptIdentityError> {
        Self::from_values(
            std::env::var("HERMES_BUILD_GIT_SHA").ok().as_deref(),
            std::env::var("GITHUB_SHA").ok().as_deref(),
            std::env::var("HERMES_BUILD_SOURCE_STATE").ok().as_deref(),
        )
    }

    fn from_values(
        hermes_build_git_sha: Option<&str>,
        github_sha: Option<&str>,
        source_state: Option<&str>,
    ) -> Result<Self, BuildScriptIdentityError> {
        let hermes_sha = parse_optional_git_sha1("HERMES_BUILD_GIT_SHA", hermes_build_git_sha)?;
        let github_sha = parse_optional_git_sha1("GITHUB_SHA", github_sha)?;

        if let (Some(hermes_sha), Some(github_sha)) = (&hermes_sha, &github_sha) {
            if hermes_sha != github_sha {
                return Err(BuildScriptIdentityError::ConflictingGitSha1);
            }
        }

        let source_state = match source_state {
            Some(value) => Some(
                BuildSourceState::parse(value)
                    .ok_or(BuildScriptIdentityError::InvalidSourceState)?,
            ),
            None => None,
        };
        let git_sha1 = hermes_sha.or(github_sha);

        if source_state == Some(BuildSourceState::Unavailable) && git_sha1.is_some() {
            return Err(BuildScriptIdentityError::UnavailableStateWithRevision);
        }

        Ok(Self {
            git_sha1,
            source_state,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct BuildRecipeInputs {
    target: String,
    profile: String,
    features: String,
}

impl BuildRecipeInputs {
    fn from_environment() -> Result<Self, BuildScriptIdentityError> {
        let target = required_recipe_value("TARGET")?;
        let profile = required_recipe_value("PROFILE")?;
        let features = cargo_feature_keys()?.join(",");

        Ok(Self {
            target,
            profile,
            features,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceIdentity {
    git_sha1: Option<String>,
    source_state: BuildSourceState,
}

impl SourceIdentity {
    const fn unavailable() -> Self {
        Self {
            git_sha1: None,
            source_state: BuildSourceState::Unavailable,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GitSourceProbe {
    Absent,
    Indeterminate,
    Available(SourceIdentity),
}

fn resolve_source_identity(
    repo_root: &Path,
    trusted_environment: TrustedEnvironmentIdentity,
) -> Result<SourceIdentity, BuildScriptIdentityError> {
    resolve_source_probe(inspect_git_source(repo_root), trusted_environment)
}

fn resolve_source_probe(
    probe: GitSourceProbe,
    trusted_environment: TrustedEnvironmentIdentity,
) -> Result<SourceIdentity, BuildScriptIdentityError> {
    match probe {
        GitSourceProbe::Available(discovered) => {
            if let Some(expected_sha) = trusted_environment.git_sha1 {
                if discovered.git_sha1.as_deref() != Some(expected_sha.as_str()) {
                    return Err(BuildScriptIdentityError::GitRevisionMismatch);
                }
            }
            if let Some(expected_state) = trusted_environment.source_state {
                if discovered.source_state != expected_state {
                    return Err(BuildScriptIdentityError::GitSourceStateMismatch);
                }
            }
            Ok(discovered)
        }
        GitSourceProbe::Absent => match (
            trusted_environment.git_sha1,
            trusted_environment.source_state,
        ) {
            (
                Some(git_sha1),
                Some(source_state @ (BuildSourceState::Clean | BuildSourceState::Dirty)),
            ) => Ok(SourceIdentity {
                git_sha1: Some(git_sha1),
                source_state,
            }),
            _ => Ok(SourceIdentity::unavailable()),
        },
        GitSourceProbe::Indeterminate => Ok(SourceIdentity::unavailable()),
    }
}

fn inspect_git_source(repo_root: &Path) -> GitSourceProbe {
    let Some(git_sha1) = git_output(repo_root, ["rev-parse", "--verify", "HEAD^{commit}"])
        .and_then(parse_git_output_line)
    else {
        return GitSourceProbe::Absent;
    };
    if !is_valid_git_sha1(&git_sha1) {
        return GitSourceProbe::Indeterminate;
    }

    let Some(status) = git_output(
        repo_root,
        [
            "status",
            "--porcelain=v1",
            "--untracked-files=all",
            "--ignore-submodules=none",
        ],
    ) else {
        return GitSourceProbe::Indeterminate;
    };

    let source_state = if status.is_empty() {
        BuildSourceState::Clean
    } else {
        BuildSourceState::Dirty
    };
    GitSourceProbe::Available(SourceIdentity {
        git_sha1: Some(git_sha1),
        source_state,
    })
}

fn git_output<const N: usize>(repo_root: &Path, args: [&str; N]) -> Option<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .ok()?;
    output.status.success().then_some(output.stdout)
}

fn parse_git_output_line(output: Vec<u8>) -> Option<String> {
    let output = std::str::from_utf8(&output).ok()?;
    let output = output.strip_suffix('\n').unwrap_or(output);
    let output = output.strip_suffix('\r').unwrap_or(output);
    (!output.is_empty() && !output.contains(['\n', '\r'])).then(|| output.to_owned())
}

fn parse_optional_git_sha1(
    variable: &'static str,
    value: Option<&str>,
) -> Result<Option<String>, BuildScriptIdentityError> {
    match value {
        Some(value) if is_valid_git_sha1(value) => Ok(Some(value.to_owned())),
        Some(_) => Err(BuildScriptIdentityError::InvalidGitSha1(variable)),
        None => Ok(None),
    }
}

fn required_recipe_value(variable: &'static str) -> Result<String, BuildScriptIdentityError> {
    let value = std::env::var(variable)
        .map_err(|_| BuildScriptIdentityError::MissingBuildRecipeInput(variable))?;
    if is_valid_recipe_component(&value) {
        Ok(value)
    } else {
        Err(BuildScriptIdentityError::InvalidBuildRecipeInput(variable))
    }
}

fn cargo_feature_keys() -> Result<Vec<String>, BuildScriptIdentityError> {
    let mut features = Vec::new();
    for (variable, value) in std::env::vars() {
        let Some(feature) = variable.strip_prefix("CARGO_FEATURE_") else {
            continue;
        };
        if value != "1" || !is_valid_feature_key(feature) {
            return Err(BuildScriptIdentityError::InvalidCargoFeatureInput);
        }
        features.push(feature.to_owned());
    }
    features.sort_unstable();
    features.dedup();
    Ok(features)
}

fn is_valid_git_sha1(value: &str) -> bool {
    value.len() == 40
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn is_valid_recipe_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn is_valid_feature_key(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b'_')
}

fn emit_build_recipe(recipe: &BuildRecipeInputs) {
    emit_rustc_env(
        "HERMES_EMBEDDED_BUILD_IDENTITY_SCHEMA",
        BUILD_IDENTITY_SCHEMA,
    );
    emit_rustc_env("HERMES_EMBEDDED_BUILD_RECIPE_SCHEMA", BUILD_RECIPE_SCHEMA);
    emit_rustc_env("HERMES_EMBEDDED_BUILD_TARGET", &recipe.target);
    emit_rustc_env("HERMES_EMBEDDED_BUILD_PROFILE", &recipe.profile);
    emit_rustc_env("HERMES_EMBEDDED_BUILD_FEATURES", &recipe.features);
}

fn emit_source_identity(source_identity: SourceIdentity) {
    emit_rustc_env(
        "HERMES_EMBEDDED_BUILD_SOURCE_STATE",
        source_identity.source_state.as_str(),
    );
    if let Some(git_sha1) = source_identity.git_sha1 {
        emit_rustc_env("HERMES_EMBEDDED_BUILD_GIT_SHA1", &git_sha1);
    }
}

fn emit_rustc_env(name: &str, value: &str) {
    println!("cargo:rustc-env={name}={value}");
}

fn emit_git_rerun_triggers(repo_root: &Path) {
    let git_pointer = repo_root.join(".git");
    println!("cargo:rerun-if-changed={}", git_pointer.display());

    let Some(git_dir) = resolve_git_dir(&git_pointer, repo_root) else {
        return;
    };
    println!("cargo:rerun-if-changed={}", git_dir.join("HEAD").display());
    println!(
        "cargo:rerun-if-changed={}",
        git_dir.join("packed-refs").display()
    );

    let head = std::fs::read_to_string(git_dir.join("HEAD")).unwrap_or_default();
    let Some(ref_path) = head.trim().strip_prefix("ref: ") else {
        return;
    };
    let ref_path = Path::new(ref_path);
    if ref_path
        .components()
        .all(|component| matches!(component, Component::Normal(_)))
    {
        println!(
            "cargo:rerun-if-changed={}",
            git_dir.join(ref_path).display()
        );
    }
}

fn resolve_git_dir(git_pointer: &Path, repo_root: &Path) -> Option<PathBuf> {
    if git_pointer.is_dir() {
        return Some(git_pointer.to_owned());
    }

    let pointer = std::fs::read_to_string(git_pointer).ok()?;
    let git_dir = pointer.trim().strip_prefix("gitdir: ")?;
    let git_dir = PathBuf::from(git_dir);
    Some(if git_dir.is_absolute() {
        git_dir
    } else {
        repo_root.join(git_dir)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum BuildScriptIdentityError {
    InvalidGitSha1(&'static str),
    ConflictingGitSha1,
    InvalidSourceState,
    UnavailableStateWithRevision,
    GitRevisionMismatch,
    GitSourceStateMismatch,
    MissingBuildRecipeInput(&'static str),
    InvalidBuildRecipeInput(&'static str),
    InvalidCargoFeatureInput,
}

impl fmt::Display for BuildScriptIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidGitSha1(variable) => {
                write!(
                    formatter,
                    "{variable} must be exactly 40 lowercase hexadecimal characters"
                )
            }
            Self::ConflictingGitSha1 => write!(
                formatter,
                "HERMES_BUILD_GIT_SHA and GITHUB_SHA must agree when both are set"
            ),
            Self::InvalidSourceState => write!(
                formatter,
                "HERMES_BUILD_SOURCE_STATE must be clean, dirty, or unavailable"
            ),
            Self::UnavailableStateWithRevision => write!(
                formatter,
                "an unavailable source state must not claim a git revision"
            ),
            Self::GitRevisionMismatch => write!(
                formatter,
                "trusted git revision does not match the checked-out source revision"
            ),
            Self::GitSourceStateMismatch => write!(
                formatter,
                "trusted source state does not match deterministic git status"
            ),
            Self::MissingBuildRecipeInput(variable) => {
                write!(formatter, "Cargo did not provide {variable}")
            }
            Self::InvalidBuildRecipeInput(variable) => {
                write!(formatter, "Cargo provided an invalid {variable}")
            }
            Self::InvalidCargoFeatureInput => write!(
                formatter,
                "Cargo provided an invalid CARGO_FEATURE_ build recipe input"
            ),
        }
    }
}

impl std::error::Error for BuildScriptIdentityError {}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA_A: &str = "0123456789abcdef0123456789abcdef01234567";
    const SHA_B: &str = "89abcdef0123456789abcdef0123456789abcdef";

    #[test]
    fn trusted_environment_rejects_malformed_full_sha1() {
        assert_eq!(
            TrustedEnvironmentIdentity::from_values(Some("abcdef"), None, None),
            Err(BuildScriptIdentityError::InvalidGitSha1(
                "HERMES_BUILD_GIT_SHA"
            ))
        );
        assert_eq!(
            TrustedEnvironmentIdentity::from_values(Some(&SHA_A.to_uppercase()), None, None),
            Err(BuildScriptIdentityError::InvalidGitSha1(
                "HERMES_BUILD_GIT_SHA"
            ))
        );
        assert_eq!(
            TrustedEnvironmentIdentity::from_values(None, Some(" "), None),
            Err(BuildScriptIdentityError::InvalidGitSha1("GITHUB_SHA"))
        );
    }

    #[test]
    fn trusted_environment_rejects_conflicting_revisions_and_states() {
        assert_eq!(
            TrustedEnvironmentIdentity::from_values(Some(SHA_A), Some(SHA_B), None),
            Err(BuildScriptIdentityError::ConflictingGitSha1)
        );
        assert_eq!(
            TrustedEnvironmentIdentity::from_values(Some(SHA_A), None, Some("unknown")),
            Err(BuildScriptIdentityError::InvalidSourceState)
        );
        assert_eq!(
            TrustedEnvironmentIdentity::from_values(Some(SHA_A), None, Some("unavailable")),
            Err(BuildScriptIdentityError::UnavailableStateWithRevision)
        );
    }

    #[test]
    fn git_absent_requires_complete_trusted_identity() {
        let partial = TrustedEnvironmentIdentity::from_values(Some(SHA_A), None, None).unwrap();
        assert_eq!(
            resolve_source_probe(GitSourceProbe::Absent, partial).unwrap(),
            SourceIdentity::unavailable()
        );

        let complete =
            TrustedEnvironmentIdentity::from_values(Some(SHA_A), None, Some("clean")).unwrap();
        assert_eq!(
            resolve_source_probe(GitSourceProbe::Absent, complete).unwrap(),
            SourceIdentity {
                git_sha1: Some(SHA_A.to_owned()),
                source_state: BuildSourceState::Clean,
            }
        );
    }

    #[test]
    fn checked_out_git_rejects_unverified_environment_claims() {
        let discovered = SourceIdentity {
            git_sha1: Some(SHA_A.to_owned()),
            source_state: BuildSourceState::Clean,
        };
        let mismatch =
            TrustedEnvironmentIdentity::from_values(Some(SHA_B), None, Some("clean")).unwrap();
        assert_eq!(
            resolve_source_probe(GitSourceProbe::Available(discovered), mismatch),
            Err(BuildScriptIdentityError::GitRevisionMismatch)
        );
    }

    #[test]
    fn recipe_input_validation_rejects_arbitrary_values() {
        assert!(is_valid_recipe_component("aarch64-apple-darwin"));
        assert!(!is_valid_recipe_component("release\nother"));
        assert!(is_valid_feature_key("DEFAULT_FEATURE"));
        assert!(!is_valid_feature_key("default-feature"));
    }
}
