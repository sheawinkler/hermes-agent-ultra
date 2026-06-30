fn background_job_counts() -> (usize, usize, usize, usize) {
    let jobs_dir = hermes_config::hermes_home().join("background_jobs");
    let mut queued = 0usize;
    let mut running = 0usize;
    let mut completed = 0usize;
    let mut failed = 0usize;
    let Ok(entries) = std::fs::read_dir(jobs_dir) else {
        return (queued, running, completed, failed);
    };
    for entry in entries.filter_map(Result::ok) {
        if entry.path().extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let status = read_json_map(&entry.path())
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_ascii_lowercase();
        match status.as_str() {
            "queued" => queued = queued.saturating_add(1),
            "running" => running = running.saturating_add(1),
            "completed" => completed = completed.saturating_add(1),
            "failed" => failed = failed.saturating_add(1),
            _ => {}
        }
    }
    (queued, running, completed, failed)
}

#[derive(Debug, Clone)]
struct BackgroundJobRecord {
    id: String,
    status: String,
    task: String,
    pid: Option<u32>,
    attempts: u64,
    created_at: String,
    started_at: String,
    finished_at: String,
    log_path: PathBuf,
    status_path: PathBuf,
}

fn collect_background_jobs(limit: usize) -> Vec<BackgroundJobRecord> {
    let jobs_dir = hermes_config::hermes_home().join("background_jobs");
    let Ok(entries) = std::fs::read_dir(jobs_dir) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let status_path = entry.path();
        if status_path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let map = read_json_map(&status_path);
        if map.is_empty() {
            continue;
        }
        let id = map
            .get("id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(ToString::to_string)
            .or_else(|| {
                status_path
                    .file_stem()
                    .and_then(|v| v.to_str())
                    .map(ToString::to_string)
            })
            .unwrap_or_else(|| "unknown".to_string());
        let status = map
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();
        let task = map
            .get("task")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let pid = map
            .get("pid")
            .and_then(|v| v.as_u64())
            .and_then(|raw| u32::try_from(raw).ok());
        let attempts = map.get("attempts").and_then(|v| v.as_u64()).unwrap_or(0);
        let created_at = map
            .get("created_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let started_at = map
            .get("started_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let finished_at = map
            .get("finished_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let log_path = map
            .get("log_path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| status_path.with_extension("log"));
        rows.push(BackgroundJobRecord {
            id,
            status,
            task,
            pid,
            attempts,
            created_at,
            started_at,
            finished_at,
            log_path,
            status_path,
        });
    }
    rows.sort_by(|a, b| b.id.cmp(&a.id));
    rows.truncate(limit.max(1));
    rows
}

fn resolve_background_job(id_or_prefix: &str) -> Option<BackgroundJobRecord> {
    let needle = id_or_prefix.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return None;
    }
    collect_background_jobs(500).into_iter().find(|job| {
        let id = job.id.to_ascii_lowercase();
        id == needle || id.starts_with(&needle)
    })
}

fn tail_text_lines(text: &str, limit: usize) -> String {
    let rows: Vec<&str> = text.lines().collect();
    let cap = limit.max(1);
    let start = rows.len().saturating_sub(cap);
    rows[start..].join("\n")
}

fn tail_file_lines(path: &Path, limit: usize) -> Result<String, AgentError> {
    let body = std::fs::read_to_string(path).map_err(|e| {
        AgentError::Io(format!(
            "Failed to read background log {}: {}",
            path.display(),
            e
        ))
    })?;
    Ok(tail_text_lines(&body, limit))
}

fn render_background_status(limit: usize) -> String {
    let (queued, running, completed, failed) = background_job_counts();
    let rows = collect_background_jobs(limit);
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Background queue status: queued={} running={} completed={} failed={}",
        queued, running, completed, failed
    );
    if rows.is_empty() {
        out.push_str("\nNo background jobs found.");
        return out;
    }
    out.push_str("\nRecent background jobs:\n");
    for (idx, row) in rows.iter().enumerate() {
        let pid_suffix = row.pid.map(|pid| format!(" pid={pid}")).unwrap_or_default();
        let _ = writeln!(
            out,
            "{}. {} [{}{}] attempts={} task={}",
            idx + 1,
            row.id,
            row.status,
            pid_suffix,
            row.attempts,
            truncate_chars(row.task.trim(), 84)
        );
    }
    out.push_str("\nUse `/background tail <job-id> [N]` to inspect logs.");
    out
}

async fn handle_mission_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    match action.as_str() {
        "init" => {
            let written = ensure_alpha_runtime_bootstrap(true)?;
            let trading_written = ensure_trading_runtime_bootstrap(true)?;
            let mut details = String::new();
            for path in written {
                let _ = writeln!(details, "- {}", path.display());
            }
            for path in trading_written {
                let _ = writeln!(details, "- {}", path.display());
            }
            emit_command_output(
                app,
                format!(
                    "Mission runtime initialized.\n{}\nUse `/mission status` to inspect active loops.",
                    details.trim_end()
                ),
            );
            Ok(CommandResult::Handled)
        }
        "recover" => {
            let recovered = recover_orphan_loop_events(600)?;
            emit_command_output(
                app,
                format!(
                    "Mission queue recovery complete. Marked {} orphaned running event(s).",
                    recovered
                ),
            );
            Ok(CommandResult::Handled)
        }
        "replay" => {
            let limit = args
                .get(1)
                .and_then(|v| v.parse::<usize>().ok())
                .unwrap_or(32);
            let replayed = replay_loop_queue(limit)?;
            emit_command_output(
                app,
                format!(
                    "Mission queue replay complete. Replayed {} event(s) (limit={}).",
                    replayed, limit
                ),
            );
            Ok(CommandResult::Handled)
        }
        "enqueue" => {
            if args.len() < 4 {
                emit_command_output(
                    app,
                    "Usage: /mission enqueue <loop-id> <event-type> <payload text>",
                );
                return Ok(CommandResult::Handled);
            }
            let loop_id = args[1];
            let event_type = args[2];
            let payload = args[3..].join(" ");
            let event = enqueue_loop_event(loop_id, event_type, &payload)?;
            emit_command_output(
                app,
                format!(
                    "Queued mission event {} loop={} type={} status={}",
                    event.id, event.loop_id, event.event_type, event.status
                ),
            );
            Ok(CommandResult::Handled)
        }
        "trading" => {
            let sub = args
                .get(1)
                .copied()
                .unwrap_or("status")
                .to_ascii_lowercase();
            match sub.as_str() {
                "status" | "show" => {
                    let report = load_last_trading_alpha_report()?;
                    emit_command_output(app, render_trading_alpha_board(&report));
                    Ok(CommandResult::Handled)
                }
                "refresh" | "run" | "scan" => {
                    let report = refresh_trading_alpha_report()?;
                    emit_command_output(app, render_trading_alpha_board(&report));
                    Ok(CommandResult::Handled)
                }
                "postmortem" => {
                    let report = load_last_trading_alpha_report()?;
                    emit_command_output(
                        app,
                        format!("Trading postmortem\n\n{}", report.postmortem),
                    );
                    Ok(CommandResult::Handled)
                }
                "autoresearch" => {
                    let report = load_last_trading_alpha_report()?;
                    let mut out = String::new();
                    out.push_str("Autoresearch artifacts\n");
                    out.push_str("----------------------\n");
                    out.push_str(&format!("hypotheses: {}\n", report.hypotheses.len()));
                    for h in report.hypotheses.iter().take(12) {
                        let _ = writeln!(
                            out,
                            "- {} novelty={:.3} expected_gain_sol={:.4} :: {}",
                            h.id, h.novelty_score, h.expected_gain_sol, h.statement
                        );
                    }
                    out.push_str("\nexperiments:\n");
                    for e in report.experiments.iter().take(12) {
                        let _ = writeln!(
                            out,
                            "- {} {} -> {} pass={}",
                            e.id, e.control, e.treatment, e.pass_criterion
                        );
                    }
                    out.push_str("\nbacktest_matrix:\n");
                    for row in report.backtest_matrix.iter().take(20) {
                        let _ = writeln!(out, "- {}", row);
                    }
                    out.push_str("\nwalkforward_checks:\n");
                    for row in report.walkforward_checks.iter().take(20) {
                        let _ = writeln!(out, "- {}", row);
                    }
                    out.push_str("\nmeta_ranking:\n");
                    for row in report.meta_ranking.iter().take(20) {
                        let _ = writeln!(out, "- {}", row);
                    }
                    emit_command_output(app, out.trim_end());
                    Ok(CommandResult::Handled)
                }
                "allocator" => {
                    let report = load_last_trading_alpha_report()?;
                    let mut out = String::new();
                    out.push_str("Capital Allocator\n");
                    out.push_str("-----------------\n");
                    for row in &report.capital_allocator {
                        let _ = writeln!(
                            out,
                            "- {} weight={:.4} capital_sol={:.6} max_loss_sol={:.6} throttle={:.3}",
                            row.project_id,
                            row.target_weight,
                            row.target_capital_sol,
                            row.max_loss_budget_sol,
                            row.throttle_factor
                        );
                    }
                    emit_command_output(app, out.trim_end());
                    Ok(CommandResult::Handled)
                }
                "governor" => {
                    let report = load_last_trading_alpha_report()?;
                    emit_command_output(
                        app,
                        format!(
                            "Portfolio Risk Governor\n\nmode={}\nhalt_new_entries={}\nmax_portfolio_drawdown_pct={:.4}\nmax_project_drawdown_pct={:.4}\nmax_ruin_probability={:.4}\nreason={}",
                            report.risk_governor.mode,
                            report.risk_governor.halt_new_entries,
                            report.risk_governor.max_portfolio_drawdown_pct,
                            report.risk_governor.max_project_drawdown_pct,
                            report.risk_governor.max_ruin_probability,
                            report.risk_governor.reason
                        ),
                    );
                    Ok(CommandResult::Handled)
                }
                "drift" => {
                    let report = load_last_trading_alpha_report()?;
                    let mut out = String::new();
                    out.push_str("Repo Drift Sentinel\n");
                    out.push_str("-------------------\n");
                    for row in &report.repo_drift {
                        let _ = writeln!(
                            out,
                            "- {} state={} head={} baseline={} dirty_files={} changed_since_baseline={}",
                            row.project_id,
                            row.drift_state,
                            row.git_head,
                            row.baseline_head,
                            row.dirty_files,
                            row.changed_since_baseline
                        );
                    }
                    emit_command_output(app, out.trim_end());
                    Ok(CommandResult::Handled)
                }
                "audit" => {
                    let report = load_last_trading_alpha_report()?;
                    let mut out = String::new();
                    out.push_str("Run Context Audits\n");
                    out.push_str("------------------\n");
                    for row in &report.run_context_audits {
                        let _ = writeln!(
                            out,
                            "- {} passed={} files_scanned={} missing={}",
                            row.project_id,
                            row.passed,
                            row.files_scanned,
                            if row.missing_metrics.is_empty() {
                                "none".to_string()
                            } else {
                                row.missing_metrics.join(",")
                            }
                        );
                    }
                    emit_command_output(app, out.trim_end());
                    Ok(CommandResult::Handled)
                }
                "provenance" => {
                    let report = load_last_trading_alpha_report()?;
                    let mut out = String::new();
                    out.push_str("Env Provenance Gates\n");
                    out.push_str("--------------------\n");
                    for row in &report.env_provenance {
                        let _ = writeln!(
                            out,
                            "- {} passed={} inspected_files={} conflicts={} decision={}",
                            row.project_id,
                            row.passed,
                            row.inspected_files.len(),
                            if row.conflicting_keys.is_empty() {
                                "none".to_string()
                            } else {
                                row.conflicting_keys.join(",")
                            },
                            row.decision
                        );
                    }
                    emit_command_output(app, out.trim_end());
                    Ok(CommandResult::Handled)
                }
                "replay" => {
                    let report = load_last_trading_alpha_report()?;
                    let mut out = String::new();
                    out.push_str("Replay Canary Harness\n");
                    out.push_str("---------------------\n");
                    for row in &report.replay_canary {
                        let _ = writeln!(
                            out,
                            "- {} sample_size={} pass_rate={:.3} decision={}",
                            row.project_id, row.sample_size, row.pass_rate, row.decision
                        );
                    }
                    emit_command_output(app, out.trim_end());
                    Ok(CommandResult::Handled)
                }
                "runbook" => {
                    let report = load_last_trading_alpha_report()?;
                    let mut out = String::new();
                    out.push_str("Automated Remediation Runbook (Dry Run)\n");
                    out.push_str("---------------------------------------\n");
                    for row in &report.remediation_runbook {
                        let _ = writeln!(
                            out,
                            "- [{}] {} :: {}\n  cmd: {}\n  why: {}",
                            row.priority, row.project_id, row.title, row.command, row.rationale
                        );
                    }
                    emit_command_output(app, out.trim_end());
                    Ok(CommandResult::Handled)
                }
                "sources" => {
                    let report = load_last_trading_alpha_report()?;
                    let mut out = String::new();
                    out.push_str("Research Source Ingestion\n");
                    out.push_str("-------------------------\n");
                    for row in &report.research_sources {
                        let _ = writeln!(
                            out,
                            "- {}:{} found={} items={} path={}",
                            row.project_id, row.source, row.found, row.items, row.path
                        );
                    }
                    emit_command_output(app, out.trim_end());
                    Ok(CommandResult::Handled)
                }
                _ => {
                    emit_command_output(
                        app,
                        "Usage: /mission trading [status|refresh|postmortem|autoresearch|allocator|governor|drift|audit|provenance|replay|runbook|sources]",
                    );
                    Ok(CommandResult::Handled)
                }
            }
        }
        "status" | "show" => {
            let loops = load_alpha_loops()?;
            let (queued, running, completed, failed) = background_job_counts();
            let board = render_mission_board(
                &app.current_model,
                app.session_objective.as_deref(),
                (queued, running, completed, failed),
            )
            .await?;
            let enabled = loops.iter().filter(|l| l.enabled).count();
            let trading = loops.iter().filter(|l| l.trading_sensitive).count();
            let public = loops.len().saturating_sub(trading);
            let mut out = String::new();
            out.push_str(&board);
            let _ = writeln!(
                out,
                "\nLoop inventory: total={} enabled={} trading_private={} public={}",
                loops.len(),
                enabled,
                trading,
                public
            );
            out.push_str("\nActions:\n");
            out.push_str("- /mission init\n");
            out.push_str("- /mission recover\n");
            out.push_str("- /mission replay [limit]\n");
            out.push_str("- /mission enqueue <loop-id> <event-type> <payload>\n");
            out.push_str("- /mission trading [status|refresh|postmortem|autoresearch|allocator|governor|drift|audit|provenance|replay|runbook|sources]\n");
            out.push_str("- /objective <text>\n");
            out.push_str("- /background <task>\n");
            emit_command_output(app, out.trim_end());
            Ok(CommandResult::Handled)
        }
        _ => {
            emit_command_output(
                app,
                "Usage: /mission [status|init|recover|replay [limit]|enqueue <loop-id> <event-type> <payload>|trading ...]",
            );
            Ok(CommandResult::Handled)
        }
    }
}

fn handle_save_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let path = app.persist_session_snapshot(args.first().copied())?;
    emit_command_output(app, format!("Session saved to {}", path.display()));
    Ok(CommandResult::Handled)
}

fn enumerate_saved_sessions(sessions_dir: &Path) -> Vec<(String, PathBuf, SystemTime)> {
    let mut entries: Vec<(String, PathBuf, SystemTime)> = std::fs::read_dir(sessions_dir)
        .ok()
        .into_iter()
        .flat_map(|rd| rd.filter_map(|e| e.ok()))
        .filter_map(|entry| {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
                return None;
            }
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default();
            if stem.trim().is_empty() {
                return None;
            }
            let modified = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            Some((stem, path, modified))
        })
        .collect();
    entries.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
    entries
}

#[derive(Debug, Clone)]
struct SnapshotIntegrity {
    valid: bool,
    reason: Option<String>,
    session_id: Option<String>,
    message_count: usize,
}

fn inspect_snapshot_integrity(path: &Path) -> SnapshotIntegrity {
    let raw = match std::fs::read_to_string(path) {
        Ok(body) => body,
        Err(err) => {
            return SnapshotIntegrity {
                valid: false,
                reason: Some(format!("read_failed: {}", err)),
                session_id: None,
                message_count: 0,
            };
        }
    };
    let data: serde_json::Value = match serde_json::from_str(&raw) {
        Ok(value) => value,
        Err(err) => {
            return SnapshotIntegrity {
                valid: false,
                reason: Some(format!("json_invalid: {}", err)),
                session_id: None,
                message_count: 0,
            };
        }
    };
    let messages = match data.get("messages").and_then(|m| m.as_array()) {
        Some(arr) => arr,
        None => {
            return SnapshotIntegrity {
                valid: false,
                reason: Some("missing_messages_array".to_string()),
                session_id: data
                    .get("session_info")
                    .and_then(|v| v.get("session_id"))
                    .and_then(|v| v.as_str())
                    .map(|v| v.to_string()),
                message_count: 0,
            };
        }
    };
    SnapshotIntegrity {
        valid: true,
        reason: None,
        session_id: data
            .get("session_info")
            .and_then(|v| v.get("session_id"))
            .and_then(|v| v.as_str())
            .map(|v| v.to_string()),
        message_count: messages.len(),
    }
}

fn is_canonical_snapshot_name(name: &str, integrity: &SnapshotIntegrity) -> bool {
    let stem = name.trim();
    let Some(session_id) = integrity
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return false;
    };
    !stem.is_empty() && stem.eq_ignore_ascii_case(session_id)
}

fn resolve_saved_session_entry<'a>(
    entries: &'a [(String, PathBuf, SystemTime)],
    requested: &str,
) -> Result<&'a (String, PathBuf, SystemTime), String> {
    if let Some(entry) = entries
        .iter()
        .find(|(name, _, _)| name.eq_ignore_ascii_case(requested))
    {
        return Ok(entry);
    }

    let prefix_matches: Vec<&(String, PathBuf, SystemTime)> = entries
        .iter()
        .filter(|(name, _, _)| name.starts_with(requested))
        .collect();
    match prefix_matches.as_slice() {
        [entry] => Ok(*entry),
        [] => Err(format!("not_found: {}", requested)),
        many => Err(format!(
            "ambiguous: {}",
            many.iter()
                .map(|entry| format!("`{}`", entry.0))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn message_from_snapshot_entry(entry: &serde_json::Value) -> hermes_core::Message {
    let role_str = entry.get("role").and_then(|r| r.as_str()).unwrap_or("User");
    let content_str = entry.get("content").and_then(|c| c.as_str()).unwrap_or("");
    match role_str {
        "Assistant" => hermes_core::Message::assistant(content_str),
        "System" => hermes_core::Message::system(content_str),
        "Tool" => hermes_core::Message::assistant(content_str),
        _ => hermes_core::Message::user(content_str),
    }
}

fn load_messages_from_snapshot(path: &Path) -> Result<Vec<hermes_core::Message>, AgentError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("Failed to read session: {}", e)))?;
    let data: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| AgentError::Config(format!("Failed to parse session: {}", e)))?;
    let messages = data
        .get("messages")
        .and_then(|m| m.as_array())
        .ok_or_else(|| AgentError::Config("Session file has no messages array.".to_string()))?;
    Ok(messages.iter().map(message_from_snapshot_entry).collect())
}

fn message_signature(message: &hermes_core::Message) -> String {
    let role = match message.role {
        hermes_core::MessageRole::System => "system",
        hermes_core::MessageRole::User => "user",
        hermes_core::MessageRole::Assistant => "assistant",
        hermes_core::MessageRole::Tool => "tool",
    };
    format!(
        "{}|{}",
        role,
        message.content.as_deref().unwrap_or_default()
    )
}

fn summarize_branch_diff(
    left_name: &str,
    left_messages: &[hermes_core::Message],
    right_name: &str,
    right_messages: &[hermes_core::Message],
) -> String {
    let left_set: HashSet<String> = left_messages.iter().map(message_signature).collect();
    let right_set: HashSet<String> = right_messages.iter().map(message_signature).collect();
    let only_left = left_set.difference(&right_set).count();
    let only_right = right_set.difference(&left_set).count();
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Branch diff: `{}` vs `{}`",
        left_name.trim(),
        right_name.trim()
    );
    let _ = writeln!(
        out,
        "  messages: {} vs {}",
        left_messages.len(),
        right_messages.len()
    );
    let _ = writeln!(out, "  unique_to_{}: {}", left_name.trim(), only_left);
    let _ = writeln!(out, "  unique_to_{}: {}", right_name.trim(), only_right);
    let left_last = left_messages
        .iter()
        .rev()
        .find(|m| m.role == hermes_core::MessageRole::Assistant)
        .and_then(|m| m.content.as_deref())
        .unwrap_or("");
    let right_last = right_messages
        .iter()
        .rev()
        .find(|m| m.role == hermes_core::MessageRole::Assistant)
        .and_then(|m| m.content.as_deref())
        .unwrap_or("");
    let _ = writeln!(
        out,
        "  last_assistant_{}: {}",
        left_name.trim(),
        truncate_chars(left_last, 120)
    );
    let _ = writeln!(
        out,
        "  last_assistant_{}: {}",
        right_name.trim(),
        truncate_chars(right_last, 120)
    );
    out.trim_end().to_string()
}

fn load_session_from_path(
    app: &mut App,
    session_name: &str,
    path: &Path,
    resume_mode: bool,
) -> Result<CommandResult, AgentError> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("Failed to read session: {}", e)))?;
    let data: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| AgentError::Config(format!("Failed to parse session: {}", e)))?;

    let Some(messages) = data.get("messages").and_then(|m| m.as_array()) else {
        emit_command_output(app, "Session file has no messages array.");
        return Ok(CommandResult::Handled);
    };

    app.messages.clear();
    app.ui_messages.clear();
    for msg in messages {
        app.messages.push(message_from_snapshot_entry(msg));
    }

    let session_info = data.get("session_info");
    if resume_mode {
        if let Some(restored_id) = session_info
            .and_then(|s| s.get("session_id"))
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            app.session_id = restored_id.to_string();
        }
    }

    let mut model_note = String::new();
    if let Some(restored_model) = session_info
        .and_then(|s| s.get("model"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        if !restored_model.eq_ignore_ascii_case(&app.current_model) {
            let previous = app.current_model.clone();
            app.switch_model(restored_model);
            model_note = format!("\nModel restored: {} -> {}", previous, app.current_model);
        }
    }

    if let Some(personality) = session_info
        .and_then(|s| s.get("personality"))
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        app.current_personality = Some(personality.to_string());
    }

    let verb = if resume_mode { "Resumed" } else { "Loaded" };
    emit_command_output(
        app,
        format!(
            "{} session '{}' ({} messages; session_id={}){}",
            verb,
            session_name,
            app.messages.len(),
            app.session_id,
            model_note
        ),
    );
    Ok(CommandResult::Handled)
}

fn handle_load_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let sessions_dir = hermes_config::hermes_home().join("sessions");

    if args.is_empty() {
        if !sessions_dir.exists() {
            emit_command_output(app, "No saved sessions found.");
            return Ok(CommandResult::Handled);
        }
        let entries = enumerate_saved_sessions(&sessions_dir);
        if entries.is_empty() {
            emit_command_output(app, "No saved sessions found.");
        } else {
            let mut out = String::from("Saved sessions:\n");
            for (idx, (name, _, _)) in entries.iter().enumerate() {
                let integrity = inspect_snapshot_integrity(&entries[idx].1);
                let marker = if integrity.valid { "✓" } else { "⚠" };
                let detail = if integrity.valid {
                    format!(
                        "session_id={} messages={}",
                        integrity.session_id.unwrap_or_else(|| "?".to_string()),
                        integrity.message_count
                    )
                } else {
                    integrity
                        .reason
                        .unwrap_or_else(|| "invalid snapshot".to_string())
                };
                if idx == 0 {
                    out.push_str(&format!("- {} `{}` (latest) — {}\n", marker, name, detail));
                } else {
                    out.push_str(&format!("- {} `{}` — {}\n", marker, name, detail));
                }
            }
            out.push_str("\nUsage: `/load <session-name>` or `/resume [session-name]`");
            emit_command_output(app, out.trim_end());
        }
        return Ok(CommandResult::Handled);
    }

    let name = args[0];
    let path = sessions_dir.join(format!("{}.json", name));
    if !path.exists() {
        emit_command_output(
            app,
            format!("Session '{}' not found at {}", name, path.display()),
        );
        return Ok(CommandResult::Handled);
    }
    load_session_from_path(app, name, &path, false)
}

fn handle_resume_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let sessions_dir = hermes_config::hermes_home().join("sessions");
    if !sessions_dir.exists() {
        emit_command_output(app, "No saved sessions found.");
        return Ok(CommandResult::Handled);
    }
    let entries = enumerate_saved_sessions(&sessions_dir);
    if entries.is_empty() {
        emit_command_output(app, "No saved sessions found.");
        return Ok(CommandResult::Handled);
    }

    if args.is_empty() {
        let pick = entries
            .iter()
            .find(|(name, path, _)| {
                let integrity = inspect_snapshot_integrity(path);
                integrity.valid
                    && integrity.message_count > 0
                    && is_canonical_snapshot_name(name, &integrity)
            })
            .or_else(|| {
                entries.iter().find(|(name, path, _)| {
                    let integrity = inspect_snapshot_integrity(path);
                    integrity.valid && is_canonical_snapshot_name(name, &integrity)
                })
            })
            .or_else(|| {
                entries
                    .iter()
                    .find(|(_, path, _)| inspect_snapshot_integrity(path).valid)
            });
        if let Some((name, path, _)) = pick {
            return load_session_from_path(app, name, path, true);
        }
        emit_command_output(
            app,
            "No valid saved sessions found (all snapshots are malformed). Use `/sessions` to inspect and `/save` to create a fresh checkpoint.",
        );
        return Ok(CommandResult::Handled);
    }

    let requested = args[0];
    match resolve_saved_session_entry(&entries, requested) {
        Ok((name, path, _)) => {
            let integrity = inspect_snapshot_integrity(path);
            if !integrity.valid {
                emit_command_output(
                    app,
                    format!(
                        "Session '{}' is present but invalid: {}.\nUse `/sessions` to inspect snapshot health.",
                        requested,
                        integrity
                            .reason
                            .unwrap_or_else(|| "malformed session snapshot".to_string())
                    ),
                );
                return Ok(CommandResult::Handled);
            }
            load_session_from_path(app, name, path, true)
        }
        Err(err) if err.starts_with("not_found:") => {
            emit_command_output(
                app,
                format!(
                    "Session '{}' not found. Use `/load` to list saved sessions.",
                    requested
                ),
            );
            Ok(CommandResult::Handled)
        }
        Err(err) => {
            emit_command_output(
                app,
                format!(
                    "Session name '{}' is ambiguous. Matches: {}",
                    requested,
                    err.trim_start_matches("ambiguous: ")
                ),
            );
            Ok(CommandResult::Handled)
        }
    }
}

fn handle_sessions_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        return handle_load_command(app, args);
    }
    let action = args[0].to_ascii_lowercase();
    if action == "doctor" || action == "verify" {
        let sessions_dir = hermes_config::hermes_home().join("sessions");
        let entries = enumerate_saved_sessions(&sessions_dir);
        if entries.is_empty() {
            emit_command_output(app, "No saved sessions found.");
            return Ok(CommandResult::Handled);
        }
        let mut invalid = Vec::new();
        let mut by_session_id: HashMap<String, Vec<String>> = HashMap::new();
        for (name, path, _) in entries {
            let integrity = inspect_snapshot_integrity(&path);
            if integrity.valid {
                if let Some(id) = integrity.session_id {
                    by_session_id.entry(id).or_default().push(name);
                }
            } else {
                invalid.push((
                    name,
                    integrity
                        .reason
                        .unwrap_or_else(|| "invalid snapshot".to_string()),
                ));
            }
        }
        let split = by_session_id
            .iter()
            .filter(|(_, names)| names.len() > 1)
            .map(|(session_id, names)| format!("{} => {}", session_id, names.join(", ")))
            .collect::<Vec<_>>();
        let mut out = String::new();
        out.push_str("Session snapshot doctor\n");
        out.push_str("-----------------------\n");
        let _ = writeln!(out, "invalid_snapshots={}", invalid.len());
        let _ = writeln!(out, "split_session_ids={}", split.len());
        if !invalid.is_empty() {
            out.push_str("invalid_details:\n");
            for (name, reason) in invalid.into_iter().take(20) {
                let _ = writeln!(out, "- {}: {}", name, reason);
            }
        }
        if !split.is_empty() {
            out.push_str("split_details:\n");
            for row in split.into_iter().take(20) {
                let _ = writeln!(out, "- {}", row);
            }
        }
        out.push_str("Recommendation: `/save` now to create a fresh canonical checkpoint.");
        emit_command_output(app, out.trim_end());
        return Ok(CommandResult::Handled);
    }
    handle_resume_command(app, args)
}

fn persist_browser_cdp_url(url: Option<&str>) -> Result<(), AgentError> {
    let env_path = hermes_config::hermes_home().join(".env");
    let mut lines: Vec<String> = std::fs::read_to_string(&env_path)
        .unwrap_or_default()
        .lines()
        .map(|line| line.to_string())
        .collect();
    let key = "CHROME_CDP_URL=";
    lines.retain(|line| !line.starts_with(key));
    if let Some(value) = url.map(str::trim).filter(|v| !v.is_empty()) {
        lines.push(format!("CHROME_CDP_URL={}", value));
    }
    if let Some(parent) = env_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("Failed to create {}: {}", parent.display(), e)))?;
    }
    let mut payload = lines.join("\n");
    if !payload.is_empty() {
        payload.push('\n');
    }
    std::fs::write(&env_path, payload)
        .map_err(|e| AgentError::Io(format!("Failed to write {}: {}", env_path.display(), e)))?;
    Ok(())
}

fn browser_http_probe_base(endpoint: &str) -> String {
    let trimmed = endpoint.trim();
    if let Some(rest) = trimmed.strip_prefix("ws://") {
        format!("http://{}", rest)
    } else if let Some(rest) = trimmed.strip_prefix("wss://") {
        format!("https://{}", rest)
    } else {
        trimmed.to_string()
    }
}

const DEFAULT_BROWSER_CDP_PORT: u16 = 9222;
const DEFAULT_BROWSER_CDP_URL: &str = "http://127.0.0.1:9222";
const DARWIN_BROWSER_APPS: &[&str] = &[
    "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome",
    "/Applications/Chromium.app/Contents/MacOS/Chromium",
    "/Applications/Brave Browser.app/Contents/MacOS/Brave Browser",
    "/Applications/Microsoft Edge.app/Contents/MacOS/Microsoft Edge",
];
const LINUX_BROWSER_GROUPS: &[(&[&str], &[&str])] = &[
    (
        &["google-chrome", "google-chrome-stable"],
        &[
            "/opt/google/chrome/chrome",
            "/usr/bin/google-chrome",
            "/usr/bin/google-chrome-stable",
        ],
    ),
    (
        &["chromium-browser", "chromium"],
        &["/usr/bin/chromium-browser", "/usr/bin/chromium"],
    ),
    (
        &["brave-browser", "brave-browser-stable", "brave"],
        &[
            "/usr/bin/brave-browser",
            "/usr/bin/brave-browser-stable",
            "/usr/bin/brave",
            "/snap/bin/brave",
            "/opt/brave.com/brave/brave-browser",
            "/opt/brave.com/brave/brave",
            "/opt/brave-bin/brave",
        ],
    ),
    (
        &["microsoft-edge", "microsoft-edge-stable", "msedge"],
        &[
            "/usr/bin/microsoft-edge",
            "/usr/bin/microsoft-edge-stable",
            "/opt/microsoft/msedge/microsoft-edge",
            "/opt/microsoft/msedge/msedge",
        ],
    ),
];
const WINDOWS_BROWSER_GROUPS: &[(&[&str], &[&[&str]])] = &[
    (
        &["chrome.exe", "chrome"],
        &[&["Google", "Chrome", "Application", "chrome.exe"]],
    ),
    (
        &["chromium.exe", "chromium"],
        &[
            &["Chromium", "Application", "chrome.exe"],
            &["Chromium", "Application", "chromium.exe"],
        ],
    ),
    (
        &["brave.exe", "brave"],
        &[&["BraveSoftware", "Brave-Browser", "Application", "brave.exe"]],
    ),
    (
        &["msedge.exe", "msedge"],
        &[&["Microsoft", "Edge", "Application", "msedge.exe"]],
    ),
];

fn chrome_debug_data_dir() -> PathBuf {
    hermes_config::hermes_home().join("chrome-debug")
}

fn chrome_debug_args(port: u16) -> Vec<String> {
    vec![
        format!("--remote-debugging-port={port}"),
        format!("--user-data-dir={}", chrome_debug_data_dir().display()),
        "--no-first-run".to_string(),
        "--no-default-browser-check".to_string(),
    ]
}

fn path_key(path: &str) -> String {
    path.replace('\\', "/").to_ascii_lowercase()
}

fn push_browser_candidate<F>(
    out: &mut Vec<String>,
    seen: &mut HashSet<String>,
    candidate: Option<String>,
    is_file: &F,
) where
    F: Fn(&str) -> bool,
{
    let Some(candidate) = candidate.filter(|v| !v.trim().is_empty()) else {
        return;
    };
    let key = path_key(&candidate);
    if seen.insert(key) && is_file(&candidate) {
        out.push(candidate);
    }
}

fn join_windows_path(base: &str, parts: &[&str]) -> String {
    let mut result = base.trim_end_matches(['\\', '/']).to_string();
    for part in parts {
        result.push('\\');
        result.push_str(part);
    }
    result
}

fn chrome_debug_candidates_with<E, W, F>(
    system: &str,
    env_get: E,
    which: W,
    is_file: F,
) -> Vec<String>
where
    E: Fn(&str) -> Option<String>,
    W: Fn(&str) -> Option<String>,
    F: Fn(&str) -> bool,
{
    let mut out = Vec::new();
    let mut seen = HashSet::new();

    if system == "Darwin" {
        for app in DARWIN_BROWSER_APPS {
            push_browser_candidate(&mut out, &mut seen, Some((*app).to_string()), &is_file);
        }
        return out;
    }

    if system == "Windows" {
        let install_bases = [
            env_get("ProgramFiles"),
            env_get("ProgramFiles(x86)"),
            env_get("LOCALAPPDATA"),
        ];
        for (names, install_groups) in WINDOWS_BROWSER_GROUPS {
            for name in *names {
                push_browser_candidate(&mut out, &mut seen, which(name), &is_file);
            }
            for base in install_bases.iter().flatten() {
                for parts in *install_groups {
                    push_browser_candidate(
                        &mut out,
                        &mut seen,
                        Some(join_windows_path(base, parts)),
                        &is_file,
                    );
                }
            }
        }
        return out;
    }

    for (names, install_paths) in LINUX_BROWSER_GROUPS {
        for name in *names {
            push_browser_candidate(&mut out, &mut seen, which(name), &is_file);
        }
        for install_path in *install_paths {
            push_browser_candidate(
                &mut out,
                &mut seen,
                Some((*install_path).to_string()),
                &is_file,
            );
        }
    }
    for base in ["/mnt/c/Program Files", "/mnt/c/Program Files (x86)"] {
        for (_, install_groups) in WINDOWS_BROWSER_GROUPS {
            for parts in *install_groups {
                push_browser_candidate(
                    &mut out,
                    &mut seen,
                    Some(join_windows_path(base, parts)),
                    &is_file,
                );
            }
        }
    }
    out
}

fn which_on_path(name: &str) -> Option<String> {
    let candidate = Path::new(name);
    if candidate.is_absolute() && candidate.is_file() {
        return Some(name.to_string());
    }
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let joined = dir.join(name);
        if joined.is_file() {
            return Some(joined.display().to_string());
        }
    }
    None
}

fn get_chrome_debug_candidates(system: &str) -> Vec<String> {
    chrome_debug_candidates_with(
        system,
        |key| std::env::var(key).ok(),
        which_on_path,
        |candidate| Path::new(candidate).is_file(),
    )
}

fn quote_posix_arg(arg: &str) -> String {
    if !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "@%_+=:,./-".contains(c))
    {
        arg.to_string()
    } else {
        format!("'{}'", arg.replace('\'', "'\\''"))
    }
}

fn quote_windows_arg(arg: &str) -> String {
    if !arg.is_empty() && !arg.chars().any(|c| c.is_whitespace() || c == '"') {
        arg.to_string()
    } else {
        format!("\"{}\"", arg.replace('"', "\\\""))
    }
}

fn manual_chrome_debug_command_with_candidates(
    port: u16,
    system: &str,
    candidates: &[String],
) -> Option<String> {
    if let Some(first) = candidates.first() {
        let mut argv = vec![first.clone()];
        argv.extend(chrome_debug_args(port));
        let rendered = if system == "Windows" {
            argv.iter()
                .map(|arg| quote_windows_arg(arg))
                .collect::<Vec<_>>()
                .join(" ")
        } else {
            argv.iter()
                .map(|arg| quote_posix_arg(arg))
                .collect::<Vec<_>>()
                .join(" ")
        };
        return Some(rendered);
    }

    if system == "Darwin" {
        return Some(format!(
            "open -a \"Google Chrome\" --args --remote-debugging-port={port} --user-data-dir=\"{}\" --no-first-run --no-default-browser-check",
            chrome_debug_data_dir().display()
        ));
    }
    None
}

fn manual_chrome_debug_command(port: u16, system: &str) -> Option<String> {
    let candidates = get_chrome_debug_candidates(system);
    manual_chrome_debug_command_with_candidates(port, system, &candidates)
}

fn try_launch_chrome_debug(port: u16, system: &str) -> bool {
    let candidates = get_chrome_debug_candidates(system);
    if candidates.is_empty() {
        return false;
    }
    let _ = std::fs::create_dir_all(chrome_debug_data_dir());
    for candidate in candidates {
        let mut command = Command::new(&candidate);
        command.args(chrome_debug_args(port));
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            command.creation_flags(hermes_core::subprocess::windows_no_window_creation_flags(
                0x00000008 | 0x00000200,
            ));
        }
        if command.spawn().is_ok() {
            return true;
        }
    }
    false
}

async fn browser_probe(endpoint: &str) -> Result<String, AgentError> {
    let base = browser_http_probe_base(endpoint)
        .trim_end_matches('/')
        .to_string();
    let url = format!("{}/json/version", base);
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(4))
        .build()
        .map_err(|e| AgentError::Io(format!("Failed to create browser probe client: {}", e)))?;
    let resp = client
        .get(&url)
        .send()
        .await
        .map_err(|e| AgentError::Io(format!("Browser probe failed at {}: {}", url, e)))?;
    let status = resp.status();
    let body = resp
        .text()
        .await
        .unwrap_or_else(|_| String::from("<unavailable>"));
    if !status.is_success() {
        return Err(AgentError::Io(format!(
            "Browser probe failed at {} with status {}",
            url, status
        )));
    }
    let payload: serde_json::Value = serde_json::from_str(&body)
        .map_err(|e| AgentError::Config(format!("Browser probe parse failed: {}", e)))?;
    let browser = payload
        .get("Browser")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let ws_url = payload
        .get("webSocketDebuggerUrl")
        .and_then(|v| v.as_str())
        .unwrap_or("<missing>");
    Ok(format!(
        "Connected CDP endpoint: {}\nBrowser: {}\nWebSocket target: {}",
        endpoint.trim(),
        browser,
        ws_url
    ))
}

async fn handle_browser_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    match action.as_str() {
        "status" | "show" => {
            let endpoint = std::env::var("CHROME_CDP_URL")
                .unwrap_or_else(|_| DEFAULT_BROWSER_CDP_URL.to_string());
            match browser_probe(&endpoint).await {
                Ok(summary) => emit_command_output(app, summary),
                Err(err) => emit_command_output(
                    app,
                    format!(
                        "Browser status (configured endpoint: {})\nProbe error: {}\nTip: `/browser connect [ws://host:port or http://host:port]` or `/browser launch`",
                        endpoint, err
                    ),
                ),
            }
            Ok(CommandResult::Handled)
        }
        "connect" => {
            let endpoint = args.get(1).copied().unwrap_or(DEFAULT_BROWSER_CDP_URL);
            std::env::set_var("CHROME_CDP_URL", endpoint);
            persist_browser_cdp_url(Some(endpoint))?;
            match browser_probe(endpoint).await {
                Ok(summary) => emit_command_output(
                    app,
                    format!(
                        "{}\n\nSaved CHROME_CDP_URL to {}/.env",
                        summary,
                        hermes_config::hermes_home().display()
                    ),
                ),
                Err(err) => emit_command_output(
                    app,
                    format!(
                        "Saved CHROME_CDP_URL={}, but probe failed: {}\nStart Chrome with --remote-debugging-port=9222 and retry `/browser status`.",
                        endpoint, err
                    ),
                ),
            }
            Ok(CommandResult::Handled)
        }
        "launch" | "start" => {
            let port = args
                .get(1)
                .and_then(|raw| raw.parse::<u16>().ok())
                .unwrap_or(DEFAULT_BROWSER_CDP_PORT);
            let endpoint = format!("http://127.0.0.1:{port}");
            let system = match std::env::consts::OS {
                "macos" => "Darwin",
                "windows" => "Windows",
                _ => "Linux",
            };
            if try_launch_chrome_debug(port, system) {
                std::env::set_var("CHROME_CDP_URL", &endpoint);
                persist_browser_cdp_url(Some(&endpoint))?;
                emit_command_output(
                    app,
                    format!(
                        "Launched Chromium-family browser debug session on {endpoint}. Saved CHROME_CDP_URL to {}/.env.",
                        hermes_config::hermes_home().display()
                    ),
                );
            } else {
                let manual = manual_chrome_debug_command(port, system).unwrap_or_else(|| {
                    "Install Chrome/Chromium/Brave/Edge and rerun `/browser launch`.".to_string()
                });
                emit_command_output(
                    app,
                    format!(
                        "Could not auto-launch a Chromium-family browser.\nManual command:\n{manual}"
                    ),
                );
            }
            Ok(CommandResult::Handled)
        }
        "disconnect" => {
            std::env::remove_var("CHROME_CDP_URL");
            persist_browser_cdp_url(None)?;
            emit_command_output(
                app,
                "Browser CDP override removed. Runtime will fall back to default local endpoint (http://localhost:9222) unless configured elsewhere.",
            );
            Ok(CommandResult::Handled)
        }
        _ => {
            emit_command_output(
                app,
                "Usage: /browser [status|connect [ws://host:port|http://host:port]|launch [port]|disconnect]",
            );
            Ok(CommandResult::Handled)
        }
    }
}

struct QueuedBackgroundJob {
    id: String,
    task: String,
    status_path: PathBuf,
    log_path: PathBuf,
}

include!("command_background_browser/triage_background.rs");
