//! Credential file protection — prevents tools from reading or writing
//! sensitive credential files, and scans content for leaked secrets.
//!
//! Requirement 22.3

use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;

use hermes_core::ToolError;
use regex::Regex;

// ---------------------------------------------------------------------------
// Protected file patterns
// ---------------------------------------------------------------------------

/// File name patterns that indicate credential / secret files.
/// Matched case-insensitively against the file name (not the full path).
const PROTECTED_FILE_NAMES: &[&str] = &[
    ".env",
    ".env.local",
    ".env.production",
    ".env.development",
    ".env.staging",
    ".env.test",
    "credentials",
    "credentials.json",
    "secrets",
    "secrets.json",
    "secrets.yaml",
    "secrets.yml",
    "secret_key",
    "service-account",
    "service_account.json",
    "service-account.json",
    "gcloud-service-key.json",
    "id_rsa",
    "id_ed25519",
    "id_ecdsa",
    "id_dsa",
    ".pem",
    ".key",
    ".p12",
    ".pfx",
    ".jks",
    ".keystore",
    ".pub", // SSH public keys — still sensitive in some contexts
];

/// Directory names that are always protected.
const PROTECTED_DIR_NAMES: &[&str] = &[".ssh", ".gnupg", ".aws", ".config/gcloud", ".kube"];

const WRITE_DENY_EXACT_ABSOLUTE: &[&str] = &[
    "/etc/hosts",
    "/etc/shadow",
    "/etc/passwd",
    "/etc/sudoers",
    "/private/etc/hosts",
    "/private/etc/passwd",
    "/private/etc/shadow",
    "/private/etc/sudoers",
];
const WRITE_DENY_PREFIX_ABSOLUTE: &[&str] = &[
    "/boot",
    "/etc/sudoers.d",
    "/etc/systemd",
    "/private/etc/ssh",
    "/private/etc/sudoers.d",
    "/private/var",
];
const WRITE_DENY_HOME_EXACT: &[&str] = &[
    ".netrc",
    ".bashrc",
    ".zshrc",
    ".profile",
    ".bash_profile",
    ".zprofile",
    ".npmrc",
    ".pypirc",
    ".pgpass",
];
const WRITE_DENY_HOME_PREFIX: &[&str] = &[
    ".ssh",
    ".aws",
    ".gnupg",
    ".kube",
    ".docker",
    ".azure",
    ".config/gh",
];

// ---------------------------------------------------------------------------
// Content secret patterns
// ---------------------------------------------------------------------------

static SECRET_PATTERNS: LazyLock<Vec<(&'static str, Regex)>> = LazyLock::new(|| {
    vec![
        (
            "Private key",
            Regex::new(r"(?i)-----BEGIN (?:RSA |EC |DSA )?PRIVATE KEY-----").unwrap(),
        ),
        (
            "API key (sk-)",
            Regex::new(r"\bsk-[a-zA-Z0-9]{20,}\b").unwrap(),
        ),
        (
            "API key (sk_live)",
            Regex::new(r"\bsk_live_[a-zA-Z0-9]{20,}\b").unwrap(),
        ),
        (
            "GitHub token",
            Regex::new(r"\bgh[ps]_[a-zA-Z0-9]{36,}\b").unwrap(),
        ),
        (
            "AWS access key",
            Regex::new(r"\bAKIA[0-9A-Z]{16}\b").unwrap(),
        ),
        (
            "AWS secret key",
            Regex::new(r"(?i)aws_secret_access_key\s*=\s*\S+").unwrap(),
        ),
        (
            "Google API key",
            Regex::new(r"\bAIza[0-9A-Za-z\-_]{35}\b").unwrap(),
        ),
        (
            "Slack token",
            Regex::new(r"\bxox[bpras]-[0-9a-zA-Z\-]{10,}\b").unwrap(),
        ),
        (
            "Telegram bot token",
            Regex::new(r"\b\d{8,12}:[A-Za-z0-9_-]{30,}\b").unwrap(),
        ),
        (
            "Stripe key",
            Regex::new(r"\b[rk]k_live_[0-9a-zA-Z]{24,}\b").unwrap(),
        ),
        (
            "JWT token",
            Regex::new(r"\beyJ[A-Za-z0-9-_]+\.eyJ[A-Za-z0-9-_]+\.[A-Za-z0-9-_]+\b").unwrap(),
        ),
        (
            "Password assignment",
            Regex::new(r"(?i)(?:password|passwd|pwd)\s*[:=]\s*\S+").unwrap(),
        ),
        (
            "Generic secret assignment",
            Regex::new(r"(?i)(?:secret|token|api_key|apikey|access_key)\s*[:=]\s*\S{8,}").unwrap(),
        ),
    ]
});

/// Immutable skill-store state that effect-capable tools must never access.
///
/// The core verifier is deliberately the only reader of the host skills root.
/// Owner-only mutation APIs and remote trust-root/audit authority paths live
/// outside this tool boundary.
#[derive(Debug, Clone)]
pub(crate) struct SkillTrustPathGuard {
    skills_root: PathBuf,
    external_authority_paths: Vec<PathBuf>,
}

impl SkillTrustPathGuard {
    pub(crate) fn new(
        skills_root: impl Into<PathBuf>,
        external_authority_paths: Vec<PathBuf>,
    ) -> Self {
        Self {
            skills_root: skills_root.into(),
            external_authority_paths,
        }
    }

    fn is_host_skill_root_path(&self, path: &Path) -> bool {
        path_matches_protected(path, &self.skills_root, true)
    }

    fn is_external_authority_path(&self, path: &Path) -> bool {
        self.external_authority_paths
            .iter()
            .any(|protected| path_matches_protected(path, protected, true))
    }

    /// A host skills-root or external root-signing authority path. This is
    /// intentionally independent of policy mode and environment flags.
    pub(crate) fn denies_path(&self, path: &Path) -> bool {
        self.is_host_skill_root_path(path) || self.is_external_authority_path(path)
    }

    /// Reject a path-like fragment embedded in terminal/code/plugin input.
    ///
    /// This intentionally treats arbitrary strings conservatively: a
    /// user-supplied effect payload has no reason to reference verifier-only
    /// state, and raw/disabled policy modes must not turn that into access.
    pub(crate) fn denies_text(&self, text: &str) -> bool {
        const VARIABLE_PROTECTED_SUFFIXES: &[&str] = &[
            "$HERMES_HOME/skills",
            "${HERMES_HOME}/skills",
            "$HERMES_AGENT_ULTRA_HOME/skills",
            "${HERMES_AGENT_ULTRA_HOME}/skills",
        ];
        if VARIABLE_PROTECTED_SUFFIXES
            .iter()
            .any(|suffix| text.contains(suffix))
            || self.protected_text_paths().any(|path| {
                let raw = path.to_string_lossy();
                if !raw.is_empty() && text.contains(raw.as_ref()) {
                    return true;
                }
                std::fs::canonicalize(path)
                    .ok()
                    .map(|canonical| canonical.to_string_lossy().to_string())
                    .is_some_and(|canonical| !canonical.is_empty() && text.contains(&canonical))
            })
        {
            return true;
        }

        text.split(|character: char| {
            character.is_whitespace()
                || matches!(
                    character,
                    '\'' | '"' | '`' | ';' | '|' | '&' | '<' | '>' | '(' | ')' | '{' | '}'
                        | '[' | ']' | ','
                )
        })
        .filter_map(|fragment| {
            let fragment = fragment.trim_matches(|character| {
                matches!(character, '\'' | '"' | '`' | ':' | '=')
            });
            if fragment.is_empty() {
                return None;
            }
            Some(
                fragment
                    .rsplit_once('=')
                    .map_or(fragment, |(_, value)| value)
                    .strip_prefix("file://")
                    .unwrap_or_else(|| {
                        fragment
                            .rsplit_once('=')
                            .map_or(fragment, |(_, value)| value)
                    }),
            )
        })
        .any(|fragment| self.denies_path(Path::new(fragment)))
    }

    fn protected_text_paths(&self) -> impl Iterator<Item = &PathBuf> {
        std::iter::once(&self.skills_root).chain(self.external_authority_paths.iter())
    }
}

/// Test-only fail-safe fallback. Production registries must bind a validated
/// `SkillTrustAuthorityPaths` before registering tools.
impl Default for SkillTrustPathGuard {
    fn default() -> Self {
        Self::new(hermes_config::skills_dir(), Vec::new())
    }
}

fn normalize_absolute_path(path: &Path) -> Option<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    Some(normalized)
}

fn path_matches_protected(path: &Path, protected: &Path, recursive: bool) -> bool {
    let Some(lexical_path) = normalize_absolute_path(path) else {
        return false;
    };
    let Some(lexical_protected) = normalize_absolute_path(protected) else {
        return false;
    };
    let lexical_match = if recursive {
        lexical_path.starts_with(&lexical_protected)
    } else {
        lexical_path == lexical_protected
    };
    if lexical_match {
        return true;
    }

    let Some(resolved_path) = canonical_path_with_existing_ancestor(&lexical_path) else {
        return false;
    };
    let Some(resolved_protected) = canonical_path_with_existing_ancestor(&lexical_protected)
    else {
        return false;
    };
    if recursive {
        resolved_path.starts_with(resolved_protected)
    } else {
        resolved_path == resolved_protected
    }
}

fn skill_trust_effect_error(path: &Path, operation: &str) -> ToolError {
    ToolError::ExecutionFailed(format!(
        "{} denied: '{}' is verifier-only skill trust state",
        operation,
        path.display()
    ))
}
// ---------------------------------------------------------------------------
// CredentialGuard
// ---------------------------------------------------------------------------

/// Guards against reading or writing credential files and content containing secrets.
#[derive(Debug, Clone)]
pub struct CredentialGuard {
    /// Additional file name patterns to protect (case-insensitive glob).
    extra_protected_names: Vec<String>,
    /// If true, also block writes that contain detected secrets.
    scan_content: bool,
    /// Core-owned skill-store state. Production registry dispatch supplies
    /// the exact validated authority paths before a handler is invoked.
    skill_trust_paths: SkillTrustPathGuard,
}

impl CredentialGuard {
    /// Create a new guard with default protected patterns and content scanning enabled.
    pub fn new() -> Self {
        Self {
            extra_protected_names: Vec::new(),
            scan_content: true,
            skill_trust_paths: SkillTrustPathGuard::default(),
        }
    }

    /// Create a guard that only checks file paths, not content.
    pub fn path_only() -> Self {
        Self {
            extra_protected_names: Vec::new(),
            scan_content: false,
            skill_trust_paths: SkillTrustPathGuard::default(),
        }
    }

    /// Add an extra protected file name pattern.
    pub fn with_extra_protected_name(mut self, name: &str) -> Self {
        self.extra_protected_names.push(name.to_lowercase());
        self
    }

    /// Bind this guard to the exact runtime skills root and explicit external
    /// remote-trust-root/audit authority paths. No environment or in-root
    /// keyring fallback is consulted.
    pub fn with_skill_trust_protected_paths(
        mut self,
        skills_root: impl Into<PathBuf>,
        external_authority_paths: Vec<PathBuf>,
    ) -> Self {
        self.skill_trust_paths = SkillTrustPathGuard::new(skills_root, external_authority_paths);
        self
    }

    /// Check whether a file path refers to a protected credential file.
    pub fn is_protected_file(&self, path: &Path) -> bool {
        self.skill_trust_paths.denies_path(path)
            || is_protected_file_with_extra(path, &self.extra_protected_names)
    }

    /// Check raw terminal/code/plugin input before it can reach an effectful
    /// backend. This is independent of tool-policy mode and raw passthrough.
    pub(crate) fn check_effect_text(&self, text: &str) -> Result<(), ToolError> {
        if self.skill_trust_paths.denies_text(text) {
            return Err(ToolError::ExecutionFailed(
                "Effect denied: verifier-only skill trust state may only be accessed by typed core/CLI owner APIs"
                    .to_string(),
            ));
        }
        Ok(())
    }

    /// Check whether reading a file is allowed.
    ///
    /// Returns `Err(ToolError)` if the file is a protected credential file.
    pub fn check_read_access(&self, path: &Path) -> Result<(), ToolError> {
        if self.skill_trust_paths.denies_path(path) {
            return Err(skill_trust_effect_error(path, "Read"));
        }
        if self.is_protected_file(path) {
            return Err(ToolError::ExecutionFailed(format!(
                "Access denied: '{}' is a protected credential file",
                path.display()
            )));
        }
        Ok(())
    }

    /// Check whether writing to a file is allowed.
    ///
    /// Returns `Err(ToolError)` if the target is a protected file or if the
    /// content contains detectable secrets (when content scanning is enabled).
    pub fn check_write_access(&self, path: &Path, content: &str) -> Result<(), ToolError> {
        if self.skill_trust_paths.denies_path(path) {
            return Err(skill_trust_effect_error(path, "Write"));
        }
        if is_write_denied(path) {
            return Err(ToolError::ExecutionFailed(format!(
                "Write denied: '{}' is a protected system or credential path",
                path.display()
            )));
        }

        // Check path first
        if self.is_protected_file(path) {
            return Err(ToolError::ExecutionFailed(format!(
                "Write denied: '{}' is a protected credential file",
                path.display()
            )));
        }

        // Optionally scan content for secrets
        if self.scan_content {
            if let Some(detection) = detect_secrets(content) {
                return Err(ToolError::ExecutionFailed(format!(
                    "Write denied: content contains detected secret ({})",
                    detection
                )));
            }
        }

        Ok(())
    }
}

impl Default for CredentialGuard {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Standalone helper functions
// ---------------------------------------------------------------------------

/// Check whether a file path refers to a protected credential file
/// (using the built-in pattern list only).
pub fn is_protected_file(path: &Path) -> bool {
    CredentialGuard::path_only().is_protected_file(path)
}

/// Check whether a file path is write-denied even if it is not a credential
/// file by name. This covers sensitive system files, shell profiles, package
/// manager credential files, and profile-aware `$HERMES_HOME/.env`.
pub fn is_write_denied(path: &Path) -> bool {
    if SkillTrustPathGuard::default().denies_path(path) {
        return true;
    }
    is_write_denied_with_roots_and_safe_root(
        path,
        std::env::var_os("HOME").map(PathBuf::from).as_deref(),
        std::env::var_os("HERMES_HOME")
            .map(PathBuf::from)
            .as_deref(),
        std::env::var_os("HERMES_WRITE_SAFE_ROOT")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .as_deref(),
    )
}

fn expand_home(path: &Path, home: Option<&Path>) -> PathBuf {
    let raw = path.to_string_lossy();
    if raw == "~" {
        return home
            .map(Path::to_path_buf)
            .unwrap_or_else(|| path.to_path_buf());
    }
    if let Some(rest) = raw.strip_prefix("~/") {
        if let Some(home) = home {
            return home.join(rest);
        }
    }
    path.to_path_buf()
}

fn path_has_component_prefix(path: &Path, root: &Path, prefix: &str) -> bool {
    let rel = match path.strip_prefix(root) {
        Ok(rel) => rel,
        Err(_) => return false,
    };
    rel == Path::new(prefix) || rel.starts_with(Path::new(prefix))
}

/// Resolve a path through every existing ancestor without treating a missing
/// final path as a reason to skip symlink checks. The suffix after the closest
/// existing ancestor is appended only after that ancestor has been
/// canonicalized.
fn canonical_path_with_existing_ancestor(path: &Path) -> Option<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }

    let mut candidate = normalized;
    let mut unresolved_suffix = Vec::<OsString>::new();
    loop {
        match candidate.canonicalize() {
            Ok(mut canonical) => {
                for part in unresolved_suffix.iter().rev() {
                    canonical.push(part);
                }
                return Some(canonical);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if std::fs::symlink_metadata(&candidate)
                    .ok()
                    .is_some_and(|metadata| metadata.file_type().is_symlink())
                {
                    return None;
                }
                unresolved_suffix.push(candidate.file_name()?.to_os_string());
                candidate = candidate.parent()?.to_path_buf();
            }
            Err(_) => return None,
        }
    }
}

fn canonical_safe_root(path: &Path) -> Option<PathBuf> {
    let canonical = canonical_path_with_existing_ancestor(path)?;
    if canonical.is_dir() && canonical.exists() {
        Some(canonical)
    } else {
        None
    }
}

/// Root-injected form for deterministic tests and callers that already know
/// the active profile/home roots.
pub fn is_write_denied_with_roots(
    path: &Path,
    home: Option<&Path>,
    hermes_home: Option<&Path>,
) -> bool {
    is_write_denied_with_roots_and_safe_root(path, home, hermes_home, None)
}

/// Root-injected form including the optional `HERMES_WRITE_SAFE_ROOT` sandbox.
/// Static system and credential denies always win over the safe-root allowance.
pub fn is_write_denied_with_roots_and_safe_root(
    path: &Path,
    home: Option<&Path>,
    hermes_home: Option<&Path>,
    safe_root: Option<&Path>,
) -> bool {
    let expanded = expand_home(path, home);
    if let Some(hermes_home) = hermes_home {
        let skills_root = expand_home(hermes_home, home).join("skills");
        if SkillTrustPathGuard::new(skills_root, Vec::new()).denies_path(&expanded) {
            return true;
        }
    }
    // Static system paths are denied before canonicalization. Their parent may
    // legitimately be absent on a host, but that must not turn a deny into an
    // allow merely because resolution cannot proceed.
    let expanded_text = expanded.to_string_lossy();
    if WRITE_DENY_EXACT_ABSOLUTE
        .iter()
        .any(|candidate| expanded_text == *candidate)
        || WRITE_DENY_PREFIX_ABSOLUTE
            .iter()
            .any(|prefix| expanded_text == *prefix || expanded_text.starts_with(&format!("{prefix}/")))
    {
        return true;
    }
    let Some(resolved) = canonical_path_with_existing_ancestor(&expanded) else {
        // An unreadable, dangling-symlink, or otherwise unresolvable path
        // cannot be proven to remain under a trusted root.
        return true;
    };
    let normalized = resolved.to_string_lossy();

    if WRITE_DENY_EXACT_ABSOLUTE
        .iter()
        .any(|candidate| normalized == *candidate)
    {
        return true;
    }
    if WRITE_DENY_PREFIX_ABSOLUTE
        .iter()
        .any(|prefix| normalized == *prefix || normalized.starts_with(&format!("{prefix}/")))
    {
        return true;
    }

    if let Some(home) = home {
        let expanded_home = expand_home(home, Some(home));
        if let Some(canonical_home) = canonical_path_with_existing_ancestor(&expanded_home) {
            if let Ok(rel) = resolved.strip_prefix(&canonical_home) {
                let rel_str = rel.to_string_lossy();
                if WRITE_DENY_HOME_EXACT
                    .iter()
                    .any(|candidate| rel_str == *candidate)
                {
                    return true;
                }
                if WRITE_DENY_HOME_PREFIX
                    .iter()
                    .any(|prefix| path_has_component_prefix(&resolved, &canonical_home, prefix))
                {
                    return true;
                }
            }
        }
    }

    if let Some(hermes_home) = hermes_home {
        if let Some(canonical_hermes_home) =
            canonical_path_with_existing_ancestor(&expand_home(hermes_home, home))
        {
            if resolved == canonical_hermes_home.join(".env") {
                return true;
            }
        }
    }

    if let Some(safe_root) = safe_root {
        let expanded_safe_root = expand_home(safe_root, home);
        let Some(canonical_safe_root) = canonical_safe_root(&expanded_safe_root) else {
            return true;
        };
        if !resolved.starts_with(&canonical_safe_root) {
            return true;
        }
    }

    false
}

fn is_protected_file_with_extra(path: &Path, extra: &[String]) -> bool {
    let path_str = path.to_string_lossy();

    // Check directory components
    for component in path.components() {
        if let std::path::Component::Normal(os_str) = component {
            if let Some(s) = os_str.to_str() {
                let lower = s.to_lowercase();
                for &dir in PROTECTED_DIR_NAMES {
                    if lower == dir || path_str.contains(&format!("/{}/", dir)) {
                        return true;
                    }
                }
            }
        }
    }

    // Check file name
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        let lower = name.to_lowercase();

        // Exact matches
        for &protected in PROTECTED_FILE_NAMES {
            if lower == protected {
                return true;
            }
        }

        // Extension matches
        if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            let ext_lower = ext.to_lowercase();
            for &protected in PROTECTED_FILE_NAMES {
                if protected.starts_with('.') && &ext_lower == &protected[1..] {
                    return true;
                }
            }
        }

        // Substring matches for credential/secret
        if lower.contains("credential") || lower.contains("secret") || lower.contains("private_key")
        {
            return true;
        }

        // Extra patterns
        for pat in extra {
            if lower == *pat {
                return true;
            }
        }
    }

    false
}

/// Detect secrets in content. Returns the name of the first detected pattern, or None.
pub fn detect_secrets(content: &str) -> Option<&'static str> {
    for (name, regex) in SECRET_PATTERNS.iter() {
        if regex.is_match(content) {
            return Some(name);
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_protected_env_files() {
        assert!(is_protected_file(Path::new(".env")));
        assert!(is_protected_file(Path::new("/home/user/project/.env")));
        assert!(is_protected_file(Path::new(".env.local")));
        assert!(is_protected_file(Path::new(".env.production")));
    }

    #[test]
    fn test_protected_key_files() {
        assert!(is_protected_file(Path::new("id_rsa")));
        assert!(is_protected_file(Path::new("/home/user/.ssh/id_rsa")));
        assert!(is_protected_file(Path::new("server.key")));
        assert!(is_protected_file(Path::new("cert.pem")));
        assert!(is_protected_file(Path::new("keystore.p12")));
        assert!(is_protected_file(Path::new("keystore.jks")));
    }

    #[test]
    fn test_protected_directory() {
        assert!(is_protected_file(Path::new("/home/user/.ssh/config")));
        assert!(is_protected_file(Path::new(
            "/home/user/.gnupg/pubring.gpg"
        )));
        assert!(is_protected_file(Path::new("/home/user/.aws/credentials")));
    }

    #[test]
    fn test_protected_name_patterns() {
        assert!(is_protected_file(Path::new("credentials.json")));
        assert!(is_protected_file(Path::new("secrets.yaml")));
        assert!(is_protected_file(Path::new("service-account.json")));
        assert!(is_protected_file(Path::new("my_credentials")));
        assert!(is_protected_file(Path::new("app_secrets")));
    }

    #[test]
    fn test_not_protected() {
        assert!(!is_protected_file(Path::new("main.rs")));
        assert!(!is_protected_file(Path::new("Cargo.toml")));
        assert!(!is_protected_file(Path::new("README.md")));
        assert!(!is_protected_file(Path::new("src/app.rs")));
        assert!(!is_protected_file(Path::new("config.yaml")));
    }

    #[test]
    fn test_check_read_access_blocked() {
        let guard = CredentialGuard::new();
        assert!(guard.check_read_access(Path::new(".env")).is_err());
        assert!(guard
            .check_read_access(Path::new("/home/.ssh/id_rsa"))
            .is_err());
    }

    #[test]
    fn test_check_read_access_allowed() {
        let guard = CredentialGuard::new();
        assert!(guard.check_read_access(Path::new("main.rs")).is_ok());
        assert!(guard.check_read_access(Path::new("config.yaml")).is_ok());
    }

    #[test]
    fn test_check_write_access_blocked_path() {
        let guard = CredentialGuard::new();
        assert!(guard
            .check_write_access(Path::new(".env"), "hello")
            .is_err());
        assert!(guard
            .check_write_access(Path::new("/etc/passwd"), "root:x:0:0")
            .is_err());
    }

    #[test]
    fn test_check_write_access_blocked_content() {
        let guard = CredentialGuard::new();
        // Contains an API key pattern
        assert!(guard
            .check_write_access(
                Path::new("config.txt"),
                "OPENAI_API_KEY=sk-abc123def456ghi789jkl012mno345"
            )
            .is_err());
        // Contains a private key
        assert!(guard
            .check_write_access(
                Path::new("notes.txt"),
                "-----BEGIN PRIVATE KEY-----\nMIIEvgABCD\n-----END PRIVATE KEY-----"
            )
            .is_err());
    }

    #[test]
    fn test_check_write_access_allowed() {
        let guard = CredentialGuard::new();
        assert!(guard
            .check_write_access(Path::new("output.txt"), "Hello world")
            .is_ok());
        assert!(guard
            .check_write_access(Path::new("main.rs"), "fn main() {}")
            .is_ok());
    }

    #[test]
    fn test_path_only_guard() {
        let guard = CredentialGuard::path_only();
        // Path check still works
        assert!(guard.check_read_access(Path::new(".env")).is_err());
        // But content scanning is disabled
        assert!(guard
            .check_write_access(
                Path::new("output.txt"),
                "OPENAI_API_KEY=sk-abc123def456ghi789jkl012mno345"
            )
            .is_ok());
    }

    #[test]
    fn test_detect_secrets() {
        assert!(detect_secrets("sk-abc123def456ghi789jkl012mno345").is_some());
        assert!(detect_secrets("-----BEGIN PRIVATE KEY-----\nMIIEvg").is_some());
        assert!(detect_secrets("password = hunter2").is_some());
        assert!(detect_secrets("api_key = 1234567890abcdef").is_some());
        assert!(detect_secrets("AKIA1234567890ABCDEF").is_some());
        assert!(detect_secrets("123456789:ABCDEFGHIJKLMNOPQRSTUVWXYZabcd1234").is_some());
        assert!(detect_secrets("Hello, world!").is_none());
        assert!(detect_secrets("fn main() { println!(\"hello\"); }").is_none());
    }

    #[test]
    fn test_extra_protected_names() {
        let guard = CredentialGuard::new().with_extra_protected_name("my_custom_secret");
        assert!(guard.is_protected_file(Path::new("my_custom_secret")));
        assert!(guard.is_protected_file(Path::new(".env"))); // still works
    }

    #[test]
    fn test_case_insensitive() {
        assert!(is_protected_file(Path::new(".ENV")));
        assert!(is_protected_file(Path::new("Credentials.JSON")));
        assert!(is_protected_file(Path::new("ID_RSA")));
        assert!(is_protected_file(Path::new("Server.KEY")));
    }

    #[test]
    fn skill_trust_paths_are_denied_for_read_write_and_effect_text() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let hermes_home = tmp.path().join("hermes");
        let skills_root = hermes_home.join("skills");
        let lock = skills_root.join(".hub").join("lock.json");
        let pending = skills_root
            .join(".hub")
            .join("pending")
            .join("draft")
            .join("candidate.json");
        let package = skills_root
            .join(".packages")
            .join("local")
            .join("draft")
            .join("0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef")
            .join("SKILL.md");
        let ordinary_skill = skills_root.join("draft").join("SKILL.md");
        let remote_root = tmp.path().join("remote-trust-root.json");
        let remote_audit = tmp.path().join("remote-keyring-audit.json");
        for path in [&lock, &pending, &package, &ordinary_skill] {
            std::fs::create_dir_all(path.parent().expect("parent")).expect("parent");
            std::fs::write(path, "state").expect("state");
        }
        for path in [&remote_root, &remote_audit] {
            std::fs::write(path, "owner-only state").expect("authority state");
        }

        let guard = CredentialGuard::path_only().with_skill_trust_protected_paths(
            skills_root.clone(),
            vec![remote_root.clone(), remote_audit.clone()],
        );
        for path in [
            &lock,
            &pending,
            &package,
            &ordinary_skill,
            &remote_root,
            &remote_audit,
        ] {
            assert!(
                guard.check_read_access(path).is_err(),
                "read unexpectedly allowed for {}",
                path.display()
            );
            assert!(
                guard.check_write_access(path, "mutate").is_err(),
                "write unexpectedly allowed for {}",
                path.display()
            );
            assert!(
                guard
                    .check_effect_text(&format!("printf x > {}", path.display()))
                    .is_err(),
                "effect text unexpectedly allowed for {}",
                path.display()
            );
        }
        assert!(is_write_denied_with_roots(
            &lock,
            None,
            Some(&hermes_home)
        ));
        assert!(guard
            .check_write_access(&hermes_home.join("notes.txt"), "safe user data")
            .is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn skill_trust_path_guard_rejects_traversal_and_symlink_aliases() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tempdir");
        let skills_root = tmp.path().join("skills");
        let hub = skills_root.join(".hub");
        std::fs::create_dir_all(&hub).expect("hub");
        std::fs::write(hub.join("revocations.json"), "{}").expect("revocations");
        let alias = tmp.path().join("hub-alias");
        symlink(&hub, &alias).expect("hub symlink");
        let guard = CredentialGuard::path_only()
            .with_skill_trust_protected_paths(skills_root.clone(), Vec::new());

        let traversal = skills_root
            .join("scratch")
            .join("..")
            .join(".hub")
            .join("revocations.json");
        let alias_target = alias.join("revocations.json");
        for path in [&traversal, &alias_target] {
            assert!(
                guard.check_read_access(path).is_err(),
                "read unexpectedly allowed for {}",
                path.display()
            );
            assert!(
                guard.check_write_access(path, "mutate").is_err(),
                "write unexpectedly allowed for {}",
                path.display()
            );
        }
        assert!(guard
            .check_effect_text(&format!("cat {}", alias_target.display()))
            .is_err());
    }

    #[test]
    fn write_deny_matches_upstream_sensitive_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let hermes_home = tmp.path().join("profile");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&hermes_home).expect("profile");

        for path in ["/etc/shadow", "/etc/passwd", "/etc/sudoers"] {
            assert!(is_write_denied_with_roots(
                Path::new(path),
                Some(&home),
                Some(&hermes_home)
            ));
        }
        for path in [
            "/etc/hosts",
            "/private/etc/hosts",
            "/private/etc/passwd",
            "/private/etc/shadow",
            "/private/etc/sudoers",
        ] {
            assert!(is_write_denied_with_roots(
                Path::new(path),
                Some(&home),
                Some(&hermes_home)
            ));
        }
        assert!(is_write_denied_with_roots(
            Path::new("/etc/sudoers.d/custom"),
            Some(&home),
            Some(&hermes_home)
        ));
        assert!(is_write_denied_with_roots(
            Path::new("/etc/systemd/system/evil.service"),
            Some(&home),
            Some(&hermes_home)
        ));
        for path in [
            "/private/etc/ssh/sshd_config",
            "/private/etc/sudoers.d/custom",
            "/private/var/db/something",
            "/boot/grub/grub.cfg",
        ] {
            assert!(is_write_denied_with_roots(
                Path::new(path),
                Some(&home),
                Some(&hermes_home)
            ));
        }

        for rel in [
            ".ssh/authorized_keys",
            ".ssh/id_ed25519",
            ".aws/credentials",
            ".gnupg/secring.gpg",
            ".kube/config",
            ".docker/config.json",
            ".azure/config",
            ".config/gh/hosts.yml",
            ".netrc",
            ".bashrc",
            ".zshrc",
            ".profile",
            ".bash_profile",
            ".zprofile",
            ".npmrc",
            ".pypirc",
            ".pgpass",
        ] {
            assert!(is_write_denied_with_roots(
                &home.join(rel),
                Some(&home),
                Some(&hermes_home)
            ));
        }

        assert!(is_write_denied_with_roots(
            &hermes_home.join(".env"),
            Some(&home),
            Some(&hermes_home)
        ));
        assert!(!is_write_denied_with_roots(
            &hermes_home.join("config.yaml"),
            Some(&home),
            Some(&hermes_home)
        ));
        assert!(!is_write_denied_with_roots(
            Path::new("/tmp/safe_file.txt"),
            Some(&home),
            Some(&hermes_home)
        ));
        assert!(!is_write_denied_with_roots(
            Path::new("/home/user/project/main.py"),
            Some(&home),
            Some(&hermes_home)
        ));
    }

    #[test]
    fn write_deny_expands_tilde_against_home_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        std::fs::create_dir_all(&home).expect("home");

        assert!(is_write_denied_with_roots(
            Path::new("~/.ssh/id_rsa"),
            Some(&home),
            None
        ));
        assert!(is_write_denied_with_roots(
            Path::new("~/.netrc"),
            Some(&home),
            None
        ));
    }

    #[test]
    fn write_safe_root_sandboxes_non_static_paths() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let safe_root = tmp.path().join("workspace");
        let outside = tmp.path().join("other/file.txt");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&safe_root).expect("safe root");

        assert!(!is_write_denied_with_roots_and_safe_root(
            &safe_root,
            Some(&home),
            None,
            Some(&safe_root)
        ));
        assert!(!is_write_denied_with_roots_and_safe_root(
            &safe_root.join("subdir/file.txt"),
            Some(&home),
            None,
            Some(&safe_root)
        ));
        assert!(is_write_denied_with_roots_and_safe_root(
            &outside,
            Some(&home),
            None,
            Some(&safe_root)
        ));
        assert!(is_write_denied_with_roots_and_safe_root(
            &home.join(".ssh/id_rsa"),
            Some(&home),
            None,
            Some(&home)
        ));
    }

    #[test]
    fn write_safe_root_expands_tilde_and_ignores_absent_root() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let workspace = home.join("workspace");
        std::fs::create_dir_all(&workspace).expect("workspace");

        assert!(!is_write_denied_with_roots_and_safe_root(
            &workspace.join("file.txt"),
            Some(&home),
            None,
            Some(Path::new("~/workspace"))
        ));
        assert!(!is_write_denied_with_roots_and_safe_root(
            &tmp.path().join("regular.txt"),
            Some(&home),
            None,
            None
        ));
    }

    #[cfg(unix)]
    #[test]
    fn write_safe_root_rejects_symlink_and_missing_parent_escapes() {
        use std::os::unix::fs::symlink;

        let tmp = tempfile::tempdir().expect("tempdir");
        let home = tmp.path().join("home");
        let safe_root = tmp.path().join("workspace");
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&home).expect("home");
        std::fs::create_dir_all(&safe_root).expect("safe root");
        std::fs::create_dir_all(&outside).expect("outside");

        symlink(&outside, safe_root.join("escape")).expect("escape symlink");
        symlink(outside.join("missing"), safe_root.join("dangling"))
            .expect("dangling symlink");

        assert!(is_write_denied_with_roots_and_safe_root(
            &safe_root.join("escape/new/file.txt"),
            Some(&home),
            None,
            Some(&safe_root)
        ));
        assert!(is_write_denied_with_roots_and_safe_root(
            &safe_root.join("dangling/new/file.txt"),
            Some(&home),
            None,
            Some(&safe_root)
        ));
        assert!(!is_write_denied_with_roots_and_safe_root(
            &safe_root.join("new/file.txt"),
            Some(&home),
            None,
            Some(&safe_root)
        ));
    }
}
