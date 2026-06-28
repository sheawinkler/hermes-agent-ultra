// ---------------------------------------------------------------------------
// Command dispatcher
// ---------------------------------------------------------------------------

enum QuickCommandDispatch {
    Handled,
    Alias { cmd: String, args: Vec<String> },
}

fn quick_command_key(cmd: &str) -> String {
    cmd.trim()
        .trim_start_matches('/')
        .split_whitespace()
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .replace('-', "_")
}

fn quick_command_args(args: &[&str]) -> String {
    args.join(" ")
}

fn split_slash_command(input: &str) -> (String, Vec<String>) {
    let trimmed = input.trim();
    let mut parts = trimmed.split_whitespace();
    let cmd = parts.next().unwrap_or(trimmed).to_string();
    let args = parts.map(ToString::to_string).collect();
    (cmd, args)
}

async fn run_quick_exec(
    name: &str,
    command: &str,
    timeout_secs: u64,
) -> Result<String, AgentError> {
    let child = tokio::process::Command::new("sh")
        .arg("-c")
        .arg(command)
        .kill_on_drop(true)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    let output = match tokio::time::timeout(Duration::from_secs(timeout_secs), child).await {
        Ok(result) => result.map_err(|e| {
            AgentError::ToolExecution(format!("quick command `{name}` failed: {e}"))
        })?,
        Err(_) => {
            return Ok(format!(
                "Quick command `{name}` timed out after {timeout_secs}s."
            ));
        }
    };

    let stdout = String::from_utf8_lossy(&output.stdout)
        .trim_end()
        .to_string();
    if !stdout.trim().is_empty() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr)
        .trim_end()
        .to_string();
    if !stderr.trim().is_empty() {
        return Ok(stderr);
    }
    Ok("Quick command completed with no output.".to_string())
}

async fn handle_quick_command(
    app: &mut App,
    cmd: &str,
    args: &[&str],
) -> Result<Option<QuickCommandDispatch>, AgentError> {
    let key = quick_command_key(cmd);
    let Some(quick) = app.config.quick_commands.get(&key).cloned() else {
        return Ok(None);
    };

    match quick.kind.trim().to_ascii_lowercase().as_str() {
        "exec" => {
            let Some(command) = quick.command.as_deref().filter(|v| !v.trim().is_empty()) else {
                emit_command_output(
                    app,
                    format!("Quick command `{key}` has no command defined."),
                );
                return Ok(Some(QuickCommandDispatch::Handled));
            };
            let output = run_quick_exec(&key, command, quick.timeout_secs()).await?;
            emit_command_output(app, output);
            Ok(Some(QuickCommandDispatch::Handled))
        }
        "alias" => {
            let Some(target) = quick.target.as_deref().filter(|v| !v.trim().is_empty()) else {
                emit_command_output(app, format!("Quick command `{key}` has no target defined."));
                return Ok(Some(QuickCommandDispatch::Handled));
            };
            let mut rewritten = target.trim().to_string();
            let extra = quick_command_args(args);
            if !extra.is_empty() {
                rewritten.push(' ');
                rewritten.push_str(&extra);
            }
            let (alias_cmd, alias_args) = split_slash_command(&rewritten);
            Ok(Some(QuickCommandDispatch::Alias {
                cmd: alias_cmd,
                args: alias_args,
            }))
        }
        other => {
            emit_command_output(
                app,
                format!("Quick command `{key}` has unsupported type `{other}`."),
            );
            Ok(Some(QuickCommandDispatch::Handled))
        }
    }
}

/// Handle a slash command.
///
/// `cmd` is the full command token including the `/` prefix
/// (e.g. `/model`, `/new`). `args` are the remaining tokens.
pub async fn handle_slash_command(
    app: &mut App,
    cmd: &str,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    if let Some(dispatch) = handle_quick_command(app, cmd, args).await? {
        match dispatch {
            QuickCommandDispatch::Handled => return Ok(CommandResult::Handled),
            QuickCommandDispatch::Alias { cmd, args } => {
                let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
                return dispatch_slash_command(app, &cmd, &arg_refs).await;
            }
        }
    }

    dispatch_slash_command(app, cmd, args).await
}

async fn dispatch_slash_command(
    app: &mut App,
    cmd: &str,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    match canonical_command(cmd) {
        "/start" => Ok(CommandResult::Handled),
        "/new" => {
            app.new_session();
            emit_command_output(app, format!("[New session started: {}]", app.session_id));
            Ok(CommandResult::Handled)
        }
        "/reset" => {
            app.reset_session();
            emit_command_output(app, "[Session reset]");
            Ok(CommandResult::Handled)
        }
        "/retry" => {
            app.retry_last().await?;
            Ok(CommandResult::Handled)
        }
        "/undo" | "/rewind" => {
            let count = match args.first() {
                Some(raw) => match raw.parse::<usize>() {
                    Ok(value) if value > 0 => value,
                    _ => {
                        emit_command_output(app, "Usage: /undo [positive-turn-count]");
                        return Ok(CommandResult::Handled);
                    }
                },
                None => 1,
            };
            match app.undo_last_n(count) {
                Some(prefill) if !prefill.trim().is_empty() => emit_command_output(
                    app,
                    format!(
                        "[Undid {} user turn{}; prompt restored to composer for editing]",
                        count,
                        if count == 1 { "" } else { "s" }
                    ),
                ),
                Some(_) => emit_command_output(
                    app,
                    format!(
                        "[Undid {} user turn{}]",
                        count,
                        if count == 1 { "" } else { "s" }
                    ),
                ),
                None => emit_command_output(app, "[No user turns to undo]"),
            }
            Ok(CommandResult::Handled)
        }
        "/history" => handle_history_command(app),
        "/recap" => handle_recap_command(app, args),
        "/context" => handle_context_command(app, args),
        "/title" => handle_session_compat_command(app, canonical_command(cmd), args),
        "/branch" => handle_branch_command(app, args),
        "/timetravel" => handle_timetravel_command(app, args),
        "/snapshot" => handle_snapshot_command(app, args),
        "/rollback" => handle_rollback_command(app, args),
        "/queue" => handle_queue_command(app, args),
        "/handoff" => handle_handoff_command(app, args),
        "/steer" => handle_steer_command(app, args),
        "/btw" => handle_btw_command(app, args),
        "/subgoal" => handle_subgoal_command(app, args),
        "/sethome" => handle_sethome_command(app, args),
        "/evolve" => handle_ops_evolve_command(app, args).await,
        "/objective" => handle_objective_command(app, args),
        "/claims" => handle_claims_command(app, args),
        "/quorum" => handle_quorum_command(app, args).await,
        "/swarm" => handle_swarm_command(app, args).await,
        "/simulate" => handle_simulate_command(app, args),
        "/specpatch" => handle_specpatch_command(app, args).await,
        "/heatmap" => handle_heatmap_command(app, args).await,
        "/studio" => handle_studio_command(app, args).await,
        "/ask" => handle_interactive_question_command(app, args),
        "/model" => handle_model_command(app, args).await,
        "/codex-runtime" => handle_codex_runtime_command(app, args),
        "/auth" => handle_auth_command(app, args).await,
        "/provider" => handle_provider_command(app).await,
        "/personality" => handle_personality_command(app, args),
        "/profile" | "/whoami" => handle_profile_command(app),
        "/version" => {
            emit_command_output(app, hermes_core::version::version_label());
            Ok(CommandResult::Handled)
        }
        "/fast" | "/skin" | "/voice" => {
            handle_runtime_ui_mode_command(app, canonical_command(cmd), args)
        }
        "/pet" => handle_pet_command(app, args),
        "/skills" => handle_skills_command(app, args).await,
        "/tools" => handle_tools_command(app, args),
        "/toolcards" => handle_toolcards_command(app, args),
        "/toolsets" => handle_toolsets_command(app),
        "/bundles" => handle_bundles_command(app),
        "/plugins" => handle_plugins_command(app),
        "/memory" => handle_memory_command(app, args),
        "/disk-cleanup" => handle_disk_cleanup_command(app, args),
        "/mcp" => handle_mcp_command(app),
        "/reload" | "/reload-skills" | "/reload-mcp" => {
            handle_reload_command(app, canonical_command(cmd))
        }
        "/cron" => handle_cron_command(app),
        "/blueprint" => handle_blueprint_command(app, args).await,
        "/suggestions" => handle_suggestions_command(app, args).await,
        "/agents" => handle_agents_command(app, args),
        "/kanban" => handle_kanban_command(app, args),
        "/plan" => handle_plan_command(app, args),
        "/lsp" => handle_lsp_command(app, args),
        "/graph" => handle_graph_command(app, args).await,
        "/qos" => handle_qos_command(app, args).await,
        "/image" => handle_image_command(app, args),
        "/config" => handle_config_command(app, args),
        "/autocompact" => handle_autocompact_command(app, args),
        "/compress" => handle_compress_command(app, args),
        "/clear-queue" => handle_clear_queue_command(app),
        "/billing" => handle_billing_command(app, args).await,
        "/usage" => handle_usage_command(app),
        "/insights" => handle_insights_command(app),
        "/stop" => handle_stop_command(app),
        "/status" => handle_status_command(app),
        "/about" => handle_about_command(app),
        "/ops" => handle_ops_command(app, args).await,
        "/telemetry" => handle_telemetry_command(app, args),
        "/runbook" => handle_runbook_command(app, args),
        "/eval" => handle_ops_eval_command(app, args).await,
        "/autopilot" => handle_ops_autopilot_command(app, args).await,
        "/mission" => handle_mission_command(app, args).await,
        "/dashboard" => handle_dashboard_command(app, args).await,
        "/platforms" => handle_platforms_command(app),
        "/platform" => handle_platform_command(app, args),
        "/integrations" => handle_integrations_command(app, args).await,
        "/commands" => handle_commands_catalog_command(app, args),
        "/boot" => handle_boot_command(app, args).await,
        "/walkthrough" => handle_walkthrough_command(app, args),
        "/triage" => handle_trigger_triage_command(app, args),
        "/subconscious" => handle_subconscious_command(app, args),
        "/log" => handle_log_command(app),
        "/debug-dump" => handle_debug_dump_command(app, args),
        "/dump-format" => handle_dump_format_command(app),
        "/experiment" => handle_experiment_command(app, args),
        "/feedback" => handle_feedback_command(app, args),
        "/restart" => handle_restart_command(app, args),
        "/update" => handle_update_command(app, args).await,
        "/redraw" => handle_redraw_command(app),
        "/paste" => handle_paste_command(app, args),
        "/gquota" => handle_gquota_command(app, args).await,
        "/approve" => handle_approve_command(app, args),
        "/deny" => handle_deny_command(app, args),
        "/copy" => handle_copy_command(app),
        "/save" => handle_save_command(app, args),
        "/load" => handle_load_command(app, args),
        "/resume" => handle_resume_command(app, args),
        "/sessions" => handle_sessions_command(app, args),
        "/background" => handle_background_command(app, args),
        "/mouse" => handle_mouse_command(app, args),
        "/verbose" => handle_verbose_command(app),
        "/statusbar" => handle_statusbar_command(app),
        "/yolo" => handle_yolo_command(app),
        "/browser" => handle_browser_command(app, args).await,
        "/reasoning" => handle_reasoning_command(app, args),
        "/raw" => handle_raw_command(app, args),
        "/policy" => handle_policy_command(app, args),
        "/help" => {
            print_help(app);
            Ok(CommandResult::Handled)
        }
        "/quit" | "/exit" => {
            emit_command_output(app, "Goodbye!");
            Ok(CommandResult::Quit)
        }
        _ => {
            match resolve_cli_skill_slash_command(app, cmd, args) {
                Ok(Some(invocation)) => {
                    app.submit_user_message(&invocation.message).await?;
                    return Ok(CommandResult::Handled);
                }
                Ok(None) => {}
                Err(err) => {
                    emit_command_output(app, format!("Skill command blocked: {err}"));
                    return Ok(CommandResult::Handled);
                }
            }
            emit_command_output(
                app,
                format!(
                    "Unknown command: {}. Type /help for available commands.",
                    cmd
                ),
            );
            Ok(CommandResult::Handled)
        }
    }
}

fn resolve_cli_skill_slash_command(
    app: &App,
    cmd: &str,
    args: &[&str],
) -> Result<Option<SkillSlashInvocation>, String> {
    let config = SkillCommandResolverConfig {
        enabled: app.config.skills.enabled.clone(),
        disabled: app.config.skills.disabled.clone(),
        ..SkillCommandResolverConfig::default()
    };
    resolve_installed_skill_slash_command(cmd, &args.join(" "), &config)
}

fn handle_toolcards_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args.first().copied().unwrap_or("help");
    let msg = match action {
        "export" => {
            "Tool-card export is handled by the interactive TUI modal loop. In TUI, run `/toolcards export` to write `~/.hermes-agent-ultra/logs/toolcards-export.txt`.".to_string()
        }
        _ => "Tool-card controls:\n  /toolcards export   Export current tool-card transcript".to_string(),
    };
    emit_command_output(app, msg);
    Ok(CommandResult::Handled)
}

// ---------------------------------------------------------------------------
// Individual command handlers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum ModelSwitchRequest {
    PickProviderThenModel,
    PickModelFromProvider(String),
    SetDirect(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct ModelCapabilityRequirements {
    require_tools: bool,
    require_vision: bool,
    require_reasoning: bool,
    require_long_context: bool,
    min_context_window: Option<u64>,
}

impl ModelCapabilityRequirements {
    const LONG_CONTEXT_DEFAULT: u64 = 128_000;

    fn is_empty(self) -> bool {
        !self.require_tools
            && !self.require_vision
            && !self.require_reasoning
            && !self.require_long_context
            && self.min_context_window.is_none()
    }

    fn effective_min_context(self) -> Option<u64> {
        match (self.require_long_context, self.min_context_window) {
            (true, Some(value)) => Some(value.max(Self::LONG_CONTEXT_DEFAULT)),
            (true, None) => Some(Self::LONG_CONTEXT_DEFAULT),
            (false, value) => value,
        }
    }

    fn summary(self) -> String {
        let mut parts = Vec::new();
        if self.require_tools {
            parts.push("tools".to_string());
        }
        if self.require_vision {
            parts.push("vision".to_string());
        }
        if self.require_reasoning {
            parts.push("reasoning".to_string());
        }
        if let Some(min_ctx) = self.effective_min_context() {
            parts.push(format!("context>={min_ctx}"));
        }
        if parts.is_empty() {
            "none".to_string()
        } else {
            parts.join(", ")
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct ResolvedModelCapabilities {
    supports_tools: bool,
    supports_vision: bool,
    supports_reasoning: bool,
    context_window: u64,
}

fn normalize_model_capability_name(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "tools" | "tool" | "function-calling" | "function_calling" => Some("tools"),
        "vision" | "image" | "images" => Some("vision"),
        "reasoning" | "reason" => Some("reasoning"),
        "long-context" | "long_context" | "longcontext" | "context" => Some("long-context"),
        _ => None,
    }
}

fn apply_model_capability_token(
    requirements: &mut ModelCapabilityRequirements,
    token: &str,
) -> Result<(), AgentError> {
    let Some(normalized) = normalize_model_capability_name(token) else {
        return Err(AgentError::Config(format!(
            "Unknown model capability '{}' (expected one of: tools, vision, reasoning, long-context).",
            token
        )));
    };
    match normalized {
        "tools" => requirements.require_tools = true,
        "vision" => requirements.require_vision = true,
        "reasoning" => requirements.require_reasoning = true,
        "long-context" => requirements.require_long_context = true,
        _ => {}
    }
    Ok(())
}

fn parse_model_command_args(
    args: &[&str],
) -> Result<(Vec<String>, ModelCapabilityRequirements, Option<String>), AgentError> {
    let mut requirements = ModelCapabilityRequirements::default();
    let mut positional = Vec::new();
    let mut provider_override: Option<String> = None;
    let mut idx = 0usize;

    while idx < args.len() {
        let token = args[idx].trim();
        if token.is_empty() {
            idx += 1;
            continue;
        }

        if matches!(
            token.to_ascii_lowercase().as_str(),
            "--vision" | "--tools" | "--reasoning" | "--long-context" | "--long_context"
        ) {
            apply_model_capability_token(&mut requirements, token.trim_start_matches('-'))?;
            idx += 1;
            continue;
        }

        if matches!(
            token.to_ascii_lowercase().as_str(),
            "--cap" | "--caps" | "--require" | "--requires"
        ) {
            let value = args
                .get(idx + 1)
                .ok_or_else(|| AgentError::Config(format!("{} requires a value.", token)))?;
            for raw in value.split(',') {
                let candidate = raw.trim();
                if candidate.is_empty() {
                    continue;
                }
                apply_model_capability_token(&mut requirements, candidate)?;
            }
            idx += 2;
            continue;
        }

        if token.eq_ignore_ascii_case("--provider") || token.eq_ignore_ascii_case("-p") {
            let provider = args
                .get(idx + 1)
                .ok_or_else(|| AgentError::Config(format!("{} requires a provider slug.", token)))?
                .trim();
            if provider.is_empty() {
                return Err(AgentError::Config(
                    "provider override cannot be empty.".to_string(),
                ));
            }
            provider_override = Some(provider.to_ascii_lowercase());
            idx += 2;
            continue;
        }

        if token.eq_ignore_ascii_case("--min-context")
            || token.eq_ignore_ascii_case("--min_context")
        {
            let value = args
                .get(idx + 1)
                .ok_or_else(|| {
                    AgentError::Config("--min-context requires a numeric value.".into())
                })?
                .trim();
            let parsed = value.parse::<u64>().map_err(|_| {
                AgentError::Config(format!(
                    "Invalid --min-context value '{}'; expected integer token count.",
                    value
                ))
            })?;
            requirements.min_context_window = Some(parsed);
            idx += 2;
            continue;
        }

        positional.push(token.to_string());
        idx += 1;
    }

    Ok((positional, requirements, provider_override))
}

fn resolve_model_capabilities(
    provider: &str,
    model_id: &str,
    client: &hermes_intelligence::models_dev::ModelsDevClient,
) -> ResolvedModelCapabilities {
    if let Some(caps) = client.capabilities(provider, model_id) {
        return ResolvedModelCapabilities {
            supports_tools: caps.supports_tools,
            supports_vision: caps.supports_vision,
            supports_reasoning: caps.supports_reasoning,
            context_window: caps.context_window.max(1),
        };
    }

    let provider_model = format!("{}:{}", provider.trim(), model_id.trim());
    let info = get_model_info(&provider_model).or_else(|| get_model_info(model_id));
    ResolvedModelCapabilities {
        supports_tools: info
            .as_ref()
            .map(|entry| entry.supports_tools)
            .unwrap_or(true),
        supports_vision: info
            .as_ref()
            .map(|entry| entry.supports_vision)
            .unwrap_or(false),
        supports_reasoning: info
            .as_ref()
            .map(|entry| entry.supports_reasoning)
            .unwrap_or(false),
        context_window: get_model_context_length(&provider_model),
    }
}

fn model_meets_requirements(
    capabilities: ResolvedModelCapabilities,
    requirements: ModelCapabilityRequirements,
) -> bool {
    if requirements.require_tools && !capabilities.supports_tools {
        return false;
    }
    if requirements.require_vision && !capabilities.supports_vision {
        return false;
    }
    if requirements.require_reasoning && !capabilities.supports_reasoning {
        return false;
    }
    if let Some(min_context) = requirements.effective_min_context() {
        if capabilities.context_window < min_context {
            return false;
        }
    }
    true
}

fn unmet_model_requirements(
    capabilities: ResolvedModelCapabilities,
    requirements: ModelCapabilityRequirements,
) -> Vec<String> {
    let mut missing = Vec::new();
    if requirements.require_tools && !capabilities.supports_tools {
        missing.push("tools".to_string());
    }
    if requirements.require_vision && !capabilities.supports_vision {
        missing.push("vision".to_string());
    }
    if requirements.require_reasoning && !capabilities.supports_reasoning {
        missing.push("reasoning".to_string());
    }
    if let Some(min_context) = requirements.effective_min_context() {
        if capabilities.context_window < min_context {
            missing.push(format!(
                "context>={} (actual={})",
                min_context, capabilities.context_window
            ));
        }
    }
    missing
}

async fn handle_model_explain_command(
    app: &mut App,
    args: &[&str],
    strict_why_not: bool,
) -> Result<CommandResult, AgentError> {
    let (mut positional, requirements, provider_override) = parse_model_command_args(args)?;
    if let Some(provider) = provider_override {
        if positional.is_empty() {
            positional.push(provider);
        } else if let Some(first) = positional.first().cloned() {
            let model_id = first
                .split_once(':')
                .map(|(_, rhs)| rhs.to_string())
                .unwrap_or(first);
            positional[0] = format!("{}:{}", provider, model_id.trim());
        }
    }
    let target = if positional.is_empty() {
        app.current_model.clone()
    } else {
        normalize_model_target(&app.current_model, &positional[0])?
    };
    let (guarded, remap_note) =
        guard_provider_model_selection_for_config(&target, &app.config).await?;
    let (provider, model_id) = split_provider_model(&guarded);
    let client = default_client();
    client.fetch(false).await;
    let capabilities = resolve_model_capabilities(provider, model_id, client);

    let mut out = String::new();
    let _ = writeln!(out, "Model capability report");
    let _ = writeln!(out, "-----------------------");
    let _ = writeln!(out, "target: {}", guarded);
    let _ = writeln!(out, "provider: {}", provider.trim());
    let _ = writeln!(out, "tools: {}", capabilities.supports_tools);
    let _ = writeln!(out, "vision: {}", capabilities.supports_vision);
    let _ = writeln!(out, "reasoning: {}", capabilities.supports_reasoning);
    let _ = writeln!(out, "context_window: {}", capabilities.context_window);
    let _ = writeln!(
        out,
        "acp_multimodal_parts: {}",
        if capabilities.supports_vision {
            "supported"
        } else {
            "text-only fallback"
        }
    );
    if let Some(note) = remap_note.as_deref() {
        let _ = writeln!(out, "catalog_guard: {}", note);
    }

    if !requirements.is_empty() {
        let unmet = unmet_model_requirements(capabilities, requirements);
        if unmet.is_empty() {
            let _ = writeln!(out, "requirements: satisfied ({})", requirements.summary());
        } else {
            let _ = writeln!(out, "requirements: FAILED ({})", requirements.summary());
            let _ = writeln!(out, "missing: {}", unmet.join(", "));
            let catalog = provider_model_ids_for_config(provider, &app.config).await;
            let alternatives: Vec<String> = catalog
                .into_iter()
                .filter(|candidate| {
                    model_meets_requirements(
                        resolve_model_capabilities(provider, candidate, client),
                        requirements,
                    )
                })
                .take(8)
                .collect();
            if alternatives.is_empty() {
                let _ = writeln!(out, "alternatives: none in provider catalog");
            } else {
                let _ = writeln!(
                    out,
                    "alternatives: {}",
                    alternatives
                        .iter()
                        .map(|m| format!("{}:{}", provider, m))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            if strict_why_not {
                return Err(AgentError::Config(out.trim_end().to_string()));
            }
        }
    } else if strict_why_not {
        let _ = writeln!(
            out,
            "why-not mode requires constraints. Example: `/model why-not --cap tools,reasoning --min-context 200000`"
        );
    }

    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn parse_model_switch_request<S: AsRef<str>>(
    args: &[&str],
    known_providers: &[S],
) -> ModelSwitchRequest {
    if args.is_empty() {
        return ModelSwitchRequest::PickProviderThenModel;
    }
    let raw = args.join(" ");
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return ModelSwitchRequest::PickProviderThenModel;
    }
    if trimmed.contains(':') {
        return ModelSwitchRequest::SetDirect(trimmed.to_string());
    }
    if known_providers
        .iter()
        .any(|p| p.as_ref().eq_ignore_ascii_case(trimmed))
    {
        return ModelSwitchRequest::PickModelFromProvider(trimmed.to_ascii_lowercase());
    }
    ModelSwitchRequest::SetDirect(trimmed.to_string())
}

fn model_catalog_guard_enabled() -> bool {
    !matches!(
        std::env::var("HERMES_MODEL_CATALOG_GUARD")
            .ok()
            .as_deref()
            .map(|v| v.trim().to_ascii_lowercase()),
        Some(v) if matches!(v.as_str(), "0" | "false" | "off" | "no")
    )
}

async fn guard_provider_model_selection_for_config(
    provider_model: &str,
    config: &GatewayConfig,
) -> Result<(String, Option<String>), AgentError> {
    if !model_catalog_guard_enabled() {
        return Ok((provider_model.to_string(), None));
    }

    let (provider, model_id) = split_provider_model(provider_model);
    let provider = provider.trim().to_ascii_lowercase();
    if provider.is_empty() {
        return Ok((provider_model.to_string(), None));
    }
    if matches!(provider.as_str(), "openai-codex" | "codex")
        || (provider == "openai" && model_id.to_ascii_lowercase().contains("codex"))
    {
        return Ok((
            provider_model.to_string(),
            Some(format!(
                "Catalog guard soft-accepted unlisted Codex model `{}`.",
                model_id.trim()
            )),
        ));
    }
    if !provider_slugs_for_config(config)
        .iter()
        .any(|slug| slug.eq_ignore_ascii_case(&provider))
    {
        return Ok((provider_model.to_string(), None));
    }

    let catalog = provider_model_ids_for_config(&provider, config).await;
    if catalog.is_empty() {
        return Ok((provider_model.to_string(), None));
    }
    let Some(candidate) = resolve_catalog_model_candidate(model_id, &catalog) else {
        let suggestions = rank_catalog_model_candidates(model_id, &catalog, 5);
        return Err(AgentError::Config(format!(
            "Model '{}' is not available for provider '{}'. Close matches: {}. Use `/model {}` to pick a valid catalog entry.",
            model_id.trim(),
            provider,
            if suggestions.is_empty() {
                "(none)".to_string()
            } else {
                suggestions.join(", ")
            },
            provider,
        )));
    };
    let guarded = format!("{}:{}", provider, candidate);
    if guarded.eq_ignore_ascii_case(provider_model) {
        return Ok((provider_model.to_string(), None));
    }
    Ok((
        guarded.clone(),
        Some(format!(
            "Model catalog guard remapped `{}` -> `{}` based on provider catalog.",
            provider_model, guarded
        )),
    ))
}

fn normalize_model_target(current_model: &str, raw: &str) -> Result<String, AgentError> {
    let trimmed = raw.trim();
    if trimmed.contains(':') {
        return normalize_provider_model(trimmed);
    }
    let (provider, _) = split_provider_model(current_model);
    normalize_provider_model(&format!("{}:{}", provider.trim(), trimmed))
}

/// Run `curses_select` safely from both plain CLI and active TUI sessions.
///
/// In TUI mode, use an embedded selector that does not toggle terminal mode.
fn run_model_picker_select(
    app: &App,
    title: &str,
    items: &[String],
    initial_index: usize,
) -> crate::SelectResult {
    if app.stream_handle.is_some() {
        crate::curses_select_embedded(title, items, initial_index)
    } else {
        crate::curses_select(title, items, initial_index)
    }
}

fn persist_current_model_selection(app: &App) -> Result<PathBuf, AgentError> {
    let cfg_path = app.state_root.join("config.yaml");
    if config_uses_nested_model_block(&cfg_path)? {
        let (provider, model_id) = split_provider_model(&app.current_model);
        let model_id = model_id.trim();
        let provider = provider.trim();
        set_user_config_value(&app.state_root, "model.default", model_id)
            .map_err(|e| AgentError::Config(e.to_string()))?;
        if !provider.is_empty() {
            set_user_config_value(&app.state_root, "model.provider", provider)
                .map_err(|e| AgentError::Config(e.to_string()))?;
        }
        if let Some(base_url) = app
            .config
            .llm_providers
            .get(provider)
            .and_then(|cfg| cfg.base_url.as_deref())
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            set_user_config_value(&app.state_root, "model.base_url", base_url)
                .map_err(|e| AgentError::Config(e.to_string()))?;
        }
    } else {
        set_user_config_value(&app.state_root, "model", &app.current_model)
            .map_err(|e| AgentError::Config(e.to_string()))?;
    }
    Ok(cfg_path)
}

fn config_uses_nested_model_block(path: &Path) -> Result<bool, AgentError> {
    if !path.exists() {
        return Ok(false);
    }
    let text = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
    if text.trim().is_empty() {
        return Ok(false);
    }
    let value: serde_yaml::Value =
        serde_yaml::from_str(&text).map_err(|e| AgentError::Config(e.to_string()))?;
    let Some(root) = value.as_mapping() else {
        return Ok(false);
    };
    Ok(root
        .get(serde_yaml::Value::String("model".to_string()))
        .is_some_and(serde_yaml::Value::is_mapping))
}

fn format_model_persistence_note(app: &App) -> String {
    let mut note = match persist_current_model_selection(app) {
        Ok(path) => format!("Persisted default model in {}.", path.display()),
        Err(err) => format!(
            "Warning: switched for this session, but failed to persist default model: {}",
            err
        ),
    };
    let main_provider = provider_slug_from_provider_model(&app.current_model);
    let stale_aux = app
        .config
        .stale_auxiliary_assignments_for_main_provider(main_provider);
    if let Some(warning) = format_stale_auxiliary_warning(main_provider, &stale_aux) {
        note.push('\n');
        note.push_str(&warning);
    }
    note
}

fn try_switch_model_or_emit_failure(app: &mut App, provider_model: &str) -> bool {
    let previous_model = app.current_model.clone();
    match app.try_switch_model(provider_model) {
        Ok(()) => true,
        Err(err) => {
            emit_command_output(
                app,
                format!(
                    "Model switch to {} failed ({}); staying on {}.",
                    provider_model, err, previous_model
                ),
            );
            false
        }
    }
}

async fn pick_model_for_provider(
    app: &mut App,
    provider: &str,
    current_model: &str,
    requirements: ModelCapabilityRequirements,
) -> Result<bool, AgentError> {
    let models = provider_model_ids_for_config(provider, &app.config).await;
    if models.is_empty() {
        emit_command_output(
            app,
            format!("No models available for provider '{}'.", provider),
        );
        return Ok(false);
    }

    let normalized_provider = provider.trim().to_ascii_lowercase();
    let mut filtered_models = models.clone();
    if !requirements.is_empty() {
        let client = default_client();
        client.fetch(false).await;
        filtered_models = models
            .iter()
            .filter(|model_id| {
                model_meets_requirements(
                    resolve_model_capabilities(&normalized_provider, model_id, client),
                    requirements,
                )
            })
            .cloned()
            .collect();
    }

    if filtered_models.is_empty() {
        emit_command_output(
            app,
            format!(
                "No models for provider '{}' satisfy required capabilities: {}.",
                provider,
                requirements.summary()
            ),
        );
        return Ok(false);
    }

    let (_, current_model_id) = split_provider_model(current_model);
    let default_index = filtered_models
        .iter()
        .position(|m| m.eq_ignore_ascii_case(current_model_id))
        .unwrap_or(0);
    let labels: Vec<String> = filtered_models.clone();
    let title = format!("Select {} model ({} available)", provider, labels.len());
    let pick = run_model_picker_select(app, &title, &labels, default_index);
    if !pick.confirmed || pick.index >= filtered_models.len() {
        emit_command_output(app, "Model switch cancelled.");
        return Ok(false);
    }
    let provider_model = format!("{}:{}", provider, filtered_models[pick.index].trim());
    let (guarded, note) =
        guard_provider_model_selection_for_config(&provider_model, &app.config).await?;
    let warning = app.model_switch_preflight_warning(&guarded);
    if !try_switch_model_or_emit_failure(app, &guarded) {
        return Ok(false);
    }
    let mut msg = format!("Model switched to: {}", guarded);
    if let Some(n) = note {
        msg.push('\n');
        msg.push_str(&n);
    }
    msg.push('\n');
    msg.push_str(&format_model_persistence_note(app));
    if let Some(warning) = warning {
        msg.push('\n');
        msg.push_str(&warning);
    }
    emit_command_output(app, msg);
    Ok(true)
}

fn parse_failover_chain(raw: &str) -> Result<Vec<String>, AgentError> {
    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for token in raw.split(',') {
        let trimmed = token.trim();
        if trimmed.is_empty() {
            continue;
        }
        let normalized = normalize_provider_model(trimmed)?;
        let key = normalized.to_ascii_lowercase();
        if seen.insert(key) {
            out.push(normalized);
        }
    }
    Ok(out)
}

fn read_failover_chain_from_env() -> Vec<String> {
    if let Ok(raw) = std::env::var("HERMES_FALLBACK_MODELS") {
        let parsed: Vec<String> = raw
            .split(',')
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string)
            .collect();
        if !parsed.is_empty() {
            return parsed;
        }
    }
    if let Ok(raw) = std::env::var("HERMES_FALLBACK_MODEL") {
        let value = raw.trim();
        if !value.is_empty() {
            return vec![value.to_string()];
        }
    }
    Vec::new()
}

fn handle_model_failover_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    match action.as_str() {
        "status" | "show" => {
            let chain_items = read_failover_chain_from_env();
            let fallback = chain_items.first().map(|s| s.as_str()).unwrap_or("(none)");
            let chain = if chain_items.is_empty() {
                "(none)".to_string()
            } else {
                chain_items.join(", ")
            };
            emit_command_output(
                app,
                format!(
                    "Failover fabric\nprimary_fallback: {}\nchain: {}\nusage: `/model failover set provider:model[,provider:model...]` or `/model failover clear`",
                    fallback, chain
                ),
            );
        }
        "clear" | "reset" => {
            std::env::remove_var("HERMES_FALLBACK_MODEL");
            std::env::remove_var("HERMES_FALLBACK_MODELS");
            let current = app.current_model.clone();
            app.switch_model(&current);
            emit_command_output(app, "Cleared retry failover chain.");
        }
        "set" => {
            let raw = args
                .get(1)
                .ok_or_else(|| {
                    AgentError::Config(
                        "Usage: /model failover set provider:model[,provider:model...]".to_string(),
                    )
                })?
                .trim();
            let chain = parse_failover_chain(raw)?;
            if chain.is_empty() {
                return Err(AgentError::Config(
                    "Failover chain cannot be empty.".to_string(),
                ));
            }
            std::env::set_var("HERMES_FALLBACK_MODELS", chain.join(","));
            if let Some(first) = chain.first() {
                std::env::set_var("HERMES_FALLBACK_MODEL", first);
            }
            let current = app.current_model.clone();
            app.switch_model(&current);
            emit_command_output(app, format!("Failover chain set: {}", chain.join(", ")));
        }
        _ => {
            emit_command_output(
                app,
                "Usage: /model failover [status|set provider:model[,provider:model...]|clear]",
            );
        }
    }
    Ok(CommandResult::Handled)
}

#[derive(Debug, Clone, Copy)]
struct BackendBestPracticeProfile {
    provider: &'static str,
    profile: &'static str,
    summary: &'static str,
    launch_hint: &'static str,
    env_overrides: &'static [(&'static str, &'static str)],
}

const VLLM_PROFILE_BALANCED_ENV: &[(&str, &str)] = &[
    ("VLLM_GPU_MEMORY_UTILIZATION", "0.88"),
    ("VLLM_ENABLE_PREFIX_CACHING", "1"),
    ("VLLM_ENABLE_CHUNKED_PREFILL", "1"),
];
const VLLM_PROFILE_THROUGHPUT_ENV: &[(&str, &str)] = &[
    ("VLLM_GPU_MEMORY_UTILIZATION", "0.92"),
    ("VLLM_MAX_NUM_SEQS", "256"),
    ("VLLM_ENABLE_PREFIX_CACHING", "1"),
];
const VLLM_PROFILE_RELIABILITY_ENV: &[(&str, &str)] = &[
    ("VLLM_GPU_MEMORY_UTILIZATION", "0.80"),
    ("VLLM_MAX_NUM_SEQS", "64"),
    ("VLLM_ENABLE_CHUNKED_PREFILL", "0"),
];
const LLAMA_CPP_PROFILE_BALANCED_ENV: &[(&str, &str)] = &[
    ("LLAMA_CPP_THREADS", "8"),
    ("LLAMA_CPP_CTX_SIZE", "8192"),
    ("LLAMA_CPP_BATCH", "512"),
];
const MLX_PROFILE_BALANCED_ENV: &[(&str, &str)] = &[
    ("MLX_QUANT", "4bit"),
    ("MLX_MAX_BATCH_SIZE", "16"),
    ("MLX_ENABLE_PROMPT_CACHE", "1"),
];
const SGLANG_PROFILE_BALANCED_ENV: &[(&str, &str)] = &[
    ("SGLANG_ENABLE_RADIX_CACHE", "1"),
    ("SGLANG_MAX_RUNNING_REQUESTS", "256"),
];
const TGI_PROFILE_BALANCED_ENV: &[(&str, &str)] = &[
    ("TGI_MAX_BATCH_TOTAL_TOKENS", "32768"),
    ("TGI_WAITING_SERVED_RATIO", "0.30"),
];
const APPLE_ANE_PROFILE_BALANCED_ENV: &[(&str, &str)] = &[
    ("APPLE_ANE_ENABLE_LOW_LATENCY", "1"),
    ("APPLE_ANE_PREFILL_TOKENS", "1024"),
];
const MISTRAL_RS_PROFILE_BALANCED_ENV: &[(&str, &str)] = &[
    ("MISTRAL_RS_PAGED_ATTENTION", "1"),
    ("MISTRAL_RS_KV_CACHE_DTYPE", "fp16"),
    ("MISTRAL_RS_SPECULATIVE_DECODING", "0"),
];

const BACKEND_BEST_PRACTICE_PROFILES: &[BackendBestPracticeProfile] = &[
    BackendBestPracticeProfile {
        provider: "vllm",
        profile: "balanced",
        summary: "Default performance profile for stable throughput and latency.",
        launch_hint:
            "vllm serve MODEL --enable-prefix-caching --enable-chunked-prefill --gpu-memory-utilization 0.88",
        env_overrides: VLLM_PROFILE_BALANCED_ENV,
    },
    BackendBestPracticeProfile {
        provider: "vllm",
        profile: "throughput",
        summary: "Higher concurrency profile for heavy parallel workloads.",
        launch_hint:
            "vllm serve MODEL --enable-prefix-caching --max-num-seqs 256 --gpu-memory-utilization 0.92",
        env_overrides: VLLM_PROFILE_THROUGHPUT_ENV,
    },
    BackendBestPracticeProfile {
        provider: "vllm",
        profile: "reliability",
        summary: "Lower-pressure profile tuned for long sessions and fewer OOM events.",
        launch_hint:
            "vllm serve MODEL --max-num-seqs 64 --gpu-memory-utilization 0.80 --disable-chunked-prefill",
        env_overrides: VLLM_PROFILE_RELIABILITY_ENV,
    },
    BackendBestPracticeProfile {
        provider: "llama-cpp",
        profile: "balanced",
        summary: "General local GGUF serving profile with predictable latency.",
        launch_hint:
            "llama-server -m MODEL.gguf -c 8192 -t 8 -b 512 --host 127.0.0.1 --port 8080",
        env_overrides: LLAMA_CPP_PROFILE_BALANCED_ENV,
    },
    BackendBestPracticeProfile {
        provider: "mlx",
        profile: "balanced",
        summary: "Apple Silicon profile prioritizing cache reuse and compact memory.",
        launch_hint:
            "python -m mlx_lm.server --model mlx-community/Qwen3-8B-4bit --host 127.0.0.1 --port 8080",
        env_overrides: MLX_PROFILE_BALANCED_ENV,
    },
    BackendBestPracticeProfile {
        provider: "apple-ane",
        profile: "balanced",
        summary: "ANE-optimized low-latency settings for on-device endpoints.",
        launch_hint: "Use your ANE OpenAI-compatible server with low-latency prefill settings.",
        env_overrides: APPLE_ANE_PROFILE_BALANCED_ENV,
    },
    BackendBestPracticeProfile {
        provider: "sglang",
        profile: "balanced",
        summary: "SGLang cache-first profile for sustained request loads.",
        launch_hint:
            "python -m sglang.launch_server --model-path MODEL --host 127.0.0.1 --port 30000",
        env_overrides: SGLANG_PROFILE_BALANCED_ENV,
    },
    BackendBestPracticeProfile {
        provider: "tgi",
        profile: "balanced",
        summary: "Text-Generation-Inference profile balancing batch depth and tail latency.",
        launch_hint:
            "text-generation-launcher --model-id MODEL --port 8082 --max-batch-total-tokens 32768",
        env_overrides: TGI_PROFILE_BALANCED_ENV,
    },
    BackendBestPracticeProfile {
        provider: "lmstudio",
        profile: "balanced",
        summary: "Desktop local serving profile for LM Studio's OpenAI-compatible server.",
        launch_hint: "Start LM Studio Local Server on 127.0.0.1:1234 and load a model.",
        env_overrides: &[],
    },
    BackendBestPracticeProfile {
        provider: "lmdeploy",
        profile: "balanced",
        summary: "LMDeploy OpenAI-compatible serving profile for local or workstation GPUs.",
        launch_hint: "lmdeploy serve api_server MODEL --server-port 23333",
        env_overrides: &[],
    },
    BackendBestPracticeProfile {
        provider: "localai",
        profile: "balanced",
        summary: "LocalAI OpenAI-compatible serving profile for mixed local backends.",
        launch_hint: "local-ai run --address 127.0.0.1:8080",
        env_overrides: &[],
    },
    BackendBestPracticeProfile {
        provider: "koboldcpp",
        profile: "balanced",
        summary: "KoboldCpp single-binary profile for GGUF local serving.",
        launch_hint: "koboldcpp --model MODEL.gguf --host 127.0.0.1 --port 5001",
        env_overrides: &[],
    },
    BackendBestPracticeProfile {
        provider: "text-generation-webui",
        profile: "balanced",
        summary: "oobabooga text-generation-webui OpenAI extension profile.",
        launch_hint: "python server.py --extensions openai --api --api-port 5000",
        env_overrides: &[],
    },
    BackendBestPracticeProfile {
        provider: "tabbyapi",
        profile: "balanced",
        summary: "TabbyAPI / ExLlamaV2 profile for quantized GPU serving.",
        launch_hint: "python main.py --host 127.0.0.1 --port 5000",
        env_overrides: &[],
    },
    BackendBestPracticeProfile {
        provider: "mistral-rs",
        profile: "balanced",
        summary: "mistral.rs runtime baseline for robust local serving.",
        launch_hint: "mistralrs-server --model MODEL --port 8083 --paged-attention",
        env_overrides: MISTRAL_RS_PROFILE_BALANCED_ENV,
    },
];

fn normalize_backend_provider(value: &str) -> String {
    let raw = value.trim().to_ascii_lowercase();
    match raw.as_str() {
        "llvm" | "ollvm" => "vllm".to_string(),
        "llama.cpp" | "llamacpp" | "llamafile" => "llama-cpp".to_string(),
        "mlx-lm" | "apple-mlx" | "vmlx" | "omlx" | "mlx-vlm" | "mlxvlm" | "mlx-openai-server" => {
            "mlx".to_string()
        }
        "ane" | "apple-neural-engine" | "neural-engine" => "apple-ane".to_string(),
        "lm-studio" | "lm_studio" | "lm studio" => "lmstudio".to_string(),
        "lm-deploy" | "lm_deploy" => "lmdeploy".to_string(),
        "local-ai" | "local_ai" => "localai".to_string(),
        "kobold-cpp" | "kobold" => "koboldcpp".to_string(),
        "oobabooga" | "textgen-webui" | "textgen_webui" | "text-generation-web-ui" => {
            "text-generation-webui".to_string()
        }
        "tabby-api" | "tabby_api" | "exllama" | "exllamav2" => "tabbyapi".to_string(),
        other => other.to_string(),
    }
}

fn backend_profile_lookup(
    provider: &str,
    profile: Option<&str>,
) -> Option<&'static BackendBestPracticeProfile> {
    let normalized = normalize_backend_provider(provider);
    let profile = profile.unwrap_or("balanced").trim().to_ascii_lowercase();
    BACKEND_BEST_PRACTICE_PROFILES.iter().find(|row| {
        row.provider.eq_ignore_ascii_case(&normalized) && row.profile.eq_ignore_ascii_case(&profile)
    })
}

fn render_backend_profiles(provider: Option<&str>) -> String {
    let mut out = String::new();
    out.push_str("Backend best-practice profiles\n");
    out.push_str("-------------------------------\n");
    let filtered: Vec<&BackendBestPracticeProfile> = if let Some(provider) = provider {
        let normalized = normalize_backend_provider(provider);
        BACKEND_BEST_PRACTICE_PROFILES
            .iter()
            .filter(|row| row.provider.eq_ignore_ascii_case(&normalized))
            .collect()
    } else {
        BACKEND_BEST_PRACTICE_PROFILES.iter().collect()
    };
    if filtered.is_empty() {
        let selected = provider.unwrap_or("(none)");
        let _ = writeln!(out, "No backend profile presets found for '{}'.", selected);
        return out.trim_end().to_string();
    }
    for row in filtered {
        let _ = writeln!(
            out,
            "- {}:{}\n  {}\n  launch: {}\n  env: {}",
            row.provider,
            row.profile,
            row.summary,
            row.launch_hint,
            row.env_overrides
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<String>>()
                .join(", ")
        );
    }
    out.push_str("\nUse `/model backend apply <provider> [profile]` to load env overrides for current runtime.");
    out.trim_end().to_string()
}

fn persist_backend_profile_env(
    provider: &str,
    profile: &str,
    env_pairs: &[(&str, &str)],
) -> Result<PathBuf, AgentError> {
    let dir = hermes_config::hermes_home()
        .join("runtime")
        .join("backend_profiles");
    std::fs::create_dir_all(&dir).map_err(|e| {
        AgentError::Io(format!(
            "Failed to create backend profile directory {}: {}",
            dir.display(),
            e
        ))
    })?;
    let path = dir.join(format!(
        "{}-{}.env",
        normalize_backend_provider(provider),
        profile.trim().to_ascii_lowercase()
    ));
    let mut body = String::new();
    for (key, value) in env_pairs {
        let _ = writeln!(body, "{}={}", key, value);
    }
    std::fs::write(&path, body).map_err(|e| {
        AgentError::Io(format!(
            "Failed to write backend profile file {}: {}",
            path.display(),
            e
        ))
    })?;
    Ok(path)
}

fn model_current_provider_and_id(model: &str) -> (String, String) {
    if let Some((provider, model_id)) = model.split_once(':') {
        (
            provider.trim().to_ascii_lowercase(),
            model_id.trim().to_string(),
        )
    } else {
        ("openai".to_string(), model.trim().to_string())
    }
}

async fn handle_model_harness_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let (current_provider, current_model_id) = model_current_provider_and_id(&app.current_model);
    let target = args.first().copied().unwrap_or_default().trim();
    let (provider, requested_model) = if target.is_empty() {
        (current_provider.clone(), current_model_id.clone())
    } else if target.contains(':') {
        let normalized = normalize_provider_model(target)?;
        let (prov, model_id) = model_current_provider_and_id(&normalized);
        (prov, model_id)
    } else {
        (normalize_backend_provider(target), current_model_id.clone())
    };

    let catalog = provider_model_ids(&provider).await;
    let catalog_total = catalog.len();
    let selected_model = requested_model.trim().to_string();
    let selected_lc = selected_model.to_ascii_lowercase();
    let selected_ok = catalog.iter().any(|candidate| {
        let lower = candidate.trim().to_ascii_lowercase();
        lower == selected_lc || lower.ends_with(&format!("/{selected_lc}"))
    });
    let credential_present = crate::app::provider_api_key_from_env(&provider).is_some();
    let auth_state_present = crate::auth::read_provider_auth_state(&provider)
        .ok()
        .flatten()
        .is_some();
    let cache_status = cached_provider_catalog_status(&provider);
    let mut out = String::new();
    let _ = writeln!(out, "Model/provider harness");
    let _ = writeln!(out, "provider: {}", provider);
    let _ = writeln!(out, "selected_model: {}", selected_model);
    let _ = writeln!(
        out,
        "credentials: api_key={} oauth_state={}",
        yes_no(credential_present),
        yes_no(auth_state_present)
    );
    let _ = writeln!(out, "catalog_total: {}", catalog_total);
    let _ = writeln!(out, "selected_in_catalog: {}", yes_no(selected_ok));
    if let Some(status) = cache_status {
        let _ = writeln!(
            out,
            "catalog_cache: verified={} age_secs={}",
            yes_no(status.verified),
            status
                .age_secs
                .map(|v| v.to_string())
                .unwrap_or_else(|| "n/a".to_string())
        );
    } else {
        let _ = writeln!(out, "catalog_cache: unavailable");
    }
    if !selected_ok {
        let sample = catalog
            .iter()
            .take(6)
            .cloned()
            .collect::<Vec<String>>()
            .join(", ");
        let _ = writeln!(
            out,
            "remediation: switch via `/model {} --provider {}` (or run `/model {}`)",
            selected_model, provider, provider
        );
        if !sample.is_empty() {
            let _ = writeln!(out, "catalog_sample: {}", sample);
        }
    }
    if provider == "openrouter" && !credential_present && !auth_state_present {
        let _ = writeln!(
            out,
            "openrouter_hint: set OPENROUTER_API_KEY or use a provider with OAuth (`/auth refresh`)."
        );
    }
    if provider == "huggingface" {
        let _ = writeln!(
            out,
            "huggingface_hint: prefer HF_TOKEN + HF_BASE_URL for full catalog enumeration."
        );
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn resolve_model_refresh_provider(app: &App, args: &[&str]) -> String {
    let raw = args.first().copied().unwrap_or_default().trim();
    if raw.is_empty() {
        return canonical_provider_id(provider_slug_from_provider_model(&app.current_model));
    }
    if let Some((provider, _)) = raw.split_once(':') {
        return canonical_provider_id(provider.trim());
    }
    canonical_provider_id(raw)
}

async fn handle_model_refresh_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let provider = resolve_model_refresh_provider(app, args);
    if provider.trim().is_empty() {
        emit_command_output(app, "Usage: /model refresh [provider|provider:model]");
        return Ok(CommandResult::Handled);
    }

    let known_provider = provider_slugs_for_config(&app.config)
        .iter()
        .any(|slug| slug.eq_ignore_ascii_case(&provider));
    let cache_cleared = clear_provider_catalog_cache(&provider)?;
    let models = provider_model_ids_for_config(&provider, &app.config).await;
    let cache_status = cached_provider_catalog_status(&provider);

    let mut out = String::new();
    let _ = writeln!(out, "Model catalog refreshed");
    let _ = writeln!(out, "provider: {}", provider);
    let _ = writeln!(out, "known_provider: {}", yes_no(known_provider));
    let _ = writeln!(out, "cache_cleared: {}", yes_no(cache_cleared));
    let _ = writeln!(out, "catalog_total: {}", models.len());
    match cache_status {
        Some(status) => {
            let _ = writeln!(
                out,
                "catalog_cache: verified={} age_secs={}",
                yes_no(status.verified),
                status
                    .age_secs
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "n/a".to_string())
            );
        }
        None => {
            let _ = writeln!(out, "catalog_cache: unavailable");
        }
    }
    if models.is_empty() {
        let _ = writeln!(out, "models_sample: (none)");
        if !known_provider {
            let _ = writeln!(
                out,
                "note: provider is not registered; configure llm_providers or use a known provider."
            );
        }
    } else {
        let sample = models
            .iter()
            .take(8)
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let _ = writeln!(out, "models_sample: {}", sample);
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn handle_model_backend_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args.first().copied().unwrap_or("list").to_ascii_lowercase();
    match action.as_str() {
        "list" | "status" => {
            let provider = args.get(1).copied();
            emit_command_output(app, render_backend_profiles(provider));
        }
        "show" => {
            let Some(provider) = args.get(1).copied() else {
                emit_command_output(app, "Usage: /model backend show <provider> [profile]");
                return Ok(CommandResult::Handled);
            };
            let profile = args.get(2).copied();
            let Some(row) = backend_profile_lookup(provider, profile) else {
                emit_command_output(
                    app,
                    format!(
                        "No backend profile found for {}:{}.",
                        provider,
                        profile.unwrap_or("balanced")
                    ),
                );
                return Ok(CommandResult::Handled);
            };
            emit_command_output(
                app,
                format!(
                    "{}:{}\n{}\nlaunch: {}\nenv: {}",
                    row.provider,
                    row.profile,
                    row.summary,
                    row.launch_hint,
                    row.env_overrides
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<String>>()
                        .join(", ")
                ),
            );
        }
        "apply" => {
            let Some(provider) = args.get(1).copied() else {
                emit_command_output(app, "Usage: /model backend apply <provider> [profile]");
                return Ok(CommandResult::Handled);
            };
            let profile = args.get(2).copied().unwrap_or("balanced");
            let Some(row) = backend_profile_lookup(provider, Some(profile)) else {
                emit_command_output(
                    app,
                    format!("No backend profile found for {}:{}.", provider, profile),
                );
                return Ok(CommandResult::Handled);
            };
            for (key, value) in row.env_overrides {
                std::env::set_var(key, value);
            }
            std::env::set_var("HERMES_LOCAL_BACKEND_PROFILE", row.profile);
            std::env::set_var("HERMES_LOCAL_BACKEND_PROVIDER", row.provider);
            let persisted = persist_backend_profile_env(row.provider, row.profile, row.env_overrides)?;
            let (current_provider, _) = model_current_provider_and_id(&app.current_model);
            if current_provider == row.provider {
                let current = app.current_model.clone();
                app.switch_model(&current);
            }
            emit_command_output(
                app,
                format!(
                    "Applied backend profile {}:{}.\nlaunch: {}\npersisted_env_file: {}\nUse `set -a && source {}` before launching external backend processes.",
                    row.provider,
                    row.profile,
                    row.launch_hint,
                    persisted.display(),
                    persisted.display()
                ),
            );
        }
        _ => emit_command_output(
            app,
            "Usage: /model backend [list|status [provider]|show <provider> [profile]|apply <provider> [profile]]",
        ),
    }
    Ok(CommandResult::Handled)
}

async fn handle_model_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if let Some(sub) = args.first().map(|v| v.trim()) {
        if sub.eq_ignore_ascii_case("failover") {
            return handle_model_failover_command(app, &args[1..]);
        }
        if sub.eq_ignore_ascii_case("backend") {
            return handle_model_backend_command(app, &args[1..]);
        }
        if sub.eq_ignore_ascii_case("harness") {
            return handle_model_harness_command(app, &args[1..]).await;
        }
        if sub.eq_ignore_ascii_case("refresh") {
            return handle_model_refresh_command(app, &args[1..]).await;
        }
        if sub.eq_ignore_ascii_case("explain") {
            return handle_model_explain_command(app, &args[1..], false).await;
        }
        if sub.eq_ignore_ascii_case("why-not")
            || sub.eq_ignore_ascii_case("whynot")
            || sub.eq_ignore_ascii_case("diagnose")
        {
            return handle_model_explain_command(app, &args[1..], true).await;
        }
    }

    let (mut positional, requirements, provider_override) = parse_model_command_args(args)?;
    if let Some(provider) = provider_override {
        if positional.is_empty() {
            positional.push(provider);
        } else if let Some(first) = positional.first().cloned() {
            let model_id = first
                .split_once(':')
                .map(|(_, rhs)| rhs.to_string())
                .unwrap_or(first);
            positional[0] = format!("{}:{}", provider, model_id.trim());
        }
    }
    let positional_refs: Vec<&str> = positional.iter().map(String::as_str).collect();
    let known_providers = provider_slugs_for_config(&app.config);
    match parse_model_switch_request(&positional_refs, &known_providers) {
        ModelSwitchRequest::SetDirect(raw) => {
            let provider_model = normalize_model_target(&app.current_model, &raw)?;
            let (guarded, note) =
                guard_provider_model_selection_for_config(&provider_model, &app.config).await?;
            if !requirements.is_empty() {
                let (provider, model_id) = split_provider_model(&guarded);
                let client = default_client();
                client.fetch(false).await;
                let caps = resolve_model_capabilities(provider, model_id, client);
                if !model_meets_requirements(caps, requirements) {
                    return Err(AgentError::Config(format!(
                        "Requested model '{}' does not satisfy required capabilities: {}.",
                        guarded,
                        requirements.summary()
                    )));
                }
            }
            let warning = app.model_switch_preflight_warning(&guarded);
            if !try_switch_model_or_emit_failure(app, &guarded) {
                return Ok(CommandResult::Handled);
            }
            let mut msg = format!("Model switched to: {}", guarded);
            if let Some(n) = note {
                msg.push('\n');
                msg.push_str(&n);
            }
            if !requirements.is_empty() {
                msg.push('\n');
                msg.push_str(&format!(
                    "Capability constraints satisfied: {}.",
                    requirements.summary()
                ));
            }
            msg.push('\n');
            msg.push_str(&format_model_persistence_note(app));
            if let Some(warning) = warning {
                msg.push('\n');
                msg.push_str(&warning);
            }
            emit_command_output(app, msg);
        }
        ModelSwitchRequest::PickModelFromProvider(provider) => {
            let current_model = app.current_model.clone();
            pick_model_for_provider(app, &provider, &current_model, requirements).await?;
        }
        ModelSwitchRequest::PickProviderThenModel => {
            emit_command_output(app, format!("Current model: {}", app.current_model));
            let providers: Vec<String> = known_providers.clone();
            if providers.is_empty() {
                emit_command_output(app, "No providers are registered for selection.");
                return Ok(CommandResult::Handled);
            }
            let (current_provider, _) = split_provider_model(&app.current_model);
            let default_provider_index = providers
                .iter()
                .position(|p| p.eq_ignore_ascii_case(current_provider))
                .unwrap_or(0);
            let provider_pick =
                run_model_picker_select(app, "Select provider", &providers, default_provider_index);
            if !provider_pick.confirmed || provider_pick.index >= providers.len() {
                emit_command_output(app, "Model switch cancelled.");
                return Ok(CommandResult::Handled);
            }
            let provider = providers[provider_pick.index].as_str();
            let current_model = app.current_model.clone();
            pick_model_for_provider(app, provider, &current_model, requirements).await?;
        }
    }
    Ok(CommandResult::Handled)
}

fn emit_command_output(app: &mut App, text: impl Into<String>) {
    let rendered = text.into();
    if app.stream_handle.is_some() {
        app.push_ui_assistant(rendered);
    } else {
        println!("{}", rendered);
    }
}

fn normalize_codex_runtime_value(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_lowercase().as_str() {
        "auto" => Some("auto"),
        "codex_app_server" | "codex-app-server" => Some("codex_app_server"),
        _ => None,
    }
}

fn parse_codex_runtime_args(args: &[&str]) -> Result<Option<&'static str>, String> {
    let raw = args.join(" ");
    let value = raw.trim().to_ascii_lowercase();
    if value.is_empty() {
        return Ok(None);
    }
    match value.as_str() {
        "on" | "codex" | "enable" => Ok(Some("codex_app_server")),
        "off" | "default" | "disable" | "hermes" => Ok(Some("auto")),
        _ => normalize_codex_runtime_value(&value)
            .map(Some)
            .ok_or_else(|| {
                format!(
                    "Unknown runtime '{}'. Use one of: auto, codex_app_server, on, off",
                    value
                )
            }),
    }
}

fn yaml_key(name: &str) -> serde_yaml::Value {
    serde_yaml::Value::String(name.to_string())
}

fn read_codex_runtime_config(path: &Path) -> Result<serde_yaml::Value, AgentError> {
    if !path.exists() {
        return Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()));
    }
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
    serde_yaml::from_str::<serde_yaml::Value>(&raw)
        .map_err(|e| AgentError::Config(format!("parse {}: {}", path.display(), e)))
}

fn codex_runtime_from_config(root: &serde_yaml::Value) -> &'static str {
    root.as_mapping()
        .and_then(|map| map.get(yaml_key("model")))
        .and_then(|model| model.as_mapping())
        .and_then(|model| model.get(yaml_key("openai_runtime")))
        .and_then(|value| value.as_str())
        .and_then(normalize_codex_runtime_value)
        .unwrap_or("auto")
}

fn model_string_to_mapping(model: &str) -> serde_yaml::Mapping {
    let mut mapping = serde_yaml::Mapping::new();
    let model = model.trim();
    if model.is_empty() {
        return mapping;
    }
    if let Some((provider, default)) = model.split_once(':') {
        if !provider.trim().is_empty() {
            mapping.insert(
                yaml_key("provider"),
                serde_yaml::Value::String(provider.trim().to_string()),
            );
        }
        if !default.trim().is_empty() {
            mapping.insert(
                yaml_key("default"),
                serde_yaml::Value::String(default.trim().to_string()),
            );
        }
    } else {
        mapping.insert(
            yaml_key("default"),
            serde_yaml::Value::String(model.to_string()),
        );
    }
    mapping
}

fn set_codex_runtime_config_value(root: &mut serde_yaml::Value, runtime: &str) {
    if !matches!(root, serde_yaml::Value::Mapping(_)) {
        *root = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    }
    let root_map = root.as_mapping_mut().expect("root mapping");
    let model_key = yaml_key("model");
    let mut model_map = match root_map.remove(&model_key) {
        Some(serde_yaml::Value::Mapping(map)) => map,
        Some(serde_yaml::Value::String(model)) => model_string_to_mapping(&model),
        Some(other) => {
            let mut map = serde_yaml::Mapping::new();
            map.insert(yaml_key("default"), other);
            map
        }
        None => serde_yaml::Mapping::new(),
    };
    model_map.insert(
        yaml_key("openai_runtime"),
        serde_yaml::Value::String(runtime.to_string()),
    );
    root_map.insert(model_key, serde_yaml::Value::Mapping(model_map));
}

fn write_codex_runtime_config(path: &Path, root: &serde_yaml::Value) -> Result<(), AgentError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("create {}: {}", parent.display(), e)))?;
    }
    let yaml = serde_yaml::to_string(root)
        .map_err(|e| AgentError::Config(format!("serialize {}: {}", path.display(), e)))?;
    std::fs::write(path, yaml)
        .map_err(|e| AgentError::Io(format!("write {}: {}", path.display(), e)))
}

fn check_codex_binary_status() -> (bool, String) {
    let output = std::process::Command::new("codex")
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output();
    match output {
        Ok(output) if output.status.success() => {
            let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
            let fallback = String::from_utf8_lossy(&output.stderr).trim().to_string();
            (true, if text.is_empty() { fallback } else { text })
        }
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            let detail = if stderr.is_empty() {
                format!("codex exited with {}", output.status)
            } else {
                stderr
            };
            (false, detail)
        }
        Err(e) => (false, e.to_string()),
    }
}

fn handle_codex_runtime_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let config_path = app.state_root.join("config.yaml");
    let mut root = read_codex_runtime_config(&config_path)?;
    let current = codex_runtime_from_config(&root);
    let new_value = match parse_codex_runtime_args(args) {
        Ok(value) => value,
        Err(message) => {
            emit_command_output(app, format!("Codex runtime error: {}", message));
            return Ok(CommandResult::Handled);
        }
    };

    let Some(new_value) = new_value else {
        let (ok, detail) = check_codex_binary_status();
        let binary_status = if ok {
            format!("OK {}", detail)
        } else {
            format!(
                "not available - {}. Install with `npm i -g @openai/codex`",
                detail
            )
        };
        emit_command_output(
            app,
            format!(
                "openai_runtime: {}\ncodex CLI: {}\nconfig: {}",
                current,
                binary_status,
                config_path.display()
            ),
        );
        return Ok(CommandResult::Handled);
    };

    if new_value == current {
        emit_command_output(app, format!("openai_runtime already set to {}", current));
        return Ok(CommandResult::Handled);
    }

    if new_value == "codex_app_server" {
        let (ok, detail) = check_codex_binary_status();
        if !ok {
            emit_command_output(
                app,
                format!(
                    "Cannot enable codex_app_server runtime: {}\nInstall with: npm i -g @openai/codex",
                    detail
                ),
            );
            return Ok(CommandResult::Handled);
        }
    }

    set_codex_runtime_config_value(&mut root, new_value);
    write_codex_runtime_config(&config_path, &root)?;
    let mut msg = format!("openai_runtime: {} -> {}", current, new_value);
    if new_value == "codex_app_server" {
        msg.push_str("\nOpenAI/Codex turns will use the Codex app-server runtime.");
    } else {
        msg.push_str("\nOpenAI/Codex turns will use the default Hermes runtime.");
    }
    msg.push_str("\nEffective on next session.");
    emit_command_output(app, msg);
    Ok(CommandResult::Handled)
}

fn format_personality_catalog(
    current_personality: Option<&str>,
    builtin_descriptions: &[(&str, &str)],
) -> String {
    let mut out = String::from("## Built-in personalities\n\n");
    if let Some(current) = current_personality.filter(|v| !v.trim().is_empty()) {
        out.push_str(&format!("Current: `{}`\n\n", current));
    } else {
        out.push_str("Current: `(none)`\n\n");
    }
    out.push_str("Use `/personality <name>` to switch.\n\n");
    for (name, usage) in builtin_descriptions {
        out.push_str(&format!("- `{}`\n  {}\n\n", name, usage));
    }
    out.trim_end().to_string()
}

fn handle_personality_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let builtin = hermes_agent::builtin_personality_names();
    let builtin_descriptions = hermes_agent::builtin_personality_descriptions();
    if args.is_empty() || (args.len() == 1 && args[0].eq_ignore_ascii_case("list")) {
        emit_command_output(
            app,
            format_personality_catalog(app.current_personality.as_deref(), builtin_descriptions),
        );
    } else {
        let name = args.join(" ");
        app.switch_personality(&name);
        let mut response = format!("Switched personality to `{}`.", name);
        if !name.contains(char::is_whitespace)
            && !name.eq_ignore_ascii_case("default")
            && !builtin.iter().any(|n| n.eq_ignore_ascii_case(&name))
        {
            response.push_str(&format!(
                "\n\nNote: `{}` is not built-in. Hermes will look for `personalities/{}.md` or treat inline text as compatibility mode.",
                name, name,
            ));
        }
        emit_command_output(app, response);
    }
    Ok(CommandResult::Handled)
}

include!("command_dispatch_model/skills_config_compress.rs");
