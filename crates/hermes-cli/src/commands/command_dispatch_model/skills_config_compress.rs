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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct CompressionRulePlaneConfig {
    #[serde(default)]
    max_assistant_render_lines: Option<usize>,
    #[serde(default)]
    max_tool_output_lines: Option<usize>,
    #[serde(default)]
    max_tool_output_line_chars: Option<usize>,
    #[serde(default)]
    max_tool_output_total_chars: Option<usize>,
}

#[derive(Debug, Clone)]
struct CompressionRenderPolicy {
    max_assistant_render_lines: usize,
    max_tool_output_lines: usize,
    max_tool_output_line_chars: usize,
    max_tool_output_total_chars: usize,
}

impl CompressionRenderPolicy {
    fn builtin_defaults() -> Self {
        Self {
            max_assistant_render_lines: 260,
            max_tool_output_lines: 180,
            max_tool_output_line_chars: 600,
            max_tool_output_total_chars: 48_000,
        }
    }

    fn apply_plane(&mut self, plane: &CompressionRulePlaneConfig) {
        if let Some(v) = plane.max_assistant_render_lines {
            self.max_assistant_render_lines = v.clamp(40, 4000);
        }
        if let Some(v) = plane.max_tool_output_lines {
            self.max_tool_output_lines = v.clamp(20, 5000);
        }
        if let Some(v) = plane.max_tool_output_line_chars {
            self.max_tool_output_line_chars = v.clamp(120, 4000);
        }
        if let Some(v) = plane.max_tool_output_total_chars {
            self.max_tool_output_total_chars = v.clamp(2000, 500_000);
        }
    }
}

fn compression_rules_dir() -> PathBuf {
    hermes_config::hermes_home().join("compression")
}

fn compression_user_rules_path() -> PathBuf {
    compression_rules_dir().join("user-rules.json")
}

fn compression_project_rules_path() -> Option<PathBuf> {
    hermes_tools::repo::detect_repo_root_from_cwd()
        .map(|root| root.join(".hermes-ultra").join("compression-rules.json"))
}

fn load_compression_plane(path: &Path) -> Option<CompressionRulePlaneConfig> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<CompressionRulePlaneConfig>(&raw).ok()
}

fn save_compression_plane(
    path: &Path,
    plane: &CompressionRulePlaneConfig,
) -> Result<(), AgentError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("Failed to create {}: {}", parent.display(), e)))?;
    }
    let payload = serde_json::to_string_pretty(plane)
        .map_err(|e| AgentError::Io(format!("Failed to encode compression rules: {}", e)))?;
    std::fs::write(path, payload)
        .map_err(|e| AgentError::Io(format!("Failed to write {}: {}", path.display(), e)))?;
    Ok(())
}

fn merged_compression_policy() -> (
    CompressionRenderPolicy,
    Option<CompressionRulePlaneConfig>,
    Option<CompressionRulePlaneConfig>,
) {
    let mut merged = CompressionRenderPolicy::builtin_defaults();
    let user = load_compression_plane(&compression_user_rules_path());
    let project = compression_project_rules_path()
        .as_deref()
        .and_then(load_compression_plane);
    if let Some(ref user_plane) = user {
        merged.apply_plane(user_plane);
    }
    if let Some(ref project_plane) = project {
        merged.apply_plane(project_plane);
    }
    (merged, user, project)
}

fn apply_compression_policy_env(policy: &CompressionRenderPolicy) {
    std::env::set_var(
        "HERMES_TUI_MAX_ASSISTANT_RENDER_LINES",
        policy.max_assistant_render_lines.to_string(),
    );
    std::env::set_var(
        "HERMES_TUI_MAX_TOOL_OUTPUT_LINES",
        policy.max_tool_output_lines.to_string(),
    );
    std::env::set_var(
        "HERMES_TUI_MAX_TOOL_OUTPUT_LINE_CHARS",
        policy.max_tool_output_line_chars.to_string(),
    );
    std::env::set_var(
        "HERMES_TUI_MAX_TOOL_OUTPUT_TOTAL_CHARS",
        policy.max_tool_output_total_chars.to_string(),
    );
}

fn render_compression_policy_status() -> String {
    let (merged, user, project) = merged_compression_policy();
    let project_path = compression_project_rules_path()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| "(repo root unavailable)".to_string());
    let mut out = String::new();
    out.push_str("Compression policy planes\n");
    out.push_str("-------------------------\n");
    out.push_str("builtin: max_assistant_render_lines=260, max_tool_output_lines=180, max_tool_output_line_chars=600, max_tool_output_total_chars=48000\n");
    let _ = writeln!(
        out,
        "user: {} ({})",
        if user.is_some() {
            "configured"
        } else {
            "not configured"
        },
        compression_user_rules_path().display()
    );
    let _ = writeln!(
        out,
        "project: {} ({})",
        if project.is_some() {
            "configured"
        } else {
            "not configured"
        },
        project_path
    );
    let _ = writeln!(
        out,
        "\nmerged:\n  - max_assistant_render_lines={}\n  - max_tool_output_lines={}\n  - max_tool_output_line_chars={}\n  - max_tool_output_total_chars={}",
        merged.max_assistant_render_lines,
        merged.max_tool_output_lines,
        merged.max_tool_output_line_chars,
        merged.max_tool_output_total_chars
    );
    out.push_str(
        "\nUse `/compress rules recommend` to generate heuristics from current transcript shape.\n\
         Use `/compress rules autotune` for dry-run tuning, or `/compress rules autotune apply [user|project]` to persist + apply.\n\
         Use `/compress rules apply` to push merged settings into live runtime env.\n\
         Use `/compress rules set user <key> <value>` or `/compress rules set project <key> <value>`.\n\
         Keys: assistant_lines | tool_lines | tool_line_chars | tool_total_chars",
    );
    out
}

fn parse_compression_rule_key(raw: &str) -> Option<&'static str> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "assistant_lines" | "max_assistant_render_lines" | "assistant" => Some("assistant_lines"),
        "tool_lines" | "max_tool_output_lines" | "tool" => Some("tool_lines"),
        "tool_line_chars" | "max_tool_output_line_chars" | "tool_chars" => Some("tool_line_chars"),
        "tool_total_chars" | "max_tool_output_total_chars" | "tool_total" => {
            Some("tool_total_chars")
        }
        _ => None,
    }
}

fn set_compression_rule_field(
    plane: &mut CompressionRulePlaneConfig,
    key: &str,
    value: usize,
) -> Result<(), AgentError> {
    let normalized = parse_compression_rule_key(key).ok_or_else(|| {
        AgentError::Config(format!(
            "Unknown compression rule key '{}'. Use assistant_lines|tool_lines|tool_line_chars|tool_total_chars.",
            key
        ))
    })?;
    match normalized {
        "assistant_lines" => plane.max_assistant_render_lines = Some(value.clamp(40, 4000)),
        "tool_lines" => plane.max_tool_output_lines = Some(value.clamp(20, 5000)),
        "tool_line_chars" => plane.max_tool_output_line_chars = Some(value.clamp(120, 4000)),
        "tool_total_chars" => plane.max_tool_output_total_chars = Some(value.clamp(2000, 500_000)),
        _ => {}
    }
    Ok(())
}

fn resolve_compression_plane_path(target: &str) -> Result<PathBuf, AgentError> {
    let normalized = target.trim().to_ascii_lowercase();
    if normalized == "user" {
        return Ok(compression_user_rules_path());
    }
    if normalized == "project" {
        return compression_project_rules_path().ok_or_else(|| {
            AgentError::Config(
                "Project plane unavailable: run inside a repository checkout.".to_string(),
            )
        });
    }
    Err(AgentError::Config(
        "Plane must be `user` or `project`.".to_string(),
    ))
}

fn recommend_compression_policy_for_app(
    app: &App,
    base: &CompressionRenderPolicy,
) -> CompressionRenderPolicy {
    let mut next = base.clone();
    let mut assistant_msgs = 0usize;
    let mut assistant_lines = 0usize;
    let mut assistant_peak_line_chars = 0usize;
    let mut tool_msgs = 0usize;
    let mut tool_lines = 0usize;
    let mut tool_peak_line_chars = 0usize;
    let mut tool_total_chars = 0usize;

    for msg in &app.messages {
        let Some(content) = msg.content.as_ref() else {
            continue;
        };
        let lines = content.lines().count().max(1);
        let peak_line_chars = content
            .lines()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or_else(|| content.chars().count());
        match msg.role {
            hermes_core::MessageRole::Assistant => {
                assistant_msgs = assistant_msgs.saturating_add(1);
                assistant_lines = assistant_lines.saturating_add(lines);
                assistant_peak_line_chars = assistant_peak_line_chars.max(peak_line_chars);
            }
            hermes_core::MessageRole::Tool => {
                tool_msgs = tool_msgs.saturating_add(1);
                tool_lines = tool_lines.saturating_add(lines);
                tool_peak_line_chars = tool_peak_line_chars.max(peak_line_chars);
                tool_total_chars = tool_total_chars.saturating_add(content.chars().count());
            }
            _ => {}
        }
    }

    if assistant_msgs >= 6 {
        let avg = assistant_lines / assistant_msgs.max(1);
        if avg > 60 {
            next.max_assistant_render_lines = next.max_assistant_render_lines.clamp(320, 4000);
        } else if avg < 24 {
            next.max_assistant_render_lines = next.max_assistant_render_lines.clamp(40, 220);
        }
        if assistant_peak_line_chars > 160 {
            next.max_tool_output_line_chars = next.max_tool_output_line_chars.clamp(720, 4000);
        }
    }

    if tool_msgs >= 2 {
        let avg_tool_lines = tool_lines / tool_msgs.max(1);
        if avg_tool_lines > 120 {
            next.max_tool_output_lines = next.max_tool_output_lines.clamp(260, 5000);
        } else if avg_tool_lines < 40 {
            next.max_tool_output_lines = next.max_tool_output_lines.clamp(20, 160);
        }
        if tool_peak_line_chars > 720 {
            next.max_tool_output_line_chars = next.max_tool_output_line_chars.clamp(920, 4000);
        }
        if tool_total_chars > 120_000 {
            next.max_tool_output_total_chars =
                next.max_tool_output_total_chars.clamp(96_000, 500_000);
        } else if tool_total_chars < 24_000 {
            next.max_tool_output_total_chars =
                next.max_tool_output_total_chars.clamp(2000, 40_000);
        }
    }

    if app.messages.len() >= 140 {
        next.max_assistant_render_lines = next.max_assistant_render_lines.clamp(40, 240);
        next.max_tool_output_total_chars = next.max_tool_output_total_chars.clamp(2000, 64_000);
    }
    next
}

fn render_compression_recommendation(
    current: &CompressionRenderPolicy,
    recommended: &CompressionRenderPolicy,
) -> String {
    let mut out = String::new();
    out.push_str("Compression policy recommendation\n");
    out.push_str("---------------------------------\n");
    let _ = writeln!(
        out,
        "assistant_lines: {} -> {}",
        current.max_assistant_render_lines, recommended.max_assistant_render_lines
    );
    let _ = writeln!(
        out,
        "tool_lines: {} -> {}",
        current.max_tool_output_lines, recommended.max_tool_output_lines
    );
    let _ = writeln!(
        out,
        "tool_line_chars: {} -> {}",
        current.max_tool_output_line_chars, recommended.max_tool_output_line_chars
    );
    let _ = writeln!(
        out,
        "tool_total_chars: {} -> {}",
        current.max_tool_output_total_chars, recommended.max_tool_output_total_chars
    );
    out.push_str(
        "\nApply with `/compress rules autotune apply` (user plane) or `/compress rules autotune apply project`.",
    );
    out
}

fn handle_compress_rules_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    match action.as_str() {
        "status" | "show" | "preview" => {
            emit_command_output(app, render_compression_policy_status());
        }
        "recommend" => {
            let (merged, _, _) = merged_compression_policy();
            let rec = recommend_compression_policy_for_app(app, &merged);
            emit_command_output(app, render_compression_recommendation(&merged, &rec));
        }
        "autotune" => {
            let (merged, _, _) = merged_compression_policy();
            let rec = recommend_compression_policy_for_app(app, &merged);
            if args
                .get(1)
                .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "apply" | "--apply"))
            {
                let target = args
                    .get(2)
                    .copied()
                    .unwrap_or("user")
                    .to_ascii_lowercase();
                let path = match resolve_compression_plane_path(&target) {
                    Ok(path) => path,
                    Err(err) => {
                        emit_command_output(app, err.to_string());
                        return Ok(CommandResult::Handled);
                    }
                };
                let plane = CompressionRulePlaneConfig {
                    max_assistant_render_lines: Some(rec.max_assistant_render_lines),
                    max_tool_output_lines: Some(rec.max_tool_output_lines),
                    max_tool_output_line_chars: Some(rec.max_tool_output_line_chars),
                    max_tool_output_total_chars: Some(rec.max_tool_output_total_chars),
                };
                save_compression_plane(&path, &plane)?;
                apply_compression_policy_env(&rec);
                emit_command_output(
                    app,
                    format!(
                        "{}\n\nAutotune applied to {} plane ({}) and runtime env updated.",
                        render_compression_recommendation(&merged, &rec),
                        target,
                        path.display()
                    ),
                );
            } else {
                emit_command_output(
                    app,
                    format!(
                        "{}\n\nDry-run only. Add `apply` to persist: `/compress rules autotune apply [user|project]`.",
                        render_compression_recommendation(&merged, &rec)
                    ),
                );
            }
        }
        "apply" => {
            let (merged, _, _) = merged_compression_policy();
            apply_compression_policy_env(&merged);
            emit_command_output(
                app,
                format!(
                    "Applied compression policy to runtime env.\n\
                     HERMES_TUI_MAX_ASSISTANT_RENDER_LINES={}\n\
                     HERMES_TUI_MAX_TOOL_OUTPUT_LINES={}\n\
                     HERMES_TUI_MAX_TOOL_OUTPUT_LINE_CHARS={}\n\
                     HERMES_TUI_MAX_TOOL_OUTPUT_TOTAL_CHARS={}",
                    merged.max_assistant_render_lines,
                    merged.max_tool_output_lines,
                    merged.max_tool_output_line_chars,
                    merged.max_tool_output_total_chars
                ),
            );
        }
        "set" => {
            let Some(plane_name) = args.get(1).copied() else {
                emit_command_output(
                    app,
                    "Usage: /compress rules set <user|project> <key> <value>",
                );
                return Ok(CommandResult::Handled);
            };
            let Some(key) = args.get(2).copied() else {
                emit_command_output(
                    app,
                    "Usage: /compress rules set <user|project> <key> <value>",
                );
                return Ok(CommandResult::Handled);
            };
            let Some(value_raw) = args.get(3).copied() else {
                emit_command_output(
                    app,
                    "Usage: /compress rules set <user|project> <key> <value>",
                );
                return Ok(CommandResult::Handled);
            };
            let value = value_raw.parse::<usize>().map_err(|_| {
                AgentError::Config(format!("Invalid value '{}' (expected positive integer).", value_raw))
            })?;
            let target = plane_name.trim().to_ascii_lowercase();
            let path = match resolve_compression_plane_path(&target) {
                Ok(path) => path,
                Err(err) => {
                    emit_command_output(app, err.to_string());
                    return Ok(CommandResult::Handled);
                }
            };
            let mut plane = load_compression_plane(&path).unwrap_or_default();
            set_compression_rule_field(&mut plane, key, value)?;
            save_compression_plane(&path, &plane)?;
            emit_command_output(
                app,
                format!(
                    "Updated {} compression rule: {}={} ({})",
                    target,
                    key,
                    value,
                    path.display()
                ),
            );
        }
        "clear" => {
            let Some(plane_name) = args.get(1).copied() else {
                emit_command_output(app, "Usage: /compress rules clear <user|project>");
                return Ok(CommandResult::Handled);
            };
            let target = plane_name.trim().to_ascii_lowercase();
            let path = match resolve_compression_plane_path(&target) {
                Ok(path) => path,
                Err(err) => {
                    emit_command_output(app, err.to_string());
                    return Ok(CommandResult::Handled);
                }
            };
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| AgentError::Io(format!("Failed to remove {}: {}", path.display(), e)))?;
                emit_command_output(app, format!("Cleared {} plane rules at {}.", target, path.display()));
            } else {
                emit_command_output(app, format!("{} plane rules already clear.", target));
            }
        }
        _ => emit_command_output(
            app,
            "Usage: /compress rules [status|preview|recommend|autotune [apply [user|project]]|apply|set <user|project> <key> <value>|clear <user|project>]",
        ),
    }
    Ok(CommandResult::Handled)
}

fn estimate_message_tokens_for_compress(messages: &[hermes_core::Message]) -> usize {
    messages
        .iter()
        .map(|m| m.content.as_ref().map_or(0, |c| c.len()) / 4)
        .sum()
}

fn handle_compress_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args
        .first()
        .map(|v| v.eq_ignore_ascii_case("rules"))
        .unwrap_or(false)
    {
        return handle_compress_rules_command(app, &args[1..]);
    }
    let msg_count = app.messages.len();
    if msg_count <= 2 {
        emit_command_output(
            app,
            format!("Context too small to compress ({} messages).", msg_count),
        );
        return Ok(CommandResult::Handled);
    }

    let before_count = msg_count;
    let before_tokens = estimate_message_tokens_for_compress(&app.messages);
    let keep = std::cmp::max(2, msg_count / 3);
    let removed = msg_count - keep;
    let summary_text = format!(
        "[Compressed: {} earlier messages summarized. {} messages retained.]",
        removed, keep,
    );

    let split_at = app.messages.len() - keep;
    let retained = app.messages.split_off(split_at);
    app.messages.clear();
    app.messages
        .push(hermes_core::Message::system(summary_text));
    app.messages.extend(retained);
    let after_count = app.messages.len();
    let after_tokens = estimate_message_tokens_for_compress(&app.messages);

    emit_command_output(
        app,
        format!(
            "Compressed: {} → {} messages / ~{} → ~{} tokens.\nCompressed context: removed {} messages, kept {}. Total now: {}.",
            before_count, after_count, before_tokens, after_tokens, removed, keep, after_count
        ),
    );
    Ok(CommandResult::Handled)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CompactionGovernanceMode {
    Off,
    Advisory,
    Enforce,
}

impl CompactionGovernanceMode {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "off" | "disable" | "disabled" | "0" => Some(Self::Off),
            "on" | "advisory" | "warn" | "1" => Some(Self::Advisory),
            "enforce" | "strict" => Some(Self::Enforce),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Advisory => "advisory",
            Self::Enforce => "enforce",
        }
    }
}

fn compaction_governance_mode() -> CompactionGovernanceMode {
    std::env::var("HERMES_CONTEXTLATTICE_COMPACTION_GOVERNANCE")
        .ok()
        .as_deref()
        .and_then(CompactionGovernanceMode::parse)
        .unwrap_or(CompactionGovernanceMode::Advisory)
}

fn handle_autocompact_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .map(|s| s.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "status".to_string());
    match action.as_str() {
        "status" | "show" => {
            let mode = compaction_governance_mode();
            emit_command_output(
                app,
                format!(
                    "Auto-compaction: enabled.\n\
                 Trigger policy: when context exceeds 80% of budget.\n\
                 Runs: once before first LLM call and after each turn.\n\
                 ContextLattice governance: {}.\n\
                 Manual override: `/autocompact now` or `/compress`.",
                    mode.as_str()
                ),
            );
            Ok(CommandResult::Handled)
        }
        "now" | "run" => handle_compress_command(app, &[]),
        "governance" | "govern" => {
            let Some(next) = args.get(1).copied() else {
                emit_command_output(
                    app,
                    format!(
                        "Compaction governance: {}.\nUsage: `/autocompact governance [off|advisory|enforce]`",
                        compaction_governance_mode().as_str()
                    ),
                );
                return Ok(CommandResult::Handled);
            };
            let Some(mode) = CompactionGovernanceMode::parse(next) else {
                emit_command_output(
                    app,
                    format!(
                        "Unknown governance mode '{}'. Use `off`, `advisory`, or `enforce`.",
                        next
                    ),
                );
                return Ok(CommandResult::Handled);
            };
            std::env::set_var("HERMES_CONTEXTLATTICE_COMPACTION_GOVERNANCE", mode.as_str());
            emit_command_output(
                app,
                format!("Compaction governance mode set to `{}`.", mode.as_str()),
            );
            Ok(CommandResult::Handled)
        }
        "help" => {
            emit_command_output(
                app,
                "Usage: `/autocompact [status|now|governance]`\n\
                 - `status`: show current auto-compaction behavior\n\
                 - `now`: run immediate compaction pass\n\
                 - `governance [off|advisory|enforce]`: ContextLattice checkpoint posture for compaction events",
            );
            Ok(CommandResult::Handled)
        }
        other => {
            emit_command_output(
                app,
                format!(
                    "Unknown /autocompact action '{}'. Use `status`, `now`, `governance`, or `help`.",
                    other
                ),
            );
            Ok(CommandResult::Handled)
        }
    }
}

fn usage_token_line(label: &str, usage: &hermes_core::UsageStats) -> String {
    format!(
        "{label}: {} prompt / {} completion / {} total tokens",
        usage.prompt_tokens, usage.completion_tokens, usage.total_tokens
    )
}

async fn handle_billing_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let output = crate::billing::handle_billing_slash_args(args).await?;
    emit_command_output(app, output);
    Ok(CommandResult::Handled)
}

fn handle_usage_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let msg_count = app.messages.len();
    let user_msgs = app
        .messages
        .iter()
        .filter(|m| m.role == hermes_core::MessageRole::User)
        .count();
    let assistant_msgs = app
        .messages
        .iter()
        .filter(|m| m.role == hermes_core::MessageRole::Assistant)
        .count();

    let estimated_tokens: usize = app
        .messages
        .iter()
        .map(|m| m.content.as_ref().map_or(0, |c| c.len()) / 4)
        .sum();

    let mut output = format!(
        "Session Usage Statistics\n  Session:     {}\n  Model:       {}\n  Messages:    {} total\n    User:      {}\n    Assistant: {}\n  Est. tokens: ~{}",
        app.session_id, app.current_model, msg_count, user_msgs, assistant_msgs, estimated_tokens
    );

    match app.last_usage.as_ref() {
        Some(usage) => {
            output.push_str("\n  ");
            output.push_str(&usage_token_line("Last response", usage));
        }
        None => output
            .push_str("\n  Last response: provider usage metadata unavailable for the latest run"),
    }

    match app.session_usage.as_ref() {
        Some(usage) => {
            output.push_str("\n  ");
            output.push_str(&usage_token_line("Actual session", usage));
        }
        None => output
            .push_str("\n  Actual session: unavailable until a provider returns usage metadata"),
    }

    let nous_credits = hermes_core::credits::render_last_nous_credits_lines();
    if !nous_credits.is_empty() {
        output.push_str("\n\n");
        output.push_str(&nous_credits.join("\n"));
    }

    emit_command_output(app, output);
    Ok(CommandResult::Handled)
}

fn handle_stop_command(app: &mut App) -> Result<CommandResult, AgentError> {
    app.interrupt_controller.interrupt(None);
    emit_command_output(
        app,
        "[Stopping current agent execution]\nAgent execution halted. You can continue typing or use /retry.",
    );
    Ok(CommandResult::Handled)
}

fn handle_status_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let msg_count = app.messages.len();
    let turns = app
        .messages
        .iter()
        .filter(|m| m.role == hermes_core::MessageRole::User)
        .count();
    let estimated_tokens: usize = app
        .messages
        .iter()
        .map(|m| m.content.as_ref().map_or(0, |c| c.len()) / 4)
        .sum();

    emit_command_output(
        app,
        format!(
            "Session Status\n  ID:            {}\n  Model:         {}\n  Personality:   {}\n  Turns:         {}\n  Messages:      {}\n  Est. tokens:   ~{}\n  Max turns:     {}",
            app.session_id,
            app.current_model,
            app.current_personality.as_deref().unwrap_or("(none)"),
            turns,
            msg_count,
            estimated_tokens,
            app.config.max_turns
        ),
    );
    Ok(CommandResult::Handled)
}

fn discover_repo_root_for_about() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("HERMES_REPO_ROOT") {
        let path = PathBuf::from(explicit.trim());
        if path.exists() {
            return Some(path);
        }
    }

    let mut probes: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        probes.push(cwd);
    }
    probes.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")));

    for probe in probes {
        for candidate in probe.ancestors() {
            if candidate.join("docs/parity").exists() && candidate.join("README.md").exists() {
                return Some(candidate.to_path_buf());
            }
        }
    }
    None
}

fn read_json_file(path: &Path) -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<serde_json::Value>(&raw).ok()
}

fn json_value_at_path<'a>(
    value: &'a serde_json::Value,
    path: &[&str],
) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn json_str_at_path(value: &serde_json::Value, path: &[&str]) -> Option<String> {
    json_value_at_path(value, path)?
        .as_str()
        .map(|s| s.to_string())
}

fn json_u64_at_path(value: &serde_json::Value, path: &[&str]) -> Option<u64> {
    json_value_at_path(value, path)?.as_u64()
}

fn latest_upstream_sync_report(report_dir: &Path) -> Option<PathBuf> {
    let mut reports: Vec<PathBuf> = std::fs::read_dir(report_dir)
        .ok()?
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let name = path.file_name()?.to_string_lossy();
            if name.starts_with("upstream-sync-") && name.ends_with(".txt") {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    reports.sort();
    reports.into_iter().last()
}

fn parse_sync_report_metadata(path: &Path) -> (std::collections::HashMap<String, String>, usize) {
    let mut meta = std::collections::HashMap::new();
    let mut pending_commit_lines = 0usize;
    let raw = std::fs::read_to_string(path).unwrap_or_default();

    let mut in_pending_section = false;
    let mut in_pending_block = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if !in_pending_section {
            if trimmed.starts_with("## Pending Upstream Commits") {
                in_pending_section = true;
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                let key = k.trim();
                if !key.is_empty()
                    && key
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
                {
                    meta.insert(key.to_string(), v.trim().to_string());
                }
            }
            continue;
        }

        if trimmed == "```" {
            if !in_pending_block {
                in_pending_block = true;
            } else {
                break;
            }
            continue;
        }
        if in_pending_block && !trimmed.is_empty() {
            pending_commit_lines = pending_commit_lines.saturating_add(1);
        }
    }

    (meta, pending_commit_lines)
}

fn yes_no(flag: bool) -> &'static str {
    if flag {
        "yes"
    } else {
        "no"
    }
}

fn handle_about_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let mut out = String::new();
    let _ = writeln!(out, "Hermes Agent Ultra — About");
    let _ = writeln!(out, "  Version:         {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(out, "  Session model:   {}", app.current_model);
    let _ = writeln!(
        out,
        "  Personality:     {}",
        app.current_personality.as_deref().unwrap_or("(none)")
    );
    if let Ok(exe) = std::env::current_exe() {
        let _ = writeln!(out, "  Binary:          {}", exe.display());
    }
    if let Ok(cwd) = std::env::current_dir() {
        let _ = writeln!(out, "  Current dir:     {}", cwd.display());
    }

    let raw_mode = app.tool_registry.raw_mode_state();
    let policy_mode = std::env::var("HERMES_TOOL_POLICY_MODE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "enforce".to_string());
    let policy_preset = std::env::var("HERMES_TOOL_POLICY_PRESET")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "off".to_string());

    let has_contextlattice_mcp = app.config.mcp_servers.iter().any(|entry| {
        let name_hit = entry.name.to_ascii_lowercase().contains("contextlattice");
        let url_hit = entry
            .url
            .as_ref()
            .map(|u| u.to_ascii_lowercase().contains("contextlattice"))
            .unwrap_or(false);
        name_hit || url_hit
    });

    let _ = writeln!(out);
    let _ = writeln!(out, "Enabled Ultra Features:");
    let _ = writeln!(
        out,
        "  - RTK raw-mode: enabled={} once={}",
        yes_no(raw_mode.enabled),
        yes_no(raw_mode.once_pending)
    );
    let _ = writeln!(
        out,
        "  - Tool policy: mode={} preset={}",
        policy_mode, policy_preset
    );
    let _ = writeln!(
        out,
        "  - Code indexing: {} (max_files={}, max_symbols={})",
        yes_no(app.config.agent.code_index_enabled),
        app.config.agent.code_index_max_files,
        app.config.agent.code_index_max_symbols
    );
    let _ = writeln!(
        out,
        "  - LSP context injection: {} (max_chars={})",
        yes_no(app.config.agent.lsp_context_enabled),
        app.config.agent.lsp_context_max_chars
    );
    let _ = writeln!(
        out,
        "  - Background review loop: {}",
        yes_no(app.config.agent.background_review_enabled)
    );
    let _ = writeln!(out, "  - Multi-registry skills: yes");
    let _ = writeln!(out, "  - Skill security scanning: yes");
    let _ = writeln!(
        out,
        "  - ContextLattice MCP configured: {}",
        yes_no(has_contextlattice_mcp)
    );

    if let Some(repo_root) = discover_repo_root_for_about() {
        let report_dir = repo_root.join(".sync-reports");
        let workstream_path = repo_root.join("docs/parity/workstream-status.json");
        let queue_path = repo_root.join("docs/parity/upstream-missing-queue.json");
        let proof_path = repo_root.join("docs/parity/global-parity-proof.json");

        let mut upstream_ref = String::from("unknown");
        let mut upstream_sha = String::from("unknown");
        let mut workstream_generated = String::from("unknown");
        if let Some(workstream) = read_json_file(&workstream_path) {
            if let Some(v) = json_str_at_path(&workstream, &["upstream_ref"]) {
                upstream_ref = v;
            }
            if let Some(v) = json_str_at_path(&workstream, &["upstream_sha"]) {
                upstream_sha = v;
            }
            if let Some(v) = json_str_at_path(&workstream, &["generated_at_utc"]) {
                workstream_generated = v;
            }
        }

        let mut queue_pending = 0u64;
        let mut queue_ported = 0u64;
        let mut queue_superseded = 0u64;
        if let Some(queue) = read_json_file(&queue_path) {
            queue_pending =
                json_u64_at_path(&queue, &["summary", "by_disposition", "pending"]).unwrap_or(0);
            queue_ported =
                json_u64_at_path(&queue, &["summary", "by_disposition", "ported"]).unwrap_or(0);
            queue_superseded =
                json_u64_at_path(&queue, &["summary", "by_disposition", "superseded"]).unwrap_or(0);
        }

        let mut release_gate_pass = String::from("unknown");
        let mut ci_gate_pass = String::from("unknown");
        if let Some(proof) = read_json_file(&proof_path) {
            if let Some(v) =
                json_value_at_path(&proof, &["release_gate", "pass"]).and_then(|v| v.as_bool())
            {
                release_gate_pass = yes_no(v).to_string();
            }
            if let Some(v) =
                json_value_at_path(&proof, &["ci_gate", "pass"]).and_then(|v| v.as_bool())
            {
                ci_gate_pass = yes_no(v).to_string();
            }
        }

        let mut latest_report_name = String::from("none");
        let mut latest_origin_sha = String::from("unknown");
        let mut latest_upstream_sha = String::from("unknown");
        let mut latest_timestamp = String::from("unknown");
        let mut latest_pending_count = 0usize;
        if let Some(report_path) = latest_upstream_sync_report(&report_dir) {
            latest_report_name = report_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| report_path.display().to_string());
            let (meta, pending_count) = parse_sync_report_metadata(&report_path);
            latest_pending_count = pending_count;
            if let Some(v) = meta.get("origin_sha") {
                latest_origin_sha = v.clone();
            }
            if let Some(v) = meta.get("upstream_sha") {
                latest_upstream_sha = v.clone();
            }
            if let Some(v) = meta.get("timestamp_utc") {
                latest_timestamp = v.clone();
            }
        }

        let _ = writeln!(out);
        let _ = writeln!(out, "Parity Snapshot:");
        let _ = writeln!(out, "  - Repo root: {}", repo_root.display());
        let _ = writeln!(out, "  - Upstream ref: {}", upstream_ref);
        let _ = writeln!(out, "  - Upstream sha: {}", upstream_sha);
        let _ = writeln!(
            out,
            "  - Workstream report generated_at: {}",
            workstream_generated
        );
        let _ = writeln!(
            out,
            "  - Queue (pending/ported/superseded): {}/{}/{}",
            queue_pending, queue_ported, queue_superseded
        );
        let _ = writeln!(
            out,
            "  - Gate status (release/ci): {}/{}",
            release_gate_pass, ci_gate_pass
        );
        let _ = writeln!(out, "  - Latest sync report: {}", latest_report_name);
        let _ = writeln!(out, "  - Latest sync timestamp_utc: {}", latest_timestamp);
        let _ = writeln!(out, "  - Latest report origin_sha: {}", latest_origin_sha);
        let _ = writeln!(
            out,
            "  - Latest report upstream_sha: {}",
            latest_upstream_sha
        );
        let _ = writeln!(
            out,
            "  - Pending upstream commits in latest report: {}",
            latest_pending_count
        );
    } else {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "Parity Snapshot: unavailable (run from a source checkout to load docs/parity + .sync-reports)."
        );
    }

    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}
