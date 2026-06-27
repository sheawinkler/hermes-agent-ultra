// ---------------------------------------------------------------------------
// CLI subcommand handlers (dispatched from main.rs)
// ---------------------------------------------------------------------------

type ToolStartCallback = Box<dyn Fn(&str, &serde_json::Value) + Send + Sync>;
type ToolCompleteCallback = Box<dyn Fn(&str, &str) + Send + Sync>;

fn resolve_cli_chat_provider_model(
    config_model: Option<&str>,
    model_override: Option<&str>,
    provider_override: Option<&str>,
) -> Result<String, AgentError> {
    resolve_cli_chat_provider_model_with(
        config_model,
        model_override,
        provider_override,
        normalize_provider_model,
    )
}

async fn query_mode_remediation_target(provider_model: &str) -> Option<QueryModelRemediation> {
    let (provider, model_id) = split_provider_model(provider_model);
    let provider = provider.trim().to_ascii_lowercase();
    if provider.is_empty() || model_id.trim().is_empty() {
        return None;
    }
    let catalog = provider_model_ids(&provider).await;
    if catalog.is_empty() {
        return None;
    }
    query_mode_remediation_target_from_catalog(provider_model, &catalog)
}

/// Handle `hermes chat [--query ...] [--preload-skill ...] [--yolo]`.
pub async fn handle_cli_chat(
    query: Option<String>,
    preload_skill: Option<String>,
    yolo: bool,
    model_override: Option<String>,
    provider_override: Option<String>,
    allow_tools_flag: bool,
) -> Result<(), hermes_core::AgentError> {
    use crate::runtime_tool_wiring::{wire_cron_scheduler_backend, wire_stdio_clarify_backend};
    use crate::terminal_backend::build_terminal_backend;
    use hermes_cli_ui::tool_preview::{build_tool_preview_from_value, tool_emoji};
    use hermes_config::load_config;
    use hermes_skills::{FileSkillStore, SkillManager};
    use hermes_tools::ToolRegistry;

    if let Some(skill) = &preload_skill {
        println!("[Preloading skill: {}]", skill);
    }
    if yolo {
        println!("[YOLO mode: tool confirmations disabled]");
    }

    let mut config =
        load_config(None).map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;

    if yolo {
        config.approval.require_approval = false;
    }

    let query_mode = query.is_some();
    let tools_enabled = query_mode_tools_enabled(query_mode, allow_tools_flag);
    if query_mode && !tools_enabled {
        println!(
            "[Query mode tools are disabled by {}=1. Unset it or pass --allow-tools to re-enable.]",
            QUERY_DISABLE_TOOLS_ENV_KEY
        );
    }

    let current_model = resolve_cli_chat_provider_model(
        config.model.as_deref(),
        model_override.as_deref(),
        provider_override.as_deref(),
    )?;
    apply_cli_chat_runtime_env(&current_model);

    let tool_registry = Arc::new(ToolRegistry::new());
    let tool_schemas = if tools_enabled {
        let terminal_backend = build_terminal_backend(&config);
        let skill_store = Arc::new(FileSkillStore::new(hermes_config::skills_dir()));
        let skill_provider: Arc<dyn hermes_core::SkillProvider> =
            Arc::new(SkillManager::new(skill_store));
        hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);
        wire_stdio_clarify_backend(&tool_registry);
        let cron_data_dir = hermes_config::cron_dir();
        std::fs::create_dir_all(&cron_data_dir)
            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
        let cron_scheduler = Arc::new(build_runtime_cron_scheduler(
            &config,
            &current_model,
            cron_data_dir,
            &tool_registry,
        ));
        cron_scheduler
            .load_persisted_jobs()
            .await
            .map_err(|e| hermes_core::AgentError::Config(format!("cron load: {e}")))?;
        cron_scheduler.start().await;
        wire_cron_scheduler_backend(&tool_registry, cron_scheduler);
        hermes_tool_planning::resolve_platform_tool_schemas(&config, "cli", &tool_registry)
    } else {
        Vec::new()
    };
    let agent_tool_registry = Arc::new(crate::app::bridge_tool_registry(&tool_registry));

    let build_query_callbacks = || {
        let on_tool_start: ToolStartCallback = Box::new(move |name: &str, args: &serde_json::Value| {
                let emoji = tool_emoji(name);
                let preview = build_tool_preview_from_value(name, args, 56).unwrap_or_default();
                if preview.is_empty() {
                    println!("┊ {emoji} {name}");
                } else {
                    println!("┊ {emoji} {name:<16} {preview}");
                }
            });
        let on_tool_complete: ToolCompleteCallback = Box::new(move |name: &str, result: &str| {
                let mut snippet: String = result.trim().chars().take(96).collect();
                if result.trim().chars().count() > 96 {
                    snippet.push_str("...");
                }
                let emoji = tool_emoji(name);
                if snippet.is_empty() {
                    println!("┊ {emoji} {name:<16} done");
                } else {
                    println!("┊ {emoji} {name:<16} done: {snippet}");
                }
            });
        hermes_agent::AgentCallbacks {
            on_tool_start: Some(on_tool_start),
            on_tool_complete: Some(on_tool_complete),
            ..Default::default()
        }
    };

    match query {
        Some(q) => {
            let mut active_model = current_model.clone();
            if let Some(remediation) = query_mode_remediation_target(&active_model).await {
                println!(
                    "[Model remediation: {} -> {}. Close matches: {}]",
                    active_model,
                    remediation.next_model,
                    if remediation.close_matches.is_empty() {
                        "(none)".to_string()
                    } else {
                        remediation.close_matches.join(", ")
                    }
                );
                active_model = remediation.next_model;
            }
            let outcome = match run_noninteractive_query(
                &config,
                &active_model,
                &q,
                Arc::clone(&agent_tool_registry),
                tool_schemas,
                build_query_callbacks(),
                crate::app::build_provider,
            )
            .await
            {
                Ok(outcome) => outcome,
                Err(err) => {
                    if query_mode_model_not_found(&err) {
                        if let Some(remediation) =
                            query_mode_remediation_target(&active_model).await
                        {
                            return Err(hermes_core::AgentError::Config(format!(
                                "{}\nModel remediation suggestion: {} -> {} (close matches: {})",
                                err,
                                active_model,
                                remediation.next_model,
                                if remediation.close_matches.is_empty() {
                                    "(none)".to_string()
                                } else {
                                    remediation.close_matches.join(", ")
                                }
                            )));
                        }
                    }
                    return Err(err);
                }
            };
            println!("{}", outcome.reply);
        }
        None => {
            println!("Starting interactive chat session...");
            println!("(Use `hermes` for the default interactive TUI)");
        }
    }
    Ok(())
}

/// Handle `hermes skills [action] [name] [--extra ...]`.
fn repo_root_for_skill_sync() -> PathBuf {
    discover_repo_root_for_about().unwrap_or_else(|| {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .unwrap_or_else(|| Path::new("."))
            .to_path_buf()
    })
}

fn env_path_or_default(env_key: &str, default: PathBuf) -> PathBuf {
    std::env::var(env_key)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or(default)
}

fn skill_sync_config_for(skills_dir: &Path) -> hermes_skills::SkillSyncConfig {
    let repo_root = repo_root_for_skill_sync();
    let bundled_dir = env_path_or_default("HERMES_BUNDLED_SKILLS", repo_root.join("skills"));
    let optional_dir =
        env_path_or_default("HERMES_OPTIONAL_SKILLS", repo_root.join("optional-skills"));
    hermes_skills::SkillSyncConfig::new(bundled_dir, optional_dir, skills_dir.to_path_buf())
}

fn skills_extra_has_flag(extra: Option<&str>, flag: &str) -> bool {
    extra
        .unwrap_or_default()
        .split_whitespace()
        .any(|part| part == flag)
}

fn print_skill_sync_summary(result: &hermes_skills::SkillSyncResult) {
    if result.skipped_opt_out {
        println!(
            "Skipped bundled skill sync: profile opted out via {}.",
            hermes_skills::NO_BUNDLED_SKILLS_MARKER
        );
        return;
    }
    println!(
        "Skills sync complete: {} new, {} updated, {} unchanged, {} user-modified kept, {} manifest entries cleaned, {} total bundled.",
        result.copied.len(),
        result.updated.len(),
        result.skipped,
        result.user_modified.len(),
        result.cleaned.len(),
        result.total_bundled
    );
    if !result.copied.is_empty() {
        println!("  Copied: {}", result.copied.join(", "));
    }
    if !result.updated.is_empty() {
        println!("  Updated: {}", result.updated.join(", "));
    }
    if !result.user_modified.is_empty() {
        println!("  User-modified kept: {}", result.user_modified.join(", "));
    }
    if !result.collisions.is_empty() {
        println!("  Collisions kept: {}", result.collisions.join(", "));
    }
    if !result.errors.is_empty() {
        println!("  Errors: {}", result.errors.join("; "));
    }
}

fn confirm_pristine_skill_removal(count: usize) -> bool {
    print!(
        "Delete {} pristine bundled skill(s)? User-modified and local/hub skills are kept. [y/N]: ",
        count
    );
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    if std::io::stdin().read_line(&mut answer).is_err() {
        return false;
    }
    matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

pub async fn handle_cli_skills(
    action: Option<String>,
    name: Option<String>,
    extra: Option<String>,
    remove: bool,
    yes: bool,
    sync_now: bool,
) -> Result<(), hermes_core::AgentError> {
    let requested_action = action.as_deref().unwrap_or("list");
    if !skills_tier_bypass_enabled() {
        let tier = skills_execution_tier();
        let tier_name = match requested_action {
            "opt-out" if remove || skills_extra_has_flag(extra.as_deref(), "--remove") => {
                Some("--remove")
            }
            "opt-in" if sync_now || skills_extra_has_flag(extra.as_deref(), "--sync") => {
                Some("--sync")
            }
            _ => name.as_deref(),
        };
        let denied = skills_action_blocked_by_tier(tier, requested_action, tier_name);

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
            let skills = collect_local_skill_summaries(&skills_dir);
            println!("Installed skills ({}):", skills_dir.display());
            for summary in &skills {
                println!(
                    "  • {} — {}",
                    format_skill_display_name(summary),
                    summary.title
                );
            }
            if skills.is_empty() {
                println!("  (no skills installed)");
            }
        }
        "sync" => {
            let cfg = skill_sync_config_for(&skills_dir);
            let result = hermes_skills::sync_skills(&cfg, true)?;
            print_skill_sync_summary(&result);
        }
        "opt-out" => {
            let cfg = skill_sync_config_for(&skills_dir);
            let remove_pristine = remove || skills_extra_has_flag(extra.as_deref(), "--remove");
            let skip_confirm = yes
                || skills_extra_has_flag(extra.as_deref(), "--yes")
                || skills_extra_has_flag(extra.as_deref(), "-y");
            let marker = hermes_skills::set_bundled_skills_opt_out(&cfg.hermes_home(), true)?;
            println!("{}", marker.message);
            println!("Marker: {}", marker.marker);

            if !remove_pristine {
                println!(
                    "Existing skills on disk were left in place. Re-run with `--remove` to delete only unmodified bundled skills."
                );
                return Ok(());
            }

            let preview = hermes_skills::remove_pristine_bundled_skills(&cfg, true)?;
            if preview.removed.is_empty() {
                println!(
                    "No pristine bundled skills to remove (nothing tracked, or all are user-modified/local)."
                );
                if !preview.skipped.is_empty() {
                    println!("Kept {} skill(s).", preview.skipped.len());
                }
                return Ok(());
            }

            println!(
                "Will remove {} unmodified bundled skill(s): {}",
                preview.removed.len(),
                preview.removed.join(", ")
            );
            if !preview.skipped.is_empty() {
                let kept = preview
                    .skipped
                    .iter()
                    .map(|item| format!("{} ({})", item.name, item.reason))
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("Keeping {} skill(s): {}", preview.skipped.len(), kept);
            }
            if !skip_confirm && !confirm_pristine_skill_removal(preview.removed.len()) {
                println!("Marker kept; no skills deleted.");
                return Ok(());
            }

            let removed = hermes_skills::remove_pristine_bundled_skills(&cfg, false)?;
            println!("{}", removed.message);
            if !removed.removed.is_empty() {
                println!("Removed: {}", removed.removed.join(", "));
            }
            if !removed.skipped.is_empty() {
                let kept = removed
                    .skipped
                    .iter()
                    .map(|item| format!("{} ({})", item.name, item.reason))
                    .collect::<Vec<_>>()
                    .join(", ");
                println!("Kept: {}", kept);
            }
        }
        "opt-in" => {
            let cfg = skill_sync_config_for(&skills_dir);
            let sync_requested = sync_now || skills_extra_has_flag(extra.as_deref(), "--sync");
            let marker = hermes_skills::set_bundled_skills_opt_out(&cfg.hermes_home(), false)?;
            println!("{}", marker.message);
            println!("Marker: {}", marker.marker);
            if sync_requested {
                let result = hermes_skills::sync_skills(&cfg, true)?;
                print_skill_sync_summary(&result);
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

            if let Ok(results) = search_multi_registry(&client, &query, 40).await {
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
                if let Ok(skills_sh_hits) = search_skills_sh_registry(&client, &query, 20).await {
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
                let taps = effective_skill_taps(&taps_file, &subscriptions_file);
                let fallback = search_skills_via_taps(&client, &taps, &query, 20).await?;
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
            let (skill_name, _requested_version) = parse_skill_name_and_version(&skill_spec);
            println!("Installing skill: {}", skill_name);

            let client = reqwest::Client::new();
            let registry_prefixed = parse_registry_prefixed_skill(&skill_name);
            let explicit = if registry_prefixed.is_some() {
                None
            } else {
                parse_explicit_github_skill(&skill_name)
            };

            let (files, install_seed, provenance) = if let Some((source, key)) = registry_prefixed {
                match source.as_str() {
                    "official" => {
                        let install_key = key.clone();
                        let resolved = resolve_official_skill_source(&client, &key).await?;
                        println!(
                            "Resolved official source: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        );
                        (
                            fetch_skill_files_from_github(&client, &resolved).await?,
                            install_key,
                            SkillInstallProvenance {
                                source: "official".to_string(),
                                identifier: key.clone(),
                                trust_level: default_trust_level_for_source("official").to_string(),
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
                        let resolved = resolve_skills_sh_source(&client, &key).await?;
                        println!(
                            "Resolved skills.sh source: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        );
                        (
                            fetch_skill_files_from_github(&client, &resolved).await?,
                            install_key,
                            SkillInstallProvenance {
                                source: "skills.sh".to_string(),
                                identifier: key.clone(),
                                trust_level: default_trust_level_for_source("skills.sh")
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
                            fetch_lobehub_skill_files(&client, &key).await?,
                            key.clone(),
                            SkillInstallProvenance {
                                source: "lobehub".to_string(),
                                identifier: key,
                                trust_level: default_trust_level_for_source("lobehub").to_string(),
                                metadata: serde_json::json!({}),
                            },
                        )
                    }
                    "clawhub" => {
                        println!("Resolved clawhub source: {}", key);
                        (
                            fetch_clawhub_skill_files(&client, &key, _requested_version.as_deref())
                                .await?,
                            key.clone(),
                            SkillInstallProvenance {
                                source: "clawhub".to_string(),
                                identifier: key,
                                trust_level: default_trust_level_for_source("clawhub").to_string(),
                                metadata: serde_json::json!({}),
                            },
                        )
                    }
                    "claude-marketplace" => {
                        let install_key = key.clone();
                        let resolved = resolve_claude_marketplace_skill(&client, &key).await?;
                        println!(
                            "Resolved claude-marketplace source: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        );
                        (
                            fetch_skill_files_from_github(&client, &resolved).await?,
                            install_key,
                            SkillInstallProvenance {
                                source: "claude-marketplace".to_string(),
                                identifier: key.clone(),
                                trust_level: default_trust_level_for_source("claude-marketplace")
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
                        let (repo, maybe_branch, skill_dir) = parse_explicit_github_skill(&key)
                            .ok_or_else(|| {
                                AgentError::Config(format!(
                                    "github/ installs require owner/repo/path, got '{}'",
                                    key
                                ))
                            })?;
                        let branch = if let Some(branch) = maybe_branch {
                            branch
                        } else {
                            github_default_branch(&client, &repo).await?
                        };
                        let resolved = ResolvedSkillSource {
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
                            fetch_skill_files_from_github(&client, &resolved).await?,
                            key,
                            SkillInstallProvenance {
                                source: "github".to_string(),
                                identifier,
                                trust_level: default_trust_level_for_source("github").to_string(),
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
                        )))
                    }
                }
            } else if let Some((repo, maybe_branch, skill_dir)) = explicit {
                let branch = if let Some(branch) = maybe_branch {
                    branch
                } else {
                    github_default_branch(&client, &repo).await?
                };
                let resolved = ResolvedSkillSource {
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
                    fetch_skill_files_from_github(&client, &resolved).await?,
                    skill_name.clone(),
                    SkillInstallProvenance {
                        source: "github".to_string(),
                        identifier,
                        trust_level: default_trust_level_for_source("github").to_string(),
                        metadata: serde_json::json!({
                            "repo": resolved.repo,
                            "branch": resolved.branch,
                            "skill_dir": resolved.skill_dir,
                        }),
                    },
                )
            } else if let Some(skill_hint) = _requested_version
                .as_deref()
                .filter(|_| looks_like_github_repo_slug(&skill_name))
            {
                let resolved =
                    resolve_skill_in_repo(&client, &skill_name, skill_hint, Some("skills")).await?;
                println!(
                    "Resolved source: {}/{} @ {}",
                    resolved.repo, resolved.skill_dir, resolved.branch
                );
                (
                    fetch_skill_files_from_github(&client, &resolved).await?,
                    skill_name.clone(),
                    SkillInstallProvenance {
                        source: "github".to_string(),
                        identifier: format!("{}/{}", resolved.repo, resolved.skill_dir),
                        trust_level: default_trust_level_for_source("github").to_string(),
                        metadata: serde_json::json!({
                            "repo": resolved.repo,
                            "branch": resolved.branch,
                            "skill_dir": resolved.skill_dir,
                        }),
                    },
                )
            } else {
                let from_index = resolve_skill_via_registry_index(&client, &skill_name, None).await;
                if let Ok(hit) = from_index {
                    if hit.source.eq_ignore_ascii_case("official") {
                        let resolved =
                            resolve_official_skill_source(&client, &hit.identifier).await?;
                        println!(
                            "Resolved source [official]: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        );
                        (
                            fetch_skill_files_from_github(&client, &resolved).await?,
                            hit.identifier,
                            SkillInstallProvenance {
                                source: "official".to_string(),
                                identifier: format!("{}/{}", resolved.repo, resolved.skill_dir),
                                trust_level: default_trust_level_for_source("official").to_string(),
                                metadata: serde_json::json!({
                                    "repo": resolved.repo,
                                    "branch": resolved.branch,
                                    "skill_dir": resolved.skill_dir,
                                }),
                            },
                        )
                    } else {
                        match hit.install_source {
                            RegistryInstallSource::GitRepo(resolved) => {
                                let branch = github_default_branch(&client, &resolved.repo).await?;
                                let resolved = ResolvedSkillSource { branch, ..resolved };
                                println!(
                                    "Resolved source [{}]: {}/{} @ {}",
                                    hit.source, resolved.repo, resolved.skill_dir, resolved.branch
                                );
                                (
                                    fetch_skill_files_from_github(&client, &resolved).await?,
                                    hit.identifier,
                                    SkillInstallProvenance {
                                        source: hit.source,
                                        identifier: format!(
                                            "{}/{}",
                                            resolved.repo, resolved.skill_dir
                                        ),
                                        trust_level: default_trust_level_for_source("github")
                                            .to_string(),
                                        metadata: serde_json::json!({
                                            "repo": resolved.repo,
                                            "branch": resolved.branch,
                                            "skill_dir": resolved.skill_dir,
                                        }),
                                    },
                                )
                            }
                            RegistryInstallSource::LobeRegistry { slug } => {
                                println!("Resolved source [lobehub]: {}", slug);
                                (
                                    fetch_lobehub_skill_files(&client, &slug).await?,
                                    slug.clone(),
                                    SkillInstallProvenance {
                                        source: "lobehub".to_string(),
                                        identifier: slug,
                                        trust_level: default_trust_level_for_source("lobehub")
                                            .to_string(),
                                        metadata: serde_json::json!({}),
                                    },
                                )
                            }
                            RegistryInstallSource::ClawRegistry { slug, version } => {
                                println!("Resolved source [clawhub]: {}", slug);
                                (
                                    fetch_clawhub_skill_files(&client, &slug, version.as_deref())
                                        .await?,
                                    slug.clone(),
                                    SkillInstallProvenance {
                                        source: "clawhub".to_string(),
                                        identifier: slug,
                                        trust_level: default_trust_level_for_source("clawhub")
                                            .to_string(),
                                        metadata: serde_json::json!({ "version_hint": version }),
                                    },
                                )
                            }
                        }
                    }
                } else {
                    let taps_file = hermes_config::hermes_home().join("skill_taps.json");
                    let subscriptions_file = skills_dir.join("subscriptions.json");
                    let taps = effective_skill_taps(&taps_file, &subscriptions_file);
                    let (resolved, route) =
                        resolve_install_via_fallback_router(&client, &skill_name, &taps).await?;
                    match route {
                        InstallFallbackSource::SkillsSh => println!(
                            "Resolved source [skills.sh fallback]: {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        ),
                        InstallFallbackSource::Tap => println!(
                            "Resolved source (tap): {}/{} @ {}",
                            resolved.repo, resolved.skill_dir, resolved.branch
                        ),
                    }
                    (
                        fetch_skill_files_from_github(&client, &resolved).await?,
                        skill_name.clone(),
                        SkillInstallProvenance {
                            source: match route {
                                InstallFallbackSource::SkillsSh => "skills.sh".to_string(),
                                InstallFallbackSource::Tap => "tap".to_string(),
                            },
                            identifier: format!("{}/{}", resolved.repo, resolved.skill_dir),
                            trust_level: default_trust_level_for_source(match route {
                                InstallFallbackSource::SkillsSh => "skills.sh",
                                InstallFallbackSource::Tap => "tap",
                            })
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

            let install_name = sanitize_skill_install_name(
                _requested_version
                    .as_deref()
                    .filter(|_| looks_like_github_repo_slug(&skill_name))
                    .unwrap_or(install_seed.as_str()),
            );
            let target = install_skill_files(&skills_dir, &install_name, &files)?;
            record_skill_install_in_hub_lock(
                &skills_dir,
                &install_name,
                &target,
                &files,
                &provenance,
            )?;
            println!("Skill '{}' installed to {}", install_name, target.display());
            maybe_run_skill_bootstrap(&install_name, &target, &files).await?;
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
            if let Some(skill_md) = find_local_skill_markdown(&skills_dir, &skill_name) {
                let content = std::fs::read_to_string(&skill_md)
                    .map_err(|e| hermes_core::AgentError::Io(format!("Read error: {}", e)))?;
                println!("{}", content);
            } else {
                println!(
                    "Skill '{}' not found under {}",
                    skill_name,
                    skills_dir.display()
                );
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
                let removed = record_skill_uninstall_in_hub_lock(&skills_dir, &skill_name)?;
                if let Some(entry) = removed {
                    println!(
                        "Skill '{}' uninstalled (source={}, id={}).",
                        skill_name, entry.source, entry.identifier
                    );
                } else {
                    println!("Skill '{}' uninstalled.", skill_name);
                }
            } else if let Some(entry) =
                record_skill_uninstall_in_hub_lock(&skills_dir, &skill_name)?
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
            let lock = read_skills_hub_lock(&skills_dir);
            if lock.installed.is_empty() {
                println!(
                    "No hub-installed skills tracked in {}.",
                    skills_hub_lock_path(&skills_dir).display()
                );
                println!("Install skills with `hermes skills install <identifier>` to enable source-aware updates.");
                return Ok(());
            }

            println!(
                "{:28} {:14} {:14} {:16} Status",
                "Skill", "Source", "Local Hash", "Upstream Hash"
            );
            println!("{}", "-".repeat(98));

            let taps_file = hermes_config::hermes_home().join("skill_taps.json");
            let subscriptions_file = skills_dir.join("subscriptions.json");
            let merged_taps = effective_skill_taps(&taps_file, &subscriptions_file);
            let client = reqwest::Client::new();

            struct PendingUpdate {
                entry: SkillHubInstalledEntry,
                files: Vec<(String, Bytes)>,
                upstream_hash: String,
            }
            let mut pending: Vec<PendingUpdate> = Vec::new();

            for entry in lock.installed {
                let local_hash = if skills_dir.join(&entry.install_path).exists() {
                    hash_installed_skill_dir(&skills_dir.join(&entry.install_path))
                        .unwrap_or_else(|_| entry.content_hash.clone())
                } else {
                    entry.content_hash.clone()
                };

                match fetch_bundle_for_lock_entry(&client, &entry, &merged_taps).await {
                    Ok(files) => {
                        let upstream_hash = hash_skill_bundle(&files);
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
                        let install_name = sanitize_skill_install_name(&update.entry.name);
                        let target =
                            install_skill_files(&skills_dir, &install_name, &update.files)?;
                        let prov = SkillInstallProvenance {
                            source: update.entry.source.clone(),
                            identifier: update.entry.identifier.clone(),
                            trust_level: update.entry.trust_level.clone(),
                            metadata: update.entry.metadata.clone(),
                        };
                        record_skill_install_in_hub_lock(
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
                        maybe_run_skill_bootstrap(&install_name, &target, &update.files).await?;
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
                    let taps = effective_skill_taps(&taps_file, &subscriptions_file);
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
                    let mut taps: Vec<String> = read_skill_taps(&taps_file);
                    if effective_skill_taps(&taps_file, &subscriptions_file).contains(&url) {
                        println!("Tap already exists: {}", url);
                    } else {
                        taps.push(url.clone());
                        write_skill_taps(&taps_file, &taps)?;
                        println!("Added tap: {}", url);
                    }
                }
                "remove" => {
                    let url = extra.ok_or_else(|| {
                        hermes_core::AgentError::Config(
                            "Missing tap URL. Usage: hermes skills tap remove <url>".into(),
                        )
                    })?;
                    if DEFAULT_SKILL_TAPS
                        .iter()
                        .any(|default_tap| default_tap == &url.as_str())
                    {
                        println!("Tap '{}' is a built-in default and cannot be removed.", url);
                        println!(
                            "Add custom taps with `hermes skills tap add <url>`; defaults remain active."
                        );
                        return Ok(());
                    }

                    let mut taps: Vec<String> = read_skill_taps(&taps_file);
                    let before_len = taps.len();
                    taps.retain(|t| t != &url);
                    if taps.len() < before_len {
                        write_skill_taps(&taps_file, &taps)?;
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
            println!("Security audit of installed skills");
            println!("==================================\n");
            if !skills_dir.exists() {
                println!("No skills installed.");
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

            if let Ok(entries) = std::fs::read_dir(&skills_dir) {
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
            println!("Available actions: list, browse, search, install, inspect, uninstall, check, update, sync, opt-out, opt-in, publish, snapshot, tap, config, quality, audit");
        }
    }
    Ok(())
}
