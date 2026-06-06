//! Shared runtime version labeling.

/// Workspace package version embedded at compile time.
pub const PACKAGE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Optional git SHA captured by the build script or release environment.
pub const BUILD_GIT_SHA: Option<&str> = option_env!("HERMES_BUILD_GIT_SHA");

/// Return the optional build git SHA after trimming empty values.
pub fn build_git_sha() -> Option<&'static str> {
    BUILD_GIT_SHA.and_then(|sha| {
        let trimmed = sha.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    })
}

/// Human-readable version line for chat and CLI slash surfaces.
pub fn version_label() -> String {
    match build_git_sha() {
        Some(sha) => format!("Hermes Agent Ultra v{PACKAGE_VERSION} ({sha})"),
        None => format!("Hermes Agent Ultra v{PACKAGE_VERSION}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_label_contains_product_and_version() {
        let label = version_label();
        assert!(label.starts_with("Hermes Agent Ultra v"));
        assert!(label.contains(PACKAGE_VERSION));
    }

    #[test]
    fn build_git_sha_is_empty_or_trimmed() {
        if let Some(sha) = build_git_sha() {
            assert_eq!(sha, sha.trim());
            assert!(!sha.is_empty());
        }
    }
}
