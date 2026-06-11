//! skills.sh registry resolution and search.

use hermes_core::AgentError;

use super::constants::SKILLS_SH_SEARCH_URL;
use super::github::github_default_branch;
use super::parse::parse_explicit_github_skill;
use super::registry::resolve_skill_via_registry_index;
use super::taps::resolve_skill_in_repo;
use super::types::{RegistryInstallSource, ResolvedSkillSource, SkillsShSearchResponse};

// ---------------------------------------------------------------------------
// Skills.sh resolution
// ---------------------------------------------------------------------------

pub(crate) async fn resolve_skills_sh_source(
    client: &reqwest::Client,
    requested: &str,
) -> Result<ResolvedSkillSource, AgentError> {
    let req = requested.trim().trim_matches('/');
    if req.is_empty() {
        return Err(AgentError::Config(
            "Missing skills.sh skill identifier.".to_string(),
        ));
    }
    if let Some((repo, _, skill_dir)) = parse_explicit_github_skill(req) {
        let branch = github_default_branch(client, &repo).await?;
        return Ok(ResolvedSkillSource {
            repo,
            branch,
            skill_dir,
        });
    }

    if let Ok(resolved) = resolve_skill_via_registry_index(client, req, Some("skills.sh")).await {
        if let RegistryInstallSource::GitHub(source) = resolved.install_source {
            let branch = github_default_branch(client, &source.repo).await?;
            return Ok(ResolvedSkillSource { branch, ..source });
        }
    }

    let search_resp = client
        .get(SKILLS_SH_SEARCH_URL)
        .query(&[("q", req), ("limit", "20")])
        .header("Accept", "application/json")
        .header("User-Agent", "hermes-agent-ultra")
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("skills.sh search request failed: {}", e)))?;
    if !search_resp.status().is_success() {
        let status = search_resp.status();
        let body = search_resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "skills.sh search failed ({}): {}",
            status, body
        )));
    }
    let payload = search_resp
        .json::<SkillsShSearchResponse>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid skills.sh payload: {}", e)))?;
    let req_l = req.to_ascii_lowercase();
    for hit in payload.skills {
        let source = hit.source.trim();
        if source.is_empty() {
            continue;
        }
        let skill_id = if hit.skill_id.trim().is_empty() {
            hit.name.trim().to_string()
        } else {
            hit.skill_id.trim().to_string()
        };
        let repo = source.to_string();
        let branch = github_default_branch(client, &repo).await?;
        if let Ok(found) = resolve_skill_in_repo(client, &repo, &skill_id, Some("skills")).await {
            return Ok(found);
        }
        if let Ok(found) = resolve_skill_in_repo(client, &repo, &req_l, Some("skills")).await {
            return Ok(found);
        }
        if let Some((repo2, _, dir)) = parse_explicit_github_skill(&hit.id) {
            return Ok(ResolvedSkillSource {
                repo: repo2,
                branch,
                skill_dir: dir,
            });
        }
    }

    Err(AgentError::Config(format!(
        "Unable to resolve skills.sh skill '{}'.",
        requested
    )))
}

pub(crate) async fn search_skills_sh_registry(
    client: &reqwest::Client,
    query: &str,
    limit: usize,
) -> Result<Vec<(String, String)>, AgentError> {
    let capped_limit = limit.clamp(1, 50).to_string();
    let search_resp = client
        .get(SKILLS_SH_SEARCH_URL)
        .query(&[("q", query), ("limit", capped_limit.as_str())])
        .header("Accept", "application/json")
        .header("User-Agent", "hermes-agent-ultra")
        .timeout(std::time::Duration::from_secs(20))
        .send()
        .await
        .map_err(|e| AgentError::Config(format!("skills.sh search request failed: {}", e)))?;
    if !search_resp.status().is_success() {
        let status = search_resp.status();
        let body = search_resp.text().await.unwrap_or_default();
        return Err(AgentError::Config(format!(
            "skills.sh search failed ({}): {}",
            status, body
        )));
    }
    let payload = search_resp
        .json::<SkillsShSearchResponse>()
        .await
        .map_err(|e| AgentError::Config(format!("Invalid skills.sh payload: {}", e)))?;

    let mut seen = std::collections::BTreeSet::new();
    let mut out = Vec::new();
    for hit in payload.skills {
        let id = hit.id.trim();
        if id.is_empty() {
            continue;
        }
        let identifier = format!("skills.sh/{}", id);
        if !seen.insert(identifier.clone()) {
            continue;
        }
        let display_name = if hit.name.trim().is_empty() {
            id.to_string()
        } else {
            hit.name.trim().to_string()
        };
        out.push((display_name, identifier));
    }
    Ok(out)
}
