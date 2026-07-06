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
        .suppress_windows_console()
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

fn handle_harness_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let mut json_mode = false;
    let mut action = "status".to_string();
    let mut repo_arg = None;
    for arg in args {
        let value = arg.trim();
        if value.is_empty() {
            continue;
        }
        if value.eq_ignore_ascii_case("json") || value == "--json" {
            json_mode = true;
            continue;
        }
        let lower = value.to_ascii_lowercase();
        if action == "status"
            && matches!(
                lower.as_str(),
                "status"
                    | "all"
                    | "skills"
                    | "proof"
                    | "roadmap"
                    | "chaos"
                    | "onboarding"
                    | "objective"
                    | "objectives"
                    | "autonomy"
                    | "help"
            )
        {
            action = lower;
            continue;
        }
        if repo_arg.is_none() {
            repo_arg = Some(PathBuf::from(value));
        }
    }
    let repo_root = repo_arg.or_else(hermes_tools::repo::detect_repo_root_from_cwd);

    if json_mode || action != "status" {
        let payload = hermes_tools::tools::harness_cockpit::harness_cockpit_action_snapshot(
            &action,
            repo_root.as_deref(),
        )
        .map_err(AgentError::Config)?;
        let pretty = serde_json::to_string_pretty(&payload)
            .map_err(|err| AgentError::Config(format!("failed to render harness JSON: {err}")))?;
        emit_command_output(app, pretty);
    } else {
        emit_command_output(
            app,
            hermes_tools::tools::harness_cockpit::render_harness_cockpit_text(
                repo_root.as_deref(),
            ),
        );
    }
    Ok(CommandResult::Handled)
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
        "/prompt" | "/compose" => handle_prompt_command(app, args),
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
        "/harness" => handle_harness_command(app, args),
        "/claims" => handle_claims_command(app, args),
        "/quorum" => handle_quorum_command(app, args).await,
        "/moa" => handle_moa_command(app, args).await,
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
        "/fast" | "/timestamps" | "/skin" | "/voice" => {
            handle_runtime_ui_mode_command(app, canonical_command(cmd), args)
        }
        "/hatch" => handle_hatch_command(app, args),
        "/learn" => handle_learn_command(app, args),
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
        "/journey" => handle_journey_command(app, args).await,
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

const PROMPT_EDITOR_HEADER: &str = "#! Compose your prompt below. Lines starting with '#!' are ignored.\n#! Save and quit to send; leave empty to cancel.\n\n";

fn prompt_editor_command() -> String {
    std::env::var("VISUAL")
        .ok()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            std::env::var("EDITOR")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| {
            if cfg!(windows) {
                "notepad".to_string()
            } else {
                "nano".to_string()
            }
        })
}

fn strip_prompt_editor_header(raw: &str) -> String {
    raw.lines()
        .filter(|line| !line.starts_with("#!"))
        .collect::<Vec<_>>()
        .join("\n")
        .trim()
        .to_string()
}

fn compose_prompt_in_editor(initial_text: &str) -> Result<Option<String>, AgentError> {
    let editor = prompt_editor_command();
    let mut parts = shlex::split(&editor).unwrap_or_else(|| vec![editor.clone()]);
    if parts.is_empty() {
        return Err(AgentError::Config("No editor command configured".to_string()));
    }

    let path = std::env::temp_dir().join(format!(
        "hermes_prompt_{}_{}.md",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    let mut seed = PROMPT_EDITOR_HEADER.to_string();
    if !initial_text.is_empty() {
        seed.push_str(initial_text);
    }
    std::fs::write(&path, seed).map_err(|err| {
        AgentError::Io(format!(
            "Failed to create prompt draft {}: {}",
            path.display(),
            err
        ))
    })?;

    let program = parts.remove(0);
    let status = match Command::new(&program)
        .args(parts)
        .arg(&path)
        .suppress_windows_console()
        .status()
    {
        Ok(status) => status,
        Err(err) => {
            let _ = std::fs::remove_file(&path);
            return Err(AgentError::Io(format!(
                "Could not launch editor `{program}`: {err}"
            )));
        }
    };
    if !status.success() {
        let _ = std::fs::remove_file(&path);
        return Err(AgentError::ToolExecution(format!(
            "Editor `{program}` exited with status {status}"
        )));
    }

    let raw = match std::fs::read_to_string(&path) {
        Ok(raw) => raw,
        Err(err) => {
            let _ = std::fs::remove_file(&path);
            return Err(AgentError::Io(format!(
                "Failed to read prompt draft {}: {}",
                path.display(),
                err
            )));
        }
    };
    let _ = std::fs::remove_file(&path);
    let prompt = strip_prompt_editor_header(&raw);
    Ok((!prompt.is_empty()).then_some(prompt))
}

fn handle_prompt_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    match compose_prompt_in_editor(&args.join(" ")) {
        Ok(Some(prompt)) => {
            app.queue_pending_agent_seed(prompt);
            emit_command_output(app, "Prompt captured from editor; sending as next turn.");
        }
        Ok(None) => {
            emit_command_output(app, "Empty prompt; nothing sent.");
        }
        Err(err) => {
            emit_command_output(app, format!("Could not open prompt editor: {err}"));
        }
    }
    Ok(CommandResult::Handled)
}

async fn handle_moa_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let prompt = args.join(" ");
    if prompt.trim().is_empty() {
        emit_command_output(
            app,
            "Usage: /moa <prompt>\nRuns one prompt through moa:default and restores your prior model. Use /model to switch to a MoA preset for the session.",
        );
        return Ok(CommandResult::Handled);
    }

    app.submit_moa_oneshot(&prompt).await?;
    Ok(CommandResult::Handled)
}

fn handle_hatch_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let description = args.join(" ").trim().to_string();
    if description.is_empty() {
        emit_command_output(app, "Usage: /hatch <pet description>");
        return Ok(CommandResult::Handled);
    }

    let description_lc = description.to_ascii_lowercase();
    let mut settings = app.pet_settings().clone();
    settings.enabled = true;
    if let Some(species) = PetSettings::species_catalog()
        .iter()
        .find(|species| description_lc.contains(**species))
    {
        settings.species = (*species).to_string();
    }
    if let Some(mood) = PetSettings::mood_catalog()
        .iter()
        .find(|mood| description_lc.contains(**mood))
    {
        settings.mood = (*mood).to_string();
    } else if ["energetic", "excited", "spark", "chaos", "sigma"]
        .iter()
        .any(|needle| description_lc.contains(needle))
    {
        settings.mood = "hyped".to_string();
    } else if ["calm", "zen", "cozy", "soft"]
        .iter()
        .any(|needle| description_lc.contains(needle))
    {
        settings.mood = "chill".to_string();
    }
    app.set_pet_settings(settings.clone())?;
    app.queue_next_turn_system_note(format!(
        "Petdex hatch request: turn this description into a concise reusable companion design with species, mood, visual motif, and behavior notes: {description}"
    ));
    emit_command_output(
        app,
        format!(
            "Pet hatch request captured.\n{}\nNext turn will receive the petdex design brief.",
            render_pet_status(&settings)
        ),
    );
    Ok(CommandResult::Handled)
}

fn handle_learn_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let subject = args.join(" ").trim().to_string();
    if subject.is_empty() {
        emit_command_output(app, "Usage: /learn <what to learn from>");
        return Ok(CommandResult::Handled);
    }

    let objective_id = load_objective_contract()?
        .map(|contract| contract.id)
        .unwrap_or_else(|| app.session_id.clone());
    let _ = append_objective_learning_entry(ObjectiveLearningLedgerEntry {
        recorded_at: String::new(),
        objective_id,
        objective_state: "learning_capture".to_string(),
        decision: "learn_command".to_string(),
        evidence_files: vec![],
        evidence_commands: vec![format!("/learn {subject}")],
        notes: subject.clone(),
    });
    app.queue_next_turn_system_note(format!(
        "Learning capture request: distill reusable project knowledge, skill steps, and future-agent guidance from this material. Mark unverified claims explicitly: {subject}"
    ));
    emit_command_output(
        app,
        "Learning request captured. It was recorded in the objective learning ledger and queued for the next turn.",
    );
    Ok(CommandResult::Handled)
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
