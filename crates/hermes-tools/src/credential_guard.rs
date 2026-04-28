//! Credential file protection — prevents tools from reading or writing
//! sensitive credential files, and scans content for leaked secrets.
//!
//! Requirement 22.3

use std::path::Path;
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
}

impl CredentialGuard {
    /// Create a new guard with default protected patterns and content scanning enabled.
    pub fn new() -> Self {
        Self {
            extra_protected_names: Vec::new(),
            scan_content: true,
        }
    }

    /// Create a guard that only checks file paths, not content.
    pub fn path_only() -> Self {
        Self {
            extra_protected_names: Vec::new(),
            scan_content: false,
        }
    }

    /// Add an extra protected file name pattern.
    pub fn with_extra_protected_name(mut self, name: &str) -> Self {
        self.extra_protected_names.push(name.to_lowercase());
        self
    }

    /// Check whether a file path refers to a protected credential file.
    pub fn is_protected_file(&self, path: &Path) -> bool {
        is_protected_file_with_extra(path, &self.extra_protected_names)
    }

    /// Check whether reading a file is allowed.
    ///
    /// Returns `Err(ToolError)` if the file is a protected credential file.
    pub fn check_read_access(&self, path: &Path) -> Result<(), ToolError> {
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
    is_protected_file_with_extra(path, &[])
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
}
