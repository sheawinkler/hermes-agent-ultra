use std::path::PathBuf;
use std::process::Stdio;

use bytes::Bytes;
use hermes_core::AgentError;
use regex::Regex;

use crate::App;
use crate::commands::{CommandResult, emit_command_output, skills_infra};

// ---------------------------------------------------------------------------
// Skills execution tier
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SkillsExecutionTier {
    Trusted,
    Balanced,
    Open,
}

impl SkillsExecutionTier {
    pub(crate) fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "trusted" => Some(Self::Trusted),
            "balanced" => Some(Self::Balanced),
            "open" | "permissive" => Some(Self::Open),
            _ => None,
        }
    }

    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Trusted => "trusted",
            Self::Balanced => "balanced",
            Self::Open => "open",
        }
    }
}

pub(crate) fn skills_execution_tier() -> SkillsExecutionTier {
    std::env::var("HERMES_SKILLS_EXECUTION_TIER")
        .ok()
        .as_deref()
        .and_then(SkillsExecutionTier::parse)
        .unwrap_or(SkillsExecutionTier::Balanced)
}

pub(crate) fn skills_tier_bypass_enabled() -> bool {
    std::env::var("HERMES_SKILLS_TIER_BYPASS")
        .ok()
        .is_some_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

fn skills_action_blocked_by_tier(
    tier: SkillsExecutionTier,
    action: &str,
    name: Option<&str>,
) -> bool {
    let name_lc = name.map(|v| v.to_ascii_lowercase());
    match tier {
        SkillsExecutionTier::Trusted => {
            matches!(
                action,
                "install" | "update" | "publish" | "uninstall" | "reset" | "subscribe"
            ) || (action == "tap" && matches!(name_lc.as_deref(), Some("add" | "remove")))
                || (action == "snapshot" && matches!(name_lc.as_deref(), Some("import")))
        }
        SkillsExecutionTier::Balanced => {
            matches!(action, "publish" | "reset")
                || (action == "snapshot" && matches!(name_lc.as_deref(), Some("import")))
        }
        SkillsExecutionTier::Open => false,
    }
}

// ---------------------------------------------------------------------------
// SkillsSlashInvocation
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
struct SkillsSlashInvocation {
    action: Option<String>,
    name: Option<String>,
    extra: Option<String>,
}

fn parse_skills_slash_invocation(args: &[&str]) -> Result<SkillsSlashInvocation, String> {
    if args.is_empty() {
        return Ok(SkillsSlashInvocation {
            action: None,
            name: None,
            extra: None,
        });
    }

    let action = args[0].to_ascii_lowercase();
    let rest = &args[1..];

    let build_joined = |values: &[&str]| -> Option<String> {
        let joined = values.join(" ").trim().to_string();
        if joined.is_empty() {
            None
        } else {
            Some(joined)
        }
    };

    let parsed = match action.as_str() {
        "list" | "browse" | "audit" | "quality" => SkillsSlashInvocation {
            action: Some(action),
            name: build_joined(rest),
            extra: None,
        },
        "search" | "install" | "inspect" | "uninstall" | "remove" | "publish" | "subscribe"
        | "reset" => SkillsSlashInvocation {
            action: Some(action),
            name: build_joined(rest),
            extra: None,
        },
        "check" => SkillsSlashInvocation {
            action: Some(action),
            name: rest.first().map(|s| s.to_string()),
            extra: None,
        },
        "update" => {
            let apply = rest
                .iter()
                .any(|v| matches!(v.to_ascii_lowercase().as_str(), "--apply" | "-a"));
            SkillsSlashInvocation {
                action: Some(action),
                name: None,
                extra: if apply {
                    Some("--apply".to_string())
                } else {
                    None
                },
            }
        }
        "snapshot" => SkillsSlashInvocation {
            action: Some(action),
            name: rest.first().map(|s| s.to_string()),
            extra: build_joined(if rest.len() > 1 { &rest[1..] } else { &[] }),
        },
        "tap" => SkillsSlashInvocation {
            action: Some(action),
            name: rest.first().map(|s| s.to_ascii_lowercase()),
            extra: build_joined(if rest.len() > 1 { &rest[1..] } else { &[] }),
        },
        "config" => SkillsSlashInvocation {
            action: Some(action),
            name: rest.first().map(|s| s.to_string()),
            extra: build_joined(if rest.len() > 1 { &rest[1..] } else { &[] }),
        },
        _ => {
            return Err(format!(
                "Unknown /skills subcommand '{}'. Use `/skills list`, `/skills quality`, or `/skills search <query>`.",
                action
            ));
        }
    };

    Ok(parsed)
}

async fn run_skills_subcommand_via_cli(
    invocation: &SkillsSlashInvocation,
) -> Result<String, AgentError> {
    let exe = std::env::current_exe()
        .map_err(|e| AgentError::Io(format!("Could not determine current executable: {}", e)))?;
    let mut cmd = tokio::process::Command::new(exe);
    cmd.arg("skills");
    if let Some(action) = invocation.action.as_deref() {
        cmd.arg(action);
    }
    if let Some(name) = invocation.name.as_deref() {
        cmd.arg(name);
    }
    if let Some(extra) = invocation.extra.as_deref() {
        cmd.arg("--extra").arg(extra);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = cmd
        .output()
        .await
        .map_err(|e| AgentError::Io(format!("Failed to execute skills command: {}", e)))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let mut combined = String::new();
    if !stdout.is_empty() {
        combined.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !combined.is_empty() {
            combined.push_str("\n\n");
        }
        combined.push_str(&format!("stderr:\n{}", stderr));
    }
    if combined.is_empty() {
        combined = if output.status.success() {
            "No output.".to_string()
        } else {
            format!("Command failed with status {}.", output.status)
        };
    }
    if !output.status.success() {
        combined = format!("(exit: {})\n{}", output.status, combined);
    }
    Ok(combined)
}

pub(crate) async fn handle_skills_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    if !args.is_empty() {
        let invocation = match parse_skills_slash_invocation(args) {
            Ok(v) => v,
            Err(msg) => {
                emit_command_output(app, msg);
                return Ok(CommandResult::Handled);
            }
        };
        let output = run_skills_subcommand_via_cli(&invocation).await?;
        emit_command_output(app, output);
        return Ok(CommandResult::Handled);
    }

    let skills_dir = hermes_config::hermes_home().join("skills");
    if !skills_dir.exists() {
        emit_command_output(
            app,
            format!(
                "No skills directory found at {}. Run `hermes setup` first.",
                skills_dir.display()
            ),
        );
        return Ok(CommandResult::Handled);
    }

    let mut skills: Vec<(String, String)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&skills_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let skill_md = path.join("SKILL.md");
            if !path.is_dir() || !skill_md.exists() {
                continue;
            }
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let title = std::fs::read_to_string(&skill_md)
                .ok()
                .and_then(|c| {
                    c.lines()
                        .find(|l| l.starts_with('#'))
                        .map(|l| l.trim_start_matches('#').trim().to_string())
                })
                .unwrap_or_else(|| "(no description)".to_string());
            skills.push((name, title));
        }
    }
    skills.sort_by(|a, b| a.0.cmp(&b.0));

    if skills.is_empty() {
        emit_command_output(
            app,
            format!(
                "No installed skills found in {}.\nInstall skills with `hermes skills install <name>`.",
                skills_dir.display()
            ),
        );
    } else {
        let mut out = format!("Installed skills ({}):\n", skills.len());
        for (name, title) in &skills {
            out.push_str(&format!("- `{}` — {}\n", name, title));
        }
        out.push_str("\nUse `hermes skills inspect <name>` for details.");
        out.push_str("\nUse `/skills quality` for score + fallback recommendations.");
        emit_command_output(app, out.trim_end());
    }
    Ok(CommandResult::Handled)
}

// ---------------------------------------------------------------------------
// CLI skills subcommand (hermes skills <action> [name] [--extra ...])
// ---------------------------------------------------------------------------

pub async fn handle_cli_skills(
    action: Option<String>,
    name: Option<String>,
    extra: Option<String>,
) -> Result<(), hermes_core::AgentError> {
    let requested_action = action.as_deref().unwrap_or("list");
    if !skills_tier_bypass_enabled() {
        let tier = skills_execution_tier();
        let denied = skills_action_blocked_by_tier(tier, requested_action, name.as_deref());

        if denied {
            return Err(hermes_core::AgentError::Config(format!(
                "skills action '{}' is blocked by skills tier '{}'. Use `/ops skills-tier open` or set HERMES_SKILLS_TIER_BYPASS=1 to override intentionally.",
                requested_action,
                tier.as_str()
            )));
        }
    }

    let skills_dir = hermes_config::hermes_home().join("skills");

    match action.as_deref().unwrap_or("list") {
        "list" => {
            if !skills_dir.exists() {
                println!(
                    "No skills directory found at {}. Run `hermes setup` first.",
                    skills_dir.display()
                );
                return Ok(());
            }
            let mut count = 0u32;
            println!("Installed skills ({}):", skills_dir.display());
            if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    let skill_md = path.join("SKILL.md");
                    if path.is_dir() && skill_md.exists() {
                        let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
                        let first_line = std::fs::read_to_string(&skill_md)
                            .ok()
                            .and_then(|c| {
                                c.lines()
                                    .find(|l| l.starts_with('#'))
                                    .map(|l| l.trim_start_matches('#').trim().to_string())
                            })
                            .unwrap_or_else(|| "(no description)".to_string());
                        println!("  • {} — {}", dir_name, first_line);
                        count += 1;
                    }
                }
            }
            if count == 0 {
                println!("  (no skills installed)");
            }
        }
        "browse" => {
            if !skills_dir.exists() {
                println!("No skills directory found.");
                return Ok(());
            }
            println!("Skills Browser");
            println!("==============\n");
            let mut categories: std::collections::HashMap<String, Vec<(String, String)>> =
                std::collections::HashMap::new();
            if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    let skill_md = path.join("SKILL.md");
                    if path.is_dir() && skill_md.exists() {
                        let dir_name = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        let content = std::fs::read_to_string(&skill_md).unwrap_or_default();
                        let first_line = content
                            .lines()
                            .find(|l| l.starts_with('#'))
                            .map(|l| l.trim_start_matches('#').trim().to_string())
                            .unwrap_or_else(|| "(no description)".to_string());
                        let category = path
                            .parent()
                            .and_then(|p| p.file_name())
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "general".to_string());
                        categories
                            .entry(category)
                            .or_default()
                            .push((dir_name, first_line));
                    }
                }
            }
            for (category, skills) in &categories {
                println!("[{}]", category);
                for (name, desc) in skills {
                    println!("  • {} — {}", name, desc);
                }
                println!();
            }
            if categories.is_empty() {
                println!("  (no skills installed)");
            }
        }
        "search" => {
            let query = name.unwrap_or_default();
            if query.is_empty() {
                println!("Usage: hermes skills search <query>");
                return Ok(());
            }
            println!("Searching registries for: \"{}\"...", query);
            let client = reqwest::Client::new();
            let mut displayed_results = false;

            if let Ok(results) = skills_infra::search_multi_registry(&client, &query, 40).await {
                if !results.is_empty() {
                    displayed_results = true;
                    println!("Multi-registry matches:");
                    for rec in results {
                        let short_desc = if rec.description.trim().is_empty() {
                            "(no description)"
                        } else {
                            rec.description.trim()
                        };
                        println!("  • [{}] {} — {}", rec.source, rec.identifier, short_desc);
                    }
                    println!(
                        "\nInstall with: hermes skills install <identifier> (example: skills.sh/anthropics/skills/skill-creator)"
                    );
                }
            }

            // Legacy hub path retained for compatibility.
            match client
                .get("https://skills.hermes.run/api/search")
                .query(&[("q", &query)])
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(data) = resp.json::<serde_json::Value>().await {
                        if let Some(results) = data.get("results").and_then(|r| r.as_array()) {
                            if results.is_empty() {
                                if !displayed_results {
                                    println!("No skills found matching \"{}\".", query);
                                }
                            } else {
                                displayed_results = true;
                                println!("\nLegacy Skills Hub matches:");
                                for skill in results {
                                    let name =
                                        skill.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                                    let desc = skill
                                        .get("description")
                                        .and_then(|d| d.as_str())
                                        .unwrap_or("");
                                    let version = skill
                                        .get("version")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("?");
                                    println!("  • {} (v{}) — {}", name, version, desc);
                                }
                                println!("\nInstall with: hermes skills install <name>");
                            }
                        } else {
                            if !displayed_results {
                                println!("Unexpected response format from Skills Hub.");
                            }
                        }
                    }
                }
                Ok(resp) => {
                    if !displayed_results {
                        println!("Skills Hub returned status {}", resp.status());
                    }
                }
                Err(e) => {
                    if !displayed_results {
                        println!("Could not reach Skills Hub: {}", e);
                    }
                }
            }
            if !displayed_results {
                if let Ok(skills_sh_hits) =
                    skills_infra::search_skills_sh_registry(&client, &query, 20).await
                {
                    if !skills_sh_hits.is_empty() {
                        displayed_results = true;
                        println!("\nSkills.sh fallback matches:");
                        for (name, identifier) in skills_sh_hits {
                            println!("  • {} — {}", name, identifier);
                        }
                        println!(
                            "\nInstall with: hermes skills install skills.sh/<owner/repo/skill>"
                        );
                    }
                }
            }
            if !displayed_results {
                let taps_file = hermes_config::hermes_home().join("skill_taps.json");
                let subscriptions_file = skills_dir.join("subscriptions.json");
                let taps = skills_infra::effective_skill_taps(&taps_file, &subscriptions_file);
                let fallback =
                    skills_infra::search_skills_via_taps(&client, &taps, &query, 20).await?;
                if fallback.is_empty() {
                    println!("No tap-backed matches found for \"{}\".", query);
                } else {
                    println!("\nTap-backed matches:");
                    for (name, source) in fallback {
                        println!("  • {} — {}", name, source);
                    }
                    println!(
                        "\nInstall with: hermes skills install <name> or hermes skills install <owner/repo/path>"
                    );
                }
            }
        }
        "install" => {
            let skill_spec = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing skill name. Usage: hermes skills install <name>".into(),
                )
            })?;
            let (skill_name, _requested_version) =
                skills_infra::parse_skill_name_and_version(&skill_spec);
            println!("Installing skill: {}", skill_name);

            let client = reqwest::Client::new();
            let registry_prefixed = skills_infra::parse_registry_prefixed_skill(&skill_name);
            let explicit = if registry_prefixed.is_some() {
                None
            } else {
                skills_infra::parse_explicit_github_skill(&skill_name)
            };

            let (files, install_seed, provenance) = if let Some((source, key)) = registry_prefixed {
                match source.as_str() {
                    "official" => {
                        let install_key = key.clone();
                        let resolved =
                            skills_infra::resolve_official_skill_source(&client, &key).await?;
                        println!(
                            "Resolved official source: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        );
                        (
                            skills_infra::fetch_skill_files_from_github(&client, &resolved).await?,
                            install_key,
                            skills_infra::SkillInstallProvenance {
                                source: "official".to_string(),
                                identifier: key.clone(),
                                trust_level: skills_infra::default_trust_level_for_source(
                                    "official",
                                )
                                .to_string(),
                                metadata: serde_json::json!({
                                    "repo": resolved.repo,
                                    "branch": resolved.branch,
                                    "skill_dir": resolved.skill_dir,
                                }),
                            },
                        )
                    }
                    "skills.sh" => {
                        let install_key = key.clone();
                        let resolved =
                            skills_infra::resolve_skills_sh_source(&client, &key).await?;
                        println!(
                            "Resolved skills.sh source: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        );
                        (
                            skills_infra::fetch_skill_files_from_github(&client, &resolved).await?,
                            install_key,
                            skills_infra::SkillInstallProvenance {
                                source: "skills.sh".to_string(),
                                identifier: key.clone(),
                                trust_level: skills_infra::default_trust_level_for_source(
                                    "skills.sh",
                                )
                                .to_string(),
                                metadata: serde_json::json!({
                                    "repo": resolved.repo,
                                    "branch": resolved.branch,
                                    "skill_dir": resolved.skill_dir,
                                }),
                            },
                        )
                    }
                    "lobehub" => {
                        println!("Resolved lobehub source: {}", key);
                        (
                            skills_infra::fetch_lobehub_skill_files(&client, &key).await?,
                            key.clone(),
                            skills_infra::SkillInstallProvenance {
                                source: "lobehub".to_string(),
                                identifier: key,
                                trust_level: skills_infra::default_trust_level_for_source(
                                    "lobehub",
                                )
                                .to_string(),
                                metadata: serde_json::json!({}),
                            },
                        )
                    }
                    "clawhub" => {
                        println!("Resolved clawhub source: {}", key);
                        (
                            skills_infra::fetch_clawhub_skill_files(
                                &client,
                                &key,
                                _requested_version.as_deref(),
                            )
                            .await?,
                            key.clone(),
                            skills_infra::SkillInstallProvenance {
                                source: "clawhub".to_string(),
                                identifier: key,
                                trust_level: skills_infra::default_trust_level_for_source(
                                    "clawhub",
                                )
                                .to_string(),
                                metadata: serde_json::json!({}),
                            },
                        )
                    }
                    "claude-marketplace" => {
                        let install_key = key.clone();
                        let resolved =
                            skills_infra::resolve_claude_marketplace_skill(&client, &key).await?;
                        println!(
                            "Resolved claude-marketplace source: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        );
                        (
                            skills_infra::fetch_skill_files_from_github(&client, &resolved).await?,
                            install_key,
                            skills_infra::SkillInstallProvenance {
                                source: "claude-marketplace".to_string(),
                                identifier: key.clone(),
                                trust_level: skills_infra::default_trust_level_for_source(
                                    "claude-marketplace",
                                )
                                .to_string(),
                                metadata: serde_json::json!({
                                    "repo": resolved.repo,
                                    "branch": resolved.branch,
                                    "skill_dir": resolved.skill_dir,
                                }),
                            },
                        )
                    }
                    "github" => {
                        let (repo, maybe_branch, skill_dir) =
                            skills_infra::parse_explicit_github_skill(&key).ok_or_else(|| {
                                AgentError::Config(format!(
                                    "github/ installs require owner/repo/path, got '{}'",
                                    key
                                ))
                            })?;
                        let branch = if let Some(branch) = maybe_branch {
                            branch
                        } else {
                            skills_infra::github_default_branch(&client, &repo).await?
                        };
                        let resolved = skills_infra::ResolvedSkillSource {
                            repo,
                            branch,
                            skill_dir,
                        };
                        let identifier = format!("{}/{}", resolved.repo, resolved.skill_dir);
                        println!(
                            "Resolved github source: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        );
                        (
                            skills_infra::fetch_skill_files_from_github(&client, &resolved).await?,
                            key,
                            skills_infra::SkillInstallProvenance {
                                source: "github".to_string(),
                                identifier,
                                trust_level: skills_infra::default_trust_level_for_source("github")
                                    .to_string(),
                                metadata: serde_json::json!({
                                    "repo": resolved.repo,
                                    "branch": resolved.branch,
                                    "skill_dir": resolved.skill_dir,
                                }),
                            },
                        )
                    }
                    _ => {
                        return Err(AgentError::Config(format!(
                            "Unsupported skill registry source '{}'",
                            source
                        )));
                    }
                }
            } else if let Some((repo, maybe_branch, skill_dir)) = explicit {
                let branch = if let Some(branch) = maybe_branch {
                    branch
                } else {
                    skills_infra::github_default_branch(&client, &repo).await?
                };
                let resolved = skills_infra::ResolvedSkillSource {
                    repo,
                    branch,
                    skill_dir,
                };
                let identifier = format!("{}/{}", resolved.repo, resolved.skill_dir);
                println!(
                    "Resolved source: {}/{} @ {}",
                    resolved.repo, resolved.skill_dir, resolved.branch
                );
                (
                    skills_infra::fetch_skill_files_from_github(&client, &resolved).await?,
                    skill_name.clone(),
                    skills_infra::SkillInstallProvenance {
                        source: "github".to_string(),
                        identifier,
                        trust_level: skills_infra::default_trust_level_for_source("github")
                            .to_string(),
                        metadata: serde_json::json!({
                            "repo": resolved.repo,
                            "branch": resolved.branch,
                            "skill_dir": resolved.skill_dir,
                        }),
                    },
                )
            } else if let Some(skill_hint) = _requested_version
                .as_deref()
                .filter(|_| skills_infra::looks_like_github_repo_slug(&skill_name))
            {
                let resolved = skills_infra::resolve_skill_in_repo(
                    &client,
                    &skill_name,
                    skill_hint,
                    Some("skills"),
                )
                .await?;
                println!(
                    "Resolved source: {}/{} @ {}",
                    resolved.repo, resolved.skill_dir, resolved.branch
                );
                (
                    skills_infra::fetch_skill_files_from_github(&client, &resolved).await?,
                    skill_name.clone(),
                    skills_infra::SkillInstallProvenance {
                        source: "github".to_string(),
                        identifier: format!("{}/{}", resolved.repo, resolved.skill_dir),
                        trust_level: skills_infra::default_trust_level_for_source("github")
                            .to_string(),
                        metadata: serde_json::json!({
                            "repo": resolved.repo,
                            "branch": resolved.branch,
                            "skill_dir": resolved.skill_dir,
                        }),
                    },
                )
            } else {
                let from_index =
                    skills_infra::resolve_skill_via_registry_index(&client, &skill_name, None)
                        .await;
                if let Ok(hit) = from_index {
                    if hit.source.eq_ignore_ascii_case("official") {
                        let resolved =
                            skills_infra::resolve_official_skill_source(&client, &hit.identifier)
                                .await?;
                        println!(
                            "Resolved source [official]: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        );
                        (
                            skills_infra::fetch_skill_files_from_github(&client, &resolved).await?,
                            hit.identifier,
                            skills_infra::SkillInstallProvenance {
                                source: "official".to_string(),
                                identifier: format!("{}/{}", resolved.repo, resolved.skill_dir),
                                trust_level: skills_infra::default_trust_level_for_source(
                                    "official",
                                )
                                .to_string(),
                                metadata: serde_json::json!({
                                    "repo": resolved.repo,
                                    "branch": resolved.branch,
                                    "skill_dir": resolved.skill_dir,
                                }),
                            },
                        )
                    } else {
                        match hit.install_source {
                            skills_infra::RegistryInstallSource::GitHub(resolved) => {
                                let branch =
                                    skills_infra::github_default_branch(&client, &resolved.repo)
                                        .await?;
                                let resolved =
                                    skills_infra::ResolvedSkillSource { branch, ..resolved };
                                println!(
                                    "Resolved source [{}]: {}/{} @ {}",
                                    hit.source, resolved.repo, resolved.skill_dir, resolved.branch
                                );
                                (
                                    skills_infra::fetch_skill_files_from_github(&client, &resolved)
                                        .await?,
                                    hit.identifier,
                                    skills_infra::SkillInstallProvenance {
                                        source: hit.source,
                                        identifier: format!(
                                            "{}/{}",
                                            resolved.repo, resolved.skill_dir
                                        ),
                                        trust_level: skills_infra::default_trust_level_for_source(
                                            "github",
                                        )
                                        .to_string(),
                                        metadata: serde_json::json!({
                                            "repo": resolved.repo,
                                            "branch": resolved.branch,
                                            "skill_dir": resolved.skill_dir,
                                        }),
                                    },
                                )
                            }
                            skills_infra::RegistryInstallSource::LobeHub { slug } => {
                                println!("Resolved source [lobehub]: {}", slug);
                                (
                                    skills_infra::fetch_lobehub_skill_files(&client, &slug).await?,
                                    slug.clone(),
                                    skills_infra::SkillInstallProvenance {
                                        source: "lobehub".to_string(),
                                        identifier: slug,
                                        trust_level: skills_infra::default_trust_level_for_source(
                                            "lobehub",
                                        )
                                        .to_string(),
                                        metadata: serde_json::json!({}),
                                    },
                                )
                            }
                            skills_infra::RegistryInstallSource::ClawHub { slug, version } => {
                                println!("Resolved source [clawhub]: {}", slug);
                                (
                                    skills_infra::fetch_clawhub_skill_files(
                                        &client,
                                        &slug,
                                        version.as_deref(),
                                    )
                                    .await?,
                                    slug.clone(),
                                    skills_infra::SkillInstallProvenance {
                                        source: "clawhub".to_string(),
                                        identifier: slug,
                                        trust_level: skills_infra::default_trust_level_for_source(
                                            "clawhub",
                                        )
                                        .to_string(),
                                        metadata: serde_json::json!({
                                            "version_hint": version
                                        }),
                                    },
                                )
                            }
                        }
                    }
                } else {
                    let taps_file = hermes_config::hermes_home().join("skill_taps.json");
                    let subscriptions_file = skills_dir.join("subscriptions.json");
                    let taps = skills_infra::effective_skill_taps(&taps_file, &subscriptions_file);
                    let (resolved, route) = skills_infra::resolve_install_via_fallback_router(
                        &client,
                        &skill_name,
                        &taps,
                    )
                    .await?;
                    match route {
                        skills_infra::InstallFallbackSource::SkillsSh => println!(
                            "Resolved source [skills.sh fallback]: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        ),
                        skills_infra::InstallFallbackSource::Tap => println!(
                            "Resolved source (tap): {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        ),
                    }
                    (
                        skills_infra::fetch_skill_files_from_github(&client, &resolved).await?,
                        skill_name.clone(),
                        skills_infra::SkillInstallProvenance {
                            source: match route {
                                skills_infra::InstallFallbackSource::SkillsSh => {
                                    "skills.sh".to_string()
                                }
                                skills_infra::InstallFallbackSource::Tap => "tap".to_string(),
                            },
                            identifier: format!("{}/{}", resolved.repo, resolved.skill_dir),
                            trust_level: skills_infra::default_trust_level_for_source(
                                match route {
                                    skills_infra::InstallFallbackSource::SkillsSh => "skills.sh",
                                    skills_infra::InstallFallbackSource::Tap => "tap",
                                },
                            )
                            .to_string(),
                            metadata: serde_json::json!({
                                "repo": resolved.repo,
                                "branch": resolved.branch,
                                "skill_dir": resolved.skill_dir,
                            }),
                        },
                    )
                }
            };

            let install_name = skills_infra::sanitize_skill_install_name(
                _requested_version
                    .as_deref()
                    .filter(|_| skills_infra::looks_like_github_repo_slug(&skill_name))
                    .unwrap_or(install_seed.as_str()),
            );
            let force = skills_infra::skills_install_force(extra.as_deref());
            let target = skills_infra::install_skill_files(
                &skills_dir,
                &install_name,
                &files,
                &provenance.identifier,
                force,
            )?;
            skills_infra::record_skill_install_in_hub_lock(
                &skills_dir,
                &install_name,
                &target,
                &files,
                &provenance,
            )?;
            println!("Skill '{}' installed to {}", install_name, target.display());
            skills_infra::maybe_run_skill_bootstrap(&install_name, &target, &files).await?;
        }
        "reset" => {
            let skill_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing skill name. Usage: hermes skills reset <name>".into(),
                )
            })?;
            let target = skills_dir.join(&skill_name);
            if target.exists() {
                std::fs::remove_dir_all(&target).map_err(|e| {
                    hermes_core::AgentError::Io(format!("Failed to remove skill dir: {}", e))
                })?;
            }
            std::fs::create_dir_all(&target).map_err(|e| {
                hermes_core::AgentError::Io(format!("Failed to create skill dir: {}", e))
            })?;
            std::fs::write(
                target.join("SKILL.md"),
                format!(
                    "# {}\n\nReset by CLI. Replace with canonical skill contents.\n",
                    skill_name
                ),
            )
            .map_err(|e| hermes_core::AgentError::Io(format!("Failed to write SKILL.md: {}", e)))?;
            println!("Skill '{}' reset at {}", skill_name, target.display());
        }
        "subscribe" => {
            let source = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing source. Usage: hermes skills subscribe <name-or-url>".into(),
                )
            })?;
            std::fs::create_dir_all(&skills_dir)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            let subscriptions_path = skills_dir.join("subscriptions.json");
            let mut subscriptions: Vec<serde_json::Value> = if subscriptions_path.exists() {
                let raw = std::fs::read_to_string(&subscriptions_path)
                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                serde_json::from_str(&raw).unwrap_or_default()
            } else {
                Vec::new()
            };
            let normalized = source.trim().to_string();
            if normalized.is_empty() {
                return Err(hermes_core::AgentError::Config(
                    "skills subscribe: source cannot be empty".into(),
                ));
            }
            let exists = subscriptions.iter().any(|item| {
                item.get("source")
                    .and_then(|v| v.as_str())
                    .map(|s| s == normalized)
                    .unwrap_or(false)
            });
            if exists {
                println!("Skill subscription already exists: {}", normalized);
                return Ok(());
            }
            subscriptions.push(serde_json::json!({
                "source": normalized,
                "added_at": chrono::Utc::now().to_rfc3339(),
                "options": extra.as_deref().unwrap_or(""),
            }));
            std::fs::write(
                &subscriptions_path,
                serde_json::to_string_pretty(&subscriptions)
                    .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?,
            )
            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            println!(
                "Subscribed to skill source '{}'. Registry: {}",
                source,
                subscriptions_path.display()
            );
        }
        "inspect" => {
            let skill_name = name.unwrap_or_default();
            let skill_md = skills_dir.join(&skill_name).join("SKILL.md");
            if skill_md.exists() {
                let content = std::fs::read_to_string(&skill_md)
                    .map_err(|e| hermes_core::AgentError::Io(format!("Read error: {}", e)))?;
                println!("{}", content);
            } else {
                println!("Skill '{}' not found at {}", skill_name, skill_md.display());
            }
        }
        "uninstall" => {
            let skill_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing skill name. Usage: hermes skills uninstall <name>".into(),
                )
            })?;
            let target = skills_dir.join(&skill_name);
            if target.exists() {
                std::fs::remove_dir_all(&target).map_err(|e| {
                    hermes_core::AgentError::Io(format!("Failed to remove skill: {}", e))
                })?;
                let removed =
                    skills_infra::record_skill_uninstall_in_hub_lock(&skills_dir, &skill_name)?;
                if let Some(entry) = removed {
                    println!(
                        "Skill '{}' uninstalled (source={}, id={}).",
                        skill_name, entry.source, entry.identifier
                    );
                } else {
                    println!("Skill '{}' uninstalled.", skill_name);
                }
            } else if let Some(entry) =
                skills_infra::record_skill_uninstall_in_hub_lock(&skills_dir, &skill_name)?
            {
                println!(
                    "Skill '{}' not found locally, but removed stale lock entry (source={}, id={}).",
                    skill_name, entry.source, entry.identifier
                );
            } else {
                println!("Skill '{}' not found.", skill_name);
            }
        }
        "check" => {
            let skill_name = name.unwrap_or_default();
            if skill_name.is_empty() {
                println!("Checking all installed skills...");
                let mut ok = 0u32;
                let mut issues = 0u32;
                if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                    for entry in entries.filter_map(|e| e.ok()) {
                        let path = entry.path();
                        if !path.is_dir() {
                            continue;
                        }
                        let dir_name = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        let skill_md = path.join("SKILL.md");
                        if !skill_md.exists() {
                            println!("  ✗ {} — missing SKILL.md", dir_name);
                            issues += 1;
                        } else {
                            let content = std::fs::read_to_string(&skill_md).unwrap_or_default();
                            if content.trim().is_empty() {
                                println!("  ⚠ {} — SKILL.md is empty", dir_name);
                                issues += 1;
                            } else {
                                println!("  ✓ {}", dir_name);
                                ok += 1;
                            }
                        }
                    }
                }
                println!("\n{} healthy, {} with issues.", ok, issues);
            } else {
                let skill_path = skills_dir.join(&skill_name);
                let skill_md = skill_path.join("SKILL.md");
                if !skill_path.exists() {
                    println!("Skill '{}' not found.", skill_name);
                } else if !skill_md.exists() {
                    println!("Skill '{}': MISSING SKILL.md", skill_name);
                } else {
                    let content = std::fs::read_to_string(&skill_md).unwrap_or_default();
                    let lines = content.lines().count();
                    let has_frontmatter = content.starts_with("---");
                    println!("Skill '{}': OK", skill_name);
                    println!("  Path: {}", skill_path.display());
                    println!("  SKILL.md: {} lines", lines);
                    println!(
                        "  Frontmatter: {}",
                        if has_frontmatter { "yes" } else { "no" }
                    );
                }
            }
        }
        "update" => {
            println!("Checking for skill updates...\n");
            if !skills_dir.exists() {
                println!("No skills installed.");
                return Ok(());
            }

            let apply_updates = extra.as_deref() == Some("--apply");
            let lock = skills_infra::read_skills_hub_lock(&skills_dir);
            if lock.installed.is_empty() {
                println!(
                    "No hub-installed skills tracked in {}.",
                    skills_infra::skills_hub_lock_path(&skills_dir).display()
                );
                println!(
                    "Install skills with `hermes skills install <identifier>` to enable source-aware updates."
                );
                return Ok(());
            }

            println!(
                "{:28} {:14} {:14} {:16} {}",
                "Skill", "Source", "Local Hash", "Upstream Hash", "Status"
            );
            println!("{}", "-".repeat(98));

            let taps_file = hermes_config::hermes_home().join("skill_taps.json");
            let subscriptions_file = skills_dir.join("subscriptions.json");
            let merged_taps = skills_infra::effective_skill_taps(&taps_file, &subscriptions_file);
            let client = reqwest::Client::new();

            struct PendingUpdate {
                entry: skills_infra::SkillHubInstalledEntry,
                files: Vec<(String, Bytes)>,
                upstream_hash: String,
            }
            let mut pending: Vec<PendingUpdate> = Vec::new();

            for entry in lock.installed {
                let local_hash = if skills_dir.join(&entry.install_path).exists() {
                    skills_infra::hash_installed_skill_dir(&skills_dir.join(&entry.install_path))
                        .unwrap_or_else(|_| entry.content_hash.clone())
                } else {
                    entry.content_hash.clone()
                };

                match skills_infra::fetch_bundle_for_lock_entry(&client, &entry, &merged_taps).await
                {
                    Ok(files) => {
                        let upstream_hash = skills_infra::hash_skill_bundle(&files);
                        let status = if local_hash == upstream_hash {
                            "✓ up-to-date"
                        } else {
                            pending.push(PendingUpdate {
                                entry: entry.clone(),
                                files,
                                upstream_hash: upstream_hash.clone(),
                            });
                            "⬆ update available"
                        };
                        println!(
                            "{:28} {:14} {:14} {:16} {}",
                            entry.name,
                            entry.source,
                            &local_hash.chars().take(14).collect::<String>(),
                            &upstream_hash.chars().take(16).collect::<String>(),
                            status
                        );
                    }
                    Err(err) => {
                        println!(
                            "{:28} {:14} {:14} {:16} unavailable ({})",
                            entry.name,
                            entry.source,
                            &local_hash.chars().take(14).collect::<String>(),
                            "-",
                            err
                        );
                    }
                }
            }

            println!();
            if pending.is_empty() {
                println!("All tracked hub skills are up to date.");
            } else {
                println!("{} update(s) available.", pending.len());
                if apply_updates {
                    println!("\nApplying updates...");
                    for update in pending {
                        let install_name =
                            skills_infra::sanitize_skill_install_name(&update.entry.name);
                        let target = skills_infra::install_skill_files(
                            &skills_dir,
                            &install_name,
                            &update.files,
                            &update.entry.identifier,
                            false,
                        )?;
                        let prov = skills_infra::SkillInstallProvenance {
                            source: update.entry.source.clone(),
                            identifier: update.entry.identifier.clone(),
                            trust_level: update.entry.trust_level.clone(),
                            metadata: update.entry.metadata.clone(),
                        };
                        skills_infra::record_skill_install_in_hub_lock(
                            &skills_dir,
                            &install_name,
                            &target,
                            &update.files,
                            &prov,
                        )?;
                        println!(
                            "  ✓ {} updated (new hash: {})",
                            install_name,
                            &update.upstream_hash.chars().take(16).collect::<String>()
                        );
                        skills_infra::maybe_run_skill_bootstrap(
                            &install_name,
                            &target,
                            &update.files,
                        )
                        .await?;
                    }
                } else {
                    println!("Run `hermes skills update --apply` to install updates.");
                }
            }
        }
        "publish" => {
            let skill_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing skill name. Usage: hermes skills publish <name>".into(),
                )
            })?;
            let skill_path = skills_dir.join(&skill_name);
            if !skill_path.exists() {
                return Err(hermes_core::AgentError::Config(format!(
                    "Skill '{}' not found.",
                    skill_name
                )));
            }
            println!("Publishing skill '{}' to Skills Hub...", skill_name);
            println!("  Source: {}", skill_path.display());

            let skill_md = skill_path.join("SKILL.md");
            if !skill_md.exists() {
                println!("  ✗ Missing SKILL.md — required for publishing.");
                return Ok(());
            }

            let content = std::fs::read_to_string(&skill_md)
                .map_err(|e| hermes_core::AgentError::Io(format!("Read error: {}", e)))?;
            let (frontmatter, _body) =
                hermes_tools::tools::skill_utils::parse_frontmatter(&content);

            let fm_name = frontmatter.get("name").and_then(|v| v.as_str());
            let fm_version = frontmatter.get("version").and_then(|v| v.as_str());
            let fm_desc = frontmatter.get("description").and_then(|v| v.as_str());
            let fm_category = frontmatter.get("category").and_then(|v| v.as_str());

            if fm_name.is_none()
                || fm_version.is_none()
                || fm_desc.is_none()
                || fm_category.is_none()
            {
                println!(
                    "  ✗ SKILL.md frontmatter must include: name, version, description, category"
                );
                let mut missing = Vec::new();
                if fm_name.is_none() {
                    missing.push("name");
                }
                if fm_version.is_none() {
                    missing.push("version");
                }
                if fm_desc.is_none() {
                    missing.push("description");
                }
                if fm_category.is_none() {
                    missing.push("category");
                }
                println!("    Missing: {}", missing.join(", "));
                return Ok(());
            }

            let publish_name = fm_name.unwrap();
            let publish_version = fm_version.unwrap();
            let publish_desc = fm_desc.unwrap();
            let publish_category = fm_category.unwrap();
            println!(
                "  ✓ name={}, version={}, category={}",
                publish_name, publish_version, publish_category
            );
            println!("  ✓ description: {}", publish_desc);

            // Package skill directory into a tarball in memory
            let mut tar_buf = Vec::new();
            {
                let enc =
                    flate2::write::GzEncoder::new(&mut tar_buf, flate2::Compression::default());
                let mut tar_builder = tar::Builder::new(enc);
                tar_builder
                    .append_dir_all(&skill_name, &skill_path)
                    .map_err(|e| hermes_core::AgentError::Io(format!("Tar error: {}", e)))?;
                tar_builder
                    .finish()
                    .map_err(|e| hermes_core::AgentError::Io(format!("Tar finish error: {}", e)))?;
            }
            println!("  ✓ Packaged {} bytes", tar_buf.len());

            // Read hub token
            let token_path = hermes_config::hermes_home().join("hub_token");
            if !token_path.exists() {
                println!("  ✗ No hub token found at {}", token_path.display());
                println!("    Run `hermes login hub` to authenticate with Skills Hub.");
                return Ok(());
            }
            let hub_token = std::fs::read_to_string(&token_path)
                .map_err(|e| hermes_core::AgentError::Io(format!("Token read error: {}", e)))?
                .trim()
                .to_string();

            // Build metadata JSON
            let metadata = serde_json::json!({
                "name": publish_name,
                "version": publish_version,
                "description": publish_desc,
                "category": publish_category,
            });

            // Upload to Skills Hub API via multipart
            let tarball_part = reqwest::multipart::Part::bytes(tar_buf)
                .file_name(format!("{}-{}.tar.gz", publish_name, publish_version))
                .mime_str("application/gzip")
                .unwrap();
            let metadata_part = reqwest::multipart::Part::text(metadata.to_string())
                .mime_str("application/json")
                .unwrap();
            let form = reqwest::multipart::Form::new()
                .part("tarball", tarball_part)
                .part("metadata", metadata_part);

            println!("  Uploading to Skills Hub...");
            match reqwest::Client::new()
                .post("https://agentskills.io/api/v1/skills")
                .bearer_auth(&hub_token)
                .multipart(form)
                .timeout(std::time::Duration::from_secs(60))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    let url = format!("https://agentskills.io/skills/{}", publish_name);
                    println!("  ✓ Published successfully!");
                    println!("  URL: {}", url);
                }
                Ok(resp) if resp.status() == reqwest::StatusCode::CONFLICT => {
                    println!(
                        "  ✗ Version {} already exists on Skills Hub.",
                        publish_version
                    );
                    println!("    Bump the version in SKILL.md frontmatter and try again.");
                }
                Ok(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED => {
                    println!("  ✗ Unauthorized. Hub token may be expired.");
                    println!("    Run `hermes login hub` to re-authenticate.");
                }
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    println!("  ✗ Upload failed (HTTP {}): {}", status, body);
                }
                Err(e) => {
                    println!("  ✗ Could not reach Skills Hub: {}", e);
                }
            }
        }
        "snapshot" => {
            let sub = name.as_deref().unwrap_or("export");
            match sub {
                "export" => {
                    let output = extra.unwrap_or_else(|| {
                        format!(
                            "skills-snapshot-{}.tar.gz",
                            chrono::Utc::now().format("%Y%m%d-%H%M%S")
                        )
                    });
                    println!("Exporting skills snapshot to: {}", output);
                    if !skills_dir.exists() {
                        println!("No skills directory found.");
                        return Ok(());
                    }
                    // Create a tar.gz archive of skills directory
                    let tar_gz = std::fs::File::create(&output).map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to create archive: {}", e))
                    })?;
                    let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
                    let mut tar = tar::Builder::new(enc);
                    tar.append_dir_all("skills", &skills_dir).map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to archive: {}", e))
                    })?;
                    tar.finish().map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to finalize archive: {}", e))
                    })?;
                    println!("Snapshot exported to: {}", output);
                }
                "import" => {
                    let input = extra.ok_or_else(|| {
                        hermes_core::AgentError::Config(
                            "Missing snapshot path. Usage: hermes skills snapshot import <path>"
                                .into(),
                        )
                    })?;
                    println!("Importing skills snapshot from: {}", input);
                    let tar_gz = std::fs::File::open(&input).map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to open archive: {}", e))
                    })?;
                    let dec = flate2::read::GzDecoder::new(tar_gz);
                    let mut archive = tar::Archive::new(dec);
                    std::fs::create_dir_all(&skills_dir).map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to create skills dir: {}", e))
                    })?;
                    archive.unpack(hermes_config::hermes_home()).map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to extract archive: {}", e))
                    })?;
                    println!("Snapshot imported successfully.");
                }
                _ => {
                    println!("Usage: hermes skills snapshot export|import [path]");
                }
            }
        }
        "tap" => {
            let sub = name.as_deref().unwrap_or("list");
            let taps_file = hermes_config::hermes_home().join("skill_taps.json");
            let subscriptions_file = skills_dir.join("subscriptions.json");
            match sub {
                "list" => {
                    let taps = skills_infra::effective_skill_taps(&taps_file, &subscriptions_file);
                    if taps.is_empty() {
                        println!("No skill taps configured.");
                    } else {
                        println!("Skill taps:");
                        for tap in &taps {
                            println!("  • {}", tap);
                        }
                    }
                }
                "add" => {
                    let url = extra.ok_or_else(|| {
                        hermes_core::AgentError::Config(
                            "Missing tap URL. Usage: hermes skills tap add <url>".into(),
                        )
                    })?;
                    let mut taps: Vec<String> = skills_infra::read_skill_taps(&taps_file);
                    if skills_infra::effective_skill_taps(&taps_file, &subscriptions_file)
                        .contains(&url)
                    {
                        println!("Tap already exists: {}", url);
                    } else {
                        taps.push(url.clone());
                        skills_infra::write_skill_taps(&taps_file, &taps)?;
                        println!("Added tap: {}", url);
                    }
                }
                "remove" => {
                    let url = extra.ok_or_else(|| {
                        hermes_core::AgentError::Config(
                            "Missing tap URL. Usage: hermes skills tap remove <url>".into(),
                        )
                    })?;
                    if skills_infra::DEFAULT_SKILL_TAPS
                        .iter()
                        .any(|default_tap| default_tap == &url.as_str())
                    {
                        println!("Tap '{}' is a built-in default and cannot be removed.", url);
                        println!(
                            "Add custom taps with `hermes skills tap add <url>`; defaults remain active."
                        );
                        return Ok(());
                    }

                    let mut taps: Vec<String> = skills_infra::read_skill_taps(&taps_file);
                    let before_len = taps.len();
                    taps.retain(|t| t != &url);
                    if taps.len() < before_len {
                        skills_infra::write_skill_taps(&taps_file, &taps)?;
                        println!("Removed tap: {}", url);
                    } else {
                        println!("Tap not found: {}", url);
                    }
                }
                _ => {
                    println!("Usage: hermes skills tap list|add|remove [url]");
                }
            }
        }
        "config" => {
            let skill_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing skill name. Usage: hermes skills config <name> [key] [value]".into(),
                )
            })?;
            let config_file = skills_dir.join(&skill_name).join("config.json");
            if let Some(key) = extra {
                // Set or get a config key
                let parts: Vec<&str> = key.splitn(2, '=').collect();
                if parts.len() == 2 {
                    let mut config: serde_json::Value = if config_file.exists() {
                        let c = std::fs::read_to_string(&config_file)
                            .unwrap_or_else(|_| "{}".to_string());
                        serde_json::from_str(&c).unwrap_or(serde_json::json!({}))
                    } else {
                        serde_json::json!({})
                    };
                    config[parts[0]] = serde_json::Value::String(parts[1].to_string());
                    let json = serde_json::to_string_pretty(&config)
                        .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
                    std::fs::write(&config_file, json)
                        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                    println!("Set {} = {} for skill '{}'", parts[0], parts[1], skill_name);
                } else {
                    // Get value
                    if config_file.exists() {
                        let c = std::fs::read_to_string(&config_file)
                            .unwrap_or_else(|_| "{}".to_string());
                        let config: serde_json::Value =
                            serde_json::from_str(&c).unwrap_or(serde_json::json!({}));
                        match config.get(&key) {
                            Some(v) => println!("{} = {}", key, v),
                            None => println!("Key '{}' not found in skill config.", key),
                        }
                    } else {
                        println!("No config for skill '{}'.", skill_name);
                    }
                }
            } else {
                // Show all config
                if config_file.exists() {
                    let content = std::fs::read_to_string(&config_file)
                        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                    println!("Config for skill '{}':", skill_name);
                    println!("{}", content);
                } else {
                    println!("No config for skill '{}'.", skill_name);
                }
            }
        }
        "quality" => {
            println!("Skill quality scorecard");
            println!("======================\n");
            if !skills_dir.exists() {
                println!("No skills installed.");
                return Ok(());
            }

            #[derive(Debug)]
            struct SkillQualityRow {
                name: String,
                score: i32,
                tier: &'static str,
                notes: Vec<String>,
            }

            let mut rows: Vec<SkillQualityRow> = Vec::new();
            let weak_regex = Regex::new(r"(?i)\b(todo|fixme|placeholder|stub)\b")
                .map_err(|e| AgentError::Config(format!("quality regex error: {}", e)))?;
            let risky_regex = Regex::new(r"(?i)\b(rm\s+-rf|mkfs|dd\s+if=|eval\s*\(|exec\s*\()")
                .map_err(|e| AgentError::Config(format!("quality regex error: {}", e)))?;

            if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    let skill_md = path.join("SKILL.md");
                    if !path.is_dir() || !skill_md.exists() {
                        continue;
                    }
                    let name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();

                    let mut score = 100i32;
                    let mut notes = Vec::new();
                    let content = std::fs::read_to_string(&skill_md).unwrap_or_default();
                    let (frontmatter, _) =
                        hermes_tools::tools::skill_utils::parse_frontmatter(&content);
                    for required in ["name", "version", "description", "category"] {
                        if frontmatter.get(required).and_then(|v| v.as_str()).is_none() {
                            score -= 8;
                            notes.push(format!("missing_frontmatter:{}", required));
                        }
                    }

                    let line_count = content.lines().count();
                    if line_count < 20 {
                        score -= 10;
                        notes.push("short_skill_doc".to_string());
                    } else if line_count > 80 {
                        score += 4;
                    }

                    let scripts_dir = path.join("scripts");
                    if scripts_dir.exists() {
                        score += 6;
                    } else {
                        score -= 4;
                        notes.push("no_scripts".to_string());
                    }
                    if path.join("examples").exists() {
                        score += 4;
                    } else {
                        notes.push("no_examples".to_string());
                    }
                    if path.join("templates").exists() {
                        score += 3;
                    }
                    if path.join("tests").exists() {
                        score += 4;
                    }

                    if weak_regex.is_match(&content) {
                        score -= 8;
                        notes.push("contains_placeholder_markers".to_string());
                    }
                    if risky_regex.is_match(&content) {
                        score -= 20;
                        notes.push("contains_risky_exec_pattern".to_string());
                    }

                    score = score.clamp(0, 100);
                    let tier = if score >= 85 {
                        "excellent"
                    } else if score >= 70 {
                        "good"
                    } else if score >= 55 {
                        "watch"
                    } else {
                        "fallback"
                    };
                    rows.push(SkillQualityRow {
                        name,
                        score,
                        tier,
                        notes,
                    });
                }
            }

            if rows.is_empty() {
                println!("No skills installed.");
                return Ok(());
            }
            rows.sort_by(|a, b| b.score.cmp(&a.score).then_with(|| a.name.cmp(&b.name)));
            println!("{:28} {:>5} {:>10}  notes", "skill", "score", "tier");
            println!("{}", "-".repeat(84));
            for row in &rows {
                let notes = if row.notes.is_empty() {
                    "-".to_string()
                } else {
                    row.notes.join(",")
                };
                println!(
                    "{:28} {:>5} {:>10}  {}",
                    row.name, row.score, row.tier, notes
                );
            }

            let fallback: Vec<&SkillQualityRow> =
                rows.iter().filter(|row| row.score < 55).collect();
            if !fallback.is_empty() {
                println!("\nFallback recommendations:");
                for row in fallback {
                    println!(
                        "- {}: run `hermes skills update --apply` or reinstall from a trusted registry source.",
                        row.name
                    );
                }
            } else {
                println!("\nFallback recommendations: none (all tracked skills >= watch tier).");
            }
        }
        "audit" => {
            let scan_dir = name
                .as_ref()
                .map(PathBuf::from)
                .filter(|p| p.is_dir())
                .unwrap_or_else(|| skills_dir.clone());
            println!(
                "Security audit of installed skills ({})",
                scan_dir.display()
            );
            println!("==================================\n");
            if !scan_dir.exists() {
                println!("No skills directory at {}.", scan_dir.display());
                return Ok(());
            }

            struct AuditFinding {
                file: String,
                pattern: String,
                severity: &'static str, // "warning" or "critical"
            }

            let shell_injection_patterns: &[(&str, &str)] = &[
                (
                    r"(?i)\b(rm\s+-rf|mkfs|dd\s+if=)",
                    "Shell command injection (destructive command)",
                ),
                (r"(?i)(:\(\)\{.*;\}|fork\s+bomb)", "Fork bomb pattern"),
                (r"(?i)\b(sudo\s+|su\s+-\s)", "Privilege escalation attempt"),
                (
                    r"(?i)(export\s+PATH|PATH\s*=\s*/)",
                    "PATH environment manipulation",
                ),
                (
                    r"(?i)chmod\s+[0-7]*777",
                    "Overly permissive file permissions",
                ),
                (r"(?i)\beval\s*\(", "Dynamic code evaluation (eval)"),
                (r"(?i)\bexec\s*\(", "Dynamic code execution (exec)"),
                (
                    r"(?i)(os\.system|subprocess\.call|subprocess\.run|subprocess\.Popen)",
                    "Subprocess execution",
                ),
            ];

            let path_traversal_patterns: &[(&str, &str)] =
                &[(r"\.\.[\\/]", "Path traversal (../)")];

            let network_patterns: &[(&str, &str)] = &[
                (r"(?i)://127\.0\.0\.1", "Internal network URL (127.0.0.1)"),
                (r"(?i)://localhost", "Internal network URL (localhost)"),
                (
                    r"(?i)://10\.\d+\.\d+\.\d+",
                    "Internal network URL (10.x.x.x)",
                ),
                (
                    r"(?i)://192\.168\.\d+\.\d+",
                    "Internal network URL (192.168.x.x)",
                ),
                (r"(?i)://0\.0\.0\.0", "Internal network URL (0.0.0.0)"),
                (r"(?i)://\[::1\]", "Internal network URL (::1)"),
            ];

            let credential_patterns: &[(&str, &str)] = &[
                (
                    r#"(?i)(password\s*=\s*['"][^'"]{3,}['"])"#,
                    "Hardcoded password",
                ),
                (
                    r#"(?i)(api[_-]?key\s*=\s*['"][^'"]{3,}['"])"#,
                    "Hardcoded API key",
                ),
                (
                    r#"(?i)(secret\s*=\s*['"][^'"]{3,}['"])"#,
                    "Hardcoded secret",
                ),
                (r"(?i)(sk-[a-zA-Z0-9]{20,})", "Exposed API key (sk-...)"),
                (r"(?i)(ghp_[a-zA-Z0-9]{30,})", "Exposed GitHub PAT"),
            ];

            let base64_suspicious: &[(&str, &str)] = &[
                (
                    r"(?i)(base64[._-]?decode|atob)\s*\(",
                    "Base64 decode invocation (potential obfuscation)",
                ),
                (
                    r"[A-Za-z0-9+/]{100,}={0,2}",
                    "Long base64-encoded content (potential obfuscation)",
                ),
            ];

            let mut total = 0u32;
            let mut total_warnings = 0u32;
            let mut total_critical = 0u32;

            fn scan_dir_recursive(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
                if let Ok(entries) = std::fs::read_dir(dir) {
                    for entry in entries.filter_map(|e| e.ok()) {
                        let p = entry.path();
                        if p.is_dir() {
                            scan_dir_recursive(&p, files);
                        } else if p.is_file() {
                            files.push(p);
                        }
                    }
                }
            }

            if let Ok(entries) = std::fs::read_dir(&scan_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    total += 1;
                    let dir_name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let mut findings: Vec<AuditFinding> = Vec::new();

                    let mut all_files = Vec::new();
                    scan_dir_recursive(&path, &mut all_files);

                    for fp in &all_files {
                        let Ok(content) = std::fs::read_to_string(fp) else {
                            continue;
                        };
                        let fname = fp
                            .strip_prefix(&path)
                            .unwrap_or(fp)
                            .to_string_lossy()
                            .to_string();

                        // Shell injection (critical)
                        for (pat, desc) in shell_injection_patterns {
                            if let Ok(re) = Regex::new(pat) {
                                if re.is_match(&content) {
                                    findings.push(AuditFinding {
                                        file: fname.clone(),
                                        pattern: desc.to_string(),
                                        severity: "critical",
                                    });
                                }
                            }
                        }

                        // Path traversal (critical)
                        for (pat, desc) in path_traversal_patterns {
                            if let Ok(re) = Regex::new(pat) {
                                if re.is_match(&content) {
                                    findings.push(AuditFinding {
                                        file: fname.clone(),
                                        pattern: desc.to_string(),
                                        severity: "critical",
                                    });
                                }
                            }
                        }

                        // Internal network URLs (warning)
                        for (pat, desc) in network_patterns {
                            if let Ok(re) = Regex::new(pat) {
                                if re.is_match(&content) {
                                    findings.push(AuditFinding {
                                        file: fname.clone(),
                                        pattern: desc.to_string(),
                                        severity: "warning",
                                    });
                                }
                            }
                        }

                        // Credential patterns (critical)
                        for (pat, desc) in credential_patterns {
                            if let Ok(re) = Regex::new(pat) {
                                if re.is_match(&content) {
                                    findings.push(AuditFinding {
                                        file: fname.clone(),
                                        pattern: desc.to_string(),
                                        severity: "critical",
                                    });
                                }
                            }
                        }

                        // Base64 suspicious (warning)
                        for (pat, desc) in base64_suspicious {
                            if let Ok(re) = Regex::new(pat) {
                                if re.is_match(&content) {
                                    findings.push(AuditFinding {
                                        file: fname.clone(),
                                        pattern: desc.to_string(),
                                        severity: "warning",
                                    });
                                }
                            }
                        }
                    }

                    if findings.is_empty() {
                        println!("  ✓ {} — clean", dir_name);
                    } else {
                        let crit_count =
                            findings.iter().filter(|f| f.severity == "critical").count();
                        let warn_count =
                            findings.iter().filter(|f| f.severity == "warning").count();
                        total_critical += crit_count as u32;
                        total_warnings += warn_count as u32;

                        let icon = if crit_count > 0 { "✗" } else { "⚠" };
                        println!(
                            "  {} {} — {} critical, {} warning(s):",
                            icon, dir_name, crit_count, warn_count
                        );
                        for f in &findings {
                            let sev_icon = if f.severity == "critical" {
                                "CRIT"
                            } else {
                                "WARN"
                            };
                            println!("    [{}] {} — {}", sev_icon, f.file, f.pattern);
                        }
                    }
                }
            }

            println!("\n{}", "=".repeat(50));
            println!("Audited {} skill(s)", total);
            println!("  Critical: {}", total_critical);
            println!("  Warnings: {}", total_warnings);
            if total_critical == 0 && total_warnings == 0 {
                println!("  Status:   All clear ✓");
            } else if total_critical > 0 {
                println!("  Status:   Action required — review critical findings");
            } else {
                println!("  Status:   Review recommended");
            }
        }
        other => {
            println!("Skills action '{}' is not recognized.", other);
            println!(
                "Available actions: list, browse, search, install, inspect, uninstall, check, update, publish, snapshot, tap, config, quality, audit"
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skills_action_blocked_by_tier_enforces_expected_matrix() {
        assert!(skills_action_blocked_by_tier(
            SkillsExecutionTier::Trusted,
            "install",
            None
        ));
        assert!(skills_action_blocked_by_tier(
            SkillsExecutionTier::Trusted,
            "tap",
            Some("add")
        ));
        assert!(!skills_action_blocked_by_tier(
            SkillsExecutionTier::Trusted,
            "list",
            None
        ));
        assert!(skills_action_blocked_by_tier(
            SkillsExecutionTier::Balanced,
            "publish",
            None
        ));
        assert!(!skills_action_blocked_by_tier(
            SkillsExecutionTier::Balanced,
            "install",
            None
        ));
        assert!(!skills_action_blocked_by_tier(
            SkillsExecutionTier::Open,
            "publish",
            None
        ));
    }
}
