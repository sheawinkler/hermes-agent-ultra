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

