//! Official Hermes skill source resolution.

use hermes_core::AgentError;

use super::constants::OFFICIAL_SKILLS_REPO;
use super::github::{github_default_branch, github_repo_tree};
use super::registry::resolve_skill_via_registry_index;
use super::types::{RegistryInstallSource, ResolvedSkillSource};

// ---------------------------------------------------------------------------
// Official skill source resolution
// ---------------------------------------------------------------------------

pub(crate) async fn resolve_official_skill_source(
    client: &reqwest::Client,
    requested: &str,
) -> Result<ResolvedSkillSource, AgentError> {
    let req = requested.trim().trim_matches('/');
    if req.is_empty() {
        return Err(AgentError::Config(
            "Missing official skill identifier (e.g., official/security/1password).".to_string(),
        ));
    }

    let normalized = canonicalize_official_skill_dir(req.trim_start_matches("official/"));
    if normalized.is_empty() {
        return Err(AgentError::Config(
            "Missing official skill identifier (e.g., official/security/1password).".to_string(),
        ));
    }

    let branch = github_default_branch(client, OFFICIAL_SKILLS_REPO).await?;
    let tree = github_repo_tree(client, OFFICIAL_SKILLS_REPO, &branch).await?;
    let has_skill_dir = |dir: &str| -> bool {
        let target = format!("{}/SKILL.md", dir.trim_matches('/'));
        tree.iter()
            .any(|entry| entry.kind == "blob" && entry.path == target)
    };

    let mut candidate_queries = vec![
        req.to_string(),
        normalized.clone(),
        format!("official/{}", normalized),
    ];
    let basename = normalized
        .split('/')
        .next_back()
        .unwrap_or(normalized.as_str())
        .to_string();
    if !basename.is_empty() {
        candidate_queries.push(basename);
    }
    candidate_queries.sort();
    candidate_queries.dedup();

    for query in candidate_queries {
        if let Ok(record) = resolve_skill_via_registry_index(client, &query, Some("official")).await
        {
            if let RegistryInstallSource::GitHub(source) = record.install_source {
                let mut candidates = official_skill_path_candidates(&source.skill_dir);
                for c in official_skill_path_candidates(&normalized) {
                    if !candidates.iter().any(|existing| existing == &c) {
                        candidates.push(c);
                    }
                }
                for candidate in candidates {
                    if has_skill_dir(&candidate) {
                        return Ok(ResolvedSkillSource {
                            repo: OFFICIAL_SKILLS_REPO.to_string(),
                            branch: branch.clone(),
                            skill_dir: candidate,
                        });
                    }
                }
            }
        }
    }

    for candidate in official_skill_path_candidates(&normalized) {
        if has_skill_dir(&candidate) {
            return Ok(ResolvedSkillSource {
                repo: OFFICIAL_SKILLS_REPO.to_string(),
                branch: branch.clone(),
                skill_dir: candidate,
            });
        }
    }

    Err(AgentError::Config(format!(
        "Official skill '{}' not found in upstream skills or optional-skills catalogs.",
        requested
    )))
}

pub(crate) fn canonicalize_official_skill_dir(path: &str) -> String {
    path.trim().trim_matches('/').to_string()
}

pub(crate) fn official_skill_path_candidates(path_like: &str) -> Vec<String> {
    let normalized = canonicalize_official_skill_dir(path_like);
    if normalized.is_empty() {
        return Vec::new();
    }
    if normalized.starts_with("skills/") || normalized.starts_with("optional-skills/") {
        return vec![normalized];
    }
    vec![
        format!("skills/{}", normalized),
        format!("optional-skills/{}", normalized),
    ]
}
