//! Skill bundle fetch and filesystem install.

use bytes::Bytes;
use hermes_core::AgentError;

use super::claude_marketplace::resolve_claude_marketplace_skill;
use super::clawhub::fetch_clawhub_skill_files;
use super::github::{fetch_skill_files_from_github, github_default_branch};
use super::hub_state::skill_guard_enforce_bundle;
use super::lobehub::fetch_lobehub_skill_files;
use super::official::resolve_official_skill_source;
use super::parse::{
    canonicalize_skills_sh_identifier, ensure_safe_relative_path, parse_explicit_github_skill,
    parse_repo_skill_identifier,
};
use super::registry::resolve_skill_via_registry_index;
use super::skills_sh::resolve_skills_sh_source;
use super::taps::{resolve_skill_in_repo, resolve_skill_via_taps};
use super::types::{RegistryInstallSource, ResolvedSkillSource, SkillHubInstalledEntry};

// ---------------------------------------------------------------------------
// Install / Exec
// ---------------------------------------------------------------------------

pub(crate) async fn fetch_bundle_for_lock_entry(
    client: &reqwest::Client,
    entry: &SkillHubInstalledEntry,
    taps: &[String],
) -> Result<Vec<(String, Bytes)>, AgentError> {
    match entry.source.as_str() {
        "official" => {
            let resolved = resolve_official_skill_source(client, &entry.identifier).await?;
            fetch_skill_files_from_github(client, &resolved).await
        }
        "skills.sh" | "skills-sh" => {
            let id = canonicalize_skills_sh_identifier(&entry.identifier);
            let resolved = resolve_skills_sh_source(client, &id).await?;
            fetch_skill_files_from_github(client, &resolved).await
        }
        "lobehub" => fetch_lobehub_skill_files(client, &entry.identifier).await,
        "clawhub" => fetch_clawhub_skill_files(client, &entry.identifier, None).await,
        "claude-marketplace" => {
            let resolved = resolve_claude_marketplace_skill(client, &entry.identifier).await?;
            fetch_skill_files_from_github(client, &resolved).await
        }
        "tap" => {
            if let Some((repo, skill_dir)) = parse_repo_skill_identifier(&entry.identifier) {
                let branch = github_default_branch(client, &repo).await?;
                return fetch_skill_files_from_github(
                    client,
                    &ResolvedSkillSource {
                        repo,
                        branch,
                        skill_dir,
                    },
                )
                .await;
            }
            let resolved = resolve_skill_via_taps(client, taps, &entry.name).await?;
            fetch_skill_files_from_github(client, &resolved).await
        }
        "github" => {
            if let Some((repo, maybe_branch, skill_dir)) =
                parse_explicit_github_skill(&entry.identifier)
            {
                let branch = if let Some(branch) = maybe_branch {
                    branch
                } else {
                    github_default_branch(client, &repo).await?
                };
                return fetch_skill_files_from_github(
                    client,
                    &ResolvedSkillSource {
                        repo,
                        branch,
                        skill_dir,
                    },
                )
                .await;
            }
            if let Some((repo, skill_dir)) = parse_repo_skill_identifier(&entry.identifier) {
                let branch = github_default_branch(client, &repo).await?;
                return fetch_skill_files_from_github(
                    client,
                    &ResolvedSkillSource {
                        repo,
                        branch,
                        skill_dir,
                    },
                )
                .await;
            }
            let resolved =
                resolve_skill_in_repo(client, &entry.identifier, &entry.name, None).await?;
            fetch_skill_files_from_github(client, &resolved).await
        }
        other => {
            if let Ok(hit) =
                resolve_skill_via_registry_index(client, &entry.identifier, Some(other)).await
            {
                return match hit.install_source {
                    RegistryInstallSource::GitHub(source) => {
                        let branch = github_default_branch(client, &source.repo).await?;
                        fetch_skill_files_from_github(
                            client,
                            &ResolvedSkillSource { branch, ..source },
                        )
                        .await
                    }
                    RegistryInstallSource::LobeHub { slug } => {
                        fetch_lobehub_skill_files(client, &slug).await
                    }
                    RegistryInstallSource::ClawHub { slug, version } => {
                        fetch_clawhub_skill_files(client, &slug, version.as_deref()).await
                    }
                };
            }
            Err(AgentError::Config(format!(
                "Unknown hub source '{}' for installed skill '{}'",
                entry.source, entry.name
            )))
        }
    }
}

pub(crate) fn install_skill_files(
    skills_dir: &std::path::Path,
    install_name: &str,
    files: &[(String, Bytes)],
    source: &str,
    force: bool,
) -> Result<std::path::PathBuf, AgentError> {
    skill_guard_enforce_bundle(install_name, source, files, force)?;

    std::fs::create_dir_all(skills_dir)
        .map_err(|e| AgentError::Io(format!("Failed to create skills dir: {}", e)))?;

    let target = skills_dir.join(install_name);
    if target.exists() {
        std::fs::remove_dir_all(&target)
            .map_err(|e| AgentError::Io(format!("Failed to remove existing skill dir: {}", e)))?;
    }
    std::fs::create_dir_all(&target)
        .map_err(|e| AgentError::Io(format!("Failed to create skill dir: {}", e)))?;

    for (rel_path, bytes) in files {
        ensure_safe_relative_path(rel_path)?;
        let output = target.join(rel_path);
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| AgentError::Io(format!("Failed to create parent dirs: {}", e)))?;
        }
        std::fs::write(&output, bytes)
            .map_err(|e| AgentError::Io(format!("Failed to write {}: {}", output.display(), e)))?;
    }

    let skill_md = target.join("SKILL.md");
    if !skill_md.exists() {
        return Err(AgentError::Config(format!(
            "Installed skill is missing SKILL.md at {}",
            skill_md.display()
        )));
    }

    Ok(target)
}
