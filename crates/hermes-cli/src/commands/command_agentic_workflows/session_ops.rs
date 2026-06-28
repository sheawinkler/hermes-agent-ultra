fn handle_session_compat_command(
    app: &mut App,
    cmd: &str,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let arg_joined = args.join(" ");
    let msg = match cmd {
        "/title" => {
            if arg_joined.trim().is_empty() {
                "Usage: /title <name>".to_string()
            } else {
                format!("Session title marker set to: {}", arg_joined.trim())
            }
        }
        "/branch" => "Use `/branch` (native) for list/diff/merge/save controls.".to_string(),
        _ => "Compatibility command acknowledged.".to_string(),
    };
    emit_command_output(app, msg);
    Ok(CommandResult::Handled)
}

fn handle_clear_queue_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let jobs_dir = hermes_config::hermes_home().join("background_jobs");
    let mut removed = 0usize;
    if let Ok(read_dir) = std::fs::read_dir(&jobs_dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("json") {
                continue;
            }
            let map = std::fs::read_to_string(&path)
                .ok()
                .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            let status = map
                .get("status")
                .and_then(|x| x.as_str())
                .unwrap_or_default()
                .to_string();
            if matches!(
                status.as_str(),
                "queued" | "running" | "failed" | "completed"
            ) {
                if status == "running" {
                    let pid = map
                        .get("pid")
                        .and_then(|v| v.as_u64())
                        .and_then(|raw| u32::try_from(raw).ok());
                    if let Some(pid) = pid {
                        if process_running(pid) {
                            let _ = terminate_pid(pid);
                        }
                    }
                }
                if std::fs::remove_file(&path).is_ok() {
                    removed += 1;
                }
            }
        }
    }
    emit_command_output(
        app,
        format!("Cleared {} queued/background status file(s).", removed),
    );
    Ok(CommandResult::Handled)
}

fn handle_insights_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let msg_count = app.messages.len();
    let user_count = app
        .messages
        .iter()
        .filter(|m| m.role == hermes_core::MessageRole::User)
        .count();
    let assistant_count = app
        .messages
        .iter()
        .filter(|m| m.role == hermes_core::MessageRole::Assistant)
        .count();
    emit_command_output(
        app,
        format!(
            "Session insights:\n  - Total messages: {}\n  - User messages: {}\n  - Hermes messages: {}\n  - Session: {}",
            msg_count, user_count, assistant_count, app.session_id
        ),
    );
    Ok(CommandResult::Handled)
}

fn handle_platforms_command(app: &mut App) -> Result<CommandResult, AgentError> {
    if app.config.platforms.is_empty() {
        emit_command_output(
            app,
            "No explicit gateway platform adapters configured (running in local CLI mode).",
        );
        return Ok(CommandResult::Handled);
    }
    let mut entries: Vec<_> = app.config.platforms.keys().cloned().collect();
    entries.sort();
    let mut out = String::from("Configured gateway platforms:\n");
    for p in entries {
        let _ = writeln!(out, "  - {}", p);
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn render_platform_command_list(app: &App) -> String {
    if app.config.platforms.is_empty() {
        return "Gateway platforms\nConnected: local CLI\nConfigured adapters: (none)\nFailed/paused: unavailable in local CLI mode".to_string();
    }

    let mut entries: Vec<_> = app.config.platforms.iter().collect();
    entries.sort_by(|a, b| a.0.cmp(b.0));
    let mut out = String::from("Gateway platforms\nConnected: local CLI\nConfigured adapters:\n");
    for (name, platform) in entries {
        let token_state = if platform
            .token
            .as_ref()
            .is_some_and(|v| !v.trim().is_empty())
        {
            "configured"
        } else {
            "missing"
        };
        let webhook_state = if platform
            .webhook_url
            .as_ref()
            .is_some_and(|v| !v.trim().is_empty())
        {
            "configured"
        } else {
            "missing"
        };
        let _ = writeln!(
            out,
            "  - {}: enabled={} token={} webhook={}",
            name, platform.enabled, token_state, webhook_state
        );
    }
    out.push_str("Failed/paused: unavailable in local CLI mode; run /platform from the gateway chat to control retry queues.");
    out
}

fn handle_platform_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args.first().copied().unwrap_or("list").to_ascii_lowercase();
    match action.as_str() {
        "list" | "status" => {
            let output = render_platform_command_list(app);
            emit_command_output(app, output);
        }
        "pause" | "resume" => {
            let Some(target) = args.get(1).copied().map(str::trim).filter(|v| !v.is_empty()) else {
                emit_command_output(app, format!("Usage: /platform {} <name>", action));
                return Ok(CommandResult::Handled);
            };
            if !app.config.platforms.contains_key(target) {
                emit_command_output(app, format!("Unknown platform: {}", target));
                return Ok(CommandResult::Handled);
            }
            emit_command_output(
                app,
                format!(
                    "Platform {} for '{}' is handled by the running gateway process. Run `/platform {} {}` from a gateway chat, or restart the gateway after config changes.",
                    action, target, action, target
                ),
            );
        }
        _ => emit_command_output(
            app,
            "Usage: /platform <list|pause|resume> [name]\n  /platform list - show platform status\n  /platform pause <name> - stop retrying a failing gateway platform\n  /platform resume <name> - re-queue a paused gateway platform",
        ),
    }
    Ok(CommandResult::Handled)
}

fn integrations_snapshot_path(session_id: &str) -> PathBuf {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    hermes_config::hermes_home().join("logs").join(format!(
        "integrations-snapshot-{}-{}.json",
        session_id, stamp
    ))
}

fn render_integrations_repair_steps(
    provider: &str,
    auth_ok: bool,
    oauth_gate: Option<(bool, String)>,
    memory_probe: &str,
) -> String {
    let mut out = String::new();
    out.push_str("Integrations repair plan\n");
    out.push_str("------------------------\n");
    let _ = writeln!(out, "provider: {}", provider);
    if !auth_ok {
        out.push_str("- auth: FAIL -> run `/auth status` then `/auth verify` (or `hermes-ultra auth add`).\n");
    } else {
        out.push_str("- auth: PASS\n");
    }
    if let Some((ok, detail)) = oauth_gate {
        if ok {
            let _ = writeln!(out, "- oauth runtime gate: PASS ({})", detail);
        } else {
            let _ = writeln!(
                out,
                "- oauth runtime gate: FAIL ({}) -> rebuild/install latest CLI binary.",
                detail
            );
        }
    }
    if memory_probe.to_ascii_lowercase().starts_with("warn") {
        let _ = writeln!(
            out,
            "- contextlattice probe: {} -> verify local orchestrator and env vars (CONTEXTLATTICE_ORCHESTRATOR_URL/MEMMCP_ORCHESTRATOR_URL).",
            memory_probe
        );
    } else {
        let _ = writeln!(out, "- contextlattice probe: {}", memory_probe);
    }
    out.push_str(
        "- tools: run `/tools` and `/integrations status` to verify adapter registry health.\n",
    );
    out.push_str(
        "- walkthrough: run `/walkthrough next` to continue operator recovery sequence.\n",
    );
    out
}

async fn handle_integrations_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    let provider = app.current_runtime_provider();
    let provider_cap = crate::providers::provider_capability_for(&provider);
    let oauth_capable = provider_cap
        .as_ref()
        .map(|cap| cap.oauth_supported)
        .unwrap_or(false);
    let managed_tools = provider_cap
        .as_ref()
        .map(|cap| cap.managed_tools_supported)
        .unwrap_or(false);
    let credential_present = crate::app::provider_api_key_from_env(&provider).is_some();
    let oauth_state_present = crate::auth::read_provider_auth_state(&provider)
        .ok()
        .flatten()
        .is_some();
    let auth_ok = credential_present || (oauth_capable && oauth_state_present);
    let oauth_gate = oauth_runtime_gate_for_provider(&provider);
    let oauth_manifest_source = if oauth_capable {
        let (_, source) = load_oauth_runtime_gate_manifest();
        source
    } else {
        "n/a".to_string()
    };

    let memory_url = std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL")
        .ok()
        .or_else(|| std::env::var("MEMMCP_ORCHESTRATOR_URL").ok())
        .unwrap_or_else(|| "http://127.0.0.1:8075".to_string());
    let memory_probe = match reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    {
        Ok(client) => {
            let health_url = format!("{}/health", memory_url.trim_end_matches('/'));
            match client.get(&health_url).send().await {
                Ok(resp) if resp.status().is_success() => format!("PASS ({})", health_url),
                Ok(resp) => format!("WARN ({} status={})", health_url, resp.status()),
                Err(err) => format!(
                    "WARN ({} error={})",
                    health_url,
                    truncate_chars(&err.to_string(), 96)
                ),
            }
        }
        Err(err) => format!(
            "WARN (client build failed: {})",
            truncate_chars(&err.to_string(), 96)
        ),
    };

    let tools_count = app.tool_registry.list_tools().len();
    let plugins_count = discover_plugin_surface(true).len();
    let mcp_count = app.config.mcp_servers.len();
    let platforms_count = app.config.platforms.len();

    if action == "repair" {
        emit_command_output(
            app,
            render_integrations_repair_steps(&provider, auth_ok, oauth_gate.clone(), &memory_probe),
        );
        return Ok(CommandResult::Handled);
    }

    if action == "snapshot" {
        let path = integrations_snapshot_path(&app.session_id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                AgentError::Io(format!("Failed to create {}: {}", parent.display(), e))
            })?;
        }
        let payload = serde_json::json!({
            "captured_at": chrono::Utc::now().to_rfc3339(),
            "session_id": app.session_id,
            "provider": provider,
            "model": app.current_model,
            "auth": {
                "oauth_capable": oauth_capable,
                "managed_tools_supported": managed_tools,
                "credential_present": credential_present,
                "oauth_state_present": oauth_state_present,
                "status": if auth_ok { "PASS" } else { "FAIL" },
                "oauth_runtime_gate": oauth_gate.as_ref().map(|(ok, detail)| serde_json::json!({"ok": ok, "detail": detail})),
            },
            "panels": {
                "providers_count": curated_provider_slugs().len(),
                "platform_adapters": platforms_count,
                "mcp_servers": mcp_count,
                "plugins": plugins_count,
                "toolsets": app.config.platform_toolsets.len(),
                "registered_tools": tools_count,
                "contextlattice_url": memory_url,
                "memory_probe": memory_probe,
            }
        });
        let json = serde_json::to_string_pretty(&payload)
            .map_err(|e| AgentError::Io(format!("Failed to encode snapshot payload: {}", e)))?;
        std::fs::write(&path, json)
            .map_err(|e| AgentError::Io(format!("Failed to write {}: {}", path.display(), e)))?;
        emit_command_output(
            app,
            format!(
                "Integration snapshot exported:\n{}\nUse `/integrations repair` for remediation guidance.",
                path.display()
            ),
        );
        return Ok(CommandResult::Handled);
    }

    let mut out = String::new();
    out.push_str("Integration Control Plane\n");
    out.push_str("=========================\n");

    if action == "status" || action == "all" || action == "auth" {
        out.push_str("Auth panel\n----------\n");
        let _ = writeln!(out, "provider: {}", provider);
        let _ = writeln!(out, "model: {}", app.current_model);
        let _ = writeln!(out, "oauth_capable: {}", oauth_capable);
        let _ = writeln!(out, "managed_tools_supported: {}", managed_tools);
        let _ = writeln!(out, "credential_present: {}", credential_present);
        let _ = writeln!(out, "oauth_state_present: {}", oauth_state_present);
        let _ = writeln!(out, "status: {}", if auth_ok { "PASS" } else { "FAIL" });
        let _ = writeln!(out, "oauth_manifest: {}", oauth_manifest_source);
        if let Some((gate_ok, gate_detail)) = oauth_gate.clone() {
            let _ = writeln!(
                out,
                "oauth_runtime_gate: {} ({})",
                if gate_ok { "PASS" } else { "FAIL" },
                gate_detail
            );
            if !gate_ok {
                out.push_str("remediation: upgrade runtime and retry auth.\n");
            }
        }
        out.push('\n');
    }

    if action == "status" || action == "all" || action == "providers" {
        let providers = curated_provider_slugs();
        out.push_str("Providers panel\n---------------\n");
        let _ = writeln!(out, "configured_providers: {}", providers.join(", "));
        let _ = writeln!(out, "provider_count: {}", providers.len());
        out.push('\n');
    }

    if action == "status" || action == "all" || action == "gateway" {
        out.push_str("Gateway panel\n-------------\n");
        let _ = writeln!(out, "platform_adapters: {}", platforms_count);
        let _ = writeln!(out, "mcp_servers: {}", mcp_count);
        let _ = writeln!(out, "plugins: {}", plugins_count);
        let _ = writeln!(out, "toolsets: {}", app.config.platform_toolsets.len());
        out.push('\n');
    }

    if action == "status" || action == "all" || action == "memory" {
        out.push_str("Memory panel\n------------\n");
        let _ = writeln!(out, "contextlattice_url: {}", memory_url);
        let _ = writeln!(out, "probe: {}", memory_probe);
        let _ = writeln!(out, "registered_tools: {}", tools_count);
        out.push('\n');
    }

    if !matches!(
        action.as_str(),
        "status" | "all" | "auth" | "providers" | "gateway" | "memory" | "repair" | "snapshot"
    ) {
        emit_command_output(
            app,
            "Usage: /integrations [status|all|auth|providers|gateway|memory|repair|snapshot]",
        );
        return Ok(CommandResult::Handled);
    }

    out.push_str("Next actions:\n");
    out.push_str("- `/boot` for startup readiness\n");
    out.push_str("- `/auth verify` for runtime credential hydration\n");
    out.push_str("- `/walkthrough next` for guided operator setup\n");
    out.push_str(
        "- `/integrations repair` for remediation plan and `/integrations snapshot` for export\n",
    );
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn handle_log_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let logs_dir = hermes_config::hermes_home().join("logs");
    let mut files = Vec::new();
    if let Ok(read_dir) = std::fs::read_dir(&logs_dir) {
        for entry in read_dir.flatten() {
            let path = entry.path();
            if path.is_file() {
                files.push(path);
            }
        }
    }
    files.sort();
    files.reverse();
    if files.is_empty() {
        emit_command_output(app, format!("No log files found in {}", logs_dir.display()));
        return Ok(CommandResult::Handled);
    }
    let mut out = format!("Recent log files in {}:\n", logs_dir.display());
    for path in files.into_iter().take(12) {
        let _ = writeln!(
            out,
            "  - {}",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
    }
    out.push_str("Use `hermes logs` for full tail output.");
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn handle_debug_dump_command(app: &mut App, _args: &[&str]) -> Result<CommandResult, AgentError> {
    let stamp = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let prefix = app.session_id.chars().take(8).collect::<String>();
    let stem = format!("debug-{}-{}", prefix, stamp);
    let snapshot_path = app.persist_session_snapshot(Some(&stem))?;
    let logs_dir = hermes_config::hermes_home().join("logs");
    let log_files = std::fs::read_dir(&logs_dir)
        .ok()
        .into_iter()
        .flat_map(|rd| rd.filter_map(|entry| entry.ok()))
        .filter(|entry| entry.path().is_file())
        .count();
    let out = format!(
        "Debug snapshot written.\n  session_id: {}\n  model: {}\n  messages: {}\n  snapshot: {}\n  logs_dir: {} ({} files)\nTip: run `hermes debug share --local` for a support bundle.",
        app.session_id,
        app.current_model,
        app.messages.len(),
        snapshot_path.display(),
        logs_dir.display(),
        log_files
    );
    emit_command_output(app, out);
    Ok(CommandResult::Handled)
}

fn handle_dump_format_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let mut out = String::new();
    let _ = writeln!(out, "Session snapshot format");
    let _ = writeln!(out, "  root keys: session_info, messages");
    let _ = writeln!(
        out,
        "  session_info keys: session_id, model, personality, message_count, created_at"
    );
    let _ = writeln!(
        out,
        "  message keys: role, content, tool_call_id, tool_calls, reasoning_content"
    );
    let _ = writeln!(
        out,
        "  save path: {}/sessions/<session-id>.json",
        app.state_root.display()
    );
    let _ = writeln!(out, "Use `/save [name]` to persist a snapshot now.");
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn handle_experiment_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        let active = current_session_steer(app)
            .filter(|value| value.to_ascii_lowercase().starts_with("experiment: "))
            .map(|value| value.trim_start_matches("Experiment: ").to_string())
            .unwrap_or_else(|| "(none)".to_string());
        emit_command_output(
            app,
            format!(
                "Experiment steering: {}\nUsage: /experiment <label or instruction> | /experiment clear",
                active
            ),
        );
        return Ok(CommandResult::Handled);
    }
    if args[0].eq_ignore_ascii_case("clear") {
        let active = current_session_steer(app)
            .map(|value| value.to_ascii_lowercase().starts_with("experiment: "))
            .unwrap_or(false);
        if active {
            set_session_steer(app, None);
            emit_command_output(app, "Cleared experiment steering context.");
        } else {
            emit_command_output(
                app,
                "No experiment steering context active. Use `/experiment <instruction>`.",
            );
        }
        return Ok(CommandResult::Handled);
    }
    let hint = args.join(" ").trim().to_string();
    if hint.is_empty() {
        emit_command_output(
            app,
            "Usage: /experiment <label or instruction> | /experiment clear",
        );
        return Ok(CommandResult::Handled);
    }
    let steer = format!("Experiment: {hint}");
    set_session_steer(app, Some(steer.clone()));
    emit_command_output(
        app,
        format!(
            "Experiment steering applied.\n{}\nUse `/model` to switch variants, then `/retry` to re-run the last turn.",
            steer
        ),
    );
    Ok(CommandResult::Handled)
}

fn feedback_log_path() -> PathBuf {
    hermes_config::hermes_home()
        .join("logs")
        .join("feedback.ndjson")
}

fn handle_feedback_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        emit_command_output(
            app,
            "Usage: /feedback <note>\nStores a local feedback record at ~/.hermes-agent-ultra/logs/feedback.ndjson.",
        );
        return Ok(CommandResult::Handled);
    }
    let note = args.join(" ").trim().to_string();
    if note.is_empty() {
        emit_command_output(app, "Usage: /feedback <note>");
        return Ok(CommandResult::Handled);
    }
    let path = feedback_log_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("Failed to create {}: {}", parent.display(), e)))?;
    }
    let record = serde_json::json!({
        "at": chrono::Utc::now().to_rfc3339(),
        "session_id": app.session_id,
        "model": app.current_model,
        "note": note,
    });
    let mut writer = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| AgentError::Io(format!("Failed to open {}: {}", path.display(), e)))?;
    writer
        .write_all(format!("{}\n", record).as_bytes())
        .map_err(|e| AgentError::Io(format!("Failed to append {}: {}", path.display(), e)))?;
    emit_command_output(app, format!("Feedback captured in {}", path.display()));
    Ok(CommandResult::Handled)
}

fn handle_restart_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let preserve_model = args.first().is_some_and(|v| {
        matches!(
            v.to_ascii_lowercase().as_str(),
            "keep-model" | "--keep-model"
        )
    });
    let previous_model = app.current_model.clone();
    app.new_session();
    if preserve_model && !previous_model.eq_ignore_ascii_case(&app.current_model) {
        app.switch_model(&previous_model);
    }
    emit_command_output(
        app,
        format!(
            "Session restarted.\n  new_session_id: {}\n  model: {}",
            app.session_id, app.current_model
        ),
    );
    Ok(CommandResult::Handled)
}

async fn handle_update_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let check_only = args
        .first()
        .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "check" | "--check"));
    let report = crate::update::check_for_updates().await?;
    let mut out = String::new();
    let _ = writeln!(out, "Update status");
    if check_only {
        let _ = writeln!(out, "  mode: check-only");
    }
    let _ = writeln!(out, "{}", report.trim());
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn handle_redraw_command(app: &mut App) -> Result<CommandResult, AgentError> {
    app.push_ui_assistant("↻ Repaint pulse requested.");
    emit_command_output(
        app,
        "Repaint pulse sent.\nIf the screen still looks stale: press Ctrl+L (lane toggle) or resize the terminal once.",
    );
    Ok(CommandResult::Handled)
}

fn handle_paste_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let text = if let Some(mock) = std::env::var("HERMES_TEST_CLIPBOARD_TEXT")
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    {
        mock
    } else {
        arboard::Clipboard::new()
            .and_then(|mut cb| cb.get_text())
            .map_err(|e| AgentError::Config(format!("Clipboard unavailable: {}", e)))?
    };
    let trimmed = text.trim();
    if trimmed.is_empty() {
        emit_command_output(app, "Clipboard is empty.");
        return Ok(CommandResult::Handled);
    }
    let pastes_dir = hermes_config::hermes_home().join("pastes");
    std::fs::create_dir_all(&pastes_dir)
        .map_err(|e| AgentError::Io(format!("Failed to create {}: {}", pastes_dir.display(), e)))?;
    let file_name = format!("paste-{}.txt", chrono::Utc::now().format("%Y%m%d-%H%M%S"));
    let path = pastes_dir.join(file_name);
    std::fs::write(&path, trimmed)
        .map_err(|e| AgentError::Io(format!("Failed to write {}: {}", path.display(), e)))?;

    let preview = if args.first().is_some_and(|v| v.eq_ignore_ascii_case("show")) {
        trimmed.to_string()
    } else {
        truncate_chars(trimmed, 280)
    };

    let mut out = String::new();
    let _ = writeln!(out, "Clipboard captured:");
    let _ = writeln!(out, "  - chars: {}", trimmed.chars().count());
    let _ = writeln!(out, "  - saved: {}", path.display());
    let _ = writeln!(out, "  - preview: {}", preview);
    let _ = writeln!(
        out,
        "Use `/background review {}` to process it in isolation.",
        path.display()
    );
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

async fn handle_gquota_command(app: &mut App, _args: &[&str]) -> Result<CommandResult, AgentError> {
    let provider = app
        .current_model
        .split_once(':')
        .map(|(p, _)| p.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "unknown".to_string());
    let gemini_vars = [
        "HERMES_GEMINI_OAUTH_API_KEY",
        "GOOGLE_API_KEY",
        "GEMINI_API_KEY",
    ];
    let mut present = Vec::new();
    for key in gemini_vars {
        if std::env::var(key)
            .ok()
            .is_some_and(|v| !v.trim().is_empty())
        {
            present.push(key.to_string());
        }
    }
    let oauth_state = crate::auth::read_provider_auth_state("google-gemini-cli")
        .ok()
        .flatten();
    let expires_at = oauth_state
        .as_ref()
        .and_then(|v| v.get("expires_at_ms"))
        .and_then(|v| v.as_i64());
    let mut out = String::new();
    let _ = writeln!(out, "Gemini quota/auth diagnostics");
    let _ = writeln!(out, "  - active provider: {}", provider);
    let _ = writeln!(
        out,
        "  - gemini creds in env: {} ({})",
        if present.is_empty() { "no" } else { "yes" },
        if present.is_empty() {
            "none".to_string()
        } else {
            present.join(", ")
        }
    );
    let _ = writeln!(
        out,
        "  - oauth state file: {}",
        if oauth_state.is_some() {
            "present"
        } else {
            "missing"
        }
    );
    if let Some(ms) = expires_at {
        let ts = chrono::DateTime::<chrono::Utc>::from_timestamp_millis(ms)
            .map(|d| d.to_rfc3339())
            .unwrap_or_else(|| "invalid".to_string());
        let _ = writeln!(out, "  - token expiry: {}", ts);
    }
    let _ = writeln!(
        out,
        "  - live quota API: unavailable in local CLI; check provider dashboard for hard usage limits."
    );
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn handle_approve_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let store = PairingStore::open_default();
    if args.is_empty() || args[0].eq_ignore_ascii_case("list") {
        let pending: Vec<_> = store
            .list()
            .unwrap_or_default()
            .into_iter()
            .filter(|d| d.status == PairingStatus::Pending)
            .collect();
        if pending.is_empty() {
            emit_command_output(
                app,
                "No pending devices to approve. Use `hermes pairing list` for full inventory.",
            );
            return Ok(CommandResult::Handled);
        }
        let mut out = String::from("Pending pairing devices:\n");
        for dev in pending {
            out.push_str(&format!(
                "  - {} ({})\n",
                dev.device_id,
                dev.name.unwrap_or_else(|| "unnamed".to_string())
            ));
        }
        out.push_str("Approve one with `/approve <device-id>` or all with `/approve all`.");
        emit_command_output(app, out.trim_end());
        return Ok(CommandResult::Handled);
    }

    if args[0].eq_ignore_ascii_case("all") {
        let mut approved = 0usize;
        for dev in store.list().unwrap_or_default() {
            if dev.status == PairingStatus::Pending && store.approve(&dev.device_id).is_ok() {
                approved += 1;
            }
        }
        emit_command_output(app, format!("Approved {} pending device(s).", approved));
        return Ok(CommandResult::Handled);
    }

    match store.approve(args[0]) {
        Ok(dev) => emit_command_output(
            app,
            format!(
                "Approved device '{}' (name={}).",
                dev.device_id,
                dev.name.unwrap_or_else(|| "unnamed".to_string())
            ),
        ),
        Err(err) => emit_command_output(
            app,
            format!(
                "Approve failed: {}. Use `/approve list` or `hermes pairing list`.",
                err
            ),
        ),
    }
    Ok(CommandResult::Handled)
}

fn handle_deny_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let store = PairingStore::open_default();
    if args.is_empty() || args[0].eq_ignore_ascii_case("list") {
        let entries = store.list().unwrap_or_default();
        let mut out = String::from("Pairing devices (deny/revoke candidates):\n");
        if entries.is_empty() {
            out.push_str("  - none\n");
        } else {
            for dev in entries {
                out.push_str(&format!("  - {} [{}]\n", dev.device_id, dev.status));
            }
        }
        out.push_str("Revoke one with `/deny <device-id>` or purge pending with `/deny pending`.");
        emit_command_output(app, out.trim_end());
        return Ok(CommandResult::Handled);
    }

    if args[0].eq_ignore_ascii_case("pending") || args[0].eq_ignore_ascii_case("clear-pending") {
        match store.clear_pending() {
            Ok(count) => emit_command_output(app, format!("Removed {} pending device(s).", count)),
            Err(err) => {
                emit_command_output(app, format!("Failed clearing pending devices: {}", err))
            }
        }
        return Ok(CommandResult::Handled);
    }

    match store.revoke(args[0]) {
        Ok(dev) => emit_command_output(
            app,
            format!(
                "Revoked device '{}' (name={}).",
                dev.device_id,
                dev.name.unwrap_or_else(|| "unnamed".to_string())
            ),
        ),
        Err(err) => emit_command_output(
            app,
            format!(
                "Deny failed: {}. Use `/deny list` or `hermes pairing list`.",
                err
            ),
        ),
    }
    Ok(CommandResult::Handled)
}

fn handle_copy_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let maybe_text = app.transcript_messages().into_iter().rev().find_map(|msg| {
        if msg.role != hermes_core::MessageRole::Assistant {
            return None;
        }
        let content = msg.content.unwrap_or_default();
        let trimmed = content.trim();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed.to_string())
        }
    });
    let Some(text) = maybe_text else {
        emit_command_output(
            app,
            "Copy skipped: no assistant message content available yet.",
        );
        return Ok(CommandResult::Handled);
    };

    match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text.clone())) {
        Ok(()) => emit_command_output(
            app,
            format!(
                "Copied latest assistant message ({} chars).",
                text.chars().count()
            ),
        ),
        Err(err) => emit_command_output(
            app,
            format!(
                "Clipboard unavailable ({}). Copy directly from transcript as fallback.",
                err
            ),
        ),
    }
    Ok(CommandResult::Handled)
}

fn handle_statusbar_command(app: &mut App) -> Result<CommandResult, AgentError> {
    emit_command_output(
        app,
        "Status bar is always enabled in the current TUI renderer.",
    );
    Ok(CommandResult::Handled)
}

fn parse_toggle_arg(raw: Option<&str>, current: bool) -> Result<bool, &'static str> {
    let Some(raw) = raw else {
        return Ok(!current);
    };
    match raw.trim().to_ascii_lowercase().as_str() {
        "" | "toggle" => Ok(!current),
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        _ => Err("Usage: /mouse [on|off|toggle]"),
    }
}

fn handle_mouse_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.len() >= 2 && args[0].eq_ignore_ascii_case("set") {
        match parse_toggle_arg(args.get(1).copied(), app.mouse_enabled()) {
            Ok(next) => {
                app.set_mouse_enabled(next);
                std::env::set_var("HERMES_TUI_MOUSE", if next { "1" } else { "0" });
                emit_command_output(
                    app,
                    format!("Mouse interactions: {}", if next { "ON" } else { "OFF" }),
                );
            }
            Err(usage) => emit_command_output(app, usage),
        }
        return Ok(CommandResult::Handled);
    }

    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        emit_command_output(
            app,
            format!(
                "Mouse interactions: {} (use `/mouse on` or `/mouse off`)",
                if app.mouse_enabled() { "ON" } else { "OFF" }
            ),
        );
        return Ok(CommandResult::Handled);
    }

    match parse_toggle_arg(args.first().copied(), app.mouse_enabled()) {
        Ok(next) => {
            app.set_mouse_enabled(next);
            std::env::set_var("HERMES_TUI_MOUSE", if next { "1" } else { "0" });
            emit_command_output(
                app,
                format!("Mouse interactions: {}", if next { "ON" } else { "OFF" }),
            );
        }
        Err(usage) => emit_command_output(app, usage),
    }
    Ok(CommandResult::Handled)
}

fn render_command_catalog(filter: Option<&str>) -> String {
    hermes_cli_ui::render_command_catalog(filter, SLASH_COMMANDS)
}

fn handle_commands_catalog_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let query = if args.is_empty() {
        None
    } else if args[0].eq_ignore_ascii_case("search") {
        let rest = args.get(1..).unwrap_or(&[]).join(" ");
        if rest.trim().is_empty() {
            None
        } else {
            Some(rest)
        }
    } else {
        let rest = args.join(" ");
        if rest.trim().is_empty() {
            None
        } else {
            Some(rest)
        }
    };
    emit_command_output(app, render_command_catalog(query.as_deref()));
    Ok(CommandResult::Handled)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReadinessState {
    Pass,
    Warn,
    Fail,
}

#[derive(Debug, Clone)]
struct ReadinessCheck {
    name: String,
    state: ReadinessState,
    detail: String,
    remediation: String,
}

fn readiness_state_label(state: ReadinessState) -> &'static str {
    match state {
        ReadinessState::Pass => "PASS",
        ReadinessState::Warn => "WARN",
        ReadinessState::Fail => "FAIL",
    }
}

fn oauth_runtime_gate_manifest_path() -> Option<PathBuf> {
    std::env::var("HERMES_OAUTH_GATE_MANIFEST_PATH")
        .ok()
        .map(|v| PathBuf::from(v.trim()))
        .filter(|path| path.exists())
        .or_else(|| {
            let path = hermes_config::hermes_home().join("oauth-gate-manifest.json");
            if path.exists() {
                Some(path)
            } else {
                None
            }
        })
}

fn load_oauth_runtime_gate_manifest() -> (OAuthRuntimeGateManifest, String) {
    if let Some(path) = oauth_runtime_gate_manifest_path() {
        if let Some(parsed) = load_oauth_runtime_gate_manifest_from_path(&path) {
            return (parsed, path.display().to_string());
        }
    }
    (
        oauth_runtime_gate_manifest_default(),
        "builtin-default".to_string(),
    )
}

fn oauth_runtime_gate_for_provider(provider: &str) -> Option<(bool, String)> {
    let (manifest, source) = load_oauth_runtime_gate_manifest();
    shared_oauth_runtime_gate_for_provider(provider, env!("CARGO_PKG_VERSION"), &manifest, source)
        .map(|gate| (gate.ok, gate.detail))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BootProfile {
    Dev,
    Standard,
    Prod,
}

impl BootProfile {
    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "dev" => Some(Self::Dev),
            "standard" | "balanced" | "default" => Some(Self::Standard),
            "prod" | "production" | "strict" => Some(Self::Prod),
            _ => None,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Dev => "dev",
            Self::Standard => "standard",
            Self::Prod => "prod",
        }
    }
}

fn boot_profile_env() -> BootProfile {
    std::env::var("HERMES_BOOT_PROFILE")
        .ok()
        .and_then(|v| BootProfile::parse(&v))
        .unwrap_or(BootProfile::Standard)
}

fn boot_profile_overall(profile: BootProfile, fail: usize, warn: usize) -> &'static str {
    match profile {
        BootProfile::Dev => {
            if fail == 0 {
                "PASS"
            } else {
                "FAIL"
            }
        }
        BootProfile::Standard => {
            if fail == 0 {
                if warn == 0 {
                    "PASS"
                } else {
                    "WARN"
                }
            } else {
                "FAIL"
            }
        }
        BootProfile::Prod => {
            if fail == 0 && warn == 0 {
                "PASS"
            } else {
                "FAIL"
            }
        }
    }
}

async fn collect_boot_readiness_checks(app: &App, quick: bool) -> Vec<ReadinessCheck> {
    let mut checks = Vec::new();
    let home = hermes_config::hermes_home();
    let config_path = home.join("config.yaml");
    let sessions_dir = home.join("sessions");
    let logs_dir = home.join("logs");
    let skills_dir = home.join("skills");

    checks.push(ReadinessCheck {
        name: "Hermes home".to_string(),
        state: if home.exists() {
            ReadinessState::Pass
        } else {
            ReadinessState::Fail
        },
        detail: format!("{}", home.display()),
        remediation: "Run `hermes-ultra setup` to initialize home directories.".to_string(),
    });

    for (name, path) in [
        ("Config", config_path.clone()),
        ("Sessions", sessions_dir.clone()),
        ("Logs", logs_dir.clone()),
        ("Skills", skills_dir.clone()),
    ] {
        checks.push(ReadinessCheck {
            name: name.to_string(),
            state: if path.exists() {
                ReadinessState::Pass
            } else {
                ReadinessState::Warn
            },
            detail: path.display().to_string(),
            remediation: "Run `hermes-ultra setup` (or create the directory manually).".to_string(),
        });
    }

    let provider = app.current_runtime_provider();
    let credential_present = crate::app::provider_api_key_from_env(&provider).is_some();
    let oauth_state_present = crate::auth::read_provider_auth_state(&provider)
        .ok()
        .flatten()
        .is_some();
    let oauth_capable = crate::providers::provider_capability_for(&provider)
        .map(|c| c.oauth_supported)
        .unwrap_or(false);
    let auth_ok = credential_present || (oauth_capable && oauth_state_present);
    checks.push(ReadinessCheck {
        name: format!("Auth ({provider})"),
        state: if auth_ok {
            ReadinessState::Pass
        } else {
            ReadinessState::Fail
        },
        detail: format!(
            "credential_present={} oauth_state_present={} oauth_capable={}",
            auth_ok || credential_present,
            oauth_state_present,
            oauth_capable
        ),
        remediation: "Run `/auth status` then `/auth verify` (or `hermes-ultra auth add`)."
            .to_string(),
    });

    if let Some((ok, detail)) = oauth_runtime_gate_for_provider(&provider) {
        checks.push(ReadinessCheck {
            name: format!("OAuth runtime gate ({provider})"),
            state: if ok {
                ReadinessState::Pass
            } else {
                ReadinessState::Fail
            },
            detail,
            remediation: "Upgrade runtime, then retry OAuth flows (`cargo install --path crates/hermes-cli --force`).".to_string(),
        });
    }

    if !quick {
        let tools = app.tool_registry.list_tools();
        checks.push(ReadinessCheck {
            name: "Tool registry".to_string(),
            state: if tools.is_empty() {
                ReadinessState::Warn
            } else {
                ReadinessState::Pass
            },
            detail: format!("registered_tools={}", tools.len()),
            remediation: "If this is unexpectedly zero, run `/reload` and verify `/tools`."
                .to_string(),
        });

        let cl_url = std::env::var("CONTEXTLATTICE_ORCHESTRATOR_URL")
            .ok()
            .or_else(|| std::env::var("MEMMCP_ORCHESTRATOR_URL").ok())
            .unwrap_or_else(|| "http://127.0.0.1:8075".to_string());
        let memory_state = match reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(2))
            .build()
        {
            Ok(client) => {
                let health_url = format!("{}/health", cl_url.trim_end_matches('/'));
                match client.get(&health_url).send().await {
                    Ok(resp) if resp.status().is_success() => (ReadinessState::Pass, health_url),
                    Ok(resp) => (
                        ReadinessState::Warn,
                        format!("{} status={}", health_url, resp.status()),
                    ),
                    Err(err) => (
                        ReadinessState::Warn,
                        format!(
                            "{} error={}",
                            health_url,
                            truncate_chars(&err.to_string(), 120)
                        ),
                    ),
                }
            }
            Err(err) => (
                ReadinessState::Warn,
                format!(
                    "client build failed: {}",
                    truncate_chars(&err.to_string(), 120)
                ),
            ),
        };
        checks.push(ReadinessCheck {
            name: "ContextLattice probe".to_string(),
            state: memory_state.0,
            detail: memory_state.1,
            remediation:
                "Start local ContextLattice orchestrator or set CONTEXTLATTICE_ORCHESTRATOR_URL."
                    .to_string(),
        });
    }

    checks
}

fn render_boot_readiness_report(checks: &[ReadinessCheck], quick: bool) -> String {
    let profile = boot_profile_env();
    let mut pass = Vec::new();
    let mut warn = Vec::new();
    let mut fail = Vec::new();
    for check in checks {
        match check.state {
            ReadinessState::Pass => pass.push(check),
            ReadinessState::Warn => warn.push(check),
            ReadinessState::Fail => fail.push(check),
        }
    }

    let mut out = String::new();
    let _ = writeln!(
        out,
        "Boot readiness gate ({})",
        if quick { "quick" } else { "full" }
    );
    out.push_str("==========================\n");
    let _ = writeln!(
        out,
        "summary: pass={} warn={} fail={}",
        pass.len(),
        warn.len(),
        fail.len()
    );
    let _ = writeln!(out, "profile: {}", profile.as_str());
    let overall = boot_profile_overall(profile, fail.len(), warn.len());
    let _ = writeln!(out, "overall: {}\n", overall);
    if profile == BootProfile::Prod && (!warn.is_empty() || !fail.is_empty()) {
        out.push_str("prod_policy: warnings are treated as launch blockers.\n\n");
    } else if profile == BootProfile::Dev && !warn.is_empty() && fail.is_empty() {
        out.push_str("dev_policy: warnings surfaced but do not block overall PASS.\n\n");
    }

    for section in [("PASS", &pass), ("WARN", &warn), ("FAIL", &fail)] {
        if section.1.is_empty() {
            continue;
        }
        let _ = writeln!(out, "{}:", section.0);
        for check in section.1 {
            let _ = writeln!(
                out,
                "  - [{}] {} :: {}",
                readiness_state_label(check.state),
                check.name,
                check.detail
            );
            let _ = writeln!(out, "      remediation: {}", check.remediation);
        }
        out.push('\n');
    }

    out.push_str("Next actions:\n");
    out.push_str("- `/auth verify`\n");
    out.push_str("- `/model`\n");
    out.push_str("- `/integrations status`\n");
    out.push_str("- `/walkthrough start quick`\n");
    out
}

async fn handle_boot_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args
        .first()
        .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "profile" | "mode"))
    {
        let token = args
            .get(1)
            .copied()
            .unwrap_or("status")
            .to_ascii_lowercase();
        match token.as_str() {
            "status" | "show" => emit_command_output(
                app,
                format!(
                    "Boot profile: {}\nUse `/boot profile list` or `/boot profile dev|standard|prod`.",
                    boot_profile_env().as_str()
                ),
            ),
            "list" => emit_command_output(
                app,
                "Boot profiles:\n- dev: warnings are advisory; only FAIL blocks overall\n- standard: current balanced pass/warn/fail behavior\n- prod: warnings and fails both block overall PASS",
            ),
            "clear" => {
                std::env::remove_var("HERMES_BOOT_PROFILE");
                emit_command_output(app, "Cleared boot profile override (default=standard).");
            }
            other => {
                let Some(profile) = BootProfile::parse(other) else {
                    emit_command_output(
                        app,
                        "Usage: /boot profile [status|list|dev|standard|prod|clear]",
                    );
                    return Ok(CommandResult::Handled);
                };
                std::env::set_var("HERMES_BOOT_PROFILE", profile.as_str());
                emit_command_output(app, format!("Boot profile set to {}.", profile.as_str()));
            }
        }
        return Ok(CommandResult::Handled);
    }

    let quick = args
        .first()
        .map(|v| matches!(v.to_ascii_lowercase().as_str(), "quick" | "--quick"))
        .unwrap_or(false);
    let checks = collect_boot_readiness_checks(app, quick).await;
    emit_command_output(app, render_boot_readiness_report(&checks, quick));
    Ok(CommandResult::Handled)
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct WalkthroughState {
    mode: String,
    current_step: usize,
    #[serde(default)]
    completed_steps: Vec<String>,
    #[serde(default)]
    updated_at: String,
}

#[derive(Debug, Clone, Copy)]
struct WalkthroughStep {
    id: &'static str,
    title: &'static str,
    command: &'static str,
    success_signal: &'static str,
}

const WALKTHROUGH_STEPS_QUICK: &[WalkthroughStep] = &[
    WalkthroughStep {
        id: "boot-gate",
        title: "Run boot readiness gate",
        command: "/boot quick",
        success_signal: "summary has fail=0",
    },
    WalkthroughStep {
        id: "auth-verify",
        title: "Verify runtime authentication",
        command: "/auth verify",
        success_signal: "provider credential is present and validated",
    },
    WalkthroughStep {
        id: "model-select",
        title: "Select active model/provider pair",
        command: "/model",
        success_signal: "current model points to intended provider:model",
    },
    WalkthroughStep {
        id: "tools-check",
        title: "Confirm tools and integrations are healthy",
        command: "/integrations status",
        success_signal: "tool registry and key integrations report healthy/warn only",
    },
    WalkthroughStep {
        id: "memory-connect",
        title: "Confirm ContextLattice memory path",
        command: "/runbook show contextlattice-connect",
        success_signal: "connection runbook has been executed successfully",
    },
];

const WALKTHROUGH_STEPS_FULL: &[WalkthroughStep] = &[
    WalkthroughStep {
        id: "boot-full",
        title: "Run full boot readiness gate",
        command: "/boot",
        success_signal: "no FAIL checks remain",
    },
    WalkthroughStep {
        id: "commands-catalog",
        title: "Review command palette and key controls",
        command: "/commands",
        success_signal: "operator knows key flows for auth/model/tools/background",
    },
    WalkthroughStep {
        id: "auth-refresh",
        title: "Run forced auth refresh if needed",
        command: "/auth refresh",
        success_signal: "provider session is refreshed and valid",
    },
    WalkthroughStep {
        id: "objective-pin",
        title: "Set or verify objective profile",
        command: "/objective profile status",
        success_signal: "objective profile is intentional for this session",
    },
    WalkthroughStep {
        id: "policy-check",
        title: "Inspect policy and route health",
        command: "/ops status",
        success_signal: "policy profile, counters, and gates look sane",
    },
    WalkthroughStep {
        id: "integration-check",
        title: "Inspect integration panels",
        command: "/integrations all",
        success_signal: "critical integrations show PASS/WARN with remediation",
    },
];

fn walkthrough_state_path() -> PathBuf {
    hermes_config::hermes_home()
        .join("walkthrough")
        .join("state.json")
}

fn walkthrough_events_path() -> PathBuf {
    hermes_config::hermes_home()
        .join("walkthrough")
        .join("events.jsonl")
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WalkthroughEvent {
    at: String,
    session_id: String,
    action: String,
    mode: String,
    #[serde(default)]
    step_id: Option<String>,
    current_step: usize,
    completed_count: usize,
}

fn append_walkthrough_event(
    session_id: &str,
    action: &str,
    state: &WalkthroughState,
    step_id: Option<&str>,
) -> Result<(), AgentError> {
    let path = walkthrough_events_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("Failed to create {}: {}", parent.display(), e)))?;
    }
    let event = WalkthroughEvent {
        at: chrono::Utc::now().to_rfc3339(),
        session_id: session_id.to_string(),
        action: action.to_string(),
        mode: if state.mode.trim().is_empty() {
            "quick".to_string()
        } else {
            state.mode.clone()
        },
        step_id: step_id.map(|v| v.to_string()),
        current_step: state.current_step,
        completed_count: state.completed_steps.len(),
    };
    let mut writer = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .map_err(|e| AgentError::Io(format!("Failed to open {}: {}", path.display(), e)))?;
    writer
        .write_all(format!("{}\n", serde_json::to_string(&event).unwrap_or_default()).as_bytes())
        .map_err(|e| AgentError::Io(format!("Failed to append {}: {}", path.display(), e)))?;
    Ok(())
}

fn load_walkthrough_events(limit: usize) -> Vec<WalkthroughEvent> {
    let path = walkthrough_events_path();
    let Ok(raw) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut events = raw
        .lines()
        .filter_map(|line| serde_json::from_str::<WalkthroughEvent>(line).ok())
        .collect::<Vec<_>>();
    if events.len() > limit {
        let trim = events.len() - limit;
        events.drain(0..trim);
    }
    events
}

fn walkthrough_steps_for_mode(mode: &str) -> &'static [WalkthroughStep] {
    if mode.eq_ignore_ascii_case("full") {
        WALKTHROUGH_STEPS_FULL
    } else {
        WALKTHROUGH_STEPS_QUICK
    }
}

fn load_walkthrough_state() -> WalkthroughState {
    let path = walkthrough_state_path();
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    serde_json::from_str::<WalkthroughState>(&raw).unwrap_or_default()
}

fn save_walkthrough_state(state: &WalkthroughState) -> Result<(), AgentError> {
    let path = walkthrough_state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("Failed to create {}: {}", parent.display(), e)))?;
    }
    let payload = serde_json::to_string_pretty(state)
        .map_err(|e| AgentError::Io(format!("Failed to encode walkthrough state: {}", e)))?;
    std::fs::write(&path, payload)
        .map_err(|e| AgentError::Io(format!("Failed to write {}: {}", path.display(), e)))?;
    Ok(())
}

fn render_walkthrough_status(state: &WalkthroughState) -> String {
    let mode = if state.mode.trim().is_empty() {
        "quick"
    } else {
        state.mode.as_str()
    };
    let steps = walkthrough_steps_for_mode(mode);
    let mut out = String::new();
    let _ = writeln!(out, "Walkthrough ({})", mode);
    out.push_str("-------------------\n");
    if steps.is_empty() {
        out.push_str("No steps registered.\n");
        return out;
    }
    for (idx, step) in steps.iter().enumerate() {
        let done = state
            .completed_steps
            .iter()
            .any(|id| id.eq_ignore_ascii_case(step.id));
        let marker = if done {
            "✓"
        } else if idx == state.current_step {
            "→"
        } else {
            " "
        };
        let _ = writeln!(out, "{} {:<18} {}", marker, step.id, step.title);
        let _ = writeln!(out, "    cmd: {}", step.command);
        let _ = writeln!(out, "    done_when: {}", step.success_signal);
    }
    out.push_str("\nUsage: /walkthrough start [quick|full] | /walkthrough next | /walkthrough done <step-id> | /walkthrough reset | /walkthrough insights");
    out
}

fn render_walkthrough_insights(state: &WalkthroughState) -> String {
    let events = load_walkthrough_events(1200);
    let mut starts_by_mode: HashMap<String, usize> = HashMap::new();
    let mut completions_by_step: HashMap<String, usize> = HashMap::new();
    let mut last_event_at: Option<String> = None;
    for event in &events {
        last_event_at = Some(event.at.clone());
        if event.action == "start" {
            *starts_by_mode.entry(event.mode.clone()).or_insert(0) += 1;
        }
        if event.action == "done" {
            if let Some(step) = &event.step_id {
                *completions_by_step.entry(step.clone()).or_insert(0) += 1;
            }
        }
    }
    let mode = if state.mode.trim().is_empty() {
        "quick"
    } else {
        state.mode.as_str()
    };
    let steps = walkthrough_steps_for_mode(mode);
    let next_step = steps.iter().find(|step| {
        !state
            .completed_steps
            .iter()
            .any(|id| id.eq_ignore_ascii_case(step.id))
    });
    let mut out = String::new();
    out.push_str("Walkthrough insights\n");
    out.push_str("--------------------\n");
    let _ = writeln!(out, "events: {}", events.len());
    let _ = writeln!(out, "active_mode: {}", mode);
    if starts_by_mode.is_empty() {
        out.push_str("starts: none\n");
    } else {
        let mut modes = starts_by_mode.into_iter().collect::<Vec<_>>();
        modes.sort_by_key(|(_, count)| std::cmp::Reverse(*count));
        out.push_str("starts:\n");
        for (name, count) in modes {
            let _ = writeln!(out, "- {} => {}", name, count);
        }
    }
    if completions_by_step.is_empty() {
        out.push_str("dropoff: no completed steps yet\n");
    } else {
        out.push_str("step_completions:\n");
        for step in steps {
            let count = completions_by_step.get(step.id).copied().unwrap_or(0);
            let _ = writeln!(out, "- {} => {}", step.id, count);
        }
    }
    let _ = writeln!(
        out,
        "resume_hint: {}",
        next_step
            .map(|step| format!("Run {} ({})", step.command, step.id))
            .unwrap_or_else(
                || "Walkthrough complete. Start full mode for deeper checks.".to_string()
            )
    );
    if let Some(ts) = last_event_at {
        let _ = writeln!(out, "last_event_at: {}", ts);
    }
    out
}

fn handle_walkthrough_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    match action.as_str() {
        "status" | "show" | "list" => {
            let mut state = load_walkthrough_state();
            if state.mode.trim().is_empty() {
                state.mode = "quick".to_string();
            }
            let _ = append_walkthrough_event(&app.session_id, "status", &state, None);
            emit_command_output(app, render_walkthrough_status(&state));
        }
        "start" => {
            let mode = args.get(1).copied().unwrap_or("quick").to_ascii_lowercase();
            let selected = if mode == "full" { "full" } else { "quick" };
            let state = WalkthroughState {
                mode: selected.to_string(),
                current_step: 0,
                completed_steps: Vec::new(),
                updated_at: chrono::Utc::now().to_rfc3339(),
            };
            save_walkthrough_state(&state)?;
            let _ = append_walkthrough_event(&app.session_id, "start", &state, None);
            let steps = walkthrough_steps_for_mode(selected);
            let first = steps.first().copied();
            emit_command_output(
                app,
                format!(
                    "Started {} walkthrough ({} steps).{}\nUse `/walkthrough done <step-id>` after each step.",
                    selected,
                    steps.len(),
                    first
                        .map(|step| format!("\nNext: {} -> {}", step.id, step.command))
                        .unwrap_or_default()
                ),
            );
        }
        "next" => {
            let mut state = load_walkthrough_state();
            if state.mode.trim().is_empty() {
                state.mode = "quick".to_string();
            }
            let _ = append_walkthrough_event(&app.session_id, "next", &state, None);
            let steps = walkthrough_steps_for_mode(&state.mode);
            let next = steps.iter().find(|step| {
                !state
                    .completed_steps
                    .iter()
                    .any(|id| id.eq_ignore_ascii_case(step.id))
            });
            if let Some(step) = next {
                emit_command_output(
                    app,
                    format!(
                        "Next walkthrough step: {}\n{}\nRun: {}",
                        step.id, step.title, step.command
                    ),
                );
            } else {
                emit_command_output(
                    app,
                    "Walkthrough complete. Run `/walkthrough start full` for expanded checks.",
                );
            }
        }
        "done" => {
            let Some(step_id) = args.get(1).copied() else {
                emit_command_output(app, "Usage: /walkthrough done <step-id>");
                return Ok(CommandResult::Handled);
            };
            let mut state = load_walkthrough_state();
            if state.mode.trim().is_empty() {
                state.mode = "quick".to_string();
            }
            let steps = walkthrough_steps_for_mode(&state.mode);
            let exists = steps
                .iter()
                .any(|step| step.id.eq_ignore_ascii_case(step_id));
            if !exists {
                emit_command_output(
                    app,
                    format!("Unknown step '{}'. Use `/walkthrough status`.", step_id),
                );
                return Ok(CommandResult::Handled);
            }
            if !state
                .completed_steps
                .iter()
                .any(|id| id.eq_ignore_ascii_case(step_id))
            {
                state.completed_steps.push(step_id.to_string());
            }
            state.current_step = steps
                .iter()
                .position(|step| {
                    !state
                        .completed_steps
                        .iter()
                        .any(|id| id.eq_ignore_ascii_case(step.id))
                })
                .unwrap_or(steps.len());
            state.updated_at = chrono::Utc::now().to_rfc3339();
            save_walkthrough_state(&state)?;
            let _ = append_walkthrough_event(&app.session_id, "done", &state, Some(step_id));
            emit_command_output(app, render_walkthrough_status(&state));
        }
        "reset" | "clear" => {
            let state = load_walkthrough_state();
            let path = walkthrough_state_path();
            if path.exists() {
                std::fs::remove_file(&path).map_err(|e| {
                    AgentError::Io(format!("Failed to remove {}: {}", path.display(), e))
                })?;
            }
            let _ = append_walkthrough_event(&app.session_id, "reset", &state, None);
            emit_command_output(
                app,
                "Walkthrough state reset. Run `/walkthrough start quick` to reinitialize.",
            );
        }
        "insights" => {
            let mut state = load_walkthrough_state();
            if state.mode.trim().is_empty() {
                state.mode = "quick".to_string();
            }
            let _ = append_walkthrough_event(&app.session_id, "insights", &state, None);
            emit_command_output(app, render_walkthrough_insights(&state));
        }
        _ => emit_command_output(
            app,
            "Usage: /walkthrough [status|start [quick|full]|next|done <step-id>|reset|insights]",
        ),
    }
    Ok(CommandResult::Handled)
}

fn print_help(app: &mut App) {
    emit_command_output(app, render_command_catalog(None));
}
