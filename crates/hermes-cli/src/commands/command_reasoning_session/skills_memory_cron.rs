#[derive(Debug, Deserialize)]
struct SkillBundleManifest {
    name: Option<String>,
    description: Option<String>,
    skills: Option<Vec<String>>,
}

#[derive(Debug, Clone)]
struct SkillBundleSummary {
    slug: String,
    description: String,
    skills: Vec<String>,
}

fn slugify_skill_bundle_name(name: &str) -> String {
    let mut slug = String::new();
    let mut last_was_dash = false;
    for ch in name.trim().to_ascii_lowercase().chars() {
        let normalized = if ch.is_ascii_alphanumeric() {
            Some(ch)
        } else if matches!(ch, '-' | '_' | ' ') {
            Some('-')
        } else {
            None
        };
        if let Some(ch) = normalized {
            if ch == '-' {
                if !last_was_dash && !slug.is_empty() {
                    slug.push('-');
                    last_was_dash = true;
                }
            } else {
                slug.push(ch);
                last_was_dash = false;
            }
        }
    }
    slug.trim_matches('-').to_string()
}

fn list_skill_bundles(root: &Path) -> Vec<SkillBundleSummary> {
    let dir = root.join("skill-bundles");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut bundles = Vec::new();
    let mut seen = HashSet::new();
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Some(ext) = path.extension().and_then(|v| v.to_str()) else {
            continue;
        };
        if !matches!(ext, "yaml" | "yml") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(manifest) = serde_yaml::from_str::<SkillBundleManifest>(&raw) else {
            continue;
        };
        let name = manifest
            .name
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| {
                path.file_stem()
                    .and_then(|v| v.to_str())
                    .unwrap_or("bundle")
                    .to_string()
            });
        let slug = slugify_skill_bundle_name(&name);
        if slug.is_empty() || !seen.insert(slug.clone()) {
            continue;
        }
        let skills: Vec<String> = manifest
            .skills
            .unwrap_or_default()
            .into_iter()
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();
        if skills.is_empty() {
            continue;
        }
        let description = manifest
            .description
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| format!("Load {} skills as a bundle", skills.len()));
        bundles.push(SkillBundleSummary {
            slug,
            description,
            skills,
        });
    }
    bundles.sort_by(|a, b| a.slug.cmp(&b.slug));
    bundles
}

fn handle_bundles_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let bundles = list_skill_bundles(&app.state_root);
    if bundles.is_empty() {
        emit_command_output(
            app,
            format!(
                "No skill bundles installed.\nCreate one with `hermes bundles create <name> --skill <s1> --skill <s2>`.\nDirectory: {}",
                app.state_root.join("skill-bundles").display()
            ),
        );
        return Ok(CommandResult::Handled);
    }

    let mut out = format!("Skill Bundles ({} installed):\n", bundles.len());
    for bundle in bundles {
        let _ = writeln!(
            out,
            "  - /{} -- {} ({} skills)",
            bundle.slug,
            bundle.description,
            bundle.skills.len()
        );
        for skill in bundle.skills {
            let _ = writeln!(out, "      - {}", skill);
        }
    }
    out.push_str("\nInvoke a bundle with `/<slug>` to load all its skills.");
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn handle_plugins_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let rows = discover_plugin_surface(true);
    if rows.is_empty() {
        let plugins_dir = hermes_config::hermes_home().join("plugins");
        emit_command_output(
            app,
            format!(
                "No plugin bundles discovered.\nUser plugin dir: {}\nInstall with `hermes plugins install <owner/repo>`.",
                plugins_dir.display()
            ),
        );
    } else {
        emit_command_output(
            app,
            format!(
                "Plugin surface ({} entries):\n{}",
                rows.len(),
                render_plugin_surface_table(&rows)
            ),
        );
    }
    Ok(CommandResult::Handled)
}

fn handle_disk_cleanup_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let cleaner = hermes_tools::tools::disk_cleanup::DiskCleanup::new(app.state_root.clone());
    let output = hermes_tools::tools::disk_cleanup::handle_slash_args(&cleaner, args);
    emit_command_output(app, output);
    Ok(CommandResult::Handled)
}

fn render_mcp_runtime_status(
    yaml_servers: &[hermes_config::McpServerEntry],
    json_config: Option<&crate::mcp_config::McpConfig>,
    json_path: &Path,
) -> String {
    let json_servers = json_config.map(|cfg| cfg.servers.as_slice()).unwrap_or(&[]);
    let mut names = HashSet::new();
    for server in yaml_servers {
        names.insert(server.name.clone());
    }
    for server in json_servers {
        names.insert(server.name.clone());
    }

    if names.is_empty() {
        return format!(
            "No MCP servers configured.\n  config.yaml entries: 0\n  mcp_servers.json: {} ({})\nAdd one with `hermes mcp add <name> --url <url>` or `hermes mcp add <name> --command <cmd>`.",
            if json_path.exists() { "present" } else { "missing" },
            json_path.display()
        );
    }

    let mut out = String::new();
    let _ = writeln!(out, "MCP runtime status");
    let _ = writeln!(out, "  config.yaml entries: {}", yaml_servers.len());
    let _ = writeln!(
        out,
        "  mcp_servers.json entries: {} ({})",
        json_servers.len(),
        json_path.display()
    );

    if let Some(config) = json_config {
        for warning in config.warnings() {
            let _ = writeln!(out, "  warning: {warning}");
        }
    }

    let yaml_names: HashSet<_> = yaml_servers
        .iter()
        .map(|server| server.name.as_str())
        .collect();
    let json_names: HashSet<_> = json_servers
        .iter()
        .map(|server| server.name.as_str())
        .collect();
    let mut sorted: Vec<_> = names.into_iter().collect();
    sorted.sort();

    out.push_str("Configured MCP servers:\n");
    for name in sorted {
        if let Some(server) = yaml_servers.iter().find(|server| server.name == name) {
            let endpoint = server
                .url
                .as_deref()
                .filter(|u| !u.is_empty())
                .or(server.command.as_deref())
                .unwrap_or("<stdio>");
            let _ = writeln!(
                out,
                "  - {:<18} {}  [source:config.yaml; parallel_tool_calls:{}; keepalive:{}]",
                server.name,
                endpoint,
                if server.supports_parallel_tool_calls {
                    "on"
                } else {
                    "off"
                },
                server
                    .keepalive_interval
                    .map(|secs| format!("{secs}s"))
                    .unwrap_or_else(|| "default".to_string())
            );
        }
        if let Some(server) = json_servers.iter().find(|server| server.name == name) {
            let _ = writeln!(
                out,
                "  - {:<18} {}  [source:mcp_servers.json; {}; enabled:{}; parallel_tool_calls:{}; keepalive:{}]",
                server.name,
                server.transport_display(),
                server.transport_kind().as_str(),
                if server.enabled { "on" } else { "off" },
                if server.supports_parallel_tool_calls {
                    "on"
                } else {
                    "off"
                },
                server
                    .keepalive_interval
                    .map(|secs| format!("{secs}s"))
                    .unwrap_or_else(|| "default".to_string())
            );
        }
    }

    let mut yaml_only: Vec<_> = yaml_names.difference(&json_names).copied().collect();
    let mut json_only: Vec<_> = json_names.difference(&yaml_names).copied().collect();
    yaml_only.sort();
    json_only.sort();
    if !yaml_only.is_empty() || !json_only.is_empty() {
        let _ = writeln!(
            out,
            "Drift: config_only=[{}] json_only=[{}]",
            yaml_only.join(","),
            json_only.join(",")
        );
    }

    out.trim_end().to_string()
}

fn handle_mcp_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let mcp_config_path = app.state_root.join("mcp_servers.json");
    let json_config = load_mcp_config_if_exists(&mcp_config_path)?;
    let out = render_mcp_runtime_status(
        &app.config.mcp_servers,
        json_config.as_ref(),
        &mcp_config_path,
    );
    emit_command_output(app, out);
    Ok(CommandResult::Handled)
}

fn render_memory_backend_status(hermes_home: &Path) -> String {
    let memories_dir = hermes_home.join("memories");
    let memory_md = memories_dir.join("MEMORY.md");
    let user_md = memories_dir.join("USER.md");
    let legacy_memory_db = hermes_home.join("memory.db");
    let disabled_marker = hermes_home.join(".memory_disabled");
    let mut out = String::new();

    if disabled_marker.exists() {
        out.push_str("Memory provider: disabled\n");
        let _ = writeln!(out, "  Marker: {}", disabled_marker.display());
        out.push_str("Run `hermes memory setup` to re-enable.");
        return out;
    }

    if memory_md.exists() || user_md.exists() {
        let mem_size = std::fs::metadata(&memory_md).map(|m| m.len()).unwrap_or(0);
        let user_size = std::fs::metadata(&user_md).map(|m| m.len()).unwrap_or(0);
        out.push_str("Memory provider: files (MEMORY.md + USER.md)\n");
        let _ = writeln!(out, "  Directory: {}", memories_dir.display());
        let _ = writeln!(
            out,
            "  MEMORY.md: {} ({:.1} KB)",
            memory_md.display(),
            mem_size as f64 / 1024.0
        );
        let _ = writeln!(
            out,
            "  USER.md:   {} ({:.1} KB)",
            user_md.display(),
            user_size as f64 / 1024.0
        );
        if legacy_memory_db.exists() {
            let _ = writeln!(
                out,
                "  Legacy file detected (unused by current memory backend): {}",
                legacy_memory_db.display()
            );
        }
        return out.trim_end().to_string();
    }

    if legacy_memory_db.exists() {
        let size = std::fs::metadata(&legacy_memory_db)
            .map(|m| m.len())
            .unwrap_or(0);
        out.push_str("Memory provider: legacy sqlite artifact only\n");
        let _ = writeln!(out, "  File: {}", legacy_memory_db.display());
        let _ = writeln!(out, "  Size: {} KB", size / 1024);
        out.push_str("Run `hermes memory setup` to initialize the current file backend.");
        return out;
    }

    out.push_str("Memory provider: not configured\n");
    out.push_str("Run `hermes memory setup` to initialize.");
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LocalSkillSummary {
    name: String,
    title: String,
    relative_dir: String,
    skill_md: PathBuf,
}

fn collect_local_skill_summaries(skills_dir: &Path) -> Vec<LocalSkillSummary> {
    let mut summaries = Vec::new();
    collect_local_skill_summaries_rec(skills_dir, skills_dir, &mut summaries);
    summaries.sort_by(|a, b| {
        a.name
            .cmp(&b.name)
            .then_with(|| a.relative_dir.cmp(&b.relative_dir))
    });
    summaries
}

fn collect_local_skill_summaries_rec(root: &Path, dir: &Path, out: &mut Vec<LocalSkillSummary>) {
    let skill_md = dir.join("SKILL.md");
    if skill_md.exists() {
        if let Some(summary) = read_local_skill_summary(root, &skill_md) {
            out.push(summary);
        }
        return;
    }

    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name();
        if name.to_string_lossy().starts_with('.') {
            continue;
        }
        collect_local_skill_summaries_rec(root, &entry.path(), out);
    }
}

fn read_local_skill_summary(root: &Path, skill_md: &Path) -> Option<LocalSkillSummary> {
    let content = std::fs::read_to_string(skill_md).ok()?;
    let parent = skill_md.parent()?;
    let fallback_name = parent.file_name()?.to_string_lossy().to_string();
    let relative_dir = parent
        .strip_prefix(root)
        .ok()
        .map(path_to_forward_slashes)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| fallback_name.clone());
    let name = frontmatter_value(&content, "name").unwrap_or(fallback_name);
    let title = frontmatter_value(&content, "description")
        .or_else(|| {
            content
                .lines()
                .find(|line| line.starts_with('#'))
                .map(|line| line.trim_start_matches('#').trim().to_string())
        })
        .filter(|line| !line.trim().is_empty())
        .unwrap_or_else(|| "(no description)".to_string());

    Some(LocalSkillSummary {
        name,
        title,
        relative_dir,
        skill_md: skill_md.to_path_buf(),
    })
}

fn frontmatter_value(content: &str, key: &str) -> Option<String> {
    let mut lines = content.lines();
    if lines.next()?.trim() != "---" {
        return None;
    }

    let needle = format!("{}:", key);
    for line in lines {
        let trimmed = line.trim();
        if trimmed == "---" {
            break;
        }
        let Some(value) = trimmed.strip_prefix(&needle) else {
            continue;
        };
        let value = value
            .trim()
            .trim_matches('"')
            .trim_matches('\'')
            .trim()
            .to_string();
        if !value.is_empty() {
            return Some(value);
        }
    }
    None
}

fn find_local_skill_markdown(skills_dir: &Path, query: &str) -> Option<PathBuf> {
    let query = query.trim().trim_start_matches('/');
    if query.is_empty() {
        return None;
    }

    collect_local_skill_summaries(skills_dir)
        .into_iter()
        .find(|summary| local_skill_summary_matches(summary, query))
        .map(|summary| summary.skill_md)
}

fn local_skill_summary_matches(summary: &LocalSkillSummary, query: &str) -> bool {
    summary.name == query
        || summary.relative_dir == query
        || summary
            .skill_md
            .parent()
            .and_then(|dir| dir.file_name())
            .map(|name| name.to_string_lossy() == query)
            .unwrap_or(false)
        || summary.name.eq_ignore_ascii_case(query)
        || summary.relative_dir.eq_ignore_ascii_case(query)
}

fn format_skill_display_name(summary: &LocalSkillSummary) -> String {
    if summary.relative_dir == summary.name {
        summary.name.clone()
    } else {
        format!("{} ({})", summary.name, summary.relative_dir)
    }
}

fn path_to_forward_slashes(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(part) => Some(part.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn handle_memory_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args.first().copied().unwrap_or("pending");
    match action {
        "status" => {
            emit_command_output(app, render_memory_backend_status(&app.state_root));
        }
        "pending" => {
            let mut out = String::from("memory.write_approval = off\n\nNo pending memory writes.");
            out.push_str("\n\n");
            out.push_str(&render_memory_backend_status(&app.state_root));
            emit_command_output(app, out.trim_end());
        }
        "setup" | "off" | "reset" => {
            emit_command_output(
                app,
                "Use `hermes memory status|setup|off|reset` outside the chat session for memory backend changes. Slash `/memory` is read-only: `/memory [status|pending]`.",
            );
        }
        _ => {
            emit_command_output(app, "Usage: /memory [status|pending]");
        }
    }
    Ok(CommandResult::Handled)
}

fn handle_reload_command(app: &mut App, cmd: &str) -> Result<CommandResult, AgentError> {
    if cmd == "/reload-mcp" {
        let refresh = app.refresh_agent_tool_snapshot();
        let mut out = format!(
            "MCP reload complete: refreshed agent tool snapshot ({} -> {} tools).",
            refresh.before_count, refresh.after_count
        );
        if refresh.changed() {
            if !refresh.added.is_empty() {
                let _ = write!(out, "\nAdded: {}", refresh.added.join(", "));
            }
            if !refresh.removed.is_empty() {
                let _ = write!(out, "\nRemoved: {}", refresh.removed.join(", "));
            }
        } else {
            out.push_str("\nNo tool changes detected.");
        }
        out.push_str("\nConnector renegotiation still requires a process restart.");
        emit_command_output(app, out);
    } else if cmd == "/reload-skills" {
        let config = SkillCommandResolverConfig {
            enabled: app.config.skills.enabled.clone(),
            disabled: app.config.skills.disabled.clone(),
            ..SkillCommandResolverConfig::default()
        };
        let snapshot = installed_skill_slash_command_snapshot(&config);
        app.queue_next_turn_system_note(build_skill_reload_system_note(&snapshot));
        emit_command_output(app, render_skill_slash_command_snapshot(&snapshot));
    } else {
        hermes_config::loader::load_dotenv();
        match hermes_config::load_config(app.state_root.to_str()) {
            Ok(cfg) => {
                app.config = Arc::new(cfg);
                emit_command_output(
                    app,
                    "Reload complete: env + config rehydrated for this session.",
                );
            }
            Err(err) => {
                emit_command_output(
                    app,
                    format!(
                        "Reload partially applied (.env refreshed), but config parse failed: {}",
                        err
                    ),
                );
            }
        }
    }
    Ok(CommandResult::Handled)
}

fn handle_cron_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let cron_data = hermes_config::cron_dir();
    let jobs_file = cron_data.join("jobs.json");
    let count = std::fs::read_to_string(&jobs_file)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_array().map(|arr| arr.len()))
        .unwrap_or(0);
    emit_command_output(
        app,
        format!(
            "Cron scheduler data dir: {}\nPersisted jobs: {}\nUse `hermes cron list` for full job table.",
            cron_data.display(),
            count
        ),
    );
    Ok(CommandResult::Handled)
}

fn blueprint_deliver_config(raw: &str) -> Option<DeliverConfig> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Some(DeliverConfig {
            target: DeliverTarget::Origin,
            platform: None,
        });
    }
    let (target_raw, platform) = raw
        .split_once(':')
        .map(|(target, platform)| (target, Some(platform.trim().to_string())))
        .unwrap_or((raw, None));
    let normalized = target_raw
        .trim()
        .to_ascii_lowercase()
        .replace(['-', '_'], "");
    let target = match normalized.as_str() {
        "origin" => DeliverTarget::Origin,
        "local" => DeliverTarget::Local,
        "telegram" => DeliverTarget::Telegram,
        "discord" => DeliverTarget::Discord,
        "slack" => DeliverTarget::Slack,
        "email" => DeliverTarget::Email,
        "whatsapp" => DeliverTarget::WhatsApp,
        "signal" => DeliverTarget::Signal,
        "matrix" => DeliverTarget::Matrix,
        "mattermost" => DeliverTarget::Mattermost,
        "dingtalk" => DeliverTarget::DingTalk,
        "feishu" => DeliverTarget::Feishu,
        "wecom" => DeliverTarget::WeCom,
        "weixin" | "wechat" | "wx" => DeliverTarget::Weixin,
        "bluebubbles" | "imessage" => DeliverTarget::BlueBubbles,
        "sms" => DeliverTarget::Sms,
        "homeassistant" | "ha" => DeliverTarget::HomeAssistant,
        "ntfy" => DeliverTarget::Ntfy,
        _ => return None,
    };
    Some(DeliverConfig {
        target,
        platform: platform.filter(|value| !value.trim().is_empty()),
    })
}

async fn handle_blueprint_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let raw = args.join(" ");
    match hermes_cron::resolve_blueprint_command(&raw) {
        Ok(BlueprintCommandAction::Catalog(text) | BlueprintCommandAction::Detail(text)) => {
            emit_command_output(app, text);
        }
        Ok(BlueprintCommandAction::Filled(spec)) => {
            let Some(deliver) = blueprint_deliver_config(&spec.deliver) else {
                emit_command_output(
                    app,
                    format!(
                        "Blueprint `{}` has unsupported deliver target `{}`.",
                        spec.key, spec.deliver
                    ),
                );
                return Ok(CommandResult::Handled);
            };

            let mut job = CronJob::new(spec.schedule.clone(), spec.prompt.clone());
            job.name = Some(spec.title.clone());
            if !spec.skills.is_empty() {
                job.skills = Some(spec.skills.clone());
            }
            job.deliver = Some(deliver);
            let job_id = app
                .cron_scheduler
                .create_job(job)
                .await
                .map_err(|e| AgentError::Config(format!("blueprint cron create: {e}")))?;
            emit_command_output(
                app,
                format!(
                    "Scheduled `{}` from blueprint `{}`.\nJob: {}\nSchedule: {}\nDeliver: {}\nManage it with `hermes cron list` or `/cron`.",
                    spec.title, spec.key, job_id, spec.schedule, spec.deliver
                ),
            );
        }
        Err(err) => {
            emit_command_output(
                app,
                format!("Blueprint error: {err}\nRun `/blueprint` to see the catalog."),
            );
        }
    }
    Ok(CommandResult::Handled)
}

fn suggestion_error(err: hermes_cron::SuggestionError) -> AgentError {
    AgentError::Config(format!("suggestions: {err}"))
}

fn render_pending_suggestions(pending: &[hermes_cron::SuggestionRecord]) -> String {
    if pending.is_empty() {
        return "No suggested automations right now.\nTry `/suggestions catalog` to see the curated starter set, or install a blueprint skill to get one.".to_string();
    }

    let mut out = String::from("Suggested automations - `/suggestions accept N` or `dismiss N`:\n");
    for (idx, suggestion) in pending.iter().enumerate() {
        let _ = writeln!(
            out,
            "\n  {}. {}  [{}]  ({})",
            idx + 1,
            suggestion.title,
            suggestion.job_spec.schedule,
            suggestion.source
        );
        if !suggestion.description.trim().is_empty() {
            let _ = writeln!(out, "     {}", suggestion.description.trim());
        }
    }
    out
}

fn render_suggestions_usage() -> &'static str {
    "Usage:\n  /suggestions              list pending\n  /suggestions accept N     schedule suggestion N\n  /suggestions dismiss N    dismiss suggestion N\n  /suggestions catalog      add curated starter automations\n  /suggestions clear        housekeeping"
}

fn cron_job_from_suggestion_spec(spec: &SuggestionJobSpec) -> Result<CronJob, String> {
    let Some(deliver) = blueprint_deliver_config(&spec.deliver) else {
        return Err(format!(
            "unsupported deliver target `{}`",
            spec.deliver.trim()
        ));
    };
    let mut job = CronJob::new(spec.schedule.clone(), spec.prompt.clone());
    job.name = Some(spec.name.clone());
    job.deliver = Some(deliver);
    if !spec.skills.is_empty() {
        job.skills = Some(spec.skills.clone());
    }
    Ok(job)
}

async fn handle_suggestions_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let store = hermes_cron::SuggestionStore::default();
    let sub = args
        .first()
        .map(|arg| arg.to_ascii_lowercase())
        .unwrap_or_default();
    let rest = args.get(1..).unwrap_or_default().join(" ");

    match sub.as_str() {
        "" => {
            let pending = store.list_pending().map_err(suggestion_error)?;
            emit_command_output(app, render_pending_suggestions(&pending));
        }
        "accept" | "add" | "schedule" => {
            if rest.trim().is_empty() {
                emit_command_output(app, "Usage: /suggestions accept <number|id>");
                return Ok(CommandResult::Handled);
            }
            let Some(suggestion) = store.get_pending(&rest).map_err(suggestion_error)? else {
                emit_command_output(
                    app,
                    format!(
                        "No pending suggestion matches '{}'. Run /suggestions to list them.",
                        rest.trim()
                    ),
                );
                return Ok(CommandResult::Handled);
            };
            let job = match cron_job_from_suggestion_spec(&suggestion.job_spec) {
                Ok(job) => job,
                Err(err) => {
                    emit_command_output(
                        app,
                        format!(
                            "Suggestion `{}` cannot be scheduled: {err}.",
                            suggestion.title
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
            };
            let job_id = app
                .cron_scheduler
                .create_job(job)
                .await
                .map_err(|e| AgentError::Config(format!("suggestion cron create: {e}")))?;
            store
                .mark_accepted(&suggestion.id)
                .map_err(suggestion_error)?;
            emit_command_output(
                app,
                format!(
                    "Scheduled '{}' ({}).\nJob: {}\nManage it with /cron.",
                    suggestion.job_spec.name, suggestion.job_spec.schedule, job_id
                ),
            );
        }
        "dismiss" | "no" | "reject" => {
            if rest.trim().is_empty() {
                emit_command_output(app, "Usage: /suggestions dismiss <number|id>");
                return Ok(CommandResult::Handled);
            }
            let dismissed = store.dismiss_suggestion(&rest).map_err(suggestion_error)?;
            if dismissed {
                emit_command_output(app, "Dismissed. Won't suggest that again.");
            } else {
                emit_command_output(
                    app,
                    format!("No pending suggestion matches '{}'.", rest.trim()),
                );
            }
        }
        "catalog" => {
            let created = store.seed_catalog_suggestions().map_err(suggestion_error)?;
            if created.is_empty() {
                emit_command_output(
                    app,
                    "No new catalog automations to add (already offered, dismissed, or your suggestion list is full). Run /suggestions to see pending.",
                );
            } else {
                let added = created
                    .iter()
                    .map(|record| record.title.as_str())
                    .collect::<Vec<_>>()
                    .join(", ");
                emit_command_output(
                    app,
                    format!(
                        "Added {} suggestion(s): {}.\nRun /suggestions to review.",
                        created.len(),
                        added
                    ),
                );
            }
        }
        "clear" => {
            let removed = store.clear_resolved().map_err(suggestion_error)?;
            emit_command_output(
                app,
                format!("Cleared {removed} resolved suggestion record(s)."),
            );
        }
        _ => emit_command_output(app, render_suggestions_usage()),
    }

    Ok(CommandResult::Handled)
}

fn background_status_rows() -> Vec<String> {
    let jobs_dir = hermes_config::hermes_home().join("background_jobs");
    let mut rows = Vec::new();
    let Ok(read_dir) = std::fs::read_dir(&jobs_dir) else {
        return rows;
    };
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let Ok(raw) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            continue;
        };
        let id = v.get("id").and_then(|x| x.as_str()).unwrap_or("unknown");
        let status = v
            .get("status")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown");
        let task = v
            .get("task")
            .and_then(|x| x.as_str())
            .unwrap_or("")
            .replace('\n', " ");
        rows.push(format!("{id}  [{status}]  {task}"));
    }
    rows.sort();
    rows
}

#[derive(Debug, Clone, Default)]
struct BackgroundQueueAudit {
    dir: PathBuf,
    total_json: usize,
    valid_json: usize,
    malformed_json: usize,
    running_jobs: usize,
    stale_running_jobs: usize,
    duplicate_ids: usize,
}

fn audit_background_queue_manifests() -> BackgroundQueueAudit {
    let dir = hermes_config::hermes_home().join("background_jobs");
    let mut audit = BackgroundQueueAudit {
        dir: dir.clone(),
        ..Default::default()
    };
    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return audit;
    };
    let mut ids = HashMap::<String, usize>::new();
    let now = SystemTime::now();
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        audit.total_json += 1;
        let modified = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        let Ok(raw) = std::fs::read_to_string(&path) else {
            audit.malformed_json += 1;
            continue;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) else {
            audit.malformed_json += 1;
            continue;
        };
        audit.valid_json += 1;
        let id = v
            .get("id")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown")
            .to_string();
        *ids.entry(id).or_insert(0) += 1;
        let status = v
            .get("status")
            .and_then(|x| x.as_str())
            .unwrap_or("unknown");
        if status.eq_ignore_ascii_case("running") {
            audit.running_jobs += 1;
            let stale = now
                .duration_since(modified)
                .map(|age| age > Duration::from_secs(24 * 60 * 60))
                .unwrap_or(false);
            if stale {
                audit.stale_running_jobs += 1;
            }
        }
    }
    audit.duplicate_ids = ids.values().filter(|count| **count > 1).count();
    audit
}

fn render_background_queue_audit(audit: &BackgroundQueueAudit) -> String {
    format!(
        "Queue manifest audit (native)\n  dir: {}\n  json={} valid={} malformed={} running={} stale_running={} duplicate_ids={}",
        audit.dir.display(),
        audit.total_json,
        audit.valid_json,
        audit.malformed_json,
        audit.running_jobs,
        audit.stale_running_jobs,
        audit.duplicate_ids
    )
}

fn env_truthy(raw: &str) -> bool {
    matches!(
        raw.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
}

fn handle_agents_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let sub = args.first().map(|s| s.trim().to_ascii_lowercase());

    if matches!(sub.as_deref(), Some("pause")) {
        std::env::set_var("HERMES_DELEGATION_PAUSED", "1");
        emit_command_output(
            app,
            "Delegation spawning paused for this runtime.\nSet with `/agents resume`.\nStatus: `/agents status`.",
        );
        return Ok(CommandResult::Handled);
    }
    if matches!(sub.as_deref(), Some("resume" | "unpause")) {
        std::env::set_var("HERMES_DELEGATION_PAUSED", "0");
        emit_command_output(
            app,
            "Delegation spawning resumed for this runtime.\nStatus: `/agents status`.",
        );
        return Ok(CommandResult::Handled);
    }
    if matches!(sub.as_deref(), Some("doctor")) {
        let audit = render_background_queue_audit(&audit_background_queue_manifests());
        emit_command_output(
            app,
            format!(
                "Agents doctor\n{}\n- delegation state: `/agents status`\n- spawn tree UI: `/agents` (TUI overlay)",
                audit
            ),
        );
        return Ok(CommandResult::Handled);
    }

    if matches!(sub.as_deref(), Some(other) if other != "status" && other != "list") {
        emit_command_output(app, "Usage: /agents [status|pause|resume|doctor]");
        return Ok(CommandResult::Handled);
    }

    let paused = std::env::var("HERMES_DELEGATION_PAUSED")
        .ok()
        .map(|raw| env_truthy(&raw))
        .unwrap_or(false);
    let rows = background_status_rows();
    if rows.is_empty() {
        let audit = render_background_queue_audit(&audit_background_queue_manifests());
        emit_command_output(
            app,
            format!(
                "Delegation spawning: {}\nBackground jobs: 0\n\nNo background jobs found.\n{}",
                if paused { "paused" } else { "active" },
                audit
            ),
        );
    } else {
        let audit = render_background_queue_audit(&audit_background_queue_manifests());
        let joined = rows.into_iter().take(20).collect::<Vec<_>>().join("\n");
        emit_command_output(
            app,
            format!(
                "Delegation spawning: {}\nBackground jobs (top 20):\n{}\n\n{}\nPause/resume: `/agents pause` or `/agents resume`",
                if paused { "paused" } else { "active" },
                joined,
                audit,
            ),
        );
    }
    Ok(CommandResult::Handled)
}
