fn handle_interactive_question_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        emit_command_output(
            app,
            "Interactive question picker:\n\
             Usage: `/ask <question> | <option 1> | <option 2> [| <option 3> ...]`\n\
             Example: `/ask Proceed with deploy? | yes (recommended)::deploy now | no::pause and inspect logs`\n\
             In TUI mode this opens a native selection UI.\n\
             In non-TUI mode, provide your answer inline as normal text.",
        );
        return Ok(CommandResult::Handled);
    }

    let raw = args.join(" ");
    let segments: Vec<String> = raw
        .split('|')
        .map(|part| part.trim().to_string())
        .filter(|part| !part.is_empty())
        .collect();
    if segments.len() < 2 {
        emit_command_output(
            app,
            "Interactive picker is available in TUI mode. For non-TUI usage provide options as `question | option1 | option2`.",
        );
        return Ok(CommandResult::Handled);
    }

    let question = segments[0].clone();
    let options = &segments[1..];
    let recommended = options
        .iter()
        .position(|opt| opt.to_ascii_lowercase().contains("recommended"))
        .unwrap_or(0);
    let selected = options
        .get(recommended)
        .map(|v| v.as_str())
        .unwrap_or("(none)");

    let mut out = String::new();
    let _ = writeln!(out, "Interactive question (non-TUI fallback)");
    let _ = writeln!(out, "Q: {}", question);
    let _ = writeln!(out, "Options:");
    for (idx, option) in options.iter().enumerate() {
        let marker = if idx == recommended {
            " (recommended)"
        } else {
            ""
        };
        let _ = writeln!(out, "  {}. {}{}", idx + 1, option, marker);
    }
    let _ = writeln!(out, "\nSelected: {}", selected);
    let _ = writeln!(
        out,
        "Tip: In TUI mode, `/ask ...` opens a selectable picker."
    );
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

const SESSION_STEER_PREFIX: &str = "[SESSION_STEER] ";
const HOME_SESSION_MARKER_FILE: &str = "home-session.json";
const SUBGOAL_DIR: &str = "subgoals";
const HANDOFF_REQUESTS_DIR: &str = "handoff_requests";

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SubgoalItem {
    text: String,
    status: String,
    created_at: String,
    updated_at: String,
    source: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SubgoalChecklist {
    session_id: String,
    objective: Option<String>,
    updated_at: String,
    items: Vec<SubgoalItem>,
}

impl SubgoalChecklist {
    fn for_session(session_id: &str, objective: Option<&str>) -> Self {
        let now = chrono::Utc::now().to_rfc3339();
        Self {
            session_id: session_id.to_string(),
            objective: objective
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned),
            updated_at: now,
            items: Vec::new(),
        }
    }
}

fn home_session_marker_path() -> PathBuf {
    hermes_config::hermes_home().join(HOME_SESSION_MARKER_FILE)
}

fn load_home_session_marker() -> Option<serde_json::Value> {
    let path = home_session_marker_path();
    let body = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&body).ok()
}

fn subgoal_checklist_path(session_id: &str) -> PathBuf {
    hermes_config::hermes_home()
        .join(SUBGOAL_DIR)
        .join(format!("{session_id}.json"))
}

fn load_subgoal_checklist(session_id: &str) -> Option<SubgoalChecklist> {
    let path = subgoal_checklist_path(session_id);
    let body = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&body).ok()
}

fn save_subgoal_checklist(checklist: &SubgoalChecklist) -> Result<PathBuf, AgentError> {
    let path = subgoal_checklist_path(&checklist.session_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("Failed to create {}: {}", parent.display(), e)))?;
    }
    let body = serde_json::to_string_pretty(checklist)
        .map_err(|e| AgentError::Config(format!("serialize subgoal checklist: {e}")))?;
    std::fs::write(&path, body)
        .map_err(|e| AgentError::Io(format!("Failed to write {}: {}", path.display(), e)))?;
    Ok(path)
}

fn render_subgoal_checklist(checklist: &SubgoalChecklist) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "Subgoal checklist");
    let _ = writeln!(out, "session: {}", checklist.session_id);
    if let Some(objective) = checklist.objective.as_deref() {
        let _ = writeln!(out, "objective: {}", truncate_chars(objective, 200));
    }
    if checklist.items.is_empty() {
        out.push_str("items: (none)\n");
    } else {
        for (idx, item) in checklist.items.iter().enumerate() {
            let marker = match item.status.as_str() {
                "completed" => "[x]",
                "impossible" => "[!]",
                _ => "[ ]",
            };
            let _ = writeln!(
                out,
                "{} {}. {} ({})",
                marker,
                idx + 1,
                item.text,
                item.status
            );
        }
    }
    out.push_str(
        "\nUsage: /subgoal <text> | /subgoal complete <n> | /subgoal impossible <n> | /subgoal undo <n> | /subgoal remove <n> | /subgoal clear",
    );
    out.trim_end().to_string()
}

fn set_session_steer(app: &mut App, steer: Option<String>) {
    app.messages.retain(|m| {
        if m.role != hermes_core::MessageRole::System {
            return true;
        }
        !m.content
            .as_deref()
            .unwrap_or_default()
            .starts_with(SESSION_STEER_PREFIX)
    });
    if let Some(steer_text) = steer
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
    {
        app.messages.push(hermes_core::Message::system(format!(
            "{SESSION_STEER_PREFIX}{steer_text}"
        )));
    }
}

fn current_session_steer(app: &App) -> Option<String> {
    app.messages
        .iter()
        .rev()
        .find(|m| m.role == hermes_core::MessageRole::System)
        .and_then(|m| m.content.as_deref())
        .and_then(|raw| raw.strip_prefix(SESSION_STEER_PREFIX))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn handle_snapshot_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let sessions_dir = hermes_config::hermes_home().join("sessions");
    if args.is_empty() || args[0].eq_ignore_ascii_case("list") {
        if !sessions_dir.exists() {
            emit_command_output(
                app,
                format!(
                    "No snapshots found in {}.\nUse `/snapshot save [name]` to create one.",
                    sessions_dir.display()
                ),
            );
            return Ok(CommandResult::Handled);
        }
        let entries = enumerate_saved_sessions(&sessions_dir);
        if entries.is_empty() {
            emit_command_output(
                app,
                format!(
                    "No snapshots found in {}.\nUse `/snapshot save [name]` to create one.",
                    sessions_dir.display()
                ),
            );
            return Ok(CommandResult::Handled);
        }
        let mut out = String::new();
        let _ = writeln!(out, "Session snapshots:");
        for (idx, (name, path, _)) in entries.iter().take(20).enumerate() {
            let marker = if idx == 0 { " (latest)" } else { "" };
            let _ = writeln!(out, "  - {}{}  -> {}", name, marker, path.display());
        }
        let _ = writeln!(
            out,
            "\nUse `/snapshot save [name]` to create, `/rollback latest` to restore latest, or `/load <snapshot-name>` to load a specific snapshot."
        );
        emit_command_output(app, out.trim_end());
        return Ok(CommandResult::Handled);
    }

    let save_name = if args[0].eq_ignore_ascii_case("save") {
        args.get(1).copied()
    } else {
        args.first().copied()
    };
    let path = app.persist_session_snapshot(save_name)?;
    emit_command_output(app, format!("Snapshot saved: {}", path.display()));
    Ok(CommandResult::Handled)
}

fn handle_rollback_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() || args[0].eq_ignore_ascii_case("list") {
        let sessions_dir = hermes_config::hermes_home().join("sessions");
        let entries = enumerate_saved_sessions(&sessions_dir);
        let mut out = String::from("Rollback controls:\n");
        out.push_str("- `/rollback undo [n]`      revert the last exchange(s)\n");
        out.push_str("- `/rollback latest`        load latest snapshot\n");
        out.push_str("- `/rollback load <name>`   load named snapshot\n");
        if entries.is_empty() {
            out.push_str("- snapshots: none yet (`/snapshot save` to create one)\n");
        } else {
            out.push_str("- recent snapshots:\n");
            for (name, _, _) in entries.into_iter().take(5) {
                out.push_str(&format!("    - {}\n", name));
            }
        }
        emit_command_output(app, out.trim_end());
        return Ok(CommandResult::Handled);
    }

    let sub = args[0];
    if sub.eq_ignore_ascii_case("undo") || sub.parse::<usize>().is_ok() {
        let steps = if sub.eq_ignore_ascii_case("undo") {
            args.get(1)
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(1)
        } else {
            sub.parse::<usize>().unwrap_or(1)
        };
        let bounded = steps.clamp(1, 64);
        let _ = app.undo_last_n(bounded);
        emit_command_output(
            app,
            format!("Rolled back {} exchange(s) via undo.", bounded),
        );
        return Ok(CommandResult::Handled);
    }

    if sub.eq_ignore_ascii_case("latest") {
        let sessions_dir = hermes_config::hermes_home().join("sessions");
        let entries = enumerate_saved_sessions(&sessions_dir);
        let Some((name, path, _)) = entries.first() else {
            emit_command_output(app, "No snapshots available to rollback.");
            return Ok(CommandResult::Handled);
        };
        return load_session_from_path(app, name, path, false);
    }

    if sub.eq_ignore_ascii_case("load") {
        let Some(name) = args.get(1).copied() else {
            emit_command_output(app, "Usage: /rollback load <snapshot-name>");
            return Ok(CommandResult::Handled);
        };
        return handle_load_command(app, &[name]);
    }

    handle_load_command(app, &[sub])
}

fn handle_timetravel_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        return handle_snapshot_command(app, &["list"]);
    }
    match args[0].to_ascii_lowercase().as_str() {
        "help" => {
            emit_command_output(
                app,
                "Usage: /timetravel [list|latest|goto <snapshot>|undo [n]|branch [label]]\n\
                 - list: show snapshot checkpoints\n\
                 - latest: jump to latest snapshot\n\
                 - goto <snapshot>: jump to named snapshot\n\
                 - undo [n]: undo latest exchange(s)\n\
                 - branch [label]: create a branch checkpoint marker",
            );
            Ok(CommandResult::Handled)
        }
        "list" | "ls" | "show" => handle_snapshot_command(app, &["list"]),
        "latest" => handle_rollback_command(app, &["latest"]),
        "goto" | "jump" => {
            let Some(name) = args.get(1).copied() else {
                emit_command_output(app, "Usage: /timetravel goto <snapshot-name>");
                return Ok(CommandResult::Handled);
            };
            handle_load_command(app, &[name])
        }
        "undo" => handle_rollback_command(app, args),
        "branch" | "fork" => {
            let label = args.get(1).copied().unwrap_or("timetravel");
            handle_branch_command(app, &[label])
        }
        other => {
            if other.parse::<usize>().is_ok() {
                handle_rollback_command(app, args)
            } else {
                emit_command_output(
                    app,
                    format!(
                        "Unknown /timetravel action '{}'. Use `/timetravel help`.",
                        other
                    ),
                );
                Ok(CommandResult::Handled)
            }
        }
    }
}

fn handle_queue_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        emit_command_output(
            app,
            "Usage: /queue <prompt>\nUse `/queue status` to inspect queued/running background jobs.",
        );
        return Ok(CommandResult::Handled);
    }

    if args[0].eq_ignore_ascii_case("status") || args[0].eq_ignore_ascii_case("list") {
        emit_command_output(app, render_background_status(12));
        return Ok(CommandResult::Handled);
    }

    handle_background_command(app, args)
}

fn handle_steer_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        let message = current_session_steer(app).map_or_else(
            || "No active steering instruction. Use `/steer <instruction>`.".to_string(),
            |v| format!("Active steering instruction:\n{}", v),
        );
        emit_command_output(app, message);
        return Ok(CommandResult::Handled);
    }

    if args[0].eq_ignore_ascii_case("clear") {
        set_session_steer(app, None);
        emit_command_output(app, "Cleared session steering instruction.");
        return Ok(CommandResult::Handled);
    }

    let steer = args.join(" ");
    set_session_steer(app, Some(steer.clone()));
    emit_command_output(
        app,
        format!(
            "Steering instruction set.\nThis is injected as system context on subsequent turns.\n\n{}",
            steer.trim()
        ),
    );
    Ok(CommandResult::Handled)
}

fn handle_btw_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        emit_command_output(
            app,
            "Usage: /btw <side-question>\nRuns an ephemeral side-question as a background task.",
        );
        return Ok(CommandResult::Handled);
    }
    let question = args.join(" ").trim().to_string();
    if question.is_empty() {
        emit_command_output(app, "Usage: /btw <side-question>");
        return Ok(CommandResult::Handled);
    }
    let task = format!(
        "Ephemeral side question (do not alter objective/contracts unless explicitly asked): {}",
        question
    );
    let job = queue_background_job(&task)?;
    emit_command_output(
        app,
        format!(
            "[/btw queued]\nQuestion: {}\nJob ID: {}\nStatus: {}\nLogs:   {}",
            question,
            job.id,
            job.status_path.display(),
            job.log_path.display()
        ),
    );
    Ok(CommandResult::Handled)
}

fn handle_handoff_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let mut configured: Vec<_> = app.config.platforms.keys().cloned().collect();
    configured.sort();
    if args.is_empty() {
        let configured_text = if configured.is_empty() {
            "(none configured)".to_string()
        } else {
            configured.join(", ")
        };
        emit_command_output(
            app,
            format!(
                "Usage: /handoff <platform>\nConfigured platforms: {}\nThis queues a handoff request under ~/.hermes-agent-ultra/handoff_requests for gateway pickup.",
                configured_text
            ),
        );
        return Ok(CommandResult::Handled);
    }

    let platform = args[0].trim().to_ascii_lowercase();
    let Some(platform_cfg) = app.config.platforms.get(&platform) else {
        emit_command_output(
            app,
            format!(
                "Unknown platform '{}'. Configured platforms: {}",
                platform,
                if configured.is_empty() {
                    "(none configured)".to_string()
                } else {
                    configured.join(", ")
                }
            ),
        );
        return Ok(CommandResult::Handled);
    };
    if !platform_cfg.enabled {
        emit_command_output(
            app,
            format!(
                "Platform '{}' is configured but disabled. Enable it in config.yaml before handoff.",
                platform
            ),
        );
        return Ok(CommandResult::Handled);
    }

    let home_channel = platform_cfg
        .home_channel
        .clone()
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {
            load_home_session_marker().and_then(|value| {
                value
                    .get("home")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string())
                    .filter(|v| !v.trim().is_empty())
            })
        });
    let Some(home_channel) = home_channel else {
        emit_command_output(
            app,
            format!(
                "No home channel marker for '{}'. Run `/sethome <channel-or-thread>` first, then retry `/handoff {}`.",
                platform, platform
            ),
        );
        return Ok(CommandResult::Handled);
    };

    let dir = hermes_config::hermes_home().join(HANDOFF_REQUESTS_DIR);
    std::fs::create_dir_all(&dir)
        .map_err(|e| AgentError::Io(format!("Failed to create {}: {}", dir.display(), e)))?;
    let request_path = dir.join(format!("{}-{}.json", app.session_id, platform));
    let payload = serde_json::json!({
        "session_id": app.session_id,
        "platform": platform,
        "home_channel": home_channel,
        "requested_at": chrono::Utc::now().to_rfc3339(),
        "requested_by": "cli",
        "state": "pending",
    });
    std::fs::write(
        &request_path,
        serde_json::to_string_pretty(&payload)
            .map_err(|e| AgentError::Config(format!("serialize handoff request: {e}")))?,
    )
    .map_err(|e| AgentError::Io(format!("Failed to write {}: {}", request_path.display(), e)))?;

    emit_command_output(
        app,
        format!(
            "Queued handoff request.\n  session: {}\n  platform: {}\n  home_channel: {}\n  request_file: {}\n\nGateway workers can pick this up immediately when running.",
            app.session_id,
            payload["platform"].as_str().unwrap_or_default(),
            payload["home_channel"].as_str().unwrap_or_default(),
            request_path.display(),
        ),
    );
    Ok(CommandResult::Handled)
}

fn handle_subgoal_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let objective = app
        .session_objective
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let mut checklist = load_subgoal_checklist(&app.session_id)
        .unwrap_or_else(|| SubgoalChecklist::for_session(&app.session_id, objective));
    checklist.objective = objective.map(ToOwned::to_owned);

    if args.is_empty()
        || matches!(
            args[0].to_ascii_lowercase().as_str(),
            "show" | "status" | "list"
        )
    {
        checklist.updated_at = chrono::Utc::now().to_rfc3339();
        let _ = save_subgoal_checklist(&checklist)?;
        emit_command_output(app, render_subgoal_checklist(&checklist));
        return Ok(CommandResult::Handled);
    }

    let action = args[0].to_ascii_lowercase();
    if action == "clear" {
        checklist.items.clear();
        checklist.updated_at = chrono::Utc::now().to_rfc3339();
        let path = save_subgoal_checklist(&checklist)?;
        emit_command_output(
            app,
            format!(
                "Subgoal checklist cleared.\nPath: {}\nUse `/subgoal <text>` to add a new item.",
                path.display()
            ),
        );
        return Ok(CommandResult::Handled);
    }

    if matches!(
        action.as_str(),
        "complete" | "done" | "impossible" | "undo" | "remove"
    ) {
        let Some(raw_idx) = args.get(1) else {
            emit_command_output(app, format!("Usage: /subgoal {} <n>", action));
            return Ok(CommandResult::Handled);
        };
        let Ok(idx_one_based) = raw_idx.trim().parse::<usize>() else {
            emit_command_output(
                app,
                format!(
                    "/subgoal {}: <n> must be an integer (1-based index).",
                    action
                ),
            );
            return Ok(CommandResult::Handled);
        };
        if idx_one_based == 0 || idx_one_based > checklist.items.len() {
            emit_command_output(
                app,
                format!(
                    "/subgoal {}: index {} is out of range (1..={}).",
                    action,
                    idx_one_based,
                    checklist.items.len()
                ),
            );
            return Ok(CommandResult::Handled);
        }
        let idx = idx_one_based - 1;
        let now = chrono::Utc::now().to_rfc3339();

        if action == "remove" {
            let removed = checklist.items.remove(idx);
            checklist.updated_at = now;
            let _ = save_subgoal_checklist(&checklist)?;
            emit_command_output(
                app,
                format!(
                    "Removed subgoal {}: {}\n\n{}",
                    idx_one_based,
                    removed.text,
                    render_subgoal_checklist(&checklist)
                ),
            );
            return Ok(CommandResult::Handled);
        }

        checklist.items[idx].status = match action.as_str() {
            "complete" | "done" => "completed".to_string(),
            "impossible" => "impossible".to_string(),
            "undo" => "pending".to_string(),
            _ => checklist.items[idx].status.clone(),
        };
        checklist.items[idx].updated_at = now.clone();
        checklist.updated_at = now;
        let _ = save_subgoal_checklist(&checklist)?;
        emit_command_output(
            app,
            format!(
                "Updated subgoal {} -> {}\n\n{}",
                idx_one_based,
                checklist.items[idx].status,
                render_subgoal_checklist(&checklist)
            ),
        );
        return Ok(CommandResult::Handled);
    }

    let text = args.join(" ").trim().to_string();
    if text.is_empty() {
        emit_command_output(app, "Usage: /subgoal <text>");
        return Ok(CommandResult::Handled);
    }
    let now = chrono::Utc::now().to_rfc3339();
    checklist.items.push(SubgoalItem {
        text,
        status: "pending".to_string(),
        created_at: now.clone(),
        updated_at: now.clone(),
        source: "user".to_string(),
    });
    checklist.updated_at = now;
    let _ = save_subgoal_checklist(&checklist)?;
    emit_command_output(app, render_subgoal_checklist(&checklist));
    Ok(CommandResult::Handled)
}

fn handle_sethome_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let marker_path = home_session_marker_path();
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        if let Some(marker) = load_home_session_marker() {
            emit_command_output(
                app,
                format!(
                    "Home marker file: {}\n{}",
                    marker_path.display(),
                    serde_json::to_string_pretty(&marker).unwrap_or_else(|_| "{}".to_string())
                ),
            );
        } else {
            emit_command_output(
                app,
                format!(
                    "No home marker set. Use `/sethome <name>`.\nMarker path: {}",
                    marker_path.display()
                ),
            );
        }
        return Ok(CommandResult::Handled);
    }

    if args[0].eq_ignore_ascii_case("clear") {
        if marker_path.exists() {
            std::fs::remove_file(&marker_path).map_err(|e| {
                AgentError::Io(format!("Failed to remove {}: {}", marker_path.display(), e))
            })?;
            emit_command_output(app, "Cleared home marker.");
        } else {
            emit_command_output(app, "Home marker already clear.");
        }
        return Ok(CommandResult::Handled);
    }

    if let Some(parent) = marker_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("Failed to create {}: {}", parent.display(), e)))?;
    }
    let value = serde_json::json!({
        "session_id": app.session_id,
        "home": args.join(" ").trim(),
        "updated_at": chrono::Utc::now().to_rfc3339(),
    });
    std::fs::write(
        &marker_path,
        serde_json::to_string_pretty(&value)
            .map_err(|e| AgentError::Config(format!("serialize home marker: {}", e)))?,
    )
    .map_err(|e| AgentError::Io(format!("Failed to write {}: {}", marker_path.display(), e)))?;
    emit_command_output(
        app,
        format!(
            "Home marker updated.\nPath: {}\nHome: {}",
            marker_path.display(),
            args.join(" ").trim()
        ),
    );
    Ok(CommandResult::Handled)
}

fn branch_checkpoint_name(session_id: &str, label: Option<&str>) -> String {
    let requested = label.unwrap_or("branch").trim();
    let sanitized = requested
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    format!(
        "branch-{}-{}",
        &session_id[..8.min(session_id.len())],
        if sanitized.is_empty() {
            "checkpoint"
        } else {
            sanitized.as_str()
        }
    )
}

fn handle_branch_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let sessions_dir = hermes_config::hermes_home().join("sessions");
    let entries = enumerate_saved_sessions(&sessions_dir);
    let action = args
        .first()
        .map(|v| v.to_ascii_lowercase())
        .unwrap_or_else(|| "save".to_string());

    match action.as_str() {
        "help" => {
            emit_command_output(
                app,
                "Usage: /branch [label]\n\
                       /branch list\n\
                       /branch diff <left> [right]\n\
                       /branch merge <source> [target]\n\
                 Notes:\n\
                 - save: creates a branch checkpoint snapshot\n\
                 - diff: compares message footprints between snapshots\n\
                 - merge: appends unique messages from source into target/current session",
            );
            return Ok(CommandResult::Handled);
        }
        "list" | "ls" | "show" => {
            if entries.is_empty() {
                emit_command_output(app, "No snapshots found. Use `/branch <label>` first.");
                return Ok(CommandResult::Handled);
            }
            let mut out = String::from("Branch checkpoints:\n");
            let mut shown = 0usize;
            for (name, path, _) in entries.iter() {
                if !name.starts_with("branch-") {
                    continue;
                }
                let integrity = inspect_snapshot_integrity(path);
                let marker = if integrity.valid { "✓" } else { "⚠" };
                let detail = if integrity.valid {
                    format!("messages={}", integrity.message_count)
                } else {
                    integrity
                        .reason
                        .unwrap_or_else(|| "invalid snapshot".to_string())
                };
                let _ = writeln!(out, "  - {} `{}` ({})", marker, name, detail);
                shown += 1;
                if shown >= 25 {
                    break;
                }
            }
            if shown == 0 {
                out.push_str("  (no branch-* checkpoints found)\n");
            }
            out.push_str(
                "\nUse `/branch diff <left> [right]` or `/branch merge <source> [target]`.",
            );
            emit_command_output(app, out.trim_end());
            return Ok(CommandResult::Handled);
        }
        "diff" => {
            let Some(left_name) = args.get(1).copied() else {
                emit_command_output(app, "Usage: /branch diff <left> [right]");
                return Ok(CommandResult::Handled);
            };
            let right_name = args.get(2).copied().unwrap_or("latest");
            let left_entry = match resolve_saved_session_entry(&entries, left_name) {
                Ok(entry) => entry,
                Err(err) if err.starts_with("not_found:") => {
                    emit_command_output(app, format!("Snapshot '{}' not found.", left_name));
                    return Ok(CommandResult::Handled);
                }
                Err(err) => {
                    emit_command_output(
                        app,
                        format!(
                            "Snapshot '{}' is ambiguous. Matches: {}",
                            left_name,
                            err.trim_start_matches("ambiguous: ")
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
            };
            let right_entry = if right_name.eq_ignore_ascii_case("latest") {
                match entries.first() {
                    Some(entry) => entry,
                    None => {
                        emit_command_output(app, "No snapshots found.");
                        return Ok(CommandResult::Handled);
                    }
                }
            } else {
                match resolve_saved_session_entry(&entries, right_name) {
                    Ok(entry) => entry,
                    Err(err) if err.starts_with("not_found:") => {
                        emit_command_output(app, format!("Snapshot '{}' not found.", right_name));
                        return Ok(CommandResult::Handled);
                    }
                    Err(err) => {
                        emit_command_output(
                            app,
                            format!(
                                "Snapshot '{}' is ambiguous. Matches: {}",
                                right_name,
                                err.trim_start_matches("ambiguous: ")
                            ),
                        );
                        return Ok(CommandResult::Handled);
                    }
                }
            };
            let left_messages = load_messages_from_snapshot(&left_entry.1)?;
            let right_messages = load_messages_from_snapshot(&right_entry.1)?;
            emit_command_output(
                app,
                summarize_branch_diff(
                    &left_entry.0,
                    &left_messages,
                    &right_entry.0,
                    &right_messages,
                ),
            );
            return Ok(CommandResult::Handled);
        }
        "merge" => {
            let Some(source_name) = args.get(1).copied() else {
                emit_command_output(app, "Usage: /branch merge <source> [target]");
                return Ok(CommandResult::Handled);
            };
            let source_entry = match resolve_saved_session_entry(&entries, source_name) {
                Ok(entry) => entry,
                Err(err) if err.starts_with("not_found:") => {
                    emit_command_output(app, format!("Snapshot '{}' not found.", source_name));
                    return Ok(CommandResult::Handled);
                }
                Err(err) => {
                    emit_command_output(
                        app,
                        format!(
                            "Snapshot '{}' is ambiguous. Matches: {}",
                            source_name,
                            err.trim_start_matches("ambiguous: ")
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
            };

            let mut target_label = "current".to_string();
            let mut merged_messages = app.messages.clone();
            if let Some(target_name) = args.get(2).copied() {
                let target_entry = match resolve_saved_session_entry(&entries, target_name) {
                    Ok(entry) => entry,
                    Err(err) if err.starts_with("not_found:") => {
                        emit_command_output(app, format!("Snapshot '{}' not found.", target_name));
                        return Ok(CommandResult::Handled);
                    }
                    Err(err) => {
                        emit_command_output(
                            app,
                            format!(
                                "Snapshot '{}' is ambiguous. Matches: {}",
                                target_name,
                                err.trim_start_matches("ambiguous: ")
                            ),
                        );
                        return Ok(CommandResult::Handled);
                    }
                };
                target_label = target_entry.0.clone();
                merged_messages = load_messages_from_snapshot(&target_entry.1)?;
            }

            let source_messages = load_messages_from_snapshot(&source_entry.1)?;
            let mut seen: HashSet<String> = merged_messages.iter().map(message_signature).collect();
            let mut appended = 0usize;
            for msg in source_messages {
                let sig = message_signature(&msg);
                if seen.insert(sig) {
                    merged_messages.push(msg);
                    appended += 1;
                }
            }
            let merged_total = merged_messages.len();
            app.messages = merged_messages;
            app.ui_messages
                .retain(|msg| msg.insert_at <= app.messages.len());
            let stem = branch_checkpoint_name(
                &app.session_id,
                Some(&format!("merge-{}-into-{}", source_entry.0, target_label)),
            );
            let path = app.persist_session_snapshot(Some(&stem))?;
            emit_command_output(
                app,
                format!(
                    "Branch merge complete.\n  source: {}\n  target: {}\n  appended_unique_messages: {}\n  merged_total_messages: {}\n  snapshot: {}",
                    source_entry.0,
                    target_label,
                    appended,
                    merged_total,
                    path.display()
                ),
            );
            return Ok(CommandResult::Handled);
        }
        _ => {}
    }

    let label = if args.is_empty() {
        None
    } else {
        Some(args.join(" "))
    };
    let stem = branch_checkpoint_name(&app.session_id, label.as_deref());
    match app.persist_session_snapshot(Some(&stem)) {
        Ok(path) => emit_command_output(
            app,
            format!(
                "Branch checkpoint saved: {}\nContinue in current session or run `/resume {}`.",
                path.display(),
                stem
            ),
        ),
        Err(err) => emit_command_output(
            app,
            format!("Branch marker requested, but snapshot failed: {}", err),
        ),
    }
    Ok(CommandResult::Handled)
}

include!("session_ops.rs");
