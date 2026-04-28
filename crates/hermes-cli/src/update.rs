//! Release version check against GitHub.

use hermes_core::AgentError;
use serde::Deserialize;

const DEFAULT_REPO: &str = "sheawinkler/hermes-agent-ultra";

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    name: Option<String>,
}

fn normalize_version(tag: &str) -> String {
    tag.trim().trim_start_matches('v').to_string()
}

/// Compare `tag_name` from GitHub with this binary's `CARGO_PKG_VERSION`.
pub async fn check_for_updates() -> Result<String, AgentError> {
    let repo = std::env::var("HERMES_UPDATE_REPO").unwrap_or_else(|_| DEFAULT_REPO.to_string());
    let url = format!("https://api.github.com/repos/{}/releases/latest", repo);

    let client = reqwest::Client::builder()
        .user_agent(format!("hermes-agent-ultra/{}", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| AgentError::Io(e.to_string()))?;

    let resp = client
        .get(&url)
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| AgentError::Io(format!("Update check HTTP error: {e}")))?;

    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(format!(
            "No published GitHub release found for repository '{}'.\n\
             Override with HERMES_UPDATE_REPO if needed.",
            repo
        ));
    }

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Ok(format!(
            "Could not reach GitHub releases for '{}': {} — {}\n\
             Set HERMES_UPDATE_REPO if your fork uses a different path.",
            repo, status, body
        ));
    }

    let rel: GitHubRelease = resp
        .json()
        .await
        .map_err(|e| AgentError::Io(format!("Update check parse error: {e}")))?;

    let remote = normalize_version(&rel.tag_name);
    let current = env!("CARGO_PKG_VERSION").to_string();

    if remote == current {
        Ok(format!(
            "Hermes v{} is up to date with latest GitHub release {} ({}).",
            current, rel.tag_name, rel.html_url
        ))
    } else {
        Ok(format!(
            "Hermes local version: {}\n\
             Latest GitHub release: {} ({}) — {}\n\
             See {} for release notes and install instructions.",
            current,
            rel.tag_name,
            rel.name.as_deref().unwrap_or(""),
            rel.html_url,
            rel.html_url
        ))
    }
}
