use async_trait::async_trait;
use hermes_core::errors::AgentError;
use serde::Deserialize;
use std::process::Command;
use crate::update::platform::Platform;

/// Release 信息
pub struct ReleaseInfo {
    pub version: String,
    pub tag: String,
    pub artifact_url: String,
    pub checksum_url: Option<String>,
    pub release_notes: Option<String>,
}

/// Release 源抽象
#[async_trait]
pub trait ReleaseSource: Send + Sync {
    fn name(&self) -> &str;
    async fn fetch_latest(&self, platform: &Platform) -> Result<ReleaseInfo, AgentError>;
}

/// GitHub Release 源
pub struct GitHubSource {
    pub repo: String,
}

impl GitHubSource {
    pub fn new() -> Self {
        let repo = std::env::var("HERMES_UPDATE_REPO")
            .unwrap_or_else(|_| "Michael-Lfx/hermes-agent-ultra".to_string());
        Self { repo }
    }

    fn api_url(&self) -> String {
        format!("https://api.github.com/repos/{}/releases/latest", self.repo)
    }

    #[allow(dead_code)]
    fn download_base_url(&self, tag: &str) -> String {
        format!("https://github.com/{}/releases/download/{}", self.repo, tag)
    }
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    body: Option<String>,
    assets: Vec<GitHubAsset>,
}

#[derive(Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

/// Use system curl (schannel on Windows) to bypass rustls TLS issues
/// with corporate VPN/proxy certificates.
fn curl_get(url: &str) -> Result<String, AgentError> {
    let mut cmd = Command::new("curl");
    cmd.args([
        "-sSfL",
        "-H", "User-Agent: hermes-agent-ultra",
    ]);
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        cmd.args(["-H", &format!("Authorization: Bearer {token}")]);
    }
    cmd.arg(url);

    let output = cmd.output()
        .map_err(|e| AgentError::Io(format!("Failed to run curl: {e}")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(AgentError::Io(format!("curl failed: {stderr}")));
    }

    String::from_utf8(output.stdout)
        .map_err(|e| AgentError::Io(format!("Invalid UTF-8 in response: {e}")))
}

#[async_trait]
impl ReleaseSource for GitHubSource {
    fn name(&self) -> &str {
        "GitHub"
    }

    async fn fetch_latest(&self, platform: &Platform) -> Result<ReleaseInfo, AgentError> {
        let body = curl_get(&self.api_url())
            .map_err(|e| AgentError::Io(format!("Failed to fetch release info: {e}")))?;

        let release: GitHubRelease = serde_json::from_str(&body)
            .map_err(|e| AgentError::Io(format!("Failed to parse release JSON: {e}")))?;

        let artifact_name = platform.artifact_name();
        let artifact_url = release
            .assets
            .iter()
            .find(|a| a.name == artifact_name)
            .map(|a| a.browser_download_url.clone())
            .ok_or_else(|| {
                AgentError::Io(format!(
                    "No artifact '{}' found in release {}", artifact_name, release.tag_name
                ))
            })?;

        let checksum_url = release
            .assets
            .iter()
            .find(|a| a.name == "checksums.sha256")
            .map(|a| a.browser_download_url.clone());

        let version = release.tag_name.trim_start_matches('v').to_string();

        Ok(ReleaseInfo {
            version,
            tag: release.tag_name,
            artifact_url,
            checksum_url,
            release_notes: release.body,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_github_source_default_repo() {
        // Clear env var to test default
        // SAFETY: test-only env manipulation; tests run single-threaded for env vars
        unsafe { std::env::remove_var("HERMES_UPDATE_REPO") };
        let source = GitHubSource::new();
        assert_eq!(source.repo, "Michael-Lfx/hermes-agent-ultra");
    }

    #[test]
    fn test_github_source_custom_repo() {
        // SAFETY: test-only env manipulation; tests run single-threaded for env vars
        unsafe { std::env::set_var("HERMES_UPDATE_REPO", "myorg/myrepo") };
        let source = GitHubSource::new();
        assert_eq!(source.repo, "myorg/myrepo");
        // Cleanup
        unsafe { std::env::remove_var("HERMES_UPDATE_REPO") };
    }

    #[test]
    fn test_api_url() {
        let source = GitHubSource { repo: "owner/repo".to_string() };
        assert_eq!(source.api_url(), "https://api.github.com/repos/owner/repo/releases/latest");
    }

    #[test]
    fn test_download_base_url() {
        let source = GitHubSource { repo: "owner/repo".to_string() };
        assert_eq!(
            source.download_base_url("v1.2.3"),
            "https://github.com/owner/repo/releases/download/v1.2.3"
        );
    }
}
