fn replay_enabled_runtime() -> bool {
    std::env::var("HERMES_REPLAY_ENABLED")
        .ok()
        .is_some_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

fn replay_log_path_for_session(state_root: &Path, session_id: &str) -> PathBuf {
    let sid = if session_id.trim().is_empty() {
        "session".to_string()
    } else {
        session_id
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    state_root
        .join("logs")
        .join("replay")
        .join(format!("{sid}.jsonl"))
}

fn authoritative_run_export_dir(state_root: &Path) -> PathBuf {
    state_root.join("run-record-v1").join("exports")
}


/// Read the immutable export once, then let core verify and retain the same
/// canonical bytes in the typed replay plan. Callers must never re-read `path`
/// to reconstruct an already verified plan.
fn read_verified_run_replay_plan(
    path: &Path,
) -> hermes_agent::run_export::RunExportResult<
    hermes_agent::deterministic_replay::VerifiedRunReplayPlan,
> {
    let bytes = std::fs::read(path).map_err(|source| hermes_agent::run_export::RunExportError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    hermes_agent::run_export::verify_run_export_replay_plan_bytes(&bytes)
}

fn render_replay_evidence_count(
    count: hermes_agent::deterministic_replay::ReplayEvidenceCount,
) -> String {
    match count {
        hermes_agent::deterministic_replay::ReplayEvidenceCount::Available(value) => {
            value.to_string()
        }
        hermes_agent::deterministic_replay::ReplayEvidenceCount::Unavailable => {
            "unavailable".to_string()
        }
    }
}

fn render_verified_run_reconstruction(
    path: &Path,
    plan: &hermes_agent::deterministic_replay::VerifiedRunReplayPlan,
    report: &hermes_agent::deterministic_replay::DeterministicReplayReport,
) -> String {
    let summary = plan.summary();
    let exact = report.exact();
    let artifact_availability = if report.artifact_count() == 0 {
        "not_required"
    } else {
        "verified"
    };
    let counters = report.counters();
    let mut output = format!(
        "Authoritative verified run reconstruction\nintegrity_verified=true\nintegrity_authenticated=false\nevidence_complete={}\neffects_reconstructed_not_executed=true\nexact={}\nreconstruction_fidelity={}\nrun_id={}\npath={}\ndigest={}\nevent_count={}\nevent_sequence=1..={}\nartifact_count={}\nartifact_availability={}\ntool_call_count={}\nfailure_count={}\nterminal_reason={}\nfinal_outcome={}",
        summary.evidence_complete(),
        exact,
        report.fidelity().as_str(),
        report.run_id(),
        path.display(),
        report.export_digest(),
        report.event_count(),
        report.event_count(),
        report.artifact_count(),
        artifact_availability,
        render_replay_evidence_count(counters.tool_call_count),
        render_replay_evidence_count(counters.failure_count),
        report.terminal_reason().as_str(),
        report.final_outcome(),
    );
    for artifact in report.artifacts() {
        let _ = writeln!(
            output,
            "\nartifact_event_id={}\nartifact_digest={}\nartifact_availability={}\nartifact_fidelity={}",
            artifact.event_id,
            artifact.envelope_digest,
            artifact.availability.as_str(),
            artifact.fidelity.as_str(),
        );
    }
    output
}

#[derive(Clone, Copy)]
enum RunExportSelection {
    Latest,
    All,
    Exact(hermes_agent::run_ledger::RunId),
}

fn parse_run_export_selection(raw: &str) -> Result<RunExportSelection, AgentError> {
    let raw = raw.trim();
    if raw.eq_ignore_ascii_case("latest") {
        return Ok(RunExportSelection::Latest);
    }
    if raw.eq_ignore_ascii_case("all") {
        return Ok(RunExportSelection::All);
    }
    raw.parse::<hermes_agent::run_ledger::RunId>()
        .map(RunExportSelection::Exact)
        .map_err(|_| {
            AgentError::Config(
                "run export selector must be latest, all, or an exact run ID".to_string(),
            )
        })
}

fn selected_run_ids_for_export(
    app: &App,
    selection: RunExportSelection,
) -> Result<Vec<hermes_agent::run_ledger::RunId>, AgentError> {
    match selection {
        RunExportSelection::Latest => app
            .run_runtime
            .session_runs(
                &app.session_id,
                hermes_agent::SessionRunSelection::Latest,
            )
            .map(|runs| runs.into_iter().map(|run| run.run_id).collect())
            .map_err(|error| AgentError::Config(format!("could not inspect current session runs: {error}"))),
        RunExportSelection::All => app
            .run_runtime
            .session_runs(&app.session_id, hermes_agent::SessionRunSelection::All)
            .map(|runs| runs.into_iter().map(|run| run.run_id).collect())
            .map_err(|error| AgentError::Config(format!("could not inspect current session runs: {error}"))),
        RunExportSelection::Exact(run_id) => app
            .run_runtime
            .inspect_run(run_id)
            .map(|_| vec![run_id])
            .map_err(|error| AgentError::Config(format!("could not inspect run {run_id}: {error}"))),
    }
}

fn render_replay_trace_tail(path: &Path, limit: usize) -> Result<String, AgentError> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        AgentError::Io(format!(
            "Failed to read legacy replay diagnostic log {}: {}",
            path.display(),
            e
        ))
    })?;
    let lines: Vec<&str> = raw
        .lines()
        .rev()
        .take(limit.max(1))
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    if lines.is_empty() {
        return Ok("Legacy replay diagnostic (unverified): log is empty.".to_string());
    }
    let mut out = String::from("Legacy replay diagnostic (unverified)\n");
    for line in lines {
        let value: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                let _ = writeln!(out, "{}", line);
                continue;
            }
        };
        let seq = value
            .get("seq")
            .and_then(|v| v.as_u64())
            .map(|n| n.to_string())
            .unwrap_or_else(|| "?".to_string());
        let event = value
            .get("event")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let trace_id = value
            .get("trace_id")
            .and_then(|v| v.as_str())
            .unwrap_or("missing");
        let prev_hash = value
            .get("prev_hash")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let event_hash = value
            .get("event_hash")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let turn = value
            .get("payload")
            .and_then(|payload| payload.get("turn"))
            .and_then(|turn| turn.as_u64())
            .map(|n| n.to_string())
            .unwrap_or_else(|| "-".to_string());
        let _ = writeln!(
            out,
            "#{seq:<4} turn={turn:<3} event={event:<24} trace={trace_id} prev={prev_hash} hash={event_hash}"
        );
    }
    Ok(out.trim_end().to_string())
}

fn replay_entries(path: &Path, limit: usize) -> Result<Vec<serde_json::Value>, AgentError> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        AgentError::Io(format!(
            "Failed to read legacy replay diagnostic log {}: {}",
            path.display(),
            e
        ))
    })?;
    Ok(raw
        .lines()
        .rev()
        .take(limit.max(1))
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect())
}

fn render_replay_trace_focus(
    path: &Path,
    trace_id: &str,
    limit: usize,
) -> Result<String, AgentError> {
    let trace_filter = trace_id.trim();
    if trace_filter.is_empty() {
        return Ok("Usage: /raw trace focus <trace-id> [N]".to_string());
    }
    let rows = replay_entries(path, limit)?;
    let filtered: Vec<serde_json::Value> = rows
        .into_iter()
        .filter(|row| {
            row.get("trace_id")
                .and_then(|v| v.as_str())
                .is_some_and(|v| v == trace_filter || v.contains(trace_filter))
        })
        .collect();
    if filtered.is_empty() {
        return Ok(format!(
            "Legacy replay diagnostic (unverified): no events found for trace '{}' in {}.",
            trace_filter,
            path.display()
        ));
    }
    let mut out = String::new();
    let _ = writeln!(
        out,
        "Legacy replay trace focus (diagnostic/unverified): {}",
        trace_filter
    );
    let _ = writeln!(out, "events: {}", filtered.len());
    let _ = writeln!(out, "path: {}", path.display());
    let _ = writeln!(out);
    for row in filtered {
        let seq = row.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
        let event = row
            .get("event")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        let turn = row
            .get("payload")
            .and_then(|payload| payload.get("turn"))
            .and_then(|turn| turn.as_u64())
            .unwrap_or(0);
        let preview = row
            .get("payload")
            .map(|v| truncate_chars(&v.to_string(), 120))
            .unwrap_or_else(|| "{}".to_string());
        let _ = writeln!(out, "#{seq:<4} turn={turn:<3} event={event:<24} {preview}");
    }
    Ok(out.trim_end().to_string())
}

fn render_replay_trace_graph(path: &Path, limit: usize) -> Result<String, AgentError> {
    let rows = replay_entries(path, limit)?;
    if rows.is_empty() {
        return Ok("Legacy replay graph (diagnostic/unverified): no entries in current window."
            .to_string());
    }
    let mut out = String::new();
    let _ = writeln!(out, "Legacy replay lineage graph (diagnostic/unverified)");
    let _ = writeln!(out, "---------------------------------------------------");
    let _ = writeln!(out, "window={} path={}", rows.len(), path.display());
    for row in rows {
        let seq = row
            .get("seq")
            .and_then(|value| value.as_u64())
            .map(|v| v.to_string())
            .unwrap_or_else(|| "?".to_string());
        let event = row
            .get("event")
            .and_then(|value| value.as_str())
            .unwrap_or("unknown");
        let trace_id = row
            .get("trace_id")
            .and_then(|value| value.as_str())
            .unwrap_or("missing");
        let prev = row
            .get("prev_hash")
            .and_then(|value| value.as_str())
            .unwrap_or("-");
        let hash = row
            .get("event_hash")
            .and_then(|value| value.as_str())
            .unwrap_or("-");
        let _ = writeln!(
            out,
            "#{:<4} {:<20} trace={} {} -> {}",
            seq, event, trace_id, prev, hash
        );
    }
    Ok(out.trim_end().to_string())
}

fn legacy_replay_trace_diagnostic(path: &Path) -> Result<(usize, usize, usize), AgentError> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        AgentError::Io(format!(
            "Failed to read legacy replay diagnostic log {}: {}",
            path.display(),
            e
        ))
    })?;
    let mut entries = 0usize;
    let mut parse_errors = 0usize;
    let mut chain_breaks = 0usize;
    let mut last_event_hash: Option<String> = None;
    for line in raw.lines() {
        let parsed: serde_json::Value = match serde_json::from_str(line) {
            Ok(v) => v,
            Err(_) => {
                parse_errors = parse_errors.saturating_add(1);
                continue;
            }
        };
        entries = entries.saturating_add(1);
        let prev_hash = parsed
            .get("prev_hash")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        let event_hash = parsed
            .get("event_hash")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        if let (Some(last), Some(prev)) = (last_event_hash.as_ref(), prev_hash.as_ref()) {
            if last != prev {
                chain_breaks = chain_breaks.saturating_add(1);
            }
        }
        if let Some(curr) = event_hash {
            last_event_hash = Some(curr);
        }
    }
    Ok((entries, parse_errors, chain_breaks))
}


fn handle_raw_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args
        .first()
        .is_some_and(|sub| sub.eq_ignore_ascii_case("trace"))
    {
        let replay_path = replay_log_path_for_session(&app.state_root, &app.session_id);
        let sub = args.get(1).map(|s| s.trim().to_ascii_lowercase());
        match sub.as_deref() {
            None | Some("status") => {
                let session_runs = app
                    .run_runtime
                    .session_runs(&app.session_id, hermes_agent::SessionRunSelection::All)
                    .map(|runs| runs.len().to_string())
                    .unwrap_or_else(|error| format!("unavailable ({error})"));
                let export_dir = authoritative_run_export_dir(&app.state_root);
                emit_command_output(
                    app,
                    format!(
                        "Legacy replay diagnostics (JSONL / unverified): {}{}\nSession: {}\nLegacy JSONL path: {}\nAuthoritative complete run export directory: {}\nSession ledger runs: {}\nUsage: /raw trace [on|off|toggle|status|tail [N]|focus <trace-id> [N]|graph [N]|legacy-check|verify <PATH>|replay <PATH>|export [latest|all|RUN_ID]|path]",
                        if replay_enabled_runtime() { "ON" } else { "OFF" },
                        if replay_path.exists() { "" } else { " (no legacy log yet)" },
                        app.session_id,
                        replay_path.display(),
                        export_dir.display(),
                        session_runs
                    ),
                );
            }
            Some("path") => {
                emit_command_output(
                    app,
                    format!(
                        "Legacy replay diagnostic path (unverified): {}\nAuthoritative complete run export directory: {}",
                        replay_path.display(),
                        authoritative_run_export_dir(&app.state_root).display()
                    ),
                );
            }
            Some("tail") => {
                let limit = args
                    .get(2)
                    .and_then(|raw| raw.trim().parse::<usize>().ok())
                    .unwrap_or(20)
                    .clamp(1, 200);
                if !replay_path.exists() {
                    emit_command_output(
                        app,
                        format!(
                            "Legacy replay diagnostic log not found for current session yet: {}",
                            replay_path.display()
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
                let rendered = render_replay_trace_tail(&replay_path, limit)?;
                emit_command_output(app, rendered);
            }
            Some("focus") => {
                let Some(trace_id) = args.get(2).copied() else {
                    emit_command_output(app, "Usage: /raw trace focus <trace-id> [N]");
                    return Ok(CommandResult::Handled);
                };
                let limit = args
                    .get(3)
                    .and_then(|raw| raw.trim().parse::<usize>().ok())
                    .unwrap_or(150)
                    .clamp(1, 1000);
                if !replay_path.exists() {
                    emit_command_output(
                        app,
                        format!(
                            "Legacy replay diagnostic log not found for current session yet: {}",
                            replay_path.display()
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
                let rendered = render_replay_trace_focus(&replay_path, trace_id, limit)?;
                emit_command_output(app, rendered);
            }
            Some("graph") => {
                let limit = args
                    .get(2)
                    .and_then(|raw| raw.trim().parse::<usize>().ok())
                    .unwrap_or(80)
                    .clamp(1, 500);
                if !replay_path.exists() {
                    emit_command_output(
                        app,
                        format!(
                            "Legacy replay diagnostic log not found for current session yet: {}",
                            replay_path.display()
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
                let rendered = render_replay_trace_graph(&replay_path, limit)?;
                emit_command_output(app, rendered);
            }
            Some("verify") => {
                if args.len() != 3 {
                    emit_command_output(app, "Usage: /raw trace verify <PATH>");
                    return Ok(CommandResult::Handled);
                }
                let raw_path = args[2];
                if Path::new(raw_path)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"))
                {
                    emit_command_output(
                        app,
                        format!(
                            "Legacy v0 JSONL replay is refused\nintegrity_verified=false\nintegrity_authenticated=false\nevidence_complete=false\nlegacy_v0_unverified_non_executable=true\npath={}",
                            raw_path
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
                match verify_canonical_run_export(app, raw_path) {
                    Ok((export_path, verified)) => {
                        let evidence_complete = verified.evidence_complete();
                        emit_command_output(
                            app,
                            format!(
                                "Authoritative run export integrity summary\nintegrity_verified=true\nintegrity_authenticated=false\nevidence_complete={}\nlegacy_delivery_result_count={}\nrun_id={}\npath={}\ndigest={}\nevent_count={}\noutbox_count={}",
                                evidence_complete,
                                verified.legacy_delivery_result_count,
                                verified.run_id,
                                export_path.display(),
                                verified.digest,
                                verified.event_count,
                                verified.outbox_count
                            ),
                        );
                    }
                    Err(error) => emit_command_output(
                        app,
                        format!(
                            "Authoritative run export integrity summary rejected\nintegrity_verified=false\nintegrity_authenticated=false\nevidence_complete=false\npath={}\nreason={}",
                            raw_path, error
                        ),
                    ),
                }
            }
            Some("replay") => {
                if args.len() != 3 {
                    emit_command_output(app, "Usage: /raw trace replay <PATH>");
                    return Ok(CommandResult::Handled);
                }
                let raw_path = args[2];
                if Path::new(raw_path)
                    .extension()
                    .and_then(|extension| extension.to_str())
                    .is_some_and(|extension| extension.eq_ignore_ascii_case("jsonl"))
                {
                    emit_command_output(
                        app,
                        format!(
                            "Legacy v0 JSONL replay is refused\nintegrity_verified=false\nintegrity_authenticated=false\nevidence_complete=false\nlegacy_v0_unverified_non_executable=true\neffects_reconstructed_not_executed=false\nexact=false\npath={}",
                            raw_path
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }

                let export_path = match canonical_authoritative_run_export_path(app, raw_path) {
                    Ok(path) => path,
                    Err(error) => {
                        emit_command_output(
                            app,
                            format!(
                                "Authoritative run reconstruction refused\nintegrity_verified=false\nintegrity_authenticated=false\nevidence_complete=false\neffects_reconstructed_not_executed=false\nexact=false\npath={}\nreason={}",
                                raw_path, error
                            ),
                        );
                        return Ok(CommandResult::Handled);
                    }
                };
                let plan = match read_verified_run_replay_plan(&export_path) {
                    Ok(plan) => plan,
                    Err(hermes_agent::run_export::RunExportError::LegacyV0NotVerifiable) => {
                        emit_command_output(
                            app,
                            format!(
                                "Legacy v0 JSONL replay is refused\nintegrity_verified=false\nintegrity_authenticated=false\nevidence_complete=false\nlegacy_v0_unverified_non_executable=true\neffects_reconstructed_not_executed=false\nexact=false\npath={}",
                                export_path.display()
                            ),
                        );
                        return Ok(CommandResult::Handled);
                    }
                    Err(error) => {
                        emit_command_output(
                            app,
                            format!(
                                "Authoritative run reconstruction refused\nintegrity_verified=false\nintegrity_authenticated=false\nevidence_complete=false\neffects_reconstructed_not_executed=false\nexact=false\npath={}\nreason={}",
                                export_path.display(),
                                error
                            ),
                        );
                        return Ok(CommandResult::Handled);
                    }
                };
                let expected_filename = format!("{}.run.json", plan.summary().run_id);
                if export_path.file_name().and_then(|name| name.to_str())
                    != Some(expected_filename.as_str())
                {
                    emit_command_output(
                        app,
                        format!(
                            "Authoritative run reconstruction refused\nintegrity_verified=false\nintegrity_authenticated=false\nevidence_complete=false\neffects_reconstructed_not_executed=false\nexact=false\npath={}\nreason=canonical export filename does not bind verified run ID {}",
                            export_path.display(),
                            plan.summary().run_id
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }

                let reader = match app.run_runtime.bind_verified_replay_plan(&plan) {
                    Ok(reader) => reader,
                    Err(error) => {
                        emit_command_output(
                            app,
                            format!(
                                "Authoritative run reconstruction refused\nintegrity_verified=true\nintegrity_authenticated=false\nevidence_complete={}\neffects_reconstructed_not_executed=false\nexact=false\npath={}\nreason={}",
                                plan.summary().evidence_complete(),
                                export_path.display(),
                                error
                            ),
                        );
                        return Ok(CommandResult::Handled);
                    }
                };
                let reconstruction =
                    hermes_agent::deterministic_replay::reconstruct_verified_run(&plan, Some(&reader));
                match reconstruction {
                    Ok(report) => {
                        emit_command_output(
                            app,
                            render_verified_run_reconstruction(&export_path, &plan, &report),
                        );
                    }
                    Err(error) => emit_command_output(
                        app,
                        format!(
                            "Authoritative run reconstruction refused\nintegrity_verified=true\nintegrity_authenticated=false\nevidence_complete={}\neffects_reconstructed_not_executed=false\nexact=false\npath={}\nreason={}",
                            plan.summary().evidence_complete(),
                            export_path.display(),
                            error
                        ),
                    ),
                }
            }
            Some("legacy-check") => {
                if !replay_path.exists() {
                    emit_command_output(
                        app,
                        format!(
                            "Legacy replay diagnostic log not found for current session yet: {}",
                            replay_path.display()
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
                let (entries, parse_errors, chain_breaks) =
                    legacy_replay_trace_diagnostic(&replay_path)?;
                emit_command_output(
                    app,
                    format!(
                        "Legacy JSONL replay diagnostic (unverified/non-executable)\nintegrity_verified=false\nentries={}\nparse_errors={}\nchain_breaks={}\npath={}",
                        entries,
                        parse_errors,
                        chain_breaks,
                        replay_path.display()
                    ),
                );
            }
            Some("export") => {
                if args.len() > 3 {
                    emit_command_output(
                        app,
                        "Usage: /raw trace export [latest|all|RUN_ID]",
                    );
                    return Ok(CommandResult::Handled);
                }
                let selector = args.get(2).copied().unwrap_or("latest");
                let selection = match parse_run_export_selection(selector) {
                    Ok(selection) => selection,
                    Err(error) => {
                        emit_command_output(
                            app,
                            format!(
                                "Run export selector rejected: {}\nUsage: /raw trace export [latest|all|RUN_ID]",
                                error
                            ),
                        );
                        return Ok(CommandResult::Handled);
                    }
                };
                let scope = match selection {
                    RunExportSelection::Latest | RunExportSelection::All => {
                        "current_persisted_session"
                    }
                    RunExportSelection::Exact(_) => "exact_accessible_local_run",
                };
                let run_ids = match selected_run_ids_for_export(app, selection) {
                    Ok(run_ids) => run_ids,
                    Err(error) => {
                        emit_command_output(
                            app,
                            format!(
                                "Authoritative run export selection failed\nselector={}\nscope={}\nreason={}",
                                selector, scope, error
                            ),
                        );
                        return Ok(CommandResult::Handled);
                    }
                };
                if run_ids.is_empty() {
                    emit_command_output(
                        app,
                        format!(
                            "No ledger runs found for the current persisted session.\nselector={}\nscope={}\nauthoritative_export_dir={}",
                            selector,
                            scope,
                            authoritative_run_export_dir(&app.state_root).display()
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }

                let mut receipts = Vec::new();
                for run_id in run_ids {
                    match app.run_runtime.publish_run_export(run_id) {
                        Ok(receipt) => receipts.push(receipt),
                        Err(error) => {
                            let mut out =
                                String::from("Authoritative complete run export publication incomplete\n");
                            let _ = writeln!(out, "selector={selector}");
                            let _ = writeln!(out, "scope={scope}");
                            for receipt in &receipts {
                                let _ = writeln!(out, "run_id={}", receipt.run_id);
                                let _ = writeln!(out, "path={}", receipt.path.display());
                                let _ = writeln!(out, "digest={}", receipt.digest);
                                let _ =
                                    writeln!(out, "newly_published={}", receipt.published);
                            }
                            let _ = writeln!(out, "publication_complete=false");
                            let _ = writeln!(out, "failed_run_id={run_id}");
                            let _ = writeln!(out, "reason={error}");
                            emit_command_output(app, out.trim_end());
                            return Ok(CommandResult::Handled);
                        }
                    }
                }

                let mut out = String::from("Authoritative complete run export publication\n");
                let _ = writeln!(out, "selector={selector}");
                let _ = writeln!(out, "scope={scope}");
                for receipt in receipts {
                    let _ = writeln!(out, "run_id={}", receipt.run_id);
                    let _ = writeln!(out, "path={}", receipt.path.display());
                    let _ = writeln!(out, "digest={}", receipt.digest);
                    let _ = writeln!(out, "newly_published={}", receipt.published);
                }
                let _ = writeln!(out, "publication_complete=true");
                emit_command_output(app, out.trim_end());
            }
            Some("on") | Some("off") | Some("toggle") => {
                let next = match sub.as_deref().unwrap_or("status") {
                    "on" => true,
                    "off" => false,
                    "toggle" => !replay_enabled_runtime(),
                    _ => replay_enabled_runtime(),
                };
                std::env::set_var("HERMES_REPLAY_ENABLED", if next { "1" } else { "0" });
                emit_command_output(
                    app,
                    format!(
                        "Legacy replay trace diagnostic mode: {}.\nThis applies to new turns in the current process and remains unverified/non-executable.",
                        if next { "ON" } else { "OFF" }
                    ),
                );
            }
            Some("help") | Some("--help") | Some("-h") => emit_command_output(
                app,
                "Replay trace controls:\n  /raw trace status                         Show legacy JSONL diagnostics and authoritative export directory\n  /raw trace on|off|toggle                  Control legacy JSONL diagnostics for new turns (unverified)\n  /raw trace tail [N]                       Show bounded legacy diagnostic events (unverified)\n  /raw trace focus <id> [N]                 Filter bounded legacy diagnostic rows (unverified)\n  /raw trace graph [N]                      Show bounded legacy diagnostic lineage (unverified)\n  /raw trace legacy-check                   Count legacy JSONL parse/chain issues (unverified/non-executable)\n  /raw trace verify <PATH>                  Summarize integrity/evidence for a complete authoritative run export\n  /raw trace replay <PATH>                  Reconstruct a verified run without providers, tools, or effect execution\n  /raw trace export [latest|all|RUN_ID]     Publish complete exports from current persisted session or exact accessible run\n  /raw trace path                           Show diagnostic and authoritative paths",
            ),
            _ => emit_command_output(
                app,
                "Usage: /raw trace [on|off|toggle|status|tail [N]|focus <trace-id> [N]|graph [N]|legacy-check|verify <PATH>|replay <PATH>|export [latest|all|RUN_ID]|path]",
            ),
        }
        return Ok(CommandResult::Handled);
    }

    let state = app.tool_registry.raw_mode_state();
    let log_dir = app.tool_registry.rtk_log_dir();
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        emit_command_output(
            app,
            format!(
                "RTK raw mode: {}{}\nDual logs: {}\nLegacy replay diagnostics (unverified): {}\nUsage: /raw [on|off|toggle|once|status|trace]",
                if state.enabled { "ON" } else { "OFF" },
                if state.once_pending {
                    " (one-shot pending)"
                } else {
                    ""
                },
                log_dir.display(),
                if replay_enabled_runtime() { "ON" } else { "OFF" }
            ),
        );
        return Ok(CommandResult::Handled);
    }

    match args[0].trim().to_ascii_lowercase().as_str() {
        "help" => emit_command_output(
            app,
            "RTK raw controls:\n  /raw status        Show current mode + log path\n  /raw on            Disable output filtering for all tool calls\n  /raw off           Re-enable RTK output filtering\n  /raw toggle        Toggle global raw mode\n  /raw once          Raw pass-through for next tool call only\n  /raw trace ...     Legacy JSONL diagnostics (unverified) plus authoritative run export controls",
        ),
        "once" => {
            app.tool_registry.set_raw_mode_once();
            emit_command_output(
                app,
                "RTK raw mode armed for next tool call only. It auto-resets after one dispatch.",
            );
        }
        "on" | "off" | "toggle" | "true" | "false" | "yes" | "no" | "1" | "0" => {
            let next = match args[0].trim().to_ascii_lowercase().as_str() {
                "on" | "true" | "yes" | "1" => true,
                "off" | "false" | "no" | "0" => false,
                "toggle" => !state.enabled,
                _ => state.enabled,
            };
            app.tool_registry.set_raw_mode(next);
            std::env::set_var("HERMES_RTK_RAW", if next { "1" } else { "0" });
            emit_command_output(
                app,
                format!(
                    "RTK raw mode: {} (dual logs: {})",
                    if next { "ON" } else { "OFF" },
                    log_dir.display()
                ),
            );
        }
        _ => emit_command_output(app, "Usage: /raw [on|off|toggle|once|status|trace]"),
    }
    Ok(CommandResult::Handled)
}
