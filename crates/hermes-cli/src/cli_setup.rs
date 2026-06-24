//! CLI setup subcommands — model, tools, config, and optional setup sections.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use hermes_cli::cli::Cli;
use hermes_cli::model_switch::{
    cached_provider_catalog_status, curated_provider_slugs, normalize_provider_model,
    provider_catalog_entries,
};
use hermes_cli::providers::provider_capability_for;
use hermes_cli::terminal_backend::build_terminal_backend;
use hermes_config::{
    ConfigError, GatewayConfig, apply_user_config_patch, hermes_home, load_config,
    load_user_config_file, save_config_yaml, user_config_field_display, validate_config,
};
use hermes_core::AgentError;

use hermes_cli::gateway_main::prompt_yes_no;
use hermes_cli::gateway_runtime::run_gateway_setup;
use hermes_cli::state_paths::hermes_state_root;
pub(crate) async fn run_model(cli: Cli, provider_model: Option<String>) -> Result<(), AgentError> {
    let config =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;

    match provider_model {
        Some(pm) => {
            let normalized = normalize_provider_model(&pm)?;
            let cfg_path = hermes_state_root(&cli).join("config.yaml");
            let mut disk =
                load_user_config_file(&cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;
            disk.model = Some(normalized.clone());
            save_config_yaml(&cfg_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;
            println!("Model switched to: {}", normalized);
            println!("Persisted default model in {}.", cfg_path.display());
        }
        None => {
            let current = config.model.as_deref().unwrap_or("gpt-4o");
            println!("Current model: {}", current);

            // List providers with merged models.dev-aware previews.
            let providers = curated_provider_slugs();
            let entries = provider_catalog_entries(&providers, 3).await;
            println!("\nAvailable providers:");
            if entries.is_empty() {
                println!("  openai       — OpenAI (gpt-4o, gpt-4o-mini, ...)");
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
                    println!(
                        "  {:<12} — {}{}{}",
                        entry.provider, preview, suffix, cap_suffix
                    );
                }
            }
            println!("\nUsage: hermes model <provider>:<model>");
        }
    }
    Ok(())
}

/// Handle `hermes tools [action]`.
pub(crate) async fn run_tools(
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
    let skill_provider = hermes_cli::skills_runtime::build_skill_provider(true)
        .map_err(|e| AgentError::Config(e.to_string()))?
        .provider;
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
        Some("setup") => {
            run_tools_setup_wizard(&cli).await?;
        }
        Some(other) => {
            println!(
                "Unknown tools action: {}. Use 'list', 'enable', 'disable', or 'setup'.",
                other
            );
        }
    }
    Ok(())
}

async fn run_tools_setup_wizard(cli: &Cli) -> Result<(), AgentError> {
    let runtime_config =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;
    let registry = Arc::new(hermes_tools::ToolRegistry::new());
    let terminal_backend = build_terminal_backend(&runtime_config);
    let skill_provider = hermes_cli::skills_runtime::build_skill_provider(true)
        .map_err(|e| AgentError::Config(e.to_string()))?
        .provider;
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
    if enabled_known_set.contains("computer_use") {
        ensure_computer_use_runtime_ready().await?;
    }
    println!(
        "Updated tools setup: {} enabled, {} disabled (config: {}).",
        enabled_known.len(),
        disabled_known.len(),
        cfg_path.display()
    );
    Ok(())
}

async fn ensure_computer_use_runtime_ready() -> Result<(), AgentError> {
    println!("\nComputer Use runtime check:");
    let mut driver_present = which::which("cua-driver").is_ok();
    if !driver_present {
        println!("  - cua-driver not found on PATH.");
        let do_install = prompt_yes_no("Install cua-driver-rs now?", true).await?;
        if do_install {
            let installed = install_cua_driver_rs_windows().await;
            if installed {
                driver_present = which::which("cua-driver").is_ok();
            }
        } else {
            println!("  - skipped installation.");
        }
    }
    if !driver_present {
        println!("  - computer_use will run in fallback capture-only mode.");
        println!("  - to enable full actions, install cua-driver-rs and reopen setup.");
        return Ok(());
    }

    if cfg!(windows) {
        match hermes_tools::ensure_cua_driver_daemon_running().await {
            Ok(()) => println!("  - Computer Use desktop service is ready."),
            Err(err) => {
                println!("  - Computer Use desktop service could not start: {err}");
                println!("  - Try reinstalling via `hermes tools` → Computer Use.");
            }
        }
    }

    let list_tools_ok = run_cua_driver_health_command(&["list-tools"]).await;
    let list_windows_ok = run_cua_driver_health_command(&["list_windows"]).await;
    if list_tools_ok && list_windows_ok {
        println!("  - cua-driver health check passed (list-tools + list_windows).");
    } else {
        println!("  - cua-driver health check has warnings (Computer Use may still work).");
    }
    Ok(())
}

async fn install_cua_driver_rs_windows() -> bool {
    if !cfg!(windows) {
        println!("  - auto-install currently implemented for Windows only.");
        return false;
    }
    let ps = which::which("powershell")
        .or_else(|_| which::which("pwsh"))
        .ok();
    let Some(ps_bin) = ps else {
        println!("  - PowerShell not found; cannot auto-install cua-driver-rs.");
        return false;
    };

    println!("  - installing cua-driver-rs via official installer...");
    let script = "irm https://raw.githubusercontent.com/trycua/cua/main/libs/cua-driver/scripts/install.ps1 | iex";
    let output = tokio::process::Command::new(ps_bin)
        .arg("-NoProfile")
        .arg("-ExecutionPolicy")
        .arg("Bypass")
        .arg("-Command")
        .arg(script)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await;
    match output {
        Ok(out) if out.status.success() => {
            println!("  - cua-driver-rs install command succeeded.");
            true
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let snippet = stderr.lines().take(3).collect::<Vec<_>>().join(" | ");
            println!("  - install failed: {}", snippet);
            false
        }
        Err(err) => {
            println!("  - install command error: {}", err);
            false
        }
    }
}

async fn run_cua_driver_health_command(args: &[&str]) -> bool {
    let output = tokio::process::Command::new("cua-driver")
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .await;
    match output {
        Ok(out) if out.status.success() => {
            println!("  - cua-driver {}: ok", args.join(" "));
            true
        }
        Ok(out) => {
            let stderr = String::from_utf8_lossy(&out.stderr);
            let snippet = stderr.lines().take(2).collect::<Vec<_>>().join(" | ");
            println!("  - cua-driver {}: failed ({})", args.join(" "), snippet);
            false
        }
        Err(err) => {
            println!("  - cua-driver {}: error ({})", args.join(" "), err);
            false
        }
    }
}

pub(crate) async fn run_optional_setup_sections(
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
pub(crate) async fn run_config(
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
            let cfg_path = base.join("config.yaml");
            let mut disk =
                load_user_config_file(&cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;
            apply_user_config_patch(&mut disk, &key, &value)
                .map_err(|e| AgentError::Config(e.to_string()))?;
            validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
            save_config_yaml(&cfg_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;
            println!("Saved {} = {} -> {}", key, value, cfg_path.display());
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
