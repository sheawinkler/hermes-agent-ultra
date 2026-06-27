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
            command.creation_flags(0x00000008 | 0x00000200);
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
enum TriggerTriageDecision {
    Drop,
    Notify,
    Escalate,
    AgentRun,
}

impl TriggerTriageDecision {
    fn as_str(self) -> &'static str {
        match self {
            Self::Drop => "drop",
            Self::Notify => "notify",
            Self::Escalate => "escalate",
            Self::AgentRun => "agent-run",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TriggerTriageAssessment {
    source: String,
    payload: String,
    severity: i32,
    decision: TriggerTriageDecision,
    requires_approval: bool,
    reasons: Vec<String>,
}

fn trigger_triage_mode() -> String {
    std::env::var("HERMES_TRIGGER_TRIAGE_MODE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "off".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TriggerTriageLearningEntry {
    at: String,
    source: String,
    outcome: String,
    decision: String,
    severity: i32,
    bias_delta: i32,
    note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct TriggerTriageLearningState {
    #[serde(default)]
    entries: Vec<TriggerTriageLearningEntry>,
}

fn trigger_triage_learning_state_path() -> PathBuf {
    hermes_config::hermes_home()
        .join("triage")
        .join("learning.json")
}

fn load_trigger_triage_learning_state() -> TriggerTriageLearningState {
    let path = trigger_triage_learning_state_path();
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    serde_json::from_str::<TriggerTriageLearningState>(&raw).unwrap_or_default()
}

fn save_trigger_triage_learning_state(
    state: &TriggerTriageLearningState,
) -> Result<(), AgentError> {
    let path = trigger_triage_learning_state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("Failed to create {}: {}", parent.display(), e)))?;
    }
    let payload = serde_json::to_string_pretty(state)
        .map_err(|e| AgentError::Io(format!("Failed to encode triage learning state: {}", e)))?;
    std::fs::write(&path, payload)
        .map_err(|e| AgentError::Io(format!("Failed to write {}: {}", path.display(), e)))?;
    Ok(())
}

fn triage_feedback_delta(outcome: &str) -> Option<i32> {
    match outcome.trim().to_ascii_lowercase().as_str() {
        "critical" | "escalate" | "confirmed" | "true-positive" | "tp" => Some(2),
        "useful" | "good" | "notify" | "watch" => Some(1),
        "neutral" | "mixed" => Some(0),
        "false-positive" | "fp" | "noise" | "noisy" => Some(-2),
        "drop" | "ignore" | "spam" => Some(-1),
        _ => None,
    }
}

fn triage_learning_bias(source: &str, payload: &str) -> (i32, Vec<String>) {
    let source_l = source.trim().to_ascii_lowercase();
    let payload_l = payload.trim().to_ascii_lowercase();
    let state = load_trigger_triage_learning_state();
    let mut total = 0i32;
    let mut reasons = Vec::new();
    for entry in state.entries.iter().rev().take(120) {
        if entry.source.eq_ignore_ascii_case(&source_l) {
            total += entry.bias_delta;
            if reasons.len() < 3 {
                reasons.push(format!(
                    "source feedback {} ({})",
                    entry.outcome, entry.bias_delta
                ));
            }
            continue;
        }
        if !entry.note.trim().is_empty()
            && payload_l.contains(entry.note.trim().to_ascii_lowercase().as_str())
        {
            total += entry.bias_delta.signum();
            if reasons.len() < 3 {
                reasons.push(format!("matched prior note '{}'", entry.note));
            }
        }
    }
    (total.clamp(-3, 3), reasons)
}

fn evaluate_trigger_triage(source: &str, payload: &str) -> TriggerTriageAssessment {
    let source_l = source.trim().to_ascii_lowercase();
    let payload_l = payload.trim().to_ascii_lowercase();
    let mode = trigger_triage_mode();
    let mut severity = 0i32;
    let mut reasons = Vec::new();

    for (needle, score, reason) in [
        ("panic", 4, "runtime panic or crash"),
        ("outage", 4, "service outage signal"),
        ("secret", 5, "secret exposure indicator"),
        ("key leak", 5, "key leak indicator"),
        ("drawdown", 4, "drawdown or loss event"),
        ("halt", 3, "trading halt or critical gate"),
        ("blocked", 2, "policy or sandbox block"),
        ("timeout", 1, "timeout/retry pressure"),
        ("latency", 1, "latency degradation"),
        ("error", 2, "error signal"),
    ] {
        if payload_l.contains(needle) || source_l.contains(needle) {
            severity += score;
            reasons.push(reason.to_string());
        }
    }

    if source_l.contains("webhook") {
        severity += 1;
        reasons.push("external webhook trigger".to_string());
    }
    if source_l.contains("cron") {
        severity += 1;
        reasons.push("scheduled trigger".to_string());
    }

    let (learning_bias, learning_reasons) = triage_learning_bias(source, payload);
    if learning_bias != 0 {
        severity += learning_bias;
        reasons.push(format!("learning bias applied ({:+})", learning_bias));
        reasons.extend(learning_reasons);
    }

    if mode == "strict" {
        severity += 1;
    } else if mode == "relaxed" {
        severity = severity.saturating_sub(1);
    }

    let (decision, requires_approval) = if severity >= 7 {
        (TriggerTriageDecision::Escalate, true)
    } else if severity >= 4 {
        (TriggerTriageDecision::AgentRun, false)
    } else if severity >= 2 {
        (TriggerTriageDecision::Notify, false)
    } else if payload_l.len() < 6 {
        (TriggerTriageDecision::Drop, false)
    } else {
        (TriggerTriageDecision::Notify, false)
    };

    TriggerTriageAssessment {
        source: source.trim().to_string(),
        payload: payload.trim().to_string(),
        severity,
        decision,
        requires_approval,
        reasons,
    }
}

fn render_trigger_triage_assessment(assessment: &TriggerTriageAssessment) -> String {
    let mut out = String::new();
    out.push_str("Trigger triage assessment\n");
    out.push_str("------------------------\n");
    let _ = writeln!(out, "source: {}", assessment.source);
    let _ = writeln!(out, "payload: {}", truncate_chars(&assessment.payload, 220));
    let _ = writeln!(out, "severity: {}", assessment.severity);
    let _ = writeln!(out, "decision: {}", assessment.decision.as_str());
    let _ = writeln!(out, "requires_approval: {}", assessment.requires_approval);
    if assessment.reasons.is_empty() {
        out.push_str("reasons: none\n");
    } else {
        out.push_str("reasons:\n");
        for reason in &assessment.reasons {
            let _ = writeln!(out, "- {}", reason);
        }
    }
    out
}

fn append_triage_learning_feedback(
    source: &str,
    payload: &str,
    outcome: &str,
    assessment: &TriggerTriageAssessment,
) -> Result<TriggerTriageLearningEntry, AgentError> {
    let delta = triage_feedback_delta(outcome).ok_or_else(|| {
        AgentError::Config(
            "Unknown triage feedback outcome. Use critical|confirmed|useful|neutral|false-positive|drop."
                .to_string(),
        )
    })?;
    let note = payload
        .split_whitespace()
        .take(10)
        .collect::<Vec<_>>()
        .join(" ")
        .trim()
        .to_string();
    let entry = TriggerTriageLearningEntry {
        at: chrono::Utc::now().to_rfc3339(),
        source: source.trim().to_ascii_lowercase(),
        outcome: outcome.trim().to_ascii_lowercase(),
        decision: assessment.decision.as_str().to_string(),
        severity: assessment.severity,
        bias_delta: delta,
        note,
    };
    let mut state = load_trigger_triage_learning_state();
    state.entries.push(entry.clone());
    if state.entries.len() > 400 {
        let remove = state.entries.len().saturating_sub(400);
        state.entries.drain(0..remove);
    }
    save_trigger_triage_learning_state(&state)?;
    Ok(entry)
}

fn render_trigger_triage_learning_status() -> String {
    let state = load_trigger_triage_learning_state();
    let mut by_source: HashMap<String, i32> = HashMap::new();
    for entry in &state.entries {
        *by_source.entry(entry.source.clone()).or_insert(0) += entry.bias_delta;
    }
    let mut ranked = by_source.into_iter().collect::<Vec<_>>();
    ranked.sort_by_key(|(_, bias)| std::cmp::Reverse(*bias));
    let mut out = String::new();
    out.push_str("Trigger triage learning\n");
    out.push_str("----------------------\n");
    let _ = writeln!(out, "entries: {}", state.entries.len());
    if ranked.is_empty() {
        out.push_str("source_bias: none\n");
    } else {
        out.push_str("source_bias:\n");
        for (source, bias) in ranked.into_iter().take(6) {
            let _ = writeln!(out, "- {} => {:+}", source, bias);
        }
    }
    if let Some(last) = state.entries.last() {
        let _ = writeln!(
            out,
            "last_feedback: {} source={} outcome={} delta={:+}",
            last.at, last.source, last.outcome, last.bias_delta
        );
    }
    out
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SubconsciousTask {
    id: String,
    source: String,
    prompt: String,
    score: f64,
    risk: String,
    requires_approval: bool,
    status: String,
    #[serde(default)]
    job_id: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SubconsciousQueueState {
    #[serde(default)]
    tasks: Vec<SubconsciousTask>,
}

fn subconscious_state_path() -> PathBuf {
    hermes_config::hermes_home()
        .join("subconscious")
        .join("queue.json")
}

fn load_subconscious_state() -> SubconsciousQueueState {
    let path = subconscious_state_path();
    let raw = std::fs::read_to_string(path).unwrap_or_default();
    serde_json::from_str::<SubconsciousQueueState>(&raw).unwrap_or_default()
}

fn save_subconscious_state(state: &SubconsciousQueueState) -> Result<(), AgentError> {
    let path = subconscious_state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("Failed to create {}: {}", parent.display(), e)))?;
    }
    let payload = serde_json::to_string_pretty(state)
        .map_err(|e| AgentError::Io(format!("Failed to encode subconscious state: {}", e)))?;
    std::fs::write(&path, payload)
        .map_err(|e| AgentError::Io(format!("Failed to write {}: {}", path.display(), e)))?;
    Ok(())
}

fn score_subconscious_task(prompt: &str) -> f64 {
    let text = prompt.to_ascii_lowercase();
    let mut score = 1.0f64;
    if text.contains("profit")
        || text.contains("wallet")
        || text.contains("sol")
        || text.contains("latency")
        || text.contains("regression")
    {
        score += 1.2;
    }
    if text.contains("fix") || text.contains("verify") || text.contains("test") {
        score += 0.8;
    }
    if let Ok(terms) = utility_terms_from_contract() {
        let mut overlap = 0.0f64;
        for (term, weight) in terms {
            if text.contains(&term.to_ascii_lowercase()) {
                overlap += weight.max(0.0);
            }
        }
        score += overlap.min(2.5);
    }
    score
}

fn risk_for_prompt(prompt: &str) -> (&'static str, bool) {
    let text = prompt.to_ascii_lowercase();
    if text.contains("rm -rf")
        || text.contains("delete ")
        || text.contains("rotate key")
        || text.contains("prod")
        || text.contains("mainnet")
    {
        return ("high", true);
    }
    if text.contains("live trading") || text.contains("wallet") || text.contains("deploy") {
        return ("medium", true);
    }
    ("low", false)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubconsciousProfile {
    Strict,
    Balanced,
    Dev,
}

impl SubconsciousProfile {
    fn as_str(self) -> &'static str {
        match self {
            Self::Strict => "strict",
            Self::Balanced => "balanced",
            Self::Dev => "dev",
        }
    }

    fn parse(raw: &str) -> Option<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "strict" => Some(Self::Strict),
            "balanced" | "standard" => Some(Self::Balanced),
            "dev" => Some(Self::Dev),
            _ => None,
        }
    }
}

fn subconscious_profile_env() -> SubconsciousProfile {
    std::env::var("HERMES_SUBCONSCIOUS_PROFILE")
        .ok()
        .and_then(|v| SubconsciousProfile::parse(&v))
        .unwrap_or(SubconsciousProfile::Balanced)
}

fn subconscious_guard_allows(
    profile: SubconsciousProfile,
    task: &SubconsciousTask,
) -> (bool, String) {
    let risk = task.risk.to_ascii_lowercase();
    match profile {
        SubconsciousProfile::Dev => (true, "dev profile allows execution".to_string()),
        SubconsciousProfile::Balanced => {
            if risk == "high" {
                (
                    false,
                    "balanced profile blocks high-risk subconscious runs".to_string(),
                )
            } else {
                (true, "balanced profile allows low/medium risk".to_string())
            }
        }
        SubconsciousProfile::Strict => {
            if task.requires_approval || risk != "low" {
                (
                    false,
                    "strict profile allows only low-risk non-approval tasks".to_string(),
                )
            } else {
                (true, "strict profile allows low-risk task".to_string())
            }
        }
    }
}

fn handle_subconscious_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    match action.as_str() {
        "status" | "list" => {
            let state = load_subconscious_state();
            let profile = subconscious_profile_env();
            let mut out = String::new();
            out.push_str("Subconscious queue\n");
            out.push_str("-----------------\n");
            let _ = writeln!(out, "profile: {}", profile.as_str());
            if state.tasks.is_empty() {
                out.push_str("No queued subconscious tasks.\n");
            } else {
                for task in state.tasks.iter().rev().take(24) {
                    let _ = writeln!(
                        out,
                        "- {} [{}] score={:.2} risk={} approval={} source={} :: {}",
                        task.id,
                        task.status,
                        task.score,
                        task.risk,
                        task.requires_approval,
                        task.source,
                        truncate_chars(&task.prompt, 100)
                    );
                }
            }
            out.push_str(
                "\nUsage: /subconscious add <prompt> | approve <id> | reject <id> | run [n] [--dry-run] [profile=<strict|balanced|dev>] | profile [status|list|strict|balanced|dev|clear] | clear",
            );
            emit_command_output(app, out.trim_end());
        }
        "add" => {
            let prompt = args.get(1..).unwrap_or(&[]).join(" ").trim().to_string();
            if prompt.is_empty() {
                emit_command_output(app, "Usage: /subconscious add <prompt>");
                return Ok(CommandResult::Handled);
            }
            let (risk, requires_approval) = risk_for_prompt(&prompt);
            let score = score_subconscious_task(&prompt);
            let mut state = load_subconscious_state();
            let id = format!(
                "sc-{}",
                uuid::Uuid::new_v4()
                    .simple()
                    .to_string()
                    .chars()
                    .take(8)
                    .collect::<String>()
            );
            let task = SubconsciousTask {
                id: id.clone(),
                source: "manual".to_string(),
                prompt,
                score,
                risk: risk.to_string(),
                requires_approval,
                status: if requires_approval {
                    "pending-approval".to_string()
                } else {
                    "pending".to_string()
                },
                job_id: None,
                created_at: chrono::Utc::now().to_rfc3339(),
                updated_at: chrono::Utc::now().to_rfc3339(),
            };
            state.tasks.push(task.clone());
            save_subconscious_state(&state)?;
            emit_command_output(
                app,
                format!(
                    "Queued subconscious task {}\nstatus={} score={:.2} risk={}\n{}",
                    task.id,
                    task.status,
                    task.score,
                    task.risk,
                    if task.requires_approval {
                        "Requires approval: /subconscious approve <id>"
                    } else {
                        "Ready to run: /subconscious run"
                    }
                ),
            );
        }
        "approve" | "reject" => {
            let Some(task_id) = args.get(1).copied() else {
                emit_command_output(app, format!("Usage: /subconscious {} <id>", action));
                return Ok(CommandResult::Handled);
            };
            let mut state = load_subconscious_state();
            let mut found = false;
            for task in &mut state.tasks {
                if task.id.eq_ignore_ascii_case(task_id) {
                    found = true;
                    task.status = if action == "approve" {
                        "pending".to_string()
                    } else {
                        "rejected".to_string()
                    };
                    task.updated_at = chrono::Utc::now().to_rfc3339();
                    break;
                }
            }
            if !found {
                emit_command_output(app, format!("Task not found: {}", task_id));
                return Ok(CommandResult::Handled);
            }
            save_subconscious_state(&state)?;
            emit_command_output(app, format!("Subconscious task {} {}", task_id, action));
        }
        "run" => {
            let mut limit = 1usize;
            let mut dry_run = false;
            let mut profile_override: Option<SubconsciousProfile> = None;
            for token in args.get(1..).unwrap_or(&[]) {
                let token_l = token.trim().to_ascii_lowercase();
                if token_l == "--dry-run" || token_l == "dry-run" || token_l == "preview" {
                    dry_run = true;
                    continue;
                }
                if let Ok(parsed) = token_l.parse::<usize>() {
                    limit = parsed.clamp(1, 8);
                    continue;
                }
                if let Some(raw) = token_l.strip_prefix("profile=") {
                    profile_override = SubconsciousProfile::parse(raw);
                    continue;
                }
                if profile_override.is_none() {
                    profile_override = SubconsciousProfile::parse(&token_l);
                }
            }
            let profile = profile_override.unwrap_or_else(subconscious_profile_env);
            let mut state = load_subconscious_state();
            let mut reviewed = 0usize;
            let mut dispatched = 0usize;
            let mut blocked = 0usize;
            let mut notes = Vec::new();
            for task in &mut state.tasks {
                if reviewed >= limit {
                    break;
                }
                if task.status != "pending" {
                    continue;
                }
                reviewed += 1;
                let (allowed, guard_note) = subconscious_guard_allows(profile, task);
                if !allowed {
                    blocked += 1;
                    notes.push(format!("{} blocked ({})", task.id, guard_note));
                    continue;
                }
                if dry_run {
                    notes.push(format!("{} would dispatch ({})", task.id, guard_note));
                    continue;
                }
                let job = queue_background_job(&task.prompt)?;
                task.status = "dispatched".to_string();
                task.job_id = Some(job.id.clone());
                task.updated_at = chrono::Utc::now().to_rfc3339();
                dispatched += 1;
                notes.push(format!("{} dispatched id={}", task.id, job.id));
            }
            if !dry_run {
                save_subconscious_state(&state)?;
            }
            emit_command_output(
                app,
                format!(
                    "{} subconscious run profile={}\nreviewed={} dispatched={} blocked={}\n{}\nUse `/background status` and `/subconscious status` for tracking.",
                    if dry_run {
                        "Dry-run"
                    } else {
                        "Executed"
                    },
                    profile.as_str(),
                    reviewed,
                    dispatched,
                    blocked,
                    if notes.is_empty() {
                        "No pending tasks matched selection.".to_string()
                    } else {
                        notes.join("\n")
                    }
                ),
            );
        }
        "profile" => {
            let token = args.get(1).copied().unwrap_or("status").to_ascii_lowercase();
            match token.as_str() {
                "status" | "show" => emit_command_output(
                    app,
                    format!(
                        "Subconscious profile: {}\nUse `/subconscious profile list` or `/subconscious profile strict|balanced|dev`.",
                        subconscious_profile_env().as_str()
                    ),
                ),
                "list" => emit_command_output(
                    app,
                    "Subconscious profiles:\n- strict: only low-risk non-approval tasks auto-dispatch\n- balanced: low/medium dispatch, high-risk blocked\n- dev: permit all pending tasks\nSet with `/subconscious profile <name>`.",
                ),
                "clear" => {
                    std::env::remove_var("HERMES_SUBCONSCIOUS_PROFILE");
                    emit_command_output(
                        app,
                        "Cleared subconscious profile override (default=balanced).",
                    );
                }
                other => {
                    let Some(next) = SubconsciousProfile::parse(other) else {
                        emit_command_output(
                            app,
                            "Usage: /subconscious profile [status|list|strict|balanced|dev|clear]",
                        );
                        return Ok(CommandResult::Handled);
                    };
                    std::env::set_var("HERMES_SUBCONSCIOUS_PROFILE", next.as_str());
                    emit_command_output(app, format!("Subconscious profile set to {}.", next.as_str()));
                }
            }
        }
        "clear" => {
            let path = subconscious_state_path();
            if path.exists() {
                std::fs::remove_file(&path).map_err(|e| {
                    AgentError::Io(format!("Failed to remove {}: {}", path.display(), e))
                })?;
            }
            emit_command_output(app, "Cleared subconscious queue.");
        }
        _ => emit_command_output(
            app,
            "Usage: /subconscious [status|add <prompt>|approve <id>|reject <id>|run [n] [--dry-run] [profile=<strict|balanced|dev>]|profile [status|list|strict|balanced|dev|clear]|clear]",
        ),
    }
    Ok(CommandResult::Handled)
}

fn handle_trigger_triage_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    match action.as_str() {
        "status" => {
            emit_command_output(
                app,
                format!(
                    "Trigger triage mode: {}\n{}\nUsage: /triage eval <source> <payload> | /triage queue <source> <payload> | /triage feedback <source> <outcome> <payload>",
                    trigger_triage_mode(),
                    render_trigger_triage_learning_status().trim_end()
                ),
            );
        }
        "list" | "rules" => {
            emit_command_output(
                app,
                "Trigger triage heuristics\n\
                 - high severity: panic/outage/secret leak/drawdown/halt -> escalate\n\
                 - medium severity: repeated errors/blocked/timeout -> agent-run\n\
                 - low severity: notify\n\
                 - empty/noise payload -> drop\n\
                 Mode override: HERMES_TRIGGER_TRIAGE_MODE={strict|balanced|relaxed}\n\
                 Feedback loop: `/triage feedback <source> <outcome> <payload>` updates persistent bias.",
            );
        }
        "feedback" => {
            let Some(source) = args.get(1).copied() else {
                emit_command_output(
                    app,
                    "Usage: /triage feedback <source> <outcome> <payload>",
                );
                return Ok(CommandResult::Handled);
            };
            let Some(outcome) = args.get(2).copied() else {
                emit_command_output(
                    app,
                    "Usage: /triage feedback <source> <outcome> <payload>",
                );
                return Ok(CommandResult::Handled);
            };
            let payload = args.get(3..).unwrap_or(&[]).join(" ").trim().to_string();
            if payload.is_empty() {
                emit_command_output(
                    app,
                    "Usage: /triage feedback <source> <outcome> <payload>",
                );
                return Ok(CommandResult::Handled);
            }
            let assessment = evaluate_trigger_triage(source, &payload);
            let entry = append_triage_learning_feedback(source, &payload, outcome, &assessment)?;
            let (bias_now, _) = triage_learning_bias(source, &payload);
            emit_command_output(
                app,
                format!(
                    "Recorded triage feedback.\nsource={} outcome={} delta={:+} decision={} severity={}\nsource_bias_now={:+}",
                    entry.source, entry.outcome, entry.bias_delta, entry.decision, entry.severity, bias_now
                ),
            );
        }
        "eval" | "queue" => {
            let Some(source) = args.get(1).copied() else {
                emit_command_output(
                    app,
                    "Usage: /triage eval <source> <payload>\nUsage: /triage queue <source> <payload>",
                );
                return Ok(CommandResult::Handled);
            };
            let payload = args.get(2..).unwrap_or(&[]).join(" ");
            if payload.trim().is_empty() {
                emit_command_output(app, "Payload cannot be empty.");
                return Ok(CommandResult::Handled);
            }
            let assessment = evaluate_trigger_triage(source, &payload);
            let mut out = render_trigger_triage_assessment(&assessment);
            if action == "queue" {
                match assessment.decision {
                    TriggerTriageDecision::Drop => {
                        out.push_str("\n\nqueue_action: dropped");
                    }
                    TriggerTriageDecision::Notify => {
                        out.push_str("\n\nqueue_action: notify-only (no agent run queued)");
                    }
                    TriggerTriageDecision::Escalate => {
                        let mut state = load_subconscious_state();
                        let id = format!(
                            "sc-{}",
                            uuid::Uuid::new_v4()
                                .simple()
                                .to_string()
                                .chars()
                                .take(8)
                                .collect::<String>()
                        );
                        state.tasks.push(SubconsciousTask {
                            id: id.clone(),
                            source: source.to_string(),
                            prompt: payload.trim().to_string(),
                            score: score_subconscious_task(&payload),
                            risk: "high".to_string(),
                            requires_approval: true,
                            status: "pending-approval".to_string(),
                            job_id: None,
                            created_at: chrono::Utc::now().to_rfc3339(),
                            updated_at: chrono::Utc::now().to_rfc3339(),
                        });
                        save_subconscious_state(&state)?;
                        let _ = write!(
                            out,
                            "\n\nqueue_action: escalated to subconscious queue as {} (requires approval)",
                            id
                        );
                    }
                    TriggerTriageDecision::AgentRun => {
                        let job = queue_background_job(payload.trim())?;
                        let _ = write!(
                            out,
                            "\n\nqueue_action: background job queued id={} status_file={}",
                            job.id,
                            job.status_path.display()
                        );
                    }
                }
            }
            emit_command_output(app, out);
        }
        _ => emit_command_output(
            app,
            "Usage: /triage [status|list|eval <source> <payload>|queue <source> <payload>|feedback <source> <outcome> <payload>]",
        ),
    }
    Ok(CommandResult::Handled)
}

fn queue_background_job(task: &str) -> Result<QueuedBackgroundJob, AgentError> {
    let task = task.trim();
    if task.is_empty() {
        return Err(AgentError::Config(
            "Background task cannot be empty.".to_string(),
        ));
    }
    let job_id = format!(
        "{}-{}",
        chrono::Utc::now().format("%Y%m%d%H%M%S"),
        uuid::Uuid::new_v4().simple()
    );
    let jobs_dir = hermes_config::hermes_home().join("background_jobs");
    std::fs::create_dir_all(&jobs_dir).map_err(|e| {
        AgentError::Io(format!(
            "Failed to create background job directory {}: {}",
            jobs_dir.display(),
            e
        ))
    })?;
    let status_path = jobs_dir.join(format!("{}.json", job_id));
    let log_path = jobs_dir.join(format!("{}.log", job_id));

    let status = serde_json::json!({
        "id": job_id,
        "task": task,
        "status": "queued",
        "attempts": 0,
        "created_at": chrono::Utc::now().to_rfc3339(),
        "started_at": serde_json::Value::Null,
        "finished_at": serde_json::Value::Null,
        "exit_code": serde_json::Value::Null,
        "log_path": log_path,
    });
    std::fs::write(
        &status_path,
        serde_json::to_string_pretty(&status).unwrap_or_else(|_| "{}".to_string()),
    )
    .map_err(|e| AgentError::Io(format!("Failed to write background status: {}", e)))?;

    schedule_background_job_execution(status_path.clone(), log_path.clone(), task.to_string());
    Ok(QueuedBackgroundJob {
        id: status["id"].as_str().unwrap_or("unknown").to_string(),
        task: task.to_string(),
        status_path,
        log_path,
    })
}

fn handle_background_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        emit_command_output(
            app,
            "Usage: /background <message>\n\
             - /background status|list\n\
             - /background tail <job-id> [N]\n\
             - /background stop <job-id>\n\
             - /background event <source> <payload>\n\
             Queues a task to run in the background while you continue chatting.",
        );
        return Ok(CommandResult::Handled);
    }
    let sub = args[0].trim().to_ascii_lowercase();
    if sub == "status" || sub == "list" {
        emit_command_output(app, render_background_status(12));
        return Ok(CommandResult::Handled);
    }
    if sub == "tail" || sub == "log" || sub == "logs" || sub == "show" {
        let limit = args
            .get(2)
            .and_then(|raw| raw.trim().parse::<usize>().ok())
            .unwrap_or(80)
            .clamp(5, 800);
        let requested_id = args
            .get(1)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                collect_background_jobs(1)
                    .into_iter()
                    .next()
                    .map(|row| row.id)
            });
        let Some(id_or_prefix) = requested_id else {
            emit_command_output(
                app,
                "Usage: /background tail <job-id> [N]\nNo jobs available yet.",
            );
            return Ok(CommandResult::Handled);
        };
        let Some(job) = resolve_background_job(&id_or_prefix) else {
            emit_command_output(
                app,
                format!(
                    "Background job '{}' not found. Use `/background status`.",
                    id_or_prefix
                ),
            );
            return Ok(CommandResult::Handled);
        };
        let tail = if job.log_path.exists() {
            tail_file_lines(&job.log_path, limit)?
        } else {
            "(log file does not exist yet)".to_string()
        };
        emit_command_output(
            app,
            format!(
                "Background job\nid: {}\nstatus: {}\nattempts: {}\ncreated_at: {}\nstarted_at: {}\nfinished_at: {}\nstatus_file: {}\nlog_file: {}\n\n--- log tail ({}) ---\n{}",
                job.id,
                job.status,
                job.attempts,
                if job.created_at.is_empty() { "(n/a)" } else { job.created_at.as_str() },
                if job.started_at.is_empty() { "(n/a)" } else { job.started_at.as_str() },
                if job.finished_at.is_empty() { "(n/a)" } else { job.finished_at.as_str() },
                job.status_path.display(),
                job.log_path.display(),
                limit,
                if tail.trim().is_empty() { "(empty)" } else { tail.trim_end() }
            ),
        );
        return Ok(CommandResult::Handled);
    }
    if sub == "stop" || sub == "cancel" || sub == "kill" {
        let requested_id = args
            .get(1)
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                collect_background_jobs(200).into_iter().find_map(|job| {
                    if matches!(job.status.as_str(), "running" | "queued") {
                        Some(job.id)
                    } else {
                        None
                    }
                })
            });
        let Some(id_or_prefix) = requested_id else {
            emit_command_output(
                app,
                "Usage: /background stop <job-id>\nNo running/queued jobs found.",
            );
            return Ok(CommandResult::Handled);
        };
        emit_command_output(app, terminate_background_job(&id_or_prefix)?);
        return Ok(CommandResult::Handled);
    }
    if sub == "event" {
        let Some(source) = args.get(1).copied() else {
            emit_command_output(app, "Usage: /background event <source> <payload>");
            return Ok(CommandResult::Handled);
        };
        let payload = args.get(2..).unwrap_or(&[]).join(" ");
        if payload.trim().is_empty() {
            emit_command_output(app, "Usage: /background event <source> <payload>");
            return Ok(CommandResult::Handled);
        }
        let triage_args = vec!["queue", source];
        let mut merged = triage_args;
        let payload_parts: Vec<String> =
            payload.split_whitespace().map(|s| s.to_string()).collect();
        let payload_refs: Vec<&str> = payload_parts.iter().map(String::as_str).collect();
        merged.extend(payload_refs);
        return handle_trigger_triage_command(app, &merged);
    }
    let job = queue_background_job(&args.join(" "))?;
    emit_command_output(
        app,
        format!(
            "[Background task queued: \"{}\"]\nJob ID: {}\nStatus: {}\nLogs:   {}\nThis task runs in a detached `hermes chat --query ...` process.",
            job.task,
            job.id,
            job.status_path.display(),
            job.log_path.display()
        ),
    );
    Ok(CommandResult::Handled)
}

#[cfg(unix)]
fn process_running(pid: u32) -> bool {
    // SAFETY: libc::kill with signal 0 only performs existence/permission check.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        true
    } else {
        matches!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::EPERM)
        )
    }
}

#[cfg(not(unix))]
fn process_running(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
fn terminate_pid(pid: u32) -> std::io::Result<()> {
    // SAFETY: pid is sourced from our own status record; SIGTERM is best-effort.
    let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if rc == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn terminate_pid(_pid: u32) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "Process termination is unsupported on this platform.",
    ))
}

fn terminate_background_job(id_or_prefix: &str) -> Result<String, AgentError> {
    let Some(job) = resolve_background_job(id_or_prefix) else {
        return Ok(format!(
            "Background job '{}' not found. Use `/background status`.",
            id_or_prefix
        ));
    };
    let mut map = read_json_map(&job.status_path);
    if map.is_empty() {
        return Err(AgentError::Io(format!(
            "Status file missing or unreadable: {}",
            job.status_path.display()
        )));
    }
    let status = map
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_ascii_lowercase();
    if status == "completed" || status == "failed" || status == "canceled" {
        return Ok(format!(
            "Background job {} already {}.\nStatus file: {}",
            job.id,
            status,
            job.status_path.display()
        ));
    }

    let mut termination_note = String::new();
    if let Some(pid) = map
        .get("pid")
        .and_then(|v| v.as_u64())
        .and_then(|raw| u32::try_from(raw).ok())
    {
        if process_running(pid) {
            match terminate_pid(pid) {
                Ok(()) => termination_note = format!("Sent SIGTERM to pid {}.", pid),
                Err(err) => termination_note = format!("Failed to terminate pid {}: {}.", pid, err),
            }
        } else {
            termination_note = format!("Pid {} was not running.", pid);
        }
    }

    map.insert(
        "status".into(),
        serde_json::Value::String("canceled".into()),
    );
    map.insert(
        "finished_at".into(),
        serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
    );
    map.insert(
        "error".into(),
        serde_json::Value::String("canceled by operator".into()),
    );
    map.insert("pid".into(), serde_json::Value::Null);
    write_json_map(&job.status_path, &map)
        .map_err(|e| AgentError::Io(format!("Failed to update background status: {}", e)))?;

    Ok(format!(
        "Canceled background job {}\nStatus file: {}\n{}",
        job.id,
        job.status_path.display(),
        if termination_note.is_empty() {
            "No active child pid recorded.".to_string()
        } else {
            termination_note
        }
    ))
}

fn claim_queued_background_job(
    status_path: &Path,
) -> Result<Option<serde_json::Map<String, serde_json::Value>>, AgentError> {
    let mut queued = read_json_map(status_path);
    if queued.is_empty() {
        return Ok(None);
    }
    let status = queued
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("queued")
        .to_ascii_lowercase();
    if status != "queued" {
        return Ok(None);
    }
    let started = chrono::Utc::now().to_rfc3339();
    let attempts = queued
        .get("attempts")
        .and_then(|v| v.as_u64())
        .unwrap_or(0)
        .saturating_add(1);
    queued.insert(
        "status".to_string(),
        serde_json::Value::String("running".into()),
    );
    queued.insert("started_at".to_string(), serde_json::Value::String(started));
    queued.insert("attempts".to_string(), serde_json::json!(attempts));
    write_json_map(status_path, &queued)
        .map_err(|e| AgentError::Io(format!("Failed to claim background job: {}", e)))?;
    Ok(Some(queued))
}

fn schedule_background_job_execution(status_path: PathBuf, log_path: PathBuf, task: String) {
    tokio::spawn(async move {
        let queued = match claim_queued_background_job(&status_path) {
            Ok(Some(claimed)) => claimed,
            Ok(None) => return,
            Err(_) => return,
        };
        let started = queued
            .get("started_at")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                let mut failed = queued.clone();
                failed.insert("status".into(), serde_json::Value::String("failed".into()));
                failed.insert(
                    "finished_at".into(),
                    serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
                );
                failed.insert(
                    "error".into(),
                    serde_json::Value::String(format!("current_exe: {}", e)),
                );
                let _ = write_json_map(&status_path, &failed);
                return;
            }
        };

        let mut cmd = tokio::process::Command::new(exe);
        cmd.arg("chat")
            .arg("--query")
            .arg(task)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Ensure detached children do not survive runtime/session teardown.
        cmd.kill_on_drop(true);

        if let Ok(home) = std::env::var("HERMES_HOME") {
            cmd.env("HERMES_HOME", home);
        }

        let child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                let mut failed = queued.clone();
                failed.insert("status".into(), serde_json::Value::String("failed".into()));
                failed.insert(
                    "finished_at".into(),
                    serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
                );
                failed.insert(
                    "error".into(),
                    serde_json::Value::String(format!("spawn failed: {}", e)),
                );
                failed.insert("pid".into(), serde_json::Value::Null);
                let _ = write_json_map(&status_path, &failed);
                return;
            }
        };
        if let Some(pid) = child.id() {
            let mut running = queued.clone();
            running.insert("pid".into(), serde_json::json!(pid));
            let _ = write_json_map(&status_path, &running);
        }

        let out = child.wait_with_output().await;
        match out {
            Ok(output) => {
                let exit = output.status.code().unwrap_or(-1);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let log = format!(
                    "task: {}\nstarted_at: {}\nfinished_at: {}\nexit_code: {}\n\n[stdout]\n{}\n\n[stderr]\n{}\n",
                    queued
                        .get("task")
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                    started,
                    chrono::Utc::now().to_rfc3339(),
                    exit,
                    stdout,
                    stderr
                );
                let _ = std::fs::write(&log_path, log);

                let mut done = queued.clone();
                done.insert(
                    "status".into(),
                    serde_json::Value::String(if output.status.success() {
                        "completed".into()
                    } else {
                        "failed".into()
                    }),
                );
                done.insert(
                    "finished_at".into(),
                    serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
                );
                done.insert("exit_code".into(), serde_json::json!(exit));
                done.insert("pid".into(), serde_json::Value::Null);
                let _ = write_json_map(&status_path, &done);
            }
            Err(e) => {
                let mut failed = queued.clone();
                failed.insert("status".into(), serde_json::Value::String("failed".into()));
                failed.insert(
                    "finished_at".into(),
                    serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
                );
                failed.insert(
                    "error".into(),
                    serde_json::Value::String(format!("spawn/output failed: {}", e)),
                );
                failed.insert("pid".into(), serde_json::Value::Null);
                let _ = write_json_map(&status_path, &failed);
            }
        }
    });
}

pub fn recover_queued_background_jobs(max_jobs: usize) -> usize {
    let jobs_dir = hermes_config::hermes_home().join("background_jobs");
    let Ok(entries) = std::fs::read_dir(&jobs_dir) else {
        return 0;
    };
    let mut recovered = 0usize;
    for entry in entries.filter_map(Result::ok) {
        if recovered >= max_jobs.max(1) {
            break;
        }
        let status_path = entry.path();
        if status_path
            .extension()
            .and_then(|ext| ext.to_str())
            .unwrap_or("")
            != "json"
        {
            continue;
        }
        let map = read_json_map(&status_path);
        let status = map
            .get("status")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if status != "queued" {
            continue;
        }
        let task = map
            .get("task")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToString::to_string);
        let log_path = map
            .get("log_path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| status_path.with_extension("log"));
        if let Some(task) = task {
            schedule_background_job_execution(status_path.clone(), log_path, task);
            recovered = recovered.saturating_add(1);
        }
    }
    recovered
}

fn read_json_map(path: &std::path::Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn write_json_map(
    path: &std::path::Path,
    map: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), std::io::Error> {
    let content = serde_json::to_string_pretty(&serde_json::Value::Object(map.clone()))
        .unwrap_or_else(|_| "{}".to_string());
    std::fs::write(path, content)
}

fn handle_verbose_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let current = tracing::enabled!(tracing::Level::DEBUG);
    if current {
        emit_command_output(
            app,
            "Verbose mode: OFF (switching to info level)\n(Runtime log level changes require restart — use `hermes -v` for verbose)",
        );
    } else {
        emit_command_output(
            app,
            "Verbose mode: ON (switching to debug level)\n(Runtime log level changes require restart — use `hermes -v` for verbose)",
        );
    }
    Ok(CommandResult::Handled)
}

fn handle_yolo_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let currently_required = app.config.approval.require_approval;
    let new_val = !currently_required;

    app.config = Arc::new({
        let mut cfg = (*app.config).clone();
        cfg.approval.require_approval = new_val;
        cfg
    });

    if !new_val {
        emit_command_output(
            app,
            "YOLO mode: ON — tool executions will not require approval.\nBe careful! The agent can now execute tools without confirmation.",
        );
    } else {
        emit_command_output(
            app,
            "YOLO mode: OFF — tool executions will require approval.",
        );
    }
    Ok(CommandResult::Handled)
}
