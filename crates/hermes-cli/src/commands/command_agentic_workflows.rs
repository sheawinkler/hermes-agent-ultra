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

include!("command_agentic_workflows/session_ops.rs");
