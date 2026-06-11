//! GitHub API helpers and skill file fetching.

use bytes::Bytes;
use hermes_core::AgentError;

use super::constants::GITHUB_API_BASE;
use super::parse::ensure_safe_relative_path;
use super::types::{GitHubRepoInfo, GitHubTreeEntry, GitHubTreeResponse, ResolvedSkillSource};

pub(crate) fn github_request(
    client: &reqwest::Client,
    url: &str,
    accept: &str,
) -> reqwest::RequestBuilder {
    let mut req = client
        .get(url)
        .header("Accept", accept)
        .header("User-Agent", "hermes-agent-ultra");
    if let Ok(token) = std::env::var("GITHUB_TOKEN")
        .or_else(|_| std::env::var("GH_TOKEN"))
        .map(|v| v.trim().to_string())
    {
        if !token.is_empty() {
            req = req.bearer_auth(token);
        }
    }
    req
}

pub(crate) async fn github_default_branch(
    client: &reqwest::Client,
    repo: &str,
) -> Result<String, AgentError> {
    let url = format!("{}/repos/{}", GITHUB_API_BASE, repo);
    let resp = github_request(client, &url, "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("GitHub request failed for {}: {}", repo, e)))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "GitHub repo lookup failed for {} ({}): {}",
            repo, status, body
        )));
    }
    let payload = resp
        .json::<GitHubRepoInfo>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid GitHub repo response: {}", e)))?;
    Ok(payload.default_branch)
}

pub(crate) async fn github_repo_tree(
    client: &reqwest::Client,
    repo: &str,
    branch: &str,
) -> Result<Vec<GitHubTreeEntry>, AgentError> {
    let encoded_branch = urlencoding::encode(branch);
    let url = format!(
        "{}/repos/{}/git/trees/{}?recursive=1",
        GITHUB_API_BASE, repo, encoded_branch
    );
    let resp = github_request(client, &url, "application/vnd.github+json")
        .timeout(std::time::Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("GitHub tree request failed: {}", e)))?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "GitHub tree lookup failed for {repo}@{branch} ({status}): {body}"
        )));
    }
    let payload = resp
        .json::<GitHubTreeResponse>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid GitHub tree response: {}", e)))?;
    Ok(payload.tree)
}

pub(crate) async fn fetch_skill_files_from_github(
    client: &reqwest::Client,
    source: &ResolvedSkillSource,
) -> Result<Vec<(String, Bytes)>, AgentError> {
    let tree = github_repo_tree(client, &source.repo, &source.branch).await?;
    let prefix = format!("{}/", source.skill_dir.trim_matches('/'));
    let mut files = Vec::new();

    for entry in tree {
        if entry.kind != "blob" || !entry.path.starts_with(&prefix) {
            continue;
        }
        let rel_path = entry.path[prefix.len()..].to_string();
        ensure_safe_relative_path(&rel_path)?;
        let raw_path = entry
            .path
            .split('/')
            .map(|segment| urlencoding::encode(segment).to_string())
            .collect::<Vec<_>>()
            .join("/");
        let raw_url = format!(
            "https://raw.githubusercontent.com/{}/{}/{}",
            source.repo, source.branch, raw_path
        );
        let bytes = match client
            .get(&raw_url)
            .header("User-Agent", "hermes-agent-ultra")
            .timeout(std::time::Duration::from_secs(25))
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => resp
                .bytes()
                .await
                .map_err(|e| AgentError::Config(format!("Invalid file payload: {}", e)))?,
            _ => {
                let encoded_path = entry
                    .path
                    .split('/')
                    .map(urlencoding::encode)
                    .collect::<Vec<_>>()
                    .join("/");
                let api_url = format!(
                    "{}/repos/{}/contents/{}?ref={}",
                    GITHUB_API_BASE,
                    source.repo,
                    encoded_path,
                    urlencoding::encode(&source.branch)
                );
                let resp = github_request(client, &api_url, "application/vnd.github.v3.raw")
                    .timeout(std::time::Duration::from_secs(25))
                    .send()
                    .await
                    .map_err(|e| {
                        AgentError::Config(format!("GitHub file download failed: {}", e))
                    })?;
                if !resp.status().is_success() {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    return Err(AgentError::Config(format!(
                        "Failed to download {} from {} ({}): {}",
                        rel_path, source.repo, status, body
                    )));
                }
                resp.bytes()
                    .await
                    .map_err(|e| AgentError::Config(format!("Invalid file payload: {}", e)))?
            }
        };
        files.push((rel_path, bytes));
    }

    if !files.iter().any(|(path, _)| path == "SKILL.md") {
        return Err(AgentError::Config(format!(
            "Resolved source {}/{} is missing SKILL.md",
            source.repo, source.skill_dir
        )));
    }
    if files.is_empty() {
        return Err(AgentError::Config(format!(
            "No files found at {}/{}",
            source.repo, source.skill_dir
        )));
    }
    Ok(files)
}
