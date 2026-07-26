async fn handle_swarm_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .copied()
        .unwrap_or("status")
        .trim()
        .to_ascii_lowercase();

    match sub.as_str() {
        "status" => {
            let policy = load_quorum_policy()?;
            let runtime = swarm_runtime_status();
            let artifact_path = latest_quorum_artifact_path(app)
                .map(|p| p.display().to_string())
                .unwrap_or_else(|| "(none yet)".to_string());
            let mut out = String::new();
            let _ = writeln!(out, "Swarm runtime");
            let _ = writeln!(out, "engine={}", runtime.engine);
            let _ = writeln!(out, "feature_enabled={}", runtime.feature_enabled);
            let _ = writeln!(
                out,
                "supported_modes={}",
                runtime
                    .supported_modes
                    .iter()
                    .map(|m| m.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            let _ = writeln!(
                out,
                "quorum_policy=enabled:{} voters:{} models:{} armed_once:{}",
                policy.enabled,
                policy.voters,
                if policy.models.is_empty() {
                    "(current model)".to_string()
                } else {
                    policy.models.join(", ")
                },
                app.quorum_armed_once
            );
            let _ = writeln!(out, "latest_artifact={}", artifact_path);
            if !runtime.notes.is_empty() {
                let _ = writeln!(out, "notes:");
                for note in runtime.notes {
                    let _ = writeln!(out, "- {}", note);
                }
            }
            emit_command_output(app, out.trim_end());
        }
        "plan" => {
            let policy = load_quorum_policy()?;
            let mode = parse_swarm_mode(args.get(1).copied());
            let pass_cap = read_swarm_pass_cap();
            let models = if policy.models.is_empty() {
                vec![app.current_model.clone()]
            } else {
                policy.models.clone()
            };
            let plan = build_swarm_execution_plan(
                mode,
                policy.voters,
                models,
                app.session_objective.clone(),
                pass_cap,
            );
            let pretty = serde_json::to_string_pretty(&plan)
                .map_err(|e| AgentError::Config(format!("failed to render swarm plan: {e}")))?;
            emit_command_output(
                app,
                format!(
                    "Swarm execution plan\n{}\n\nUsage: /swarm run [passes] [mode]\nmode: concurrent|sequential|graph",
                    pretty
                ),
            );
        }
        "run" => {
            let pass_override = args
                .get(1)
                .and_then(|raw| raw.trim().parse::<usize>().ok())
                .map(|v| v.clamp(1, 64));
            let mode = if pass_override.is_some() {
                parse_swarm_mode(args.get(2).copied())
            } else {
                parse_swarm_mode(args.get(1).copied())
            };
            if let Some(passes) = pass_override {
                std::env::set_var("HERMES_QUORUM_VOTER_PASSES", passes.to_string());
            }
            let policy = load_quorum_policy()?;
            if !policy.enabled {
                emit_command_output(
                    app,
                    "Swarm run blocked: quorum policy is OFF.\nRun `/swarm on` (or `/quorum on`) first to keep cost explicit.",
                );
                return Ok(CommandResult::Handled);
            }
            install_quorum_system_hint(app, policy.voters, &policy.models);
            app.quorum_armed_once = true;
            emit_command_output(
                app,
                format!(
                    "Swarm run armed.\nmode={}\npass_cap={}\nnext user prompt will execute multi-voter fan-out + synthesis and persist an artifact.",
                    mode.as_str(),
                    read_swarm_pass_cap(),
                ),
            );
        }
        "cancel" => {
            app.quorum_armed_once = false;
            clear_quorum_system_hints(app);
            emit_command_output(app, "Swarm run canceled. Pending one-shot fan-out was disarmed.");
        }
        "artifact" => {
            let Some(path) = latest_quorum_artifact_path(app) else {
                emit_command_output(app, "No swarm/quorum artifact exists yet for this runtime.");
                return Ok(CommandResult::Handled);
            };
            let summary = std::fs::read_to_string(&path)
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .map(|v| {
                    let session_id = v
                        .get("session_id")
                        .and_then(|x| x.as_str())
                        .unwrap_or("unknown");
                    let saved_at = v
                        .get("saved_at")
                        .and_then(|x| x.as_str())
                        .unwrap_or("unknown");
                    let voters = v
                        .get("voters")
                        .and_then(|x| x.as_array())
                        .map(|arr| arr.len())
                        .unwrap_or(0);
                    format!("session_id={session_id}\nsaved_at={saved_at}\nvoters={voters}")
                })
                .unwrap_or_else(|| "(unable to parse artifact summary)".to_string());
            emit_command_output(
                app,
                format!("Latest swarm artifact\npath={}\n{}", path.display(), summary),
            );
        }
        "on" | "off" | "enable" | "disable" | "true" | "false" | "1" | "0" | "voters"
        | "models" => return handle_quorum_command(app, args).await,
        _ => emit_command_output(
            app,
            "Usage: /swarm [status|plan [mode]|run [passes] [mode]|cancel|artifact|on|off|voters <2..8>|models <a,b,c>]",
        ),
    }
    Ok(CommandResult::Handled)
}

fn specpatch_block_reason(command: &str) -> Option<&'static str> {
    let lower = command.to_ascii_lowercase();
    if lower.contains("rm -rf /")
        || lower.contains("dd if=")
        || lower.contains("mkfs")
        || lower.contains("shutdown")
    {
        return Some("destructive command pattern");
    }
    if lower.contains("git reset --hard") || lower.contains("git clean -fdx") {
        return Some("history/destructive git command pattern");
    }
    None
}

fn slash_command_payload_from_history(app: &App, cmd: &str, args: &[&str]) -> String {
    let fallback = args.join(" ");
    let Some(last) = app.input_history.last() else {
        return fallback;
    };
    if let Some(raw) = last.strip_prefix(cmd) {
        return raw.trim().to_string();
    }
    fallback
}

async fn run_shell_capture(command: &str) -> Result<(i32, String, String), AgentError> {
    let output = tokio::process::Command::new("bash")
        .arg("-lc")
        .arg(command)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .suppress_windows_console()
        .output()
        .await
        .map_err(|e| AgentError::Io(format!("shell command failed: {}", e)))?;
    let code = output.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Ok((code, stdout, stderr))
}

async fn handle_specpatch_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let payload = slash_command_payload_from_history(app, "/specpatch", args);
    if payload.is_empty() {
        emit_command_output(
            app,
            "Usage: /specpatch <verify_cmd> | <candidate_cmd_1> | <candidate_cmd_2> ...",
        );
        return Ok(CommandResult::Handled);
    }
    let segments: Vec<String> = payload
        .split('|')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    if segments.len() < 2 {
        emit_command_output(
            app,
            "Need at least a verify command and one candidate.\nExample: /specpatch \"cargo test -p hermes-cli\" | \"git apply fix.patch\"",
        );
        return Ok(CommandResult::Handled);
    }
    let verify_cmd = segments[0].clone();
    let candidates = &segments[1..];

    if let Some(reason) = specpatch_block_reason(&verify_cmd) {
        emit_command_output(app, format!("specpatch blocked verify_cmd: {}", reason));
        return Ok(CommandResult::Handled);
    }

    let mut out = String::new();
    out.push_str("SpecPatch executor\n");
    out.push_str("------------------\n");
    let _ = writeln!(out, "verify_cmd: {}", verify_cmd);

    let mut winner: Option<String> = None;
    for (idx, candidate) in candidates.iter().enumerate() {
        if let Some(reason) = specpatch_block_reason(candidate) {
            let _ = writeln!(
                out,
                "[{}] blocked candidate: {} ({})",
                idx + 1,
                candidate,
                reason
            );
            continue;
        }
        let _ = writeln!(out, "[{}] candidate: {}", idx + 1, candidate);
        let (code, stdout, stderr) = run_shell_capture(candidate).await?;
        let _ = writeln!(out, "    apply_exit={}", code);
        if !stdout.is_empty() {
            let _ = writeln!(
                out,
                "    apply_stdout={}",
                stdout.lines().next().unwrap_or("")
            );
        }
        if !stderr.is_empty() {
            let _ = writeln!(
                out,
                "    apply_stderr={}",
                stderr.lines().next().unwrap_or("")
            );
        }
        let (v_code, v_stdout, v_stderr) = run_shell_capture(&verify_cmd).await?;
        let _ = writeln!(out, "    verify_exit={}", v_code);
        if !v_stdout.is_empty() {
            let _ = writeln!(
                out,
                "    verify_stdout={}",
                v_stdout.lines().next().unwrap_or("")
            );
        }
        if !v_stderr.is_empty() {
            let _ = writeln!(
                out,
                "    verify_stderr={}",
                v_stderr.lines().next().unwrap_or("")
            );
        }
        if v_code == 0 {
            winner = Some(candidate.clone());
            break;
        }
    }

    if let Some(chosen) = winner {
        let _ = writeln!(out, "\nwinner={}", chosen);
    } else {
        out.push_str("\nNo candidate passed verify command.\n");
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn objective_runtime_ledger_path() -> PathBuf {
    hermes_config::hermes_home()
        .join("alpha")
        .join("objective_runtime_ledger.jsonl")
}

fn normalize_repo_relative_path(repo_root: &Path, raw: &str) -> Option<String> {
    let trimmed = raw
        .trim()
        .trim_matches('"')
        .trim_matches('\'')
        .trim_matches('`')
        .trim_matches(',');
    if trimmed.is_empty() {
        return None;
    }
    let path = PathBuf::from(trimmed);
    if path.is_absolute() {
        let rel = path.strip_prefix(repo_root).ok()?;
        return Some(rel.display().to_string());
    }
    Some(path.display().to_string())
}

fn extract_marker_paths(text: &str) -> Vec<String> {
    let Ok(re) = Regex::new(r"(?:path|file)=([^\s\],;]+)") else {
        return Vec::new();
    };
    re.captures_iter(text)
        .filter_map(|caps| caps.get(1).map(|m| m.as_str().to_string()))
        .collect()
}

async fn count_git_tracked_files(repo_root: &Path) -> Result<usize, AgentError> {
    let output = tokio::process::Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("ls-files")
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .suppress_windows_console()
        .output()
        .await
        .map_err(|e| AgentError::Io(format!("git ls-files failed: {}", e)))?;
    if !output.status.success() {
        return Ok(0);
    }
    let count = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|line| !line.trim().is_empty())
        .count();
    Ok(count)
}

async fn handle_heatmap_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let repo_root = if let Some(path) = args.first() {
        PathBuf::from(path)
    } else if let Some(root) = discover_repo_root_for_about() {
        root
    } else {
        std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    };
    if !repo_root.exists() {
        emit_command_output(
            app,
            format!("Repo path does not exist: {}", repo_root.display()),
        );
        return Ok(CommandResult::Handled);
    }

    let mut counts: HashMap<String, u64> = HashMap::new();
    let ledger_path = objective_runtime_ledger_path();
    if ledger_path.exists() {
        let raw = std::fs::read_to_string(&ledger_path).unwrap_or_default();
        for line in raw.lines() {
            let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
                continue;
            };
            if let Some(files) = value.get("evidence_files").and_then(|v| v.as_array()) {
                for raw_path in files.iter().filter_map(|v| v.as_str()) {
                    if let Some(path) = normalize_repo_relative_path(&repo_root, raw_path) {
                        *counts.entry(path).or_insert(0) += 1;
                    }
                }
            }
        }
    }
    for msg in &app.messages {
        if let Some(content) = msg.content.as_deref() {
            for raw_path in extract_marker_paths(content) {
                if let Some(path) = normalize_repo_relative_path(&repo_root, &raw_path) {
                    *counts.entry(path).or_insert(0) += 1;
                }
            }
        }
    }

    let tracked = count_git_tracked_files(&repo_root).await?;
    let mut rows: Vec<(String, u64, bool)> = counts
        .into_iter()
        .map(|(path, hits)| {
            let exists = repo_root.join(&path).exists();
            (path, hits, exists)
        })
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let verified_existing = rows.iter().filter(|(_, _, exists)| *exists).count();
    let coverage_pct = if tracked == 0 {
        0.0
    } else {
        (verified_existing as f64 / tracked as f64) * 100.0
    };

    let mut out = String::new();
    out.push_str("Context heatmap\n");
    out.push_str("---------------\n");
    let _ = writeln!(out, "repo_root={}", repo_root.display());
    let _ = writeln!(out, "tracked_files={}", tracked);
    let _ = writeln!(out, "observed_paths={}", rows.len());
    let _ = writeln!(
        out,
        "verified_existing_paths={} ({:.2}% coverage of tracked files)",
        verified_existing, coverage_pct
    );
    for (path, hits, exists) in rows.iter().take(30) {
        let _ = writeln!(out, "- hits={:<4} exists={} path={}", hits, exists, path);
    }
    if rows.is_empty() {
        out.push_str("- no evidence paths recorded yet\n");
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

const STUDIO_REPLAY_USAGE: &str =
    "Usage: /studio replay [status|verify <PATH>|diff <EXPORT_A> <EXPORT_B>]";

fn canonical_authoritative_run_export_path(app: &App, raw: &str) -> Result<PathBuf, AgentError> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err(AgentError::Config(
            "authoritative export path must not be empty".to_string(),
        ));
    }
    let export_dir = authoritative_run_export_dir(&app.state_root);
    let canonical_export_dir = std::fs::canonicalize(&export_dir).map_err(|error| {
        AgentError::Io(format!(
            "could not resolve authoritative export directory {}: {}",
            export_dir.display(),
            error
        ))
    })?;
    let supplied = PathBuf::from(raw);
    let candidate = if supplied.is_absolute() {
        supplied
    } else {
        std::fs::canonicalize(&supplied)
            .unwrap_or_else(|_| canonical_export_dir.join(supplied))
    };
    let canonical = std::fs::canonicalize(&candidate).map_err(|error| {
        AgentError::Io(format!(
            "could not resolve authoritative export path {}: {}",
            candidate.display(),
            error
        ))
    })?;
    if canonical.parent() != Some(canonical_export_dir.as_path())
        || !canonical
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".run.json"))
    {
        return Err(AgentError::Config(format!(
            "export path must be a canonical run-record-v1 export under {}",
            canonical_export_dir.display()
        )));
    }
    Ok(canonical)
}

fn verify_canonical_run_export(
    app: &App,
    raw: &str,
) -> Result<(PathBuf, hermes_agent::run_export::VerifiedRunExport), AgentError> {
    let path = canonical_authoritative_run_export_path(app, raw)?;
    let verified = hermes_agent::run_export::verify_run_export(&path).map_err(|error| {
        AgentError::Config(format!(
            "authoritative run export verification rejected {}: {}",
            path.display(),
            error
        ))
    })?;
    let expected_filename = format!("{}.run.json", verified.run_id);
    if path.file_name().and_then(|name| name.to_str()) != Some(expected_filename.as_str()) {
        return Err(AgentError::Config(format!(
            "canonical export filename does not bind verified run ID {}",
            verified.run_id
        )));
    }
    Ok((path, verified))
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn verified_run_export_event_hashes(
    path: &Path,
    verified: &hermes_agent::run_export::VerifiedRunExport,
) -> Result<HashSet<String>, AgentError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|error| AgentError::Io(format!("Failed to read {}: {}", path.display(), error)))?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|error| {
        AgentError::Config(format!(
            "verified export changed before diff parsing {}: {}",
            path.display(),
            error
        ))
    })?;
    let expected_run_id = verified.run_id.to_string();
    if parsed.get("complete").and_then(|value| value.as_bool()) != Some(true)
        || parsed
            .pointer("/header/run_id")
            .and_then(|value| value.as_str())
            != Some(expected_run_id.as_str())
        || parsed
            .pointer("/digest/value")
            .and_then(|value| value.as_str())
            != Some(verified.digest.as_str())
        || parsed.get("total_event_count").and_then(|value| value.as_u64())
            != Some(verified.event_count)
    {
        return Err(AgentError::Config(format!(
            "verified export metadata changed before diff parsing: {}",
            path.display()
        )));
    }
    let events = parsed
        .get("events")
        .and_then(|value| value.as_array())
        .ok_or_else(|| {
            AgentError::Config(format!(
                "verified export has no complete event array: {}",
                path.display()
            ))
        })?;
    if events.is_empty()
        || u64::try_from(events.len()).ok() != Some(verified.event_count)
    {
        return Err(AgentError::Config(format!(
            "verified export event count is empty or inconsistent: {}",
            path.display()
        )));
    }
    let mut hashes = HashSet::with_capacity(events.len());
    for event in events {
        let hash = event.get("event_hash").and_then(|value| value.as_str()).ok_or_else(|| {
            AgentError::Config(format!(
                "verified export event lacks an event hash: {}",
                path.display()
            ))
        })?;
        if !is_sha256_hex(hash) || !hashes.insert(hash.to_string()) {
            return Err(AgentError::Config(format!(
                "verified export event hashes are incomplete or duplicated: {}",
                path.display()
            )));
        }
    }
    Ok(hashes)
}

async fn handle_studio_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        emit_command_output(app, STUDIO_REPLAY_USAGE);
        return Ok(CommandResult::Handled);
    }
    let section = args[0].trim().to_ascii_lowercase();
    if section != "replay" {
        emit_command_output(app, STUDIO_REPLAY_USAGE);
        return Ok(CommandResult::Handled);
    }
    let action = args
        .get(1)
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "status".to_string());
    match action.as_str() {
        "status" => {
            let replay_path = replay_log_path_for_session(&app.state_root, &app.session_id);
            let export_dir = authoritative_run_export_dir(&app.state_root);
            let session_runs = app
                .run_runtime
                .session_runs(&app.session_id, hermes_agent::SessionRunSelection::All)
                .map(|runs| runs.len().to_string())
                .unwrap_or_else(|error| format!("unavailable ({error})"));
            emit_command_output(
                app,
                format!(
                    "Replay studio status\nsession={}\nauthoritative_export_dir={}\nsession_ledger_runs={}\nlegacy_jsonl_diagnostic_path={}\nlegacy_jsonl_exists={}\nlegacy_jsonl_proof=false",
                    app.session_id,
                    export_dir.display(),
                    session_runs,
                    replay_path.display(),
                    replay_path.exists()
                ),
            );
        }
        "verify" => {
            if args.len() != 3 {
                emit_command_output(app, "Usage: /studio replay verify <PATH>");
                return Ok(CommandResult::Handled);
            }
            let raw_path = args[2];
            match verify_canonical_run_export(app, raw_path) {
                Ok((path, verified)) => {
                    let evidence_complete = verified.evidence_complete();
                    emit_command_output(
                        app,
                        format!(
                            "Authoritative run export verification\nstructural_verification=true\nproof={}\nevidence_complete={}\nlegacy_delivery_result_count={}\nrun_id={}\npath={}\ndigest={}\nevent_count={}\noutbox_count={}",
                            evidence_complete,
                            evidence_complete,
                            verified.legacy_delivery_result_count,
                            verified.run_id,
                            path.display(),
                            verified.digest,
                            verified.event_count,
                            verified.outbox_count
                        ),
                    );
                }
                Err(error) => emit_command_output(
                    app,
                    format!(
                        "Authoritative run export verification rejected\nproof=false\npath={}\nreason={}",
                        raw_path, error
                    ),
                ),
            }
        }
        "diff" => {
            if args.len() != 4 {
                emit_command_output(app, "Usage: /studio replay diff <EXPORT_A> <EXPORT_B>");
                return Ok(CommandResult::Handled);
            }
            let raw_a = args[2];
            let raw_b = args[3];
            let (a_path, a_verified) = match verify_canonical_run_export(app, raw_a) {
                Ok(verified) => verified,
                Err(error) => {
                    emit_command_output(
                        app,
                        format!(
                            "Replay diff refused\nproof=false\ninput=A\npath={}\nreason={}",
                            raw_a, error
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
            };
            let (b_path, b_verified) = match verify_canonical_run_export(app, raw_b) {
                Ok(verified) => verified,
                Err(error) => {
                    emit_command_output(
                        app,
                        format!(
                            "Replay diff refused\nproof=false\ninput=B\npath={}\nreason={}",
                            raw_b, error
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
            };

            let a_hashes = match verified_run_export_event_hashes(&a_path, &a_verified) {
                Ok(hashes) => hashes,
                Err(error) => {
                    emit_command_output(
                        app,
                        format!(
                            "Replay diff refused\nproof=false\ninput=A\npath={}\nreason={}",
                            a_path.display(),
                            error
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
            };
            let b_hashes = match verified_run_export_event_hashes(&b_path, &b_verified) {
                Ok(hashes) => hashes,
                Err(error) => {
                    emit_command_output(
                        app,
                        format!(
                            "Replay diff refused\nproof=false\ninput=B\npath={}\nreason={}",
                            b_path.display(),
                            error
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
            };

            let a_reverified = match hermes_agent::run_export::verify_run_export(&a_path) {
                Ok(verified) if verified == a_verified => verified,
                Ok(_) => {
                    emit_command_output(
                        app,
                        format!(
                            "Replay diff refused\nproof=false\ninput=A\npath={}\nreason=export changed during diff",
                            a_path.display()
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
                Err(error) => {
                    emit_command_output(
                        app,
                        format!(
                            "Replay diff refused\nproof=false\ninput=A\npath={}\nreason=export no longer verifies: {}",
                            a_path.display(),
                            error
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
            };
            let b_reverified = match hermes_agent::run_export::verify_run_export(&b_path) {
                Ok(verified) if verified == b_verified => verified,
                Ok(_) => {
                    emit_command_output(
                        app,
                        format!(
                            "Replay diff refused\nproof=false\ninput=B\npath={}\nreason=export changed during diff",
                            b_path.display()
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
                Err(error) => {
                    emit_command_output(
                        app,
                        format!(
                            "Replay diff refused\nproof=false\ninput=B\npath={}\nreason=export no longer verifies: {}",
                            b_path.display(),
                            error
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
            };

            let only_a = a_hashes.difference(&b_hashes).count();
            let only_b = b_hashes.difference(&a_hashes).count();
            let overlap = a_hashes.intersection(&b_hashes).count();
            let evidence_complete =
                a_reverified.evidence_complete() && b_reverified.evidence_complete();
            emit_command_output(
                app,
                format!(
                    "Authoritative run export diff\nstructural_verification=verified\nproof={}\nA run_id={} digest={} event_count={} unique_event_hashes={} evidence_complete={} legacy_delivery_result_count={} path={}\nB run_id={} digest={} event_count={} unique_event_hashes={} evidence_complete={} legacy_delivery_result_count={} path={}\noverlap_hashes={}\nonly_in_a={}\nonly_in_b={}",
                    evidence_complete,
                    a_reverified.run_id,
                    a_reverified.digest,
                    a_reverified.event_count,
                    a_hashes.len(),
                    a_reverified.evidence_complete(),
                    a_reverified.legacy_delivery_result_count,
                    a_path.display(),
                    b_reverified.run_id,
                    b_reverified.digest,
                    b_reverified.event_count,
                    b_hashes.len(),
                    b_reverified.evidence_complete(),
                    b_reverified.legacy_delivery_result_count,
                    b_path.display(),
                    overlap,
                    only_a,
                    only_b
                ),
            );
        }
        _ => emit_command_output(app, STUDIO_REPLAY_USAGE),
    }
    Ok(CommandResult::Handled)
}
