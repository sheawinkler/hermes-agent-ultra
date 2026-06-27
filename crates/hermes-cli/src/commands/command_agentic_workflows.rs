fn handle_claims_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .copied()
        .unwrap_or("status")
        .trim()
        .to_ascii_lowercase();
    match sub.as_str() {
        "status" => {
            let policy = load_claim_verifier_policy()?;
            emit_command_output(
                app,
                format!(
                    "Claim verifier policy\nenabled={}\nrequired={}\nmax_retries={}\nupdated_at={}\n\nWhen enabled, repo-review finalization enforces verified evidence tags before completion claims.",
                    policy.enabled, policy.required, policy.max_retries, policy.updated_at
                ),
            );
        }
        "on" | "enable" | "true" | "1" => {
            let policy = set_claim_verifier_enabled(true)?;
            std::env::set_var("HERMES_CLAIM_VERIFIER_ENABLED", "1");
            emit_command_output(
                app,
                format!(
                    "Claim verifier enabled.\nrequired={}\nmax_retries={}",
                    policy.required, policy.max_retries
                ),
            );
        }
        "off" | "disable" | "false" | "0" => {
            let policy = set_claim_verifier_enabled(false)?;
            std::env::set_var("HERMES_CLAIM_VERIFIER_ENABLED", "0");
            emit_command_output(
                app,
                format!(
                    "Claim verifier disabled.\nrequired={}\nmax_retries={}",
                    policy.required, policy.max_retries
                ),
            );
        }
        _ => emit_command_output(app, "Usage: /claims [status|on|off]"),
    }
    Ok(CommandResult::Handled)
}

fn clear_quorum_system_hints(app: &mut App) {
    app.messages.retain(|m| {
        if m.role != hermes_core::MessageRole::System {
            return true;
        }
        !m.content
            .as_deref()
            .unwrap_or_default()
            .starts_with("[QUORUM_MODE] ")
    });
}

fn install_quorum_system_hint(app: &mut App, voters: usize, models: &[String]) {
    clear_quorum_system_hints(app);
    let model_hint = if models.is_empty() {
        "current-model-only".to_string()
    } else {
        models.join(", ")
    };
    app.messages.push(hermes_core::Message::system(format!(
        "[QUORUM_MODE] Quorum reasoning is enabled. For complex decisions, evaluate at least {} independent hypotheses and present: (1) strongest case, (2) strongest counter-case, (3) final synthesis with explicit confidence. Preferred voter models: {}.",
        voters, model_hint
    )));
}

async fn handle_quorum_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .copied()
        .unwrap_or("status")
        .trim()
        .to_ascii_lowercase();
    match sub.as_str() {
        "status" => {
            let policy = load_quorum_policy()?;
            emit_command_output(
                app,
                format!(
                    "Quorum policy\nenabled={}\nmode={}\nvoters={}\nmodels={}\narmed_once={}\nupdated_at={}\n\nQuorum is optional and off by default to control token cost.",
                    policy.enabled,
                    policy.mode,
                    policy.voters,
                    if policy.models.is_empty() {
                        "(none)".to_string()
                    } else {
                        policy.models.join(", ")
                    },
                    app.quorum_armed_once,
                    policy.updated_at
                ),
            );
        }
        "on" | "enable" | "true" | "1" => {
            let policy = set_quorum_policy(true, None, None)?;
            std::env::set_var("HERMES_QUORUM_ENABLED", "1");
            install_quorum_system_hint(app, policy.voters, &policy.models);
            app.quorum_armed_once = false;
            emit_command_output(
                app,
                format!(
                    "Quorum mode enabled (optional deep reasoning).\nvoters={}\nmodels={}",
                    policy.voters,
                    if policy.models.is_empty() {
                        "(current model)".to_string()
                    } else {
                        policy.models.join(", ")
                    }
                ),
            );
        }
        "off" | "disable" | "false" | "0" => {
            let policy = set_quorum_policy(false, None, None)?;
            std::env::set_var("HERMES_QUORUM_ENABLED", "0");
            clear_quorum_system_hints(app);
            app.quorum_armed_once = false;
            emit_command_output(
                app,
                format!(
                    "Quorum mode disabled.\nvoters={}\nmodels={}",
                    policy.voters,
                    if policy.models.is_empty() {
                        "(none)".to_string()
                    } else {
                        policy.models.join(", ")
                    }
                ),
            );
        }
        "voters" => {
            let Some(raw) = args.get(1) else {
                emit_command_output(app, "Usage: /quorum voters <2..8>");
                return Ok(CommandResult::Handled);
            };
            let voters = raw.parse::<usize>().ok().unwrap_or(3).clamp(2, 8);
            let current = load_quorum_policy()?;
            let policy = set_quorum_policy(current.enabled, Some(voters), None)?;
            if policy.enabled {
                install_quorum_system_hint(app, policy.voters, &policy.models);
            }
            emit_command_output(app, format!("Quorum voters updated to {}.", policy.voters));
        }
        "models" => {
            if args.len() < 2 {
                emit_command_output(
                    app,
                    "Usage: /quorum models <provider:model[,provider:model,...]>",
                );
                return Ok(CommandResult::Handled);
            }
            let joined = args[1..].join(" ");
            let parsed: Vec<String> = joined
                .split(',')
                .map(|m| m.trim().to_string())
                .filter(|m| !m.is_empty())
                .collect();
            let (default_provider, _) = split_provider_model(&app.current_model);
            let default_provider = default_provider.trim().to_ascii_lowercase();
            let mut models: Vec<String> = Vec::new();
            let mut notes: Vec<String> = Vec::new();
            for raw in parsed {
                let normalized = if raw.contains(':') {
                    normalize_provider_model(raw.as_str())?
                } else {
                    normalize_provider_model(format!("{}:{}", default_provider, raw).as_str())?
                };
                let (provider, model_id) = split_provider_model(&normalized);
                let provider = provider.trim().to_ascii_lowercase();
                let model_id = model_id.trim();
                if provider.is_empty() || model_id.is_empty() {
                    continue;
                }
                let mut final_model = normalized.clone();
                let catalog = provider_model_ids(&provider).await;
                if !catalog.is_empty() {
                    if let Some(candidate) = resolve_catalog_model_candidate(model_id, &catalog) {
                        final_model = format!("{}:{}", provider, candidate.trim());
                        if !final_model.eq_ignore_ascii_case(&normalized) {
                            notes.push(format!("{} -> {}", normalized, final_model));
                        }
                    } else if let Some(fallback) = catalog.first() {
                        let close = rank_catalog_model_candidates(model_id, &catalog, 3);
                        final_model = format!("{}:{}", provider, fallback.trim());
                        notes.push(format!(
                            "{} -> {} (close: {})",
                            normalized,
                            final_model,
                            if close.is_empty() {
                                "(none)".to_string()
                            } else {
                                close.join(", ")
                            }
                        ));
                    }
                }
                if !models
                    .iter()
                    .any(|existing| existing.eq_ignore_ascii_case(&final_model))
                {
                    models.push(final_model);
                }
            }
            let current = load_quorum_policy()?;
            let policy = set_quorum_policy(current.enabled, None, Some(models))?;
            if policy.enabled {
                install_quorum_system_hint(app, policy.voters, &policy.models);
            }
            emit_command_output(
                app,
                if notes.is_empty() {
                    format!(
                        "Quorum models updated: {}",
                        if policy.models.is_empty() {
                            "(none)".to_string()
                        } else {
                            policy.models.join(", ")
                        }
                    )
                } else {
                    format!(
                        "Quorum models updated: {}\nCatalog remaps: {}",
                        if policy.models.is_empty() {
                            "(none)".to_string()
                        } else {
                            policy.models.join(", ")
                        },
                        notes.join(" | ")
                    )
                },
            );
        }
        "run" => {
            let policy = load_quorum_policy()?;
            if !policy.enabled {
                emit_command_output(
                    app,
                    "Quorum mode is OFF. Run `/quorum on` first (kept optional to control token cost).",
                );
                return Ok(CommandResult::Handled);
            }
            install_quorum_system_hint(app, policy.voters, &policy.models);
            app.quorum_armed_once = true;
            emit_command_output(
                app,
                "Quorum deep-reasoning armed for subsequent turns.\nNext user prompt will run multi-voter fan-out across configured models and return synthesis (plus persisted quorum artifact).",
            );
        }
        _ => emit_command_output(
            app,
            "Usage: /quorum [status|on|off|voters <2..8>|models <a,b,c>|run]",
        ),
    }
    Ok(CommandResult::Handled)
}

fn parse_swarm_mode(input: Option<&str>) -> SwarmExecutionMode {
    match input
        .unwrap_or("concurrent")
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "sequential" | "sequence" => SwarmExecutionMode::Sequential,
        "graph" | "dag" => SwarmExecutionMode::Graph,
        _ => SwarmExecutionMode::Concurrent,
    }
}

fn read_swarm_pass_cap() -> usize {
    let raw = std::env::var("HERMES_QUORUM_VOTER_PASSES").unwrap_or_else(|_| "6".to_string());
    let normalized = raw.trim().to_ascii_lowercase();
    if matches!(normalized.as_str(), "0" | "off" | "unlimited" | "infinite") {
        return 64;
    }
    normalized.parse::<usize>().ok().unwrap_or(6).clamp(1, 64)
}

fn latest_quorum_artifact_path(app: &App) -> Option<PathBuf> {
    let dir = app.state_root.join("quorum");
    let entries = std::fs::read_dir(&dir).ok()?;
    let mut best_session: Option<(SystemTime, PathBuf)> = None;
    let mut best_any: Option<(SystemTime, PathBuf)> = None;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let modified = entry
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .unwrap_or(SystemTime::UNIX_EPOCH);
        if let Some((best_time, _)) = &best_any {
            if modified > *best_time {
                best_any = Some((modified, path.clone()));
            }
        } else {
            best_any = Some((modified, path.clone()));
        }

        let file_name = path
            .file_name()
            .and_then(|v| v.to_str())
            .unwrap_or_default();
        if !file_name.starts_with(&format!("{}-", app.session_id)) {
            continue;
        }
        if let Some((best_time, _)) = &best_session {
            if modified > *best_time {
                best_session = Some((modified, path.clone()));
            }
        } else {
            best_session = Some((modified, path.clone()));
        }
    }
    best_session.or(best_any).map(|(_, path)| path)
}

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

fn read_replay_export_rows(path: &Path) -> Result<Vec<serde_json::Value>, AgentError> {
    let raw = std::fs::read_to_string(path)
        .map_err(|e| AgentError::Io(format!("Failed to read {}: {}", path.display(), e)))?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        AgentError::Config(format!(
            "Failed to parse replay export {}: {}",
            path.display(),
            e
        ))
    })?;
    Ok(parsed
        .get("rows")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default())
}

async fn handle_studio_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        emit_command_output(
            app,
            "Usage: /studio replay [status|verify [path]|diff <export_a.json> <export_b.json>]",
        );
        return Ok(CommandResult::Handled);
    }
    let section = args[0].trim().to_ascii_lowercase();
    if section != "replay" {
        emit_command_output(
            app,
            "Usage: /studio replay [status|verify [path]|diff <export_a.json> <export_b.json>]",
        );
        return Ok(CommandResult::Handled);
    }
    let action = args
        .get(1)
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "status".to_string());
    match action.as_str() {
        "status" => {
            let replay_path = replay_log_path_for_session(&app.session_id);
            let export_dir = hermes_config::hermes_home()
                .join("logs")
                .join("replay")
                .join("exports");
            emit_command_output(
                app,
                format!(
                    "Replay studio status\nsession={}\nreplay_log={}\nreplay_exists={}\nexport_dir={}",
                    app.session_id,
                    replay_path.display(),
                    replay_path.exists(),
                    export_dir.display()
                ),
            );
        }
        "verify" => {
            let replay_path = args
                .get(2)
                .map(PathBuf::from)
                .unwrap_or_else(|| replay_log_path_for_session(&app.session_id));
            if !replay_path.exists() {
                emit_command_output(
                    app,
                    format!("Replay file not found: {}", replay_path.display()),
                );
                return Ok(CommandResult::Handled);
            }
            if replay_path
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("json"))
            {
                let rows = read_replay_export_rows(&replay_path)?;
                emit_command_output(
                    app,
                    format!(
                        "Replay export verification\npath={}\nrows={}\nstatus={}",
                        replay_path.display(),
                        rows.len(),
                        if rows.is_empty() { "empty" } else { "ok" }
                    ),
                );
            } else {
                let (entries, parse_errors, chain_breaks) = replay_trace_integrity(&replay_path)?;
                emit_command_output(
                    app,
                    format!(
                        "Replay log verification\npath={}\nentries={}\nparse_errors={}\nchain_breaks={}\nstatus={}",
                        replay_path.display(),
                        entries,
                        parse_errors,
                        chain_breaks,
                        if parse_errors == 0 && chain_breaks == 0 {
                            "pass"
                        } else {
                            "fail"
                        }
                    ),
                );
            }
        }
        "diff" => {
            if args.len() < 4 {
                emit_command_output(
                    app,
                    "Usage: /studio replay diff <export_a.json> <export_b.json>",
                );
                return Ok(CommandResult::Handled);
            }
            let a = PathBuf::from(args[2]);
            let b = PathBuf::from(args[3]);
            let a_rows = read_replay_export_rows(&a)?;
            let b_rows = read_replay_export_rows(&b)?;
            let a_hashes: HashSet<String> = a_rows
                .iter()
                .filter_map(|row| {
                    row.get("event_hash")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .collect();
            let b_hashes: HashSet<String> = b_rows
                .iter()
                .filter_map(|row| {
                    row.get("event_hash")
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string())
                })
                .collect();
            let only_a = a_hashes.difference(&b_hashes).count();
            let only_b = b_hashes.difference(&a_hashes).count();
            let overlap = a_hashes.intersection(&b_hashes).count();
            emit_command_output(
                app,
                format!(
                    "Replay diff\nA={} rows={} hashes={}\nB={} rows={} hashes={}\noverlap_hashes={}\nonly_in_a={}\nonly_in_b={}",
                    a.display(),
                    a_rows.len(),
                    a_hashes.len(),
                    b.display(),
                    b_rows.len(),
                    b_hashes.len(),
                    overlap,
                    only_a,
                    only_b
                ),
            );
        }
        _ => emit_command_output(
            app,
            "Usage: /studio replay [status|verify [path]|diff <export_a.json> <export_b.json>]",
        ),
    }
    Ok(CommandResult::Handled)
}

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
