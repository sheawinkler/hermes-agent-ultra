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
    if hermes_agent::provider::is_openai_dynamic_model_alias(provider_model) {
        return None;
    }
    let (provider, model_id) = split_provider_model(provider_model);
    let provider = provider.trim().to_ascii_lowercase();
    if provider.is_empty() || model_id.trim().is_empty() {
        return None;
    }
    if model_id.trim().eq_ignore_ascii_case("dynamic")
        || provider_model.trim().eq_ignore_ascii_case("dynamic")
    {
        return None;
    }
    let runtime_provider = crate::app::normalize_runtime_provider_name(provider.as_str());
    if matches!(
        runtime_provider.as_str(),
        "openai" | "openai-codex" | "codex"
    ) && hermes_agent::provider::is_openai_dynamic_model_alias(model_id)
    {
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
            let active_dynamic_selector = active_model.trim().eq_ignore_ascii_case("dynamic")
                || active_model
                    .trim()
                    .to_ascii_lowercase()
                    .ends_with(":dynamic");
            if !active_dynamic_selector {
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
        "install" => include!("command_cli_chat_skills/install.rs"),
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
        "update" => include!("command_cli_chat_skills/update.rs"),
        "publish" => include!("command_cli_chat_skills/publish.rs"),
        "snapshot" => include!("command_cli_chat_skills/snapshot.rs"),
        "tap" => include!("command_cli_chat_skills/tap.rs"),
        "config" => include!("command_cli_chat_skills/config.rs"),
        "quality" => include!("command_cli_chat_skills/quality.rs"),
        "audit" => include!("command_cli_chat_skills/audit.rs"),
        other => {
            println!("Skills action '{}' is not recognized.", other);
            println!("Available actions: list, browse, search, install, inspect, uninstall, check, update, sync, opt-out, opt-in, publish, snapshot, tap, config, quality, audit");
        }
    }
    Ok(())
}
