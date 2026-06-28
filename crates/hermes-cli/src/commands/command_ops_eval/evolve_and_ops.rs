include!("evolve_and_ops/self_evolution_dashboard.rs");

async fn handle_dashboard_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    let mut params = serde_json::json!({
        "action": action
    });
    if let Some(host) = args.get(1) {
        params["host"] = serde_json::Value::String((*host).to_string());
    }
    if let Some(port) = args.get(2).and_then(|raw| raw.parse::<u16>().ok()) {
        params["port"] = serde_json::json!(port);
    }
    if args
        .iter()
        .any(|arg| arg.eq_ignore_ascii_case("--insecure"))
    {
        params["insecure"] = serde_json::json!(true);
    }

    let raw = app
        .tool_registry
        .dispatch_async("dashboard_control", params)
        .await;
    let parsed: serde_json::Value =
        serde_json::from_str(&raw).unwrap_or_else(|_| serde_json::json!({"result": raw}));

    if let Some(err) = parsed.get("error").and_then(|v| v.as_str()) {
        emit_command_output(app, format!("Dashboard command failed: {err}"));
        return Ok(CommandResult::Handled);
    }

    let rendered = match action.as_str() {
        "enable" | "on" => format!(
            "Dashboard enabled at {}\nConfig: {}",
            parsed
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown"),
            parsed
                .get("config_path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        ),
        "disable" | "off" => format!(
            "Dashboard disabled (URL: {})\nConfig: {}",
            parsed
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown"),
            parsed
                .get("config_path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        ),
        "url" => format!(
            "{}\n{}",
            parsed
                .get("url")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown"),
            parsed
                .get("config_path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        ),
        _ => format!(
            "{}\nConfig: {}",
            dashboard_status_line_from_payload(&parsed),
            parsed
                .get("config_path")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
        ),
    };
    emit_command_output(app, rendered);
    Ok(CommandResult::Handled)
}

async fn run_current_hermes_cli_command(args: &[&str]) -> Result<String, AgentError> {
    let exe = std::env::current_exe()
        .map_err(|e| AgentError::Io(format!("resolve current executable: {e}")))?;
    let output = tokio::process::Command::new(exe)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .await
        .map_err(|e| AgentError::Io(format!("run current hermes command failed: {e}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    let mut msg = String::new();
    if !stdout.is_empty() {
        msg.push_str(&stdout);
    }
    if !stderr.is_empty() {
        if !msg.is_empty() {
            msg.push_str("\n\n");
        }
        msg.push_str("stderr:\n");
        msg.push_str(&stderr);
    }
    if msg.is_empty() {
        msg = format!("(exit: {})", output.status);
    } else if !output.status.success() {
        msg = format!("(exit: {})\n{}", output.status, msg);
    }
    Ok(msg)
}

fn handle_simulate_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        let counters = app.tool_registry.policy_counters();
        emit_command_output(
            app,
            format!(
                "Tool-policy simulation\n\
                 usage: /simulate <tool_name> [json-params]\n\
                 examples:\n  /simulate terminal {{\"cmd\":\"ls\"}}\n  /simulate skill_manage {{\"action\":\"view\",\"skill\":\"contextlattice-agent-contract\"}}\n\
                 counters: allow={} deny={} audit_only={} simulate={} would_block={}",
                counters.allow, counters.deny, counters.audit_only, counters.simulate, counters.would_block
            ),
        );
        return Ok(CommandResult::Handled);
    }

    let tool_name = args[0].trim();
    if tool_name.is_empty() {
        emit_command_output(app, "Usage: /simulate <tool_name> [json-params]");
        return Ok(CommandResult::Handled);
    }
    let params = if args.len() > 1 {
        let raw = args[1..].join(" ");
        match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(v) if v.is_object() => v,
            Ok(_) => {
                emit_command_output(app, "simulate params must be a JSON object.");
                return Ok(CommandResult::Handled);
            }
            Err(err) => {
                emit_command_output(
                    app,
                    format!("simulate params parse error: {}\nraw={}", err, raw),
                );
                return Ok(CommandResult::Handled);
            }
        }
    } else {
        serde_json::json!({})
    };

    let decision = app
        .tool_registry
        .evaluate_policy_preview(tool_name, &params);
    let payload = serde_json::json!({
        "tool": tool_name,
        "params": params,
        "decision": {
            "allow": decision.allow,
            "mode": decision.mode.as_str(),
            "audited_only": decision.audited_only,
            "simulated": decision.simulated,
            "would_block": decision.would_block,
            "code": decision.code,
            "reason": decision.reason,
        }
    });
    emit_command_output(
        app,
        serde_json::to_string_pretty(&payload)
            .map_err(|e| AgentError::Config(format!("serialize simulate result: {e}")))?,
    );
    Ok(CommandResult::Handled)
}

fn route_learning_state_path() -> PathBuf {
    hermes_config::hermes_home().join("route-learning.json")
}

fn route_health_state_path() -> PathBuf {
    hermes_config::hermes_home().join("route-health.json")
}

fn route_autotune_state_path() -> PathBuf {
    hermes_config::hermes_home().join("route-autotune.json")
}

fn route_autotune_env_path() -> PathBuf {
    hermes_config::hermes_home().join("route-autotune.env")
}

fn summarize_route_health_state(path: &Path) -> String {
    let Some(report) = read_json_file(path) else {
        return "route_health=unknown".to_string();
    };
    let overall = report
        .get("overall")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    let score = report
        .get("summary")
        .and_then(|v| v.get("health_score"))
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    let generated = report
        .get("generated_at")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    format!(
        "route_health={} score={:.2} @ {}",
        overall, score, generated
    )
}

fn summarize_route_health_details(path: &Path) -> Option<String> {
    let report = read_json_file(path)?;
    let entries = report.get("entries")?.as_array()?;
    if entries.is_empty() {
        return Some("route_health_trace=no_entries".to_string());
    }
    let mut ranked = entries
        .iter()
        .filter_map(|entry| {
            let key = entry.get("key").and_then(|v| v.as_str())?;
            let tier = entry
                .get("tier")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            let health = entry
                .get("health_score")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            let reasons = entry
                .get("reasons")
                .and_then(|v| v.as_array())
                .map(|arr| {
                    arr.iter()
                        .filter_map(|v| v.as_str())
                        .map(|v| v.to_string())
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            Some((key.to_string(), tier.to_string(), health, reasons))
        })
        .collect::<Vec<_>>();
    if ranked.is_empty() {
        return Some("route_health_trace=no_parseable_entries".to_string());
    }
    ranked.sort_by(|a, b| a.2.total_cmp(&b.2));
    let hottest = ranked
        .iter()
        .take(3)
        .map(|(key, tier, health, reasons)| {
            let reason_text = if reasons.is_empty() {
                "no_reasons".to_string()
            } else {
                reasons.join("|")
            };
            format!("{key} tier={tier} score={health:.2} reasons={reason_text}")
        })
        .collect::<Vec<_>>()
        .join(" ; ");
    Some(format!("route_health_trace={}", hottest))
}

fn handle_ops_budget_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        let budget = RepoReviewBudgetRuntime::from_env();
        emit_command_output(
            app,
            format!(
                "repo_review_budget profile={}\n\
                 repeat_threshold={} low_signal_threshold={} keep_repeat={} keep_low_signal={} min_signal_score={:.2}",
                budget.profile.as_str(),
                budget.repeat_threshold,
                budget.low_signal_threshold,
                budget.keep_repeat,
                budget.keep_low_signal,
                budget.min_signal_score
            ),
        );
        return Ok(CommandResult::Handled);
    }
    match args[0].to_ascii_lowercase().as_str() {
        "list" => emit_command_output(
            app,
            "Repo-review budget profiles:\n- balanced: default trim cadence\n- aggressive: trim repetitive discovery quickly\n- relaxed: allow broader exploration before trimming\n- off: effectively disable trimming",
        ),
        "clear" => {
            for key in [
                REPO_REVIEW_BUDGET_ENV_REPEAT_THRESHOLD,
                REPO_REVIEW_BUDGET_ENV_LOW_SIGNAL_THRESHOLD,
                REPO_REVIEW_BUDGET_ENV_KEEP_REPEAT,
                REPO_REVIEW_BUDGET_ENV_KEEP_LOW_SIGNAL,
                REPO_REVIEW_BUDGET_ENV_MIN_SIGNAL_SCORE,
                REPO_REVIEW_BUDGET_ENV_PROFILE,
            ] {
                std::env::remove_var(key);
            }
            emit_command_output(app, "Cleared repo-review budget runtime overrides.");
        }
        profile_raw => {
            let Some(profile) = RepoReviewBudgetProfile::parse(profile_raw) else {
                emit_command_output(
                    app,
                    "Usage: /ops budget [status|list|balanced|aggressive|relaxed|off|clear]",
                );
                return Ok(CommandResult::Handled);
            };
            apply_repo_review_budget_profile(profile);
            let budget = RepoReviewBudgetRuntime::from_env();
            emit_command_output(
                app,
                format!(
                    "repo_review_budget set to '{}' (repeat={} low_signal={} keep_repeat={} keep_low_signal={} min_signal={:.2})",
                    profile.as_str(),
                    budget.repeat_threshold,
                    budget.low_signal_threshold,
                    budget.keep_repeat,
                    budget.keep_low_signal,
                    budget.min_signal_score
                ),
            );
        }
    }
    Ok(CommandResult::Handled)
}

fn handle_ops_tool_profile_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let mode = std::env::var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "off".to_string());
    if args.is_empty()
        || args
            .first()
            .is_some_and(|v| matches!(v.to_ascii_lowercase().as_str(), "status" | "show"))
    {
        emit_command_output(
            app,
            format!(
                "repo_review_tool_profile mode={}\nUse `/ops tool-profile [off|balanced|focus]`.\nEscape hatch: include `allow all tools` or `disable narrowing` in your request.",
                mode
            ),
        );
        return Ok(CommandResult::Handled);
    }
    if args[0].eq_ignore_ascii_case("list") {
        emit_command_output(
            app,
            "Repo-review tool profile modes:\n- off: disable narrowing (open tool lane)\n- balanced: filter messaging/non-repo noise only\n- focus: balanced + stricter repo-first filtering",
        );
        return Ok(CommandResult::Handled);
    }
    if args[0].eq_ignore_ascii_case("clear") {
        std::env::remove_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE");
        emit_command_output(
            app,
            "Cleared repo-review tool profile override (default=balanced).",
        );
        return Ok(CommandResult::Handled);
    }
    let next = args[0].to_ascii_lowercase();
    if !matches!(next.as_str(), "off" | "balanced" | "focus") {
        emit_command_output(
            app,
            "Usage: /ops tool-profile [status|list|off|balanced|focus|clear]",
        );
        return Ok(CommandResult::Handled);
    }
    std::env::set_var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE", next.as_str());
    emit_command_output(
        app,
        format!("repo_review_tool_profile mode set to `{}`", next),
    );
    Ok(CommandResult::Handled)
}

async fn handle_ops_eval_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    let Some(repo_root) = discover_repo_root_for_about() else {
        emit_command_output(
            app,
            "Eval controls are unavailable outside source checkout.",
        );
        return Ok(CommandResult::Handled);
    };
    let report_dir = repo_root.join(".sync-reports");
    match sub.as_str() {
        "status" => {
            let latest = latest_json_report(&report_dir, "session-eval-harness-")
                .or_else(|| latest_json_report(&report_dir, "eval-trend-gate-"));
            if let Some(path) = latest {
                let summary = summarize_gate_report(&path, "eval")
                    .unwrap_or_else(|| format!("latest eval report: {}", path.display()));
                emit_command_output(
                    app,
                    format!(
                        "{summary}\nRun `/ops eval run` to generate a fresh session-backed report."
                    ),
                );
            } else {
                emit_command_output(
                    app,
                    "No eval reports found yet. Run `/ops eval run` to generate one.",
                );
            }
            Ok(CommandResult::Handled)
        }
        "run" => {
            let (report, path) = run_session_eval_harness_native(
                &repo_root,
                &hermes_config::hermes_home().join("sessions"),
                25,
                None,
            )?;
            let out = format_json_report_with_path(&report, &path)?;
            emit_command_output(app, out);
            Ok(CommandResult::Handled)
        }
        "latest" => {
            let Some(path) = latest_json_report(&report_dir, "session-eval-harness-")
                .or_else(|| latest_json_report(&report_dir, "eval-trend-gate-"))
            else {
                emit_command_output(app, "No eval reports found.");
                return Ok(CommandResult::Handled);
            };
            let raw = std::fs::read_to_string(&path)
                .map_err(|e| AgentError::Io(format!("read {}: {}", path.display(), e)))?;
            emit_command_output(
                app,
                format!(
                    "Latest eval report: {}\n{}",
                    path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_else(|| path.display().to_string()),
                    raw
                ),
            );
            Ok(CommandResult::Handled)
        }
        _ => {
            emit_command_output(app, "Usage: /ops eval [status|run|latest]");
            Ok(CommandResult::Handled)
        }
    }
}

async fn handle_qos_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    match sub.as_str() {
        "status" | "show" => {
            let learning_path = route_learning_state_path();
            let health_path = route_health_state_path();
            let autotune_path = route_autotune_state_path();
            let autotune_env = route_autotune_env_path();
            let learning_entries = read_json_file(&learning_path)
                .and_then(|v| {
                    v.get("entries")
                        .and_then(|e| e.as_array())
                        .map(|arr| arr.len())
                })
                .unwrap_or(0usize);
            let health_summary = summarize_route_health_state(&health_path);
            let mut out = String::new();
            let _ = writeln!(out, "Provider QoS router");
            let _ = writeln!(
                out,
                "  route_learning_entries={} ({})",
                learning_entries,
                learning_path.display()
            );
            let _ = writeln!(out, "  {} ({})", health_summary, health_path.display());
            if let Some(trace) = summarize_route_health_details(&health_path) {
                let _ = writeln!(out, "  {}", trace);
            }
            let _ = writeln!(
                out,
                "  route_autotune_state={} ({})",
                if autotune_path.exists() {
                    "present"
                } else {
                    "missing"
                },
                autotune_path.display()
            );
            let _ = writeln!(
                out,
                "  route_autotune_env={} ({})",
                if autotune_env.exists() {
                    "present"
                } else {
                    "missing"
                },
                autotune_env.display()
            );
            let _ = writeln!(
                out,
                "  actions: /qos health | /qos autotune plan | /qos autotune apply"
            );
            emit_command_output(app, out.trim_end());
            Ok(CommandResult::Handled)
        }
        "health" => {
            let out = run_current_hermes_cli_command(&["route-health", "--json"]).await?;
            emit_command_output(app, out);
            Ok(CommandResult::Handled)
        }
        "autotune" => {
            let action = args.get(1).copied().unwrap_or("plan").to_ascii_lowercase();
            let out = match action.as_str() {
                "plan" => {
                    run_current_hermes_cli_command(&["route-autotune", "plan", "--json"]).await?
                }
                "apply" => {
                    run_current_hermes_cli_command(&[
                        "route-autotune",
                        "apply",
                        "--apply",
                        "--json",
                    ])
                    .await?
                }
                _ => {
                    emit_command_output(app, "Usage: /qos autotune [plan|apply]");
                    return Ok(CommandResult::Handled);
                }
            };
            emit_command_output(app, out);
            Ok(CommandResult::Handled)
        }
        "help" => {
            emit_command_output(app, "Usage: /qos [status|health|autotune [plan|apply]]");
            Ok(CommandResult::Handled)
        }
        _ => {
            emit_command_output(app, "Usage: /qos [status|health|autotune [plan|apply]]");
            Ok(CommandResult::Handled)
        }
    }
}

fn handle_ops_skills_tier_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        emit_command_output(
            app,
            format!(
                "skills_tier={} (bypass={})",
                skills_execution_tier().as_str(),
                if skills_tier_bypass_enabled() {
                    "ON"
                } else {
                    "OFF"
                }
            ),
        );
        return Ok(CommandResult::Handled);
    }

    let Some(next) = SkillsExecutionTier::parse(args[0]) else {
        emit_command_output(
            app,
            "Usage: /ops skills-tier [status|trusted|balanced|open]",
        );
        return Ok(CommandResult::Handled);
    };
    std::env::set_var("HERMES_SKILLS_EXECUTION_TIER", next.as_str());
    emit_command_output(
        app,
        format!(
            "skills_tier set to '{}' for this runtime process.",
            next.as_str()
        ),
    );
    Ok(CommandResult::Handled)
}

async fn handle_ops_gate_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    let repo_root = discover_repo_root_for_about();
    match sub.as_str() {
        "status" => {
            if let Some(repo_root) = repo_root.as_ref() {
                let report_dir = repo_root.join(".sync-reports");
                let eval = latest_json_report(&report_dir, "eval-trend-gate-")
                    .and_then(|p| summarize_gate_report(&p, "eval_trend"))
                    .unwrap_or_else(|| "eval_trend=unknown".to_string());
                let slo = latest_json_report(&report_dir, "slo-auto-rollback-")
                    .and_then(|p| summarize_gate_report(&p, "slo_rollback"))
                    .unwrap_or_else(|| "slo_rollback=unknown".to_string());
                let elite = latest_json_report(&report_dir, "elite-sync-gate-")
                    .and_then(|p| summarize_gate_report(&p, "elite_sync_gate"))
                    .unwrap_or_else(|| "elite_sync_gate=unknown".to_string());
                emit_command_output(app, format!("{}\n{}\n{}", eval, slo, elite));
            } else {
                emit_command_output(app, "Gate status unavailable outside source checkout.");
            }
            Ok(CommandResult::Handled)
        }
        "eval" => {
            let Some(repo_root) = repo_root.as_ref() else {
                emit_command_output(app, "Eval gate unavailable outside source checkout.");
                return Ok(CommandResult::Handled);
            };
            let (report, path) = run_eval_trend_gate_native(
                repo_root,
                None,
                None,
                None,
                EvalTrendGateOptions {
                    allow_missing_baseline: true,
                    ..Default::default()
                },
            )?;
            let out = format_json_report_with_path(&report, &path)?;
            emit_command_output(app, out);
            Ok(CommandResult::Handled)
        }
        "elite" => {
            let Some(repo_root) = repo_root.as_ref() else {
                emit_command_output(app, "Elite gate unavailable outside source checkout.");
                return Ok(CommandResult::Handled);
            };
            let (report, path) = run_elite_sync_gate_native(repo_root).await?;
            let out = format_json_report_with_path(&report, &path)?;
            emit_command_output(app, out);
            Ok(CommandResult::Handled)
        }
        "slo" => {
            let Some(repo_root) = repo_root.as_ref() else {
                emit_command_output(app, "SLO gate unavailable outside source checkout.");
                return Ok(CommandResult::Handled);
            };
            let check_cmd = std::env::var("HERMES_SLO_CHECK_CMD").ok();
            let rollback_cmd = std::env::var("HERMES_SLO_ROLLBACK_CMD").ok();
            let (Some(check), Some(rollback)) = (check_cmd, rollback_cmd) else {
                emit_command_output(
                    app,
                    "Set HERMES_SLO_CHECK_CMD and HERMES_SLO_ROLLBACK_CMD, then run `/ops gate slo`.",
                );
                return Ok(CommandResult::Handled);
            };
            let (report, path) =
                run_slo_auto_rollback_native(repo_root, &check, &rollback, false, None).await?;
            let out = format_json_report_with_path(&report, &path)?;
            emit_command_output(app, out);
            Ok(CommandResult::Handled)
        }
        _ => {
            emit_command_output(app, "Usage: /ops gate [status|eval|elite|slo]");
            Ok(CommandResult::Handled)
        }
    }
}

async fn handle_ops_evolve_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    let Some(repo_root) = discover_repo_root_for_about() else {
        emit_command_output(
            app,
            "Self-evolution controls are unavailable outside source checkout.",
        );
        return Ok(CommandResult::Handled);
    };
    let report_dir = repo_root.join(".sync-reports");
    match sub.as_str() {
        "status" => {
            let summary = latest_json_report(&report_dir, "self-evolution-loop-")
                .and_then(|p| summarize_self_evolution_report(&p, "self_evolution"))
                .unwrap_or_else(|| "self_evolution=unknown (no reports yet)".to_string());
            emit_command_output(
                app,
                format!(
                    "{}\nRun `/ops evolve run` to execute the loop now.",
                    summary
                ),
            );
            Ok(CommandResult::Handled)
        }
        "run" => {
            let objective = app.session_objective.as_deref().unwrap_or_default();
            let (report, path) = run_self_evolution_loop_native(&repo_root, objective).await?;
            let out = format_json_report_with_path(&report, &path)?;
            emit_command_output(app, out);
            Ok(CommandResult::Handled)
        }
        "recommend" | "recs" => {
            let Some(path) = latest_json_report(&report_dir, "self-evolution-loop-") else {
                emit_command_output(
                    app,
                    "No self-evolution reports found. Run `/ops evolve run` first.",
                );
                return Ok(CommandResult::Handled);
            };
            let recs = self_evolution_recommendations(&path);
            if recs.is_empty() {
                emit_command_output(
                    app,
                    format!(
                        "No recommendations found in {}.",
                        path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.display().to_string())
                    ),
                );
            } else {
                let file_label = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                emit_command_output(
                    app,
                    format!(
                        "Self-evolution recommendations ({file_label}):\n{}",
                        recs.join("\n")
                    ),
                );
            }
            Ok(CommandResult::Handled)
        }
        _ => {
            emit_command_output(app, "Usage: /ops evolve [status|run|recommend]");
            Ok(CommandResult::Handled)
        }
    }
}

async fn handle_ops_autopilot_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let sub = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    let mode = std::env::var("HERMES_PERF_AUTOPILOT_MODE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "advisory".to_string());
    let profile = std::env::var("HERMES_PERF_AUTOPILOT_PROFILE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "off".to_string());

    let Some(repo_root) = discover_repo_root_for_about() else {
        emit_command_output(
            app,
            "Autopilot controls are unavailable outside source checkout.",
        );
        return Ok(CommandResult::Handled);
    };
    let report_dir = repo_root.join(".sync-reports");
    let latest = latest_json_report(&report_dir, "performance-autopilot-");

    match sub.as_str() {
        "status" => {
            let summary = latest
                .as_ref()
                .and_then(|p| summarize_performance_autopilot_report(p, "autopilot"))
                .unwrap_or_else(|| "autopilot=unknown (no reports yet)".to_string());
            emit_command_output(
                app,
                format!(
                    "{}\nmode={} profile={}\nUse `/ops autopilot run` then `/ops autopilot recommend`.",
                    summary, mode, profile
                ),
            );
            Ok(CommandResult::Handled)
        }
        "run" => {
            let (report, json_path, md_path) =
                run_performance_autopilot_native(&repo_root, None).await?;
            let mut out = format_json_report_with_path(&report, &json_path)?;
            let _ = write!(out, "\nmarkdown_path={}", md_path.display());
            emit_command_output(app, out);
            Ok(CommandResult::Handled)
        }
        "recommend" | "recs" => {
            let Some(path) = latest else {
                emit_command_output(
                    app,
                    "No performance autopilot reports found. Run `/ops autopilot run` first.",
                );
                return Ok(CommandResult::Handled);
            };
            let recs = performance_autopilot_recommendations(&path);
            if recs.is_empty() {
                emit_command_output(
                    app,
                    format!(
                        "No recommendations found in {}.",
                        path.file_name()
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| path.display().to_string())
                    ),
                );
            } else {
                let file_label = path
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| path.display().to_string());
                emit_command_output(
                    app,
                    format!(
                        "Autopilot recommendations ({file_label}):\n{}",
                        recs.join("\n")
                    ),
                );
            }
            Ok(CommandResult::Handled)
        }
        "apply" => {
            let env_path =
                report_dir.join(format!("performance-autopilot-env-{}.env", app.session_id));
            let (report, json_path, md_path) =
                run_performance_autopilot_native(&repo_root, Some(&env_path)).await?;
            let mut out = format_json_report_with_path(&report, &json_path)?;
            let _ = write!(out, "\nmarkdown_path={}", md_path.display());
            let kvs = parse_env_file_kv(&env_path);
            let mut applied = Vec::new();
            for (k, v) in kvs {
                if AUTOPILOT_ALLOWED_ENV_KEYS
                    .iter()
                    .any(|allowed| *allowed == k)
                {
                    std::env::set_var(&k, &v);
                    applied.push((k, v));
                }
            }
            write_autopilot_runtime_event(&report_dir, &app.session_id, &mode, &profile, &applied);
            let applied_keys = if applied.is_empty() {
                "(none)".to_string()
            } else {
                applied
                    .iter()
                    .map(|(k, _)| k.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            emit_command_output(
                app,
                format!(
                    "{out}\n\nApplied safe runtime knobs: {applied_keys}\nmode={mode} profile={profile}\nlog: {}",
                    report_dir.join("performance-autopilot-runtime.jsonl").display()
                ),
            );
            Ok(CommandResult::Handled)
        }
        "profile" => {
            let next = args.get(1).map(|v| v.to_ascii_lowercase());
            match next.as_deref() {
                None | Some("status") => emit_command_output(
                    app,
                    format!("autopilot profile={profile} (mode={mode})"),
                ),
                Some("list") => emit_command_output(
                    app,
                    "Autopilot profiles:\n- balanced: default stability/perf mix\n- throughput: lower latency and tighter loop cadence\n- quality: stronger verification and replay focus\n- reliability: prioritize retries/recovery and degraded-source tolerance\n- safety: strictest gate posture with conservative policy knobs",
                ),
                Some("balanced" | "throughput" | "quality" | "reliability" | "safety") => {
                    let value = next.unwrap_or_else(|| "off".to_string());
                    std::env::set_var("HERMES_PERF_AUTOPILOT_PROFILE", &value);
                    emit_command_output(app, format!("autopilot profile set to '{}'", value));
                }
                Some(other) => {
                    emit_command_output(
                        app,
                        format!(
                            "Unknown profile '{}'. Use `/ops autopilot profile list`.",
                            other
                        ),
                    );
                }
            }
            Ok(CommandResult::Handled)
        }
        "mode" => {
            let next = args.get(1).map(|v| v.to_ascii_lowercase());
            match next.as_deref() {
                None | Some("status") => emit_command_output(app, format!("autopilot mode={mode}")),
                Some("list") => emit_command_output(
                    app,
                    "Autopilot modes:\n- off: disabled\n- advisory: report + recommendations only\n- enforce: intended to pair with `/ops autopilot apply` during incidents",
                ),
                Some("off" | "advisory" | "enforce") => {
                    let value = next.unwrap_or_else(|| "advisory".to_string());
                    std::env::set_var("HERMES_PERF_AUTOPILOT_MODE", &value);
                    emit_command_output(app, format!("autopilot mode set to '{}'", value));
                }
                Some(other) => {
                    emit_command_output(
                        app,
                        format!("Unknown mode '{}'. Use `/ops autopilot mode list`.", other),
                    );
                }
            }
            Ok(CommandResult::Handled)
        }
        "clear" => {
            std::env::remove_var("HERMES_PERF_AUTOPILOT_MODE");
            std::env::remove_var("HERMES_PERF_AUTOPILOT_PROFILE");
            std::env::remove_var("HERMES_PERF_AUTOPILOT_STATUS");
            emit_command_output(
                app,
                "Cleared autopilot runtime overrides (mode/profile/status).",
            );
            Ok(CommandResult::Handled)
        }
        _ => {
            emit_command_output(
                app,
                "Usage: /ops autopilot [status|run|recommend|apply|profile [status|list|balanced|throughput|quality|reliability|safety]|mode [status|list|off|advisory|enforce]|clear]",
            );
            Ok(CommandResult::Handled)
        }
    }
}

async fn handle_ops_cockpit_command(
    app: &mut App,
    _args: &[&str],
) -> Result<CommandResult, AgentError> {
    let counters = app.tool_registry.policy_counters();
    let budget = RepoReviewBudgetRuntime::from_env();
    let board = render_mission_board(
        &app.current_model,
        app.session_objective.as_deref(),
        background_job_counts(),
    )
    .await?;
    let route_health = summarize_route_health_state(&route_health_state_path());
    let eval_summary = if let Some(repo_root) = discover_repo_root_for_about() {
        let report_dir = repo_root.join(".sync-reports");
        latest_json_report(&report_dir, "session-eval-harness-")
            .or_else(|| latest_json_report(&report_dir, "eval-trend-gate-"))
            .and_then(|p| summarize_gate_report(&p, "eval"))
            .unwrap_or_else(|| "eval=unknown".to_string())
    } else {
        "eval=unavailable".to_string()
    };
    let snapshot_count =
        enumerate_saved_sessions(&hermes_config::hermes_home().join("sessions")).len();
    let mut out = String::new();
    out.push_str("Ops Cockpit\n");
    out.push_str("===========\n");
    let _ = writeln!(out, "session: {}", app.session_id);
    let _ = writeln!(out, "model: {}", app.current_model);
    let _ = writeln!(
        out,
        "policy: profile={} mode={} preset={} sandbox={} skills_tier={}",
        current_policy_profile_name(),
        std::env::var("HERMES_TOOL_POLICY_MODE").unwrap_or_else(|_| "enforce".into()),
        std::env::var("HERMES_TOOL_POLICY_PRESET").unwrap_or_else(|_| "relaxed".into()),
        std::env::var("HERMES_EXECUTION_SANDBOX_PROFILE").unwrap_or_else(|_| "balanced".into()),
        std::env::var("HERMES_SKILLS_EXECUTION_TIER").unwrap_or_else(|_| "balanced".into())
    );
    let _ = writeln!(
        out,
        "planner_capability_router={} compaction_governance={} replay_trace={}",
        plan_capability_mode().as_str(),
        compaction_governance_mode().as_str(),
        if replay_enabled_runtime() {
            "on"
        } else {
            "off"
        }
    );
    let _ = writeln!(
        out,
        "repo_review_budget: profile={} repeat={} low_signal={} keep_repeat={} keep_low_signal={} min_signal={:.2}",
        budget.profile.as_str(),
        budget.repeat_threshold,
        budget.low_signal_threshold,
        budget.keep_repeat,
        budget.keep_low_signal,
        budget.min_signal_score
    );
    let _ = writeln!(
        out,
        "policy_counters: allow={} deny={} audit_only={} simulate={} would_block={}",
        counters.allow, counters.deny, counters.audit_only, counters.simulate, counters.would_block
    );
    let _ = writeln!(
        out,
        "qos: {} | learning_entries={} | snapshots={}",
        route_health,
        read_json_file(&route_learning_state_path())
            .and_then(|v| v
                .get("entries")
                .and_then(|e| e.as_array())
                .map(|arr| arr.len()))
            .unwrap_or(0usize),
        snapshot_count
    );
    let _ = writeln!(out, "eval: {}", eval_summary);
    out.push('\n');
    out.push_str(&board);
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

async fn handle_ops_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        let yolo = !app.config.approval.require_approval;
        let policy_mode = std::env::var("HERMES_TOOL_POLICY_MODE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "enforce".to_string());
        let policy_preset = std::env::var("HERMES_TOOL_POLICY_PRESET")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "off".to_string());
        let counters = app.tool_registry.policy_counters();
        let dashboard_status = {
            let raw = app
                .tool_registry
                .dispatch_async("dashboard_control", serde_json::json!({"action":"status"}))
                .await;
            let parsed: serde_json::Value = serde_json::from_str(&raw).unwrap_or_else(
                |_| serde_json::json!({"enabled":false,"url":"unknown","error":"unparseable"}),
            );
            dashboard_status_line_from_payload(&parsed)
        };
        let gate_status = if let Some(repo_root) = discover_repo_root_for_about() {
            let report_dir = repo_root.join(".sync-reports");
            let eval = latest_json_report(&report_dir, "eval-trend-gate-")
                .and_then(|p| summarize_gate_report(&p, "eval"))
                .unwrap_or_else(|| "eval=unknown".to_string());
            let slo = latest_json_report(&report_dir, "slo-auto-rollback-")
                .and_then(|p| summarize_gate_report(&p, "slo"))
                .unwrap_or_else(|| "slo=unknown".to_string());
            let evolve = latest_json_report(&report_dir, "self-evolution-loop-")
                .and_then(|p| summarize_self_evolution_report(&p, "evolve"))
                .unwrap_or_else(|| "evolve=unknown".to_string());
            let autopilot = latest_json_report(&report_dir, "performance-autopilot-")
                .and_then(|p| summarize_performance_autopilot_report(&p, "autopilot"))
                .unwrap_or_else(|| "autopilot=unknown".to_string());
            format!("{eval}; {slo}; {evolve}; {autopilot}")
        } else {
            "unavailable (non-source checkout)".to_string()
        };
        let autopilot_mode = std::env::var("HERMES_PERF_AUTOPILOT_MODE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "advisory".to_string());
        let autopilot_profile = std::env::var("HERMES_PERF_AUTOPILOT_PROFILE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "off".to_string());
        let repo_review_budget = RepoReviewBudgetRuntime::from_env();
        let tool_profile_mode = std::env::var("HERMES_REPO_REVIEW_TOOL_PROFILE_MODE")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .unwrap_or_else(|| "off".to_string());

        let out = format!(
            "Operator Control Plane\n\
             \n\
             Runtime:\n\
               session:      {}\n\
               model:        {}\n\
               personality:  {}\n\
             \n\
             Controls:\n\
               yolo:         {}\n\
               mouse:        {}\n\
               statusbar:    ON\n\
               reasoning:    `/ops reasoning status` + `/ops reasoning set ...`\n\
               raw:          toggle via `/ops raw`\n\
               verbose:      toggle via `/ops verbose`\n\
             \n\
             Policy/Gates:\n\
               tool_policy:  mode={} preset={}\n\
               autopilot:    mode={} profile={}\n\
               tool_profile: {}\n\
               repo_budget:  profile={} repeat={} low_signal={} keep_repeat={} keep_low_signal={} min_signal={:.2}\n\
               task_depth:   {}\n\
               policy_counts allow={} deny={} audit_only={} simulate={} would_block={}\n\
               skills_tier:  {} (bypass={})\n\
               {}\n\
               gate_status:  {}\n\
             \n\
             Quick actions:\n\
               /ops model [provider|provider:model]\n\
               /ops mode [status|list|strict|standard|dev]\n\
               /ops personality [list|name]\n\
               /ops mouse [on|off|toggle]\n\
               /ops yolo\n\
               /ops reasoning [status|on|off|toggle|set <level>]\n\
               /ops raw [on|off|toggle|once|trace ...]\n\
               /ops verbose\n\
               /ops dashboard [status|on|off|url] [host] [port]\n\
               /ops skills-tier [status|trusted|balanced|open]\n\
               /ops tool-profile [status|list|off|balanced|focus]\n\
               /ops budget [status|list|balanced|aggressive|relaxed|off|clear]\n\
               /ops evolve [status|run|recommend]\n\
               /ops eval [status|run|latest]\n\
               /ops autopilot [status|run|recommend|apply|profile|mode|clear]\n\
               /ops gate [status|eval|elite|slo]\n\
               /ops cockpit\n\
               /mission [status|init]\n\
               /ops help",
            app.session_id,
            app.current_model,
            app.current_personality.as_deref().unwrap_or("(none)"),
            if yolo { "ON" } else { "OFF" },
            if app.mouse_enabled() { "ON" } else { "OFF" },
            policy_mode,
            policy_preset,
            autopilot_mode,
            autopilot_profile,
            tool_profile_mode,
            repo_review_budget.profile.as_str(),
            repo_review_budget.repeat_threshold,
            repo_review_budget.low_signal_threshold,
            repo_review_budget.keep_repeat,
            repo_review_budget.keep_low_signal,
            repo_review_budget.min_signal_score,
            task_depth_runtime_summary(),
            counters.allow,
            counters.deny,
            counters.audit_only,
            counters.simulate,
            counters.would_block,
            skills_execution_tier().as_str(),
            if skills_tier_bypass_enabled() {
                "ON"
            } else {
                "OFF"
            },
            dashboard_status,
            gate_status,
        );
        emit_command_output(app, out);
        return Ok(CommandResult::Handled);
    }

    match args[0].to_ascii_lowercase().as_str() {
        "help" => {
            emit_command_output(
                app,
                "Operator control plane commands:\n\
                 - /ops status\n\
                 - /ops model [provider|provider:model]\n\
                 - /ops mode [status|list|strict|standard|dev]\n\
                 - /ops personality [list|name]\n\
                 - /ops mouse [on|off|toggle]\n\
                 - /ops yolo\n\
                 - /ops reasoning [status|on|off|toggle|set <level>]\n\
                 - /ops raw [on|off|toggle|once|trace ...]\n\
                 - /ops verbose\n\
                 - /ops statusbar\n\
                 - /ops dashboard [status|on|off|url] [host] [port]\n\
                 - /ops skills-tier [status|trusted|balanced|open]\n\
                 - /ops tool-profile [status|list|off|balanced|focus]\n\
                 - /ops budget [status|list|balanced|aggressive|relaxed|off|clear]\n\
                 - /ops evolve [status|run|recommend]\n\
                 - /ops eval [status|run|latest]\n\
                 - /ops autopilot [status|run|recommend|apply|profile|mode|clear]\n\
                 - /ops gate [status|eval|elite|slo]\n\
                 - /ops cockpit\n\
                 - /mission [status|init]",
            );
            Ok(CommandResult::Handled)
        }
        "model" => handle_model_command(app, &args[1..]).await,
        "mode" => handle_policy_command(app, &args[1..]),
        "personality" => handle_personality_command(app, &args[1..]),
        "mouse" => handle_mouse_command(app, &args[1..]),
        "yolo" => handle_yolo_command(app),
        "reasoning" => handle_reasoning_command(app, &args[1..]),
        "raw" => handle_raw_command(app, &args[1..]),
        "verbose" => handle_verbose_command(app),
        "statusbar" => handle_statusbar_command(app),
        "dashboard" => handle_dashboard_command(app, &args[1..]).await,
        "skills-tier" => handle_ops_skills_tier_command(app, &args[1..]),
        "tool-profile" | "toolprofile" | "tool_profile" => {
            handle_ops_tool_profile_command(app, &args[1..])
        }
        "budget" => handle_ops_budget_command(app, &args[1..]),
        "evolve" => handle_ops_evolve_command(app, &args[1..]).await,
        "eval" => handle_ops_eval_command(app, &args[1..]).await,
        "autopilot" => handle_ops_autopilot_command(app, &args[1..]).await,
        "gate" => handle_ops_gate_command(app, &args[1..]).await,
        "cockpit" => handle_ops_cockpit_command(app, &args[1..]).await,
        other => {
            emit_command_output(
                app,
                format!(
                    "Unknown /ops target '{}'. Try `/ops help` for available controls.",
                    other
                ),
            );
            Ok(CommandResult::Handled)
        }
    }
}
