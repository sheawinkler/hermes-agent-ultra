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
        "sync" => SkillsSlashInvocation {
            action: Some(action),
            name: None,
            extra: None,
        },
        "opt-out" | "opt-in" => SkillsSlashInvocation {
            action: Some(action),
            name: None,
            extra: build_joined(rest),
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
                "Unknown /skills subcommand '{}'. Use `/skills list`, `/skills sync`, `/skills opt-out`, `/skills opt-in`, `/skills quality`, or `/skills search <query>`.",
                action
            ))
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
        if matches!(invocation.action.as_deref(), Some("opt-out" | "opt-in")) {
            for arg in extra.split_whitespace() {
                cmd.arg(arg);
            }
        } else {
            cmd.arg("--extra").arg(extra);
        }
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    cmd.suppress_windows_console();
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

async fn handle_skills_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
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

    let skills = collect_local_skill_summaries(&skills_dir);

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
        for summary in &skills {
            out.push_str(&format!(
                "- `{}` — {}\n",
                format_skill_display_name(summary),
                summary.title
            ));
        }
        out.push_str("\nUse `hermes skills inspect <name>` for details.");
        out.push_str("\nUse `/skills quality` for score + fallback recommendations.");
        emit_command_output(app, out.trim_end());
    }
    Ok(CommandResult::Handled)
}

fn handle_tools_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.first().is_some_and(|sub| {
        sub.eq_ignore_ascii_case("enable") || sub.eq_ignore_ascii_case("disable")
    }) {
        return handle_tools_toggle_command(app, args);
    }

    if args
        .first()
        .is_some_and(|sub| sub.eq_ignore_ascii_case("trust"))
    {
        let counters = app.tool_registry.policy_counters();
        let tools = app.tool_registry.list_tools();
        let mut risk: Vec<(String, i32, String)> = tools
            .iter()
            .map(|tool| {
                let mut score = 100i32;
                if !tool.env_deps.is_empty() {
                    score -= 15;
                }
                if matches!(
                    tool.name.as_str(),
                    "terminal" | "execute_code" | "shell_exec" | "bash" | "python_exec"
                ) {
                    score -= 35;
                }
                if tool.toolset.eq_ignore_ascii_case("network")
                    || tool.name.contains("webhook")
                    || tool.name.contains("http")
                {
                    score -= 20;
                }
                if tool.name.contains("secrets")
                    || tool.name.contains("token")
                    || tool.name.contains("oauth")
                {
                    score -= 25;
                }
                score = score.clamp(0, 100);
                let tier = if score >= 80 {
                    "low-risk"
                } else if score >= 55 {
                    "moderate-risk"
                } else {
                    "high-risk"
                };
                (tool.name.clone(), score, tier.to_string())
            })
            .collect();
        risk.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let mut out = String::new();
        out.push_str("Tool trust scorecard (heuristic)\n");
        out.push_str("--------------------------------\n");
        let _ = writeln!(
            out,
            "policy_counters: allow={} deny={} audit_only={} simulate={} would_block={}",
            counters.allow,
            counters.deny,
            counters.audit_only,
            counters.simulate,
            counters.would_block
        );
        let _ = writeln!(out, "registered_tools={}", risk.len());
        for (name, score, tier) in risk.iter().take(20) {
            let _ = writeln!(out, "- {name:<28} score={score:>3} tier={tier}");
        }
        out.push_str("\nUse `/ops status` and `/raw trace verify` for live enforcement + trace integrity signals.");
        emit_command_output(app, out.trim_end());
        return Ok(CommandResult::Handled);
    }

    let tools = app.tool_registry.list_tools();
    if tools.is_empty() {
        emit_command_output(app, "No tools registered.");
    } else {
        let disabled: HashSet<&str> = app
            .config
            .tools_config
            .disabled
            .iter()
            .map(String::as_str)
            .collect();
        let mut out = format!("Registered tools ({}):\n", tools.len());
        for tool in &tools {
            let state = if disabled.contains(tool.name.as_str()) {
                "disabled"
            } else {
                "enabled"
            };
            out.push_str(&format!(
                "- `{}` [{}] — {}\n",
                tool.name, state, tool.description
            ));
        }
        out.push_str(
            "\n\nUse `/tools trust` for a risk/score summary, `/tools enable <name>` to enable, or `/tools disable <name>` to disable.",
        );
        emit_command_output(app, out.trim_end());
    }
    Ok(CommandResult::Handled)
}

fn handle_tools_toggle_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args[0].to_ascii_lowercase();
    let tool_name = args.get(1..).unwrap_or_default().join(" ");
    let tool_name = tool_name.trim();
    if tool_name.is_empty() {
        emit_command_output(
            app,
            "Usage: /tools [list] [filter] | /tools enable <name> | /tools disable <name>",
        );
        return Ok(CommandResult::Handled);
    }

    let cfg_path = app.state_root.join("config.yaml");
    let mut disk = hermes_config::load_user_config_file(&cfg_path)
        .map_err(|e| AgentError::Config(e.to_string()))?;
    match action.as_str() {
        "enable" => {
            if !disk.tools_config.enabled.iter().any(|t| t == tool_name) {
                disk.tools_config.enabled.push(tool_name.to_string());
            }
            disk.tools_config.disabled.retain(|t| t != tool_name);
        }
        "disable" => {
            if !disk.tools_config.disabled.iter().any(|t| t == tool_name) {
                disk.tools_config.disabled.push(tool_name.to_string());
            }
            disk.tools_config.enabled.retain(|t| t != tool_name);
        }
        _ => unreachable!("validated tools action"),
    }
    hermes_config::save_config_yaml(&cfg_path, &disk)
        .map_err(|e| AgentError::Config(e.to_string()))?;

    {
        let config = Arc::make_mut(&mut app.config);
        config.tools_config = disk.tools_config.clone();
    }
    app.tool_schemas =
        hermes_tool_planning::resolve_platform_tool_schemas(&app.config, "cli", &app.tool_registry);

    let state = if action == "enable" {
        "Enabled"
    } else {
        "Disabled"
    };
    emit_command_output(
        app,
        format!(
            "{state} tool `{tool_name}` for this session and saved it to {}.",
            cfg_path.display()
        ),
    );
    Ok(CommandResult::Handled)
}

fn handle_config_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        // Show full config
        let config_json = serde_json::to_string_pretty(&*app.config)
            .unwrap_or_else(|e| format!("<serialization error: {}>", e));
        emit_command_output(app, config_json);
    } else {
        match args[0] {
            "get" => {
                if args.len() < 2 {
                    emit_command_output(app, "Usage: /config get <key>");
                } else {
                    let key = args[1];
                    let value = get_config_value(app, key);
                    match value {
                        Some(v) => emit_command_output(app, format!("{} = {}", key, v)),
                        None => emit_command_output(
                            app,
                            format!("Key '{}' not found in configuration.", key),
                        ),
                    }
                }
            }
            "set" => {
                if args.len() < 3 {
                    emit_command_output(app, "Usage: /config set <key> <value>");
                } else {
                    let key = args[1];
                    let value = args[2..].join(" ");
                    if set_config_value(app, key, &value) {
                        emit_command_output(app, format!("Set {} = {}", key, value));
                    } else {
                        emit_command_output(app, format!("Unknown configuration key: {}", key));
                    }
                }
            }
            _ => {
                emit_command_output(
                    app,
                    format!("Unknown config action '{}'. Use 'get' or 'set'.", args[0]),
                );
            }
        }
    }
    Ok(CommandResult::Handled)
}

/// Get a configuration value by dotted key path.
fn get_config_value(app: &App, key: &str) -> Option<String> {
    match key {
        "model" => app.config.model.clone(),
        "personality" => app.config.personality.clone(),
        "max_turns" => Some(app.config.max_turns.to_string()),
        "system_prompt" => app.config.system_prompt.clone(),
        _ => None,
    }
}

/// Set a configuration value by dotted key path.
fn set_config_value(app: &mut App, key: &str, value: &str) -> bool {
    match key {
        "model" => {
            app.config = Arc::new({
                let mut cfg = (*app.config).clone();
                cfg.model = Some(value.to_string());
                cfg
            });
            app.switch_model(value);
            true
        }
        "personality" => {
            app.config = Arc::new({
                let mut cfg = (*app.config).clone();
                cfg.personality = Some(value.to_string());
                cfg
            });
            app.switch_personality(value);
            true
        }
        "max_turns" => {
            if let Ok(turns) = value.parse::<u32>() {
                app.config = Arc::new({
                    let mut cfg = (*app.config).clone();
                    cfg.max_turns = turns;
                    cfg
                });
                true
            } else {
                false
            }
        }
        _ => false,
    }
}
