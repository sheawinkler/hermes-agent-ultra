/// Handle `hermes model [provider:model]`.
async fn run_model(cli: Cli, provider_model: Option<String>) -> Result<(), AgentError> {
    let config =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;

    match provider_model {
        Some(pm) => {
            let normalized = normalize_provider_model(&pm)?;
            let cfg_path = hermes_state_root(&cli).join("config.yaml");
            let mut disk =
                load_user_config_file(&cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;
            let main_provider = provider_slug_from_provider_model(&normalized).to_string();
            let stale_aux = disk.stale_auxiliary_assignments_for_main_provider(&main_provider);
            disk.model = Some(normalized.clone());
            save_config_yaml(&cfg_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;
            println!("Model switched to: {}", normalized);
            println!("Persisted default model in {}.", cfg_path.display());
            if let Some(warning) = format_stale_auxiliary_warning(&main_provider, &stale_aux) {
                println!("{warning}");
            }
        }
        None => {
            let current = config.model.as_deref().unwrap_or("gpt-5.5");
            println!("Current model: {}", current);

            // List providers with merged models.dev-aware previews.
            let entries = provider_catalog_entries_for_config(&config).await;
            println!("\nAvailable providers:");
            if entries.is_empty() {
                println!("  openai       — OpenAI (gpt-5.5, gpt-5.5-pro, ...)");
                println!("  anthropic    — Anthropic (claude-3-5-sonnet, claude-3-opus, ...)");
                println!("  openrouter   — OpenRouter (multi-provider routing)");
                println!("  stepfun      — Step Plan / StepFun (step-3.5-flash, ...)");
            } else {
                for entry in entries {
                    let preview = entry.models.join(", ");
                    let suffix = if entry.total_models > entry.models.len() {
                        format!(" (+{} more)", entry.total_models - entry.models.len())
                    } else {
                        String::new()
                    };
                    let mut caps = Vec::new();
                    if let Some(cap) = provider_capability_for(&entry.provider) {
                        if cap.oauth_supported {
                            caps.push("oauth");
                        }
                        if cap.models_dev_merged {
                            caps.push("models.dev");
                        }
                        if cap.managed_tools_supported {
                            caps.push("managed-tools");
                        }
                    }
                    if let Some(cache_status) = cached_provider_catalog_status(&entry.provider) {
                        if cache_status.verified {
                            if let Some(age) = cache_status.age_secs {
                                caps.push(if age < 60 {
                                    "signed-cache:fresh"
                                } else {
                                    "signed-cache"
                                });
                            } else {
                                caps.push("signed-cache");
                            }
                        } else {
                            caps.push("cache-unverified");
                        }
                    }
                    let cap_suffix = if caps.is_empty() {
                        String::new()
                    } else {
                        format!(" [{}]", caps.join(", "))
                    };
                    let description = provider_picker_description(&entry.provider);
                    println!(
                        "  {:<18} - {} - {}{}{}",
                        entry.provider, description, preview, suffix, cap_suffix
                    );
                }
            }
            println!("\nUsage: hermes model <provider>:<model>");
        }
    }
    Ok(())
}

/// Handle `hermes tools [action]`.
async fn run_tools(
    cli: Cli,
    action: Option<String>,
    name: Option<String>,
    platform: Option<String>,
    summary: bool,
) -> Result<(), AgentError> {
    let runtime_config =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;
    let registry = Arc::new(hermes_tools::ToolRegistry::new());
    let terminal_backend = build_terminal_backend(&runtime_config);
    let skill_store = Arc::new(FileSkillStore::new(hermes_config::skills_dir()));
    let skill_provider: Arc<dyn hermes_core::SkillProvider> =
        Arc::new(SkillManager::new(skill_store));
    hermes_tools::register_builtin_tools(&registry, terminal_backend, skill_provider);
    let tools = registry.list_tools();
    let base: PathBuf = cli
        .config_dir
        .as_ref()
        .map(PathBuf::from)
        .unwrap_or_else(hermes_home);
    let cfg_path = base.join("config.yaml");
    let mut disk =
        load_user_config_file(&cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;

    match action.as_deref() {
        None | Some("list") => {
            let enabled = &disk.tools_config.enabled;
            let disabled = &disk.tools_config.disabled;
            if summary {
                println!(
                    "Tool summary (platform={}):",
                    platform.as_deref().unwrap_or("cli")
                );
                println!(
                    "  enabled: {}",
                    if enabled.is_empty() {
                        "(none)".to_string()
                    } else {
                        enabled.join(", ")
                    }
                );
                println!(
                    "  disabled: {}",
                    if disabled.is_empty() {
                        "(none)".to_string()
                    } else {
                        disabled.join(", ")
                    }
                );
                return Ok(());
            }

            if tools.is_empty() {
                println!("No tools registered (tools are loaded at runtime).");
            } else {
                println!("Registered tools ({}):", tools.len());
                for tool in &tools {
                    let state = if disabled.iter().any(|t| t == &tool.name) {
                        "disabled"
                    } else {
                        "enabled"
                    };
                    println!("  • {} [{}] — {}", tool.name, state, tool.description);
                }
                println!("\nScope: {}", platform.as_deref().unwrap_or("cli"));
            }
        }
        Some("enable") => {
            let tool_name = name.ok_or_else(|| {
                AgentError::Config("tools enable: usage `hermes tools enable <name>`".into())
            })?;
            if !disk.tools_config.enabled.iter().any(|t| t == &tool_name) {
                disk.tools_config.enabled.push(tool_name.clone());
            }
            disk.tools_config.disabled.retain(|t| t != &tool_name);
            save_config_yaml(&cfg_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;
            println!(
                "Enabled tool '{}' for platform '{}'.",
                tool_name,
                platform.as_deref().unwrap_or("cli")
            );
        }
        Some("disable") => {
            let tool_name = name.ok_or_else(|| {
                AgentError::Config("tools disable: usage `hermes tools disable <name>`".into())
            })?;
            if !disk.tools_config.disabled.iter().any(|t| t == &tool_name) {
                disk.tools_config.disabled.push(tool_name.clone());
            }
            disk.tools_config.enabled.retain(|t| t != &tool_name);
            save_config_yaml(&cfg_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;
            println!(
                "Disabled tool '{}' for platform '{}'.",
                tool_name,
                platform.as_deref().unwrap_or("cli")
            );
        }
        Some(other) => {
            println!(
                "Unknown tools action: {}. Use 'list', 'enable', or 'disable'.",
                other
            );
        }
    }
    Ok(())
}

async fn run_computer_use(
    action: Option<String>,
    json_output: bool,
    include: Vec<String>,
    skip: Vec<String>,
) -> Result<(), AgentError> {
    let action = action.unwrap_or_else(|| "status".to_string());
    let driver_cmd = hermes_tools::backends::computer_use::cua_driver_command_from_env();
    match action.as_str() {
        "status" => {
            let available =
                hermes_tools::backends::computer_use::CuaDriverBackend::command_available_from_env(
                );
            if json_output {
                println!(
                    "{}",
                    serde_json::json!({
                        "driver_command": driver_cmd,
                        "available": available,
                    })
                );
            } else if available {
                println!("cua-driver: available ({driver_cmd})");
                println!("  doctor: hermes computer-use doctor");
            } else {
                println!("cua-driver: not installed or not on PATH (looked for {driver_cmd})");
                println!("{}", computer_use_install_hint());
            }
            Ok(())
        }
        "doctor" => {
            if !hermes_tools::backends::computer_use::CuaDriverBackend::command_available_from_env()
            {
                if json_output {
                    println!(
                        "{}",
                        serde_json::json!({
                            "ok": false,
                            "error": "cua-driver unavailable",
                            "driver_command": driver_cmd,
                        })
                    );
                } else {
                    println!("cua-driver: not installed or not on PATH (looked for {driver_cmd})");
                    println!("{}", computer_use_install_hint());
                }
                return Ok(());
            }
            let backend = hermes_tools::backends::computer_use::CuaDriverBackend::from_env();
            let mut args = serde_json::Map::new();
            if !include.is_empty() {
                args.insert("include".into(), serde_json::json!(include));
            }
            if !skip.is_empty() {
                args.insert("skip".into(), serde_json::json!(skip));
            }
            let report =
                hermes_tools::ComputerUseBackend::call_tool(&backend, "health_report", args.into())
                    .await
                    .map_err(|e| AgentError::ToolExecution(e.to_string()))?;
            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&report)
                        .map_err(|e| AgentError::Config(format!("serialize report: {e}")))?
                );
            } else {
                println!("{}", render_computer_use_health_report(&report));
            }
            Ok(())
        }
        "manifest" => {
            let (command, args) =
                hermes_tools::backends::computer_use::resolve_mcp_invocation(&driver_cmd).await;
            if json_output {
                println!(
                    "{}",
                    serde_json::json!({
                        "driver_command": driver_cmd,
                        "mcp_command": command,
                        "mcp_args": args,
                    })
                );
            } else {
                println!("driver command: {driver_cmd}");
                println!("MCP invocation: {} {}", command, args.join(" "));
            }
            Ok(())
        }
        "install-hint" | "install" => {
            println!("{}", computer_use_install_hint());
            Ok(())
        }
        other => Err(AgentError::Config(format!(
            "Unknown computer-use action '{other}'. Use status, doctor, manifest, or install-hint."
        ))),
    }
}

fn computer_use_install_hint() -> &'static str {
    if cfg!(windows) {
        "Install cua-driver with:\n  irm https://raw.githubusercontent.com/trycua/cua/main/libs/cua-driver/scripts/install.ps1 | iex\nThen run:\n  hermes computer-use doctor"
    } else {
        "Install cua-driver with:\n  /bin/bash -c \"$(curl -fsSL https://raw.githubusercontent.com/trycua/cua/main/libs/cua-driver/scripts/install.sh)\"\nThen run:\n  hermes computer-use doctor"
    }
}

fn render_computer_use_health_report(report: &serde_json::Value) -> String {
    let structured = report
        .get("structuredContent")
        .or_else(|| report.get("structured_content"))
        .unwrap_or(report);
    let overall = structured
        .get("overall")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let platform = structured
        .get("platform")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown-platform");
    let driver_version = structured
        .get("driver_version")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown-version");

    let mut out = format!("cua-driver {driver_version} on {platform}: {overall}\n");
    if let Some(checks) = structured.get("checks").and_then(|v| v.as_array()) {
        for check in checks {
            let name = check
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("check");
            let status = check
                .get("status")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let message = check.get("message").and_then(|v| v.as_str()).unwrap_or("");
            out.push_str(&format!("  - {name}: {status}"));
            if !message.is_empty() {
                out.push_str(&format!(" - {message}"));
            }
            out.push('\n');
            if let Some(hint) = check.get("hint").and_then(|v| v.as_str()) {
                out.push_str(&format!("    hint: {hint}\n"));
            }
        }
    } else if let Some(content) = report.get("content").and_then(|v| v.as_array()) {
        for item in content {
            if let Some(text) = item.get("text").and_then(|v| v.as_str()) {
                out.push_str(text);
                if !text.ends_with('\n') {
                    out.push('\n');
                }
            }
        }
    }
    out.trim_end().to_string()
}

async fn run_tools_setup_wizard(cli: &Cli) -> Result<(), AgentError> {
    let runtime_config =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;
    let registry = Arc::new(hermes_tools::ToolRegistry::new());
    let terminal_backend = build_terminal_backend(&runtime_config);
    let skill_store = Arc::new(FileSkillStore::new(hermes_config::skills_dir()));
    let skill_provider: Arc<dyn hermes_core::SkillProvider> =
        Arc::new(SkillManager::new(skill_store));
    hermes_tools::register_builtin_tools(&registry, terminal_backend, skill_provider);
    let mut tools = registry.list_tools();
    if tools.is_empty() {
        println!("No tools registered (tools are loaded at runtime).");
        return Ok(());
    }
    tools.sort_by(|a, b| a.name.cmp(&b.name));

    let cfg_path = hermes_state_root(cli).join("config.yaml");
    let mut disk =
        load_user_config_file(&cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;
    let explicit_enabled = !disk.tools_config.enabled.is_empty();

    let mut pre_selected: HashSet<usize> = HashSet::new();
    let mut rows: Vec<String> = Vec::with_capacity(tools.len());
    let summarize = |text: &str| -> String {
        let flattened: String = text
            .chars()
            .map(|ch| match ch {
                '\n' | '\r' | '\t' => ' ',
                c if c.is_control() => ' ',
                c => c,
            })
            .collect();
        let compact = flattened.split_whitespace().collect::<Vec<_>>().join(" ");
        let max_chars = 120usize;
        if compact.chars().count() <= max_chars {
            compact
        } else {
            let mut out = compact
                .chars()
                .take(max_chars.saturating_sub(1))
                .collect::<String>();
            out.push('…');
            out
        }
    };
    for (idx, tool) in tools.iter().enumerate() {
        let currently_enabled = if explicit_enabled {
            disk.tools_config
                .enabled
                .iter()
                .any(|name| name == &tool.name)
        } else {
            !disk
                .tools_config
                .disabled
                .iter()
                .any(|name| name == &tool.name)
        };
        if currently_enabled {
            pre_selected.insert(idx);
        }
        rows.push(format!(
            "{:<24} {:<8} {}",
            tool.name,
            if currently_enabled {
                "enabled"
            } else {
                "disabled"
            },
            summarize(&tool.description)
        ));
    }

    let result = hermes_cli::curses_checklist(
        "Select enabled tools",
        &rows,
        &pre_selected,
        Some(&|selected| format!("{} selected", selected.len())),
    );
    if !result.confirmed {
        println!("Tools setup cancelled.");
        return Ok(());
    }

    let mut enabled_known: Vec<String> = result
        .selected
        .iter()
        .copied()
        .filter_map(|idx| tools.get(idx).map(|t| t.name.clone()))
        .collect();
    enabled_known.sort();
    enabled_known.dedup();
    let enabled_known_set: HashSet<String> = enabled_known.iter().cloned().collect();

    let mut known_tool_names: Vec<String> = tools.iter().map(|t| t.name.clone()).collect();
    known_tool_names.sort();
    known_tool_names.dedup();
    let known_tool_set: HashSet<String> = known_tool_names.iter().cloned().collect();

    let mut disabled_known: Vec<String> = known_tool_names
        .into_iter()
        .filter(|name| !enabled_known_set.contains(name))
        .collect();
    disabled_known.sort();
    disabled_known.dedup();

    // Preserve unknown/custom tool keys while replacing known-tool state.
    disk.tools_config
        .enabled
        .retain(|name| !known_tool_set.contains(name));
    disk.tools_config
        .disabled
        .retain(|name| !known_tool_set.contains(name));
    disk.tools_config.enabled.extend(enabled_known.clone());
    disk.tools_config.disabled.extend(disabled_known.clone());
    disk.tools_config.enabled.sort();
    disk.tools_config.enabled.dedup();
    disk.tools_config.disabled.sort();
    disk.tools_config.disabled.dedup();

    save_config_yaml(&cfg_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;
    println!(
        "Updated tools setup: {} enabled, {} disabled (config: {}).",
        enabled_known.len(),
        disabled_known.len(),
        cfg_path.display()
    );
    Ok(())
}

async fn run_optional_setup_sections(
    cli: &Cli,
    current_config: &GatewayConfig,
) -> Result<(), AgentError> {
    let items = vec![
        "Messaging platforms (gateway setup wizard)".to_string(),
        "Tools (interactive enable/disable checklist)".to_string(),
        "Memory backend setup (initialize MEMORY.md/USER.md)".to_string(),
        "Sentrux MCP setup (quality workflow backend)".to_string(),
    ];
    let mut pre_selected: HashSet<usize> = HashSet::new();
    if current_config.platforms.values().any(|p| p.enabled) {
        pre_selected.insert(0);
    }
    if !current_config.tools_config.enabled.is_empty()
        || !current_config.tools_config.disabled.is_empty()
    {
        pre_selected.insert(1);
    }
    let memory_root = hermes_home();
    let memory_enabled = !memory_root.join(".memory_disabled").exists();
    let memory_ready = memory_enabled
        && memory_root.join("memories").join("MEMORY.md").exists()
        && memory_root.join("memories").join("USER.md").exists();
    if memory_ready {
        pre_selected.insert(2);
    }
    if current_config
        .mcp_servers
        .iter()
        .any(|entry| entry.name.eq_ignore_ascii_case("sentrux"))
    {
        pre_selected.insert(3);
    }

    let selected = hermes_cli::curses_checklist(
        "Optional setup sections",
        &items,
        &pre_selected,
        Some(&|choice| {
            if choice.is_empty() {
                "none selected".to_string()
            } else {
                format!("{} selected", choice.len())
            }
        }),
    );
    if !selected.confirmed {
        println!("Skipped optional setup sections.");
        return Ok(());
    }
    let mut order: Vec<usize> = selected.selected.iter().copied().collect();
    order.sort_unstable();
    for idx in order {
        match idx {
            0 => {
                println!("\nOpening gateway setup...");
                run_gateway_setup(cli).await?;
            }
            1 => {
                println!("\nOpening tools setup...");
                run_tools_setup_wizard(cli).await?;
            }
            2 => {
                println!("\nOpening memory setup...");
                hermes_cli::commands::handle_cli_memory(Some("setup".to_string()), None, false)
                    .await?;
            }
            3 => {
                println!("\nOpening sentrux MCP setup...");
                hermes_cli::commands::handle_cli_mcp(
                    Some("sentrux-setup".to_string()),
                    None,
                    None,
                    None,
                    None,
                    false,
                )
                .await?;
            }
            _ => {}
        }
    }
    Ok(())
}

/// Handle `hermes config [action] [key] [value]`.
async fn run_config(
    cli: Cli,
    action: Option<String>,
    key: Option<String>,
    value: Option<String>,
) -> Result<(), AgentError> {
    let config =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;

    match action.as_deref() {
        None => {
            // Show full config as JSON
            let json = serde_json::to_string_pretty(&config)
                .map_err(|e| AgentError::Config(e.to_string()))?;
            println!("{}", json);
        }
        Some("get") => {
            let key = key.ok_or_else(|| {
                AgentError::Config("Missing key. Usage: hermes config get <key>".into())
            })?;
            match user_config_field_display(&config, &key) {
                Ok(s) => println!("{}", s),
                Err(ConfigError::NotFound(_)) => println!("Unknown config key: {}", key),
                Err(e) => return Err(AgentError::Config(e.to_string())),
            }
        }
        Some("set") => {
            let key = key.ok_or_else(|| {
                AgentError::Config("Missing key. Usage: hermes config set <key> <value>".into())
            })?;
            let value = value.ok_or_else(|| {
                AgentError::Config("Missing value. Usage: hermes config set <key> <value>".into())
            })?;
            let base: PathBuf = cli
                .config_dir
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(hermes_home);
            let outcome = set_user_config_value(&base, &key, &value)
                .map_err(|e| AgentError::Config(e.to_string()))?;
            match (outcome.config_path, outcome.env_path, outcome.env_key) {
                (Some(cfg_path), Some(env_path), Some(env_key)) => {
                    println!(
                        "Saved {} = {} -> {} and {} -> {}",
                        key,
                        value,
                        cfg_path.display(),
                        env_key,
                        env_path.display()
                    );
                }
                (Some(cfg_path), _, _) => {
                    println!("Saved {} = {} -> {}", key, value, cfg_path.display());
                }
                (_, Some(env_path), Some(env_key)) => {
                    println!("Saved {} -> {}", env_key, env_path.display());
                }
                _ => {}
            }
        }
        Some("show") => {
            let json = serde_json::to_string_pretty(&config)
                .map_err(|e| AgentError::Config(e.to_string()))?;
            println!("{}", json);
        }
        Some("path") => {
            let base: PathBuf = cli
                .config_dir
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(hermes_home);
            let cfg_path = base.join("config.yaml");
            println!("{}", cfg_path.display());
        }
        Some("env-path") => {
            let env_path = hermes_home().join(".env");
            println!("{}", env_path.display());
            if env_path.exists() {
                println!("(exists)");
            } else {
                println!("(not found — create it to set environment overrides)");
            }
        }
        Some("check") | Some("validate") => {
            println!("Validating configuration...");
            match validate_config(&config) {
                Ok(()) => println!("Configuration is valid. ✓"),
                Err(e) => println!("Configuration error: {}", e),
            }
        }
        Some("edit") => {
            let base: PathBuf = cli
                .config_dir
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(hermes_home);
            let cfg_path = base.join("config.yaml");
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
            println!("Opening {} with {}...", cfg_path.display(), editor);
            let status = std::process::Command::new(&editor).arg(&cfg_path).status();
            match status {
                Ok(s) if s.success() => println!("Config saved."),
                Ok(s) => println!("Editor exited with: {}", s),
                Err(e) => println!("Could not launch editor '{}': {}", editor, e),
            }
        }
        Some("migrate") => {
            println!("Config Migration");
            println!("----------------");
            let base: PathBuf = cli
                .config_dir
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(hermes_home);
            let old_json = base.join("config.json");
            let new_yaml = base.join("config.yaml");
            if old_json.exists() && !new_yaml.exists() {
                println!("Found legacy config.json — converting to config.yaml...");
                match std::fs::read_to_string(&old_json) {
                    Ok(content) => {
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                            match serde_yaml::to_string(&val) {
                                Ok(yaml) => {
                                    std::fs::write(&new_yaml, &yaml)
                                        .map_err(|e| AgentError::Io(e.to_string()))?;
                                    println!("Migrated config.json -> config.yaml");
                                    println!("The old config.json was preserved.");
                                }
                                Err(e) => println!("YAML conversion error: {}", e),
                            }
                        } else {
                            println!("Could not parse config.json as JSON.");
                        }
                    }
                    Err(e) => println!("Could not read config.json: {}", e),
                }
            } else if new_yaml.exists() {
                println!("config.yaml already exists. No migration needed.");
            } else {
                println!("No legacy config.json found. Nothing to migrate.");
            }
        }
        Some(other) => {
            println!("Unknown config action: '{}'.", other);
            println!("Available: show, get, set, path, env-path, check, edit, migrate");
        }
    }
    Ok(())
}

