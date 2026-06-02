use async_trait::async_trait;
use hermes_core::errors::AgentError;
use serde::Deserialize;
use std::process::Command;
use crate::update::github::{ReleaseInfo, ReleaseSource};
use crate::update::platform::Platform;

/// ModelScope Release 源
pub struct ModelScopeSource {
    pub repo: String,       // "flowy2025/agent"
    pub prefix: String,     // "hermes-agent-ultra"
}

impl Default for ModelScopeSource {
    fn default() -> Self {
        Self::new()
    }
}

impl ModelScopeSource {
    pub fn new() -> Self {
        let repo = std::env::var("HERMES_MODELSCOPE_REPO")
            .unwrap_or_else(|_| "flowy2025/agent".to_string());
        Self {
            repo,
            prefix: "hermes-agent-ultra".to_string(),
        }
    }

    /// URL to fetch latest.json
    fn latest_json_url(&self) -> String {
        format!(
            "https://modelscope.cn/api/v1/datasets/{}/repo?Revision=master&FilePath={}/latest.json",
            self.repo, self.prefix
        )
    }

    /// URL to download a specific file
    fn file_download_url(&self, version_tag: &str, filename: &str) -> String {
        format!(
            "https://modelscope.cn/api/v1/datasets/{}/repo?Revision=master&FilePath={}/{}/{}",
            self.repo, self.prefix, version_tag, filename
        )
    }
}

/// Use system curl (schannel on Windows) to fetch content from ModelScope.
/// Unlike the github variant, this does NOT send a GITHUB_TOKEN header.
fn curl_get(url: &str) -> Result<String, AgentError> {
    let mut cmd = Command::new("curl");
    cmd.args([
        "-sSfL",
        "-H", "User-Agent: hermes-agent-ultra",
    ]);
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

#[derive(Deserialize)]
struct LatestJson {
    version: String,
    tag: String,
    artifacts: Vec<ArtifactEntry>,
}

#[derive(Deserialize)]
struct ArtifactEntry {
    name: String,
    #[allow(dead_code)]
    url: Option<String>,
}

#[async_trait]
impl ReleaseSource for ModelScopeSource {
    fn name(&self) -> &str {
        "ModelScope"
    }

    async fn fetch_latest(&self, platform: &Platform) -> Result<ReleaseInfo, AgentError> {
        let body = curl_get(&self.latest_json_url())
            .map_err(|e| AgentError::Io(format!("Failed to fetch latest.json from ModelScope: {e}")))?;

        let latest: LatestJson = serde_json::from_str(&body)
            .map_err(|e| AgentError::Io(format!("Failed to parse latest.json: {e}")))?;

        let artifact_name = platform.artifact_name();
        let has_artifact = latest.artifacts.iter().any(|a| a.name == artifact_name);
        if !has_artifact {
            return Err(AgentError::Io(format!(
                "No artifact '{}' found in ModelScope release {}",
                artifact_name, latest.tag
            )));
        }

        let artifact_url = self.file_download_url(&latest.tag, &artifact_name);

        let checksum_url = if latest.artifacts.iter().any(|a| a.name == "checksums.sha256") {
            Some(self.file_download_url(&latest.tag, "checksums.sha256"))
        } else {
            None
        };

        Ok(ReleaseInfo {
            version: latest.version,
            tag: latest.tag,
            artifact_url,
            checksum_url,
            release_notes: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_modelscope_source_default_repo() {
        // SAFETY: test-only env manipulation; tests run single-threaded for env vars
        unsafe { std::env::remove_var("HERMES_MODELSCOPE_REPO") };
        let source = ModelScopeSource::new();
        assert_eq!(source.repo, "flowy2025/agent");
        assert_eq!(source.prefix, "hermes-agent-ultra");
    }

    #[test]
    fn test_modelscope_source_custom_repo() {
        // SAFETY: test-only env manipulation; tests run single-threaded for env vars
        unsafe { std::env::set_var("HERMES_MODELSCOPE_REPO", "myorg/myrepo") };
        let source = ModelScopeSource::new();
        assert_eq!(source.repo, "myorg/myrepo");
        assert_eq!(source.prefix, "hermes-agent-ultra");
        // Cleanup
        unsafe { std::env::remove_var("HERMES_MODELSCOPE_REPO") };
    }

    #[test]
    fn test_latest_json_url() {
        let source = ModelScopeSource {
            repo: "flowy2025/agent".to_string(),
            prefix: "hermes-agent-ultra".to_string(),
        };
        assert_eq!(
            source.latest_json_url(),
            "https://modelscope.cn/api/v1/datasets/flowy2025/agent/repo?Revision=master&FilePath=hermes-agent-ultra/latest.json"
        );
    }

    #[test]
    fn test_file_download_url() {
        let source = ModelScopeSource {
            repo: "flowy2025/agent".to_string(),
            prefix: "hermes-agent-ultra".to_string(),
        };
        assert_eq!(
            source.file_download_url("v0.1.0", "hermes-linux-x86_64.tar.gz"),
            "https://modelscope.cn/api/v1/datasets/flowy2025/agent/repo?Revision=master&FilePath=hermes-agent-ultra/v0.1.0/hermes-linux-x86_64.tar.gz"
        );
    }
}
