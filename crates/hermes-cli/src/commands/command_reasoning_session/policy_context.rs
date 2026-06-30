#[derive(Debug, Clone, Copy)]
struct PolicyProfile {
    name: &'static str,
    preset: &'static str,
    mode: &'static str,
    sandbox: &'static str,
    skills_tier: &'static str,
    description: &'static str,
}

const POLICY_PROFILES: &[PolicyProfile] = &[
    PolicyProfile {
        name: "strict",
        preset: "strict",
        mode: "enforce",
        sandbox: "strict",
        skills_tier: "trusted",
        description: "maximum guardrails; strongest deny + sandbox posture",
    },
    PolicyProfile {
        name: "standard",
        preset: "balanced",
        mode: "enforce",
        sandbox: "balanced",
        skills_tier: "balanced",
        description: "default production posture with balanced safety and throughput",
    },
    PolicyProfile {
        name: "dev",
        preset: "dev",
        mode: "audit",
        sandbox: "dev",
        skills_tier: "open",
        description: "development posture with audit/simulate-friendly behavior",
    },
];

fn resolve_policy_profile(input: &str) -> Option<PolicyProfile> {
    let token = input.trim().to_ascii_lowercase();
    POLICY_PROFILES.iter().copied().find(|profile| {
        profile.name == token
            || (token == "balanced" && profile.name == "standard")
            || (token == "prod" && profile.name == "standard")
    })
}

fn current_policy_profile_name() -> &'static str {
    let preset = std::env::var("HERMES_TOOL_POLICY_PRESET")
        .ok()
        .unwrap_or_else(|| "off".to_string())
        .trim()
        .to_ascii_lowercase();
    match preset.as_str() {
        "strict" => "strict",
        "dev" => "dev",
        _ => "standard",
    }
}

fn apply_policy_profile(app: &mut App, profile: PolicyProfile) {
    std::env::set_var("HERMES_TOOL_POLICY_PRESET", profile.preset);
    std::env::set_var("HERMES_TOOL_POLICY_MODE", profile.mode);
    std::env::set_var("HERMES_EXECUTION_SANDBOX_PROFILE", profile.sandbox);
    std::env::set_var("HERMES_SKILLS_EXECUTION_TIER", profile.skills_tier);
    app.tool_registry.set_policy(ToolPolicyEngine::from_env());
}

fn handle_policy_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() || args[0].eq_ignore_ascii_case("status") {
        let counters = app.tool_registry.policy_counters();
        emit_command_output(
            app,
            format!(
                "Policy profile: {}\nPreset: {}\nMode: {}\nSandbox: {}\nSkills tier: {}\nCounters: allow={} deny={} audit_only={} simulate={} would_block={}\n\nUse `/policy list` or `/policy strict|standard|dev`.",
                current_policy_profile_name(),
                std::env::var("HERMES_TOOL_POLICY_PRESET").unwrap_or_else(|_| "balanced".into()),
                std::env::var("HERMES_TOOL_POLICY_MODE").unwrap_or_else(|_| "enforce".into()),
                std::env::var("HERMES_EXECUTION_SANDBOX_PROFILE")
                    .unwrap_or_else(|_| "balanced".into()),
                std::env::var("HERMES_SKILLS_EXECUTION_TIER")
                    .unwrap_or_else(|_| "balanced".into()),
                counters.allow,
                counters.deny,
                counters.audit_only,
                counters.simulate,
                counters.would_block
            ),
        );
        return Ok(CommandResult::Handled);
    }

    if args[0].eq_ignore_ascii_case("list") {
        let mut out = String::from("Policy profiles:\n");
        for profile in POLICY_PROFILES {
            let marker = if current_policy_profile_name() == profile.name {
                "*"
            } else {
                " "
            };
            let _ = writeln!(
                out,
                "{} {:<9} preset={} mode={} sandbox={} skills_tier={} — {}",
                marker,
                profile.name,
                profile.preset,
                profile.mode,
                profile.sandbox,
                profile.skills_tier,
                profile.description
            );
        }
        out.push_str("\nSelect with `/policy strict`, `/policy standard`, or `/policy dev`.");
        emit_command_output(app, out.trim_end());
        return Ok(CommandResult::Handled);
    }

    if let Some(profile) = resolve_policy_profile(args[0]) {
        apply_policy_profile(app, profile);
        emit_command_output(
            app,
            format!(
                "Policy profile switched to `{}`.\nPreset={} Mode={} Sandbox={} SkillsTier={}",
                profile.name, profile.preset, profile.mode, profile.sandbox, profile.skills_tier
            ),
        );
        return Ok(CommandResult::Handled);
    }

    emit_command_output(app, "Usage: /policy [status|list|strict|standard|dev]");
    Ok(CommandResult::Handled)
}

fn handle_history_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let transcript = app.transcript_messages();
    if transcript.is_empty() {
        emit_command_output(app, "No conversation history yet.");
        return Ok(CommandResult::Handled);
    }
    let mut out = String::from("Recent conversation history:\n");
    for (idx, msg) in transcript.iter().enumerate().rev().take(12).rev() {
        let role = match msg.role {
            hermes_core::MessageRole::User => "USER",
            hermes_core::MessageRole::Assistant => "HERMES",
            hermes_core::MessageRole::System => "SYSTEM",
            hermes_core::MessageRole::Tool => "TOOL",
        };
        let preview = msg
            .content
            .as_deref()
            .unwrap_or("")
            .lines()
            .next()
            .unwrap_or("")
            .trim();
        let clipped = if preview.chars().count() > 96 {
            let mut s: String = preview.chars().take(95).collect();
            s.push('…');
            s
        } else {
            preview.to_string()
        };
        let _ = writeln!(out, "{:>3}. {:<7} {}", idx + 1, role, clipped);
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn truncate_chars(input: &str, max_len: usize) -> String {
    if max_len == 0 {
        return String::new();
    }
    if input.chars().count() <= max_len {
        return input.to_string();
    }
    let mut out: String = input.chars().take(max_len.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn handle_recap_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let requested = args
        .first()
        .and_then(|raw| raw.trim().parse::<usize>().ok())
        .unwrap_or(24)
        .clamp(1, 200);
    let transcript = app.transcript_messages();
    if transcript.is_empty() {
        emit_command_output(app, "No activity yet. Start with a prompt first.");
        return Ok(CommandResult::Handled);
    }

    let start = transcript.len().saturating_sub(requested);
    let window = &transcript[start..];
    let mut user_msgs = 0usize;
    let mut assistant_msgs = 0usize;
    let mut tool_msgs = 0usize;
    let mut system_msgs = 0usize;
    let mut tool_call_count = 0usize;
    let mut char_count = 0usize;

    for msg in window {
        match msg.role {
            hermes_core::MessageRole::User => user_msgs += 1,
            hermes_core::MessageRole::Assistant => assistant_msgs += 1,
            hermes_core::MessageRole::Tool => tool_msgs += 1,
            hermes_core::MessageRole::System => system_msgs += 1,
        }
        tool_call_count += msg.tool_calls.as_ref().map(|c| c.len()).unwrap_or(0);
        char_count += msg.content.as_deref().map(str::len).unwrap_or(0);
    }

    let latest_user = window
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermes_core::MessageRole::User))
        .and_then(|m| m.content.as_deref())
        .map(|c| truncate_chars(c.trim(), 120))
        .unwrap_or_else(|| "(none)".to_string());
    let latest_assistant = window
        .iter()
        .rev()
        .find(|m| matches!(m.role, hermes_core::MessageRole::Assistant))
        .and_then(|m| m.content.as_deref())
        .map(|c| truncate_chars(c.trim(), 120))
        .unwrap_or_else(|| "(none)".to_string());

    let approx_tokens = (char_count / 4).max(1);
    emit_command_output(
        app,
        format!(
            "Session recap (last {} messages)\n\
             model: {}\n\
             roles: user={} assistant={} tool={} system={}\n\
             tool_calls: {}\n\
             approx_tokens: {}\n\
             latest_user: {}\n\
             latest_hermes: {}",
            window.len(),
            app.current_model,
            user_msgs,
            assistant_msgs,
            tool_msgs,
            system_msgs,
            tool_call_count,
            approx_tokens,
            latest_user,
            latest_assistant
        ),
    );
    Ok(CommandResult::Handled)
}

fn handle_context_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    match action.as_str() {
        "status" => {
            let transcript = app.transcript_messages();
            let total_chars: usize = transcript
                .iter()
                .map(|m| m.content.as_deref().map(str::len).unwrap_or(0))
                .sum();
            let approx_tokens = (total_chars / 4).max(1);
            let context_files = if app.config.agent.skip_context_files {
                "disabled"
            } else {
                "enabled"
            };
            emit_command_output(
                app,
                format!(
                    "Context status\n\
                     model: {}\n\
                     transcript_messages: {}\n\
                     approx_tokens: {}\n\
                     context_files: {}\n\
                     hint: run `/context breakdown` for per-message footprint or `/context compress` for immediate compaction",
                    app.current_model,
                    transcript.len(),
                    approx_tokens,
                    context_files
                ),
            );
        }
        "breakdown" => {
            let transcript = app.transcript_messages();
            if transcript.is_empty() {
                emit_command_output(app, "No transcript yet.");
                return Ok(CommandResult::Handled);
            }
            let mut out = String::from("Context breakdown (recent)\n");
            for (idx, msg) in transcript.iter().enumerate().rev().take(20).rev() {
                let role = match msg.role {
                    hermes_core::MessageRole::User => "USER",
                    hermes_core::MessageRole::Assistant => "HERMES",
                    hermes_core::MessageRole::Tool => "TOOL",
                    hermes_core::MessageRole::System => "SYSTEM",
                };
                let chars = msg.content.as_deref().map(str::len).unwrap_or(0);
                let est_tokens = (chars / 4).max(1);
                let preview = msg
                    .content
                    .as_deref()
                    .unwrap_or("")
                    .lines()
                    .next()
                    .unwrap_or("")
                    .trim();
                let _ = writeln!(
                    out,
                    "{:>3}. {:<7} chars={:<5} tok≈{:<5} {}",
                    idx + 1,
                    role,
                    chars,
                    est_tokens,
                    truncate_chars(preview, 70)
                );
            }
            emit_command_output(app, out.trim_end());
        }
        "compress" | "compact" => {
            return handle_compress_command(app, &[]);
        }
        _ => {
            emit_command_output(
                app,
                "Usage: /context [status|breakdown|compress]\nAlias: /summary -> /recap",
            );
        }
    }
    Ok(CommandResult::Handled)
}

async fn handle_provider_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let providers = provider_slugs_for_config(&app.config);
    if providers.is_empty() {
        emit_command_output(app, "No providers registered.");
        return Ok(CommandResult::Handled);
    }
    let entries = provider_catalog_entries_for_config(&app.config).await;
    if entries.is_empty() {
        emit_command_output(
            app,
            format!(
                "Configured providers: {}\nCurrent model: {}",
                providers.join(", "),
                app.current_model
            ),
        );
        return Ok(CommandResult::Handled);
    }
    let mut out = format!("Current model: {}\n\nProviders:\n", app.current_model);
    for entry in entries {
        let preview = entry.models.join(", ");
        let suffix = if entry.total_models > entry.models.len() {
            format!(" (+{} more)", entry.total_models - entry.models.len())
        } else {
            String::new()
        };
        let _ = writeln!(out, "  - {:<14} {}{}", entry.provider, preview, suffix);
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

async fn handle_auth_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    match action.as_str() {
        "status" => {
            let provider = app.current_runtime_provider();
            let credential_present = crate::app::provider_api_key_from_env(&provider).is_some();
            let state = if credential_present {
                "present"
            } else {
                "missing"
            };
            let gate_line = oauth_runtime_gate_for_provider(&provider)
                .map(|(ok, detail)| {
                    format!(
                        "oauth_runtime_gate: {} ({})",
                        if ok { "PASS" } else { "FAIL" },
                        detail
                    )
                })
                .unwrap_or_else(|| "oauth_runtime_gate: n/a".to_string());
            emit_command_output(
                app,
                format!(
                    "Auth status\nprovider: {}\nmodel: {}\ncredential: {}\n{}\nnext: `/auth verify` (passive refresh check) or `/auth refresh` (forced token refresh)",
                    provider, app.current_model, state, gate_line
                ),
            );
        }
        "verify" => {
            let provider = app.current_runtime_provider();
            if let Some((ok, detail)) = oauth_runtime_gate_for_provider(&provider) {
                if !ok {
                    emit_command_output(
                        app,
                        format!(
                            "Auth verify blocked by OAuth runtime gate for `{}`.\n{}\nUpgrade runtime and retry.",
                            provider, detail
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
            }
            let summary = app.verify_runtime_auth(false).await?;
            emit_command_output(
                app,
                format!(
                    "{}\nnext: if provider rejects again, run `/auth refresh` then retry.",
                    summary
                ),
            );
        }
        "refresh" | "force" => {
            let provider = app.current_runtime_provider();
            if let Some((ok, detail)) = oauth_runtime_gate_for_provider(&provider) {
                if !ok {
                    emit_command_output(
                        app,
                        format!(
                            "Auth refresh blocked by OAuth runtime gate for `{}`.\n{}\nUpgrade runtime and retry.",
                            provider, detail
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
            }
            let summary = app.verify_runtime_auth(true).await?;
            emit_command_output(
                app,
                format!(
                    "{}\nforced refresh complete; retry your request now.",
                    summary
                ),
            );
        }
        _ => emit_command_output(
            app,
            "Usage: /auth [status|verify|refresh]\n- status: show active provider auth state\n- verify: passive credential hydration + verification\n- refresh: force OAuth/session token refresh",
        ),
    }
    Ok(CommandResult::Handled)
}

fn handle_telemetry_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args
        .first()
        .copied()
        .unwrap_or("status")
        .to_ascii_lowercase();
    let provider = app
        .current_model
        .split_once(':')
        .map(|(p, _)| p.to_string())
        .unwrap_or_else(|| "openai".to_string());
    let provider_health =
        hermes_tools::tools::telemetry_snapshot::provider_health_snapshot(&provider);
    let session = app.session_info();
    let mut out = String::new();
    let _ = writeln!(out, "Telemetry snapshot");
    let _ = writeln!(out, "session: {}", session.session_id);
    let _ = writeln!(out, "model: {}", app.current_model);
    let _ = writeln!(out, "messages: {}", session.message_count);
    let _ = writeln!(out, "provider health: {}", provider_health);

    if let Some(repo_root) = hermes_tools::repo::detect_repo_root_from_cwd() {
        let _ = writeln!(
            out,
            "{}",
            hermes_tools::tools::telemetry_snapshot::telemetry_gate_line(&repo_root)
        );
    }

    if action == "lane" {
        let _ = writeln!(out, "lane hints:");
        for hint in hermes_tools::tools::telemetry_snapshot::lane_hints() {
            let _ = writeln!(out, "- {}", hint);
        }
    } else if action != "status" {
        emit_command_output(
            app,
            "Usage: /telemetry [status|lane]\n- status: session/provider + gate snapshots\n- lane: status plus TUI activity-lane controls",
        );
        return Ok(CommandResult::Handled);
    }

    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn handle_runbook_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args.first().copied().unwrap_or("list").to_ascii_lowercase();
    if action == "list" || action == "status" {
        emit_command_output(app, hermes_tools::tools::runbook::render_runbook_list());
        return Ok(CommandResult::Handled);
    }
    if action == "show" {
        let Some(name) = args.get(1).map(|v| v.to_ascii_lowercase()) else {
            emit_command_output(app, "Usage: /runbook show <name>");
            return Ok(CommandResult::Handled);
        };
        let Some(runbook) = hermes_tools::tools::runbook::find_runbook(&name) else {
            emit_command_output(
                app,
                format!(
                    "Unknown runbook `{}`. Use `/runbook list` for available entries.",
                    name
                ),
            );
            return Ok(CommandResult::Handled);
        };
        emit_command_output(app, hermes_tools::tools::runbook::render_runbook(runbook));
        return Ok(CommandResult::Handled);
    }
    emit_command_output(app, "Usage: /runbook [list|show <name>]");
    Ok(CommandResult::Handled)
}

fn handle_profile_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let home = hermes_config::hermes_home();
    let selected = app.config.profile.current.as_deref().unwrap_or("default");
    let mut out = String::new();
    let _ = writeln!(out, "Active profile: {}", selected);
    let _ = writeln!(out, "Hermes home: {}", home.display());
    let _ = writeln!(out, "Session id: {}", app.session_id);
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

fn handle_runtime_ui_mode_command(
    app: &mut App,
    cmd: &str,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let msg = match cmd {
        "/skin" => {
            let first = args.first().copied().unwrap_or("status");
            if first.eq_ignore_ascii_case("list") {
                let active = std::env::var("HERMES_THEME").unwrap_or_else(|_| "ultra-neon".to_string());
                let active_canonical = canonical_skin_name(&active).unwrap_or("ultra-neon");
                let mut out = String::new();
                let _ = writeln!(out, "Built-in skins (active: {}):", active_canonical);
                for (name, detail) in BUILTIN_SKINS {
                    let marker = if *name == active_canonical { "✓" } else { " " };
                    let _ = writeln!(out, "  {} {:<30} {}", marker, name, detail);
                }
                let _ = writeln!(
                    out,
                    "\nUse `/skin <name>` or `/skin set <name>` to switch immediately."
                );
                out.trim_end().to_string()
            } else if first.eq_ignore_ascii_case("status") || first.eq_ignore_ascii_case("show") {
                let active = std::env::var("HERMES_THEME").unwrap_or_else(|_| "ultra-neon".to_string());
                let active_canonical = canonical_skin_name(&active).unwrap_or("ultra-neon");
                format!(
                    "Current skin: {}\nUse `/skin list` to browse options.\nUse `/skin <name>` to switch now.",
                    active_canonical
                )
            } else {
                let requested = if first.eq_ignore_ascii_case("set") {
                    args.get(1).copied().unwrap_or("")
                } else {
                    first
                };
                if requested.trim().is_empty() {
                    "Usage: `/skin list` or `/skin <name>`".to_string()
                } else if let Some(canonical) = canonical_skin_name(requested) {
                    std::env::set_var("HERMES_THEME", canonical);
                    app.request_theme_change(canonical);
                    format!(
                        "Skin switched to `{}`.\nApplied in this TUI session and exported as HERMES_THEME for child processes.",
                        canonical
                    )
                } else {
                    format!(
                        "Unknown skin `{}`. Use `/skin list` for built-ins.",
                        requested
                    )
                }
            }
        }
        "/fast" => format!(
            "Fast mode compatibility command received (`{}`).\nCurrent model: {}\nTip: switch to a lower-latency model via `/model`.",
            args.first().copied().unwrap_or("status"),
            app.current_model
        ),
        "/timestamps" => "Transcript timestamps are controlled by the interactive TUI. Use `/timestamps [on|off|toggle|status]` or Ctrl+T while the TUI is active.".to_string(),
        "/voice" => "Voice mode uses provider/platform capabilities; no separate TUI voice engine is active in this session.".to_string(),
        _ => "Unsupported runtime UI mode command.".to_string(),
    };
    emit_command_output(app, msg);
    Ok(CommandResult::Handled)
}

fn render_pet_status(settings: &PetSettings) -> String {
    format!(
        "Pet status:\n  - enabled: {}\n  - species: {}\n  - mood: {}\n  - dock: {}\n  - speed_ms: {}\n\nUse `/pet on`, `/pet off`, `/pet toggle`, `/pet set <species>`, `/pet mood <mood>`, `/pet dock <left|right>`, `/pet speed <ms>`, `/pet list`.",
        if settings.enabled { "ON" } else { "OFF" },
        settings.species,
        settings.mood,
        settings.dock.as_str(),
        settings.tick_ms
    )
}

fn parse_pet_species(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    PetSettings::species_catalog()
        .iter()
        .find(|candidate| **candidate == normalized)
        .map(|candidate| (*candidate).to_string())
}

fn parse_pet_mood(value: &str) -> Option<String> {
    let normalized = value.trim().to_ascii_lowercase();
    PetSettings::mood_catalog()
        .iter()
        .find(|candidate| **candidate == normalized)
        .map(|candidate| (*candidate).to_string())
}

fn parse_pet_dock(value: &str) -> Option<PetDock> {
    let normalized = value.trim().to_ascii_lowercase();
    match normalized.as_str() {
        "left" => Some(PetDock::Left),
        "right" => Some(PetDock::Right),
        _ => None,
    }
}

fn handle_pet_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let action = args.first().copied().unwrap_or("status");
    let mut settings = app.pet_settings().clone();

    match action.to_ascii_lowercase().as_str() {
        "status" => {
            emit_command_output(app, render_pet_status(&settings));
        }
        "list" => {
            emit_command_output(
                app,
                format!(
                    "Available pets:\n  - species: {}\n  - moods: {}\n  - dock: left, right",
                    PetSettings::species_catalog().join(", "),
                    PetSettings::mood_catalog().join(", ")
                ),
            );
        }
        "on" | "off" | "toggle" | "wake" | "sleep" | "tuck" => {
            let action_lc = action.to_ascii_lowercase();
            let normalized_toggle = match action_lc.as_str() {
                "wake" => Some("on"),
                "sleep" | "tuck" => Some("off"),
                other => Some(other),
            };
            match parse_toggle_arg(normalized_toggle, settings.enabled) {
                Ok(enabled) => {
                    settings.enabled = enabled;
                    app.set_pet_settings(settings.clone())?;
                    emit_command_output(
                        app,
                        format!(
                            "Pet {}.\n{}",
                            if settings.enabled { "enabled" } else { "hidden" },
                            render_pet_status(&settings)
                        ),
                    );
                }
                Err(_) => emit_command_output(
                    app,
                    "Usage: /pet [status|on|off|toggle|wake|tuck|list|set <species>|mood <mood>|dock <left|right>|speed <ms>]",
                ),
            }
        }
        "set" | "species" => {
            let Some(raw) = args.get(1).copied() else {
                emit_command_output(
                    app,
                    format!(
                        "Usage: /pet set <species>\nAvailable species: {}",
                        PetSettings::species_catalog().join(", ")
                    ),
                );
                return Ok(CommandResult::Handled);
            };
            if let Some(species) = parse_pet_species(raw) {
                settings.species = species;
                app.set_pet_settings(settings.clone())?;
                emit_command_output(app, render_pet_status(&settings));
            } else {
                emit_command_output(
                    app,
                    format!(
                        "Unknown species '{}'. Available: {}",
                        raw,
                        PetSettings::species_catalog().join(", ")
                    ),
                );
            }
        }
        "mood" => {
            let Some(raw) = args.get(1).copied() else {
                emit_command_output(
                    app,
                    format!(
                        "Usage: /pet mood <mood>\nAvailable moods: {}",
                        PetSettings::mood_catalog().join(", ")
                    ),
                );
                return Ok(CommandResult::Handled);
            };
            if let Some(mood) = parse_pet_mood(raw) {
                settings.mood = mood;
                app.set_pet_settings(settings.clone())?;
                emit_command_output(app, render_pet_status(&settings));
            } else {
                emit_command_output(
                    app,
                    format!(
                        "Unknown mood '{}'. Available: {}",
                        raw,
                        PetSettings::mood_catalog().join(", ")
                    ),
                );
            }
        }
        "dock" => {
            let Some(raw) = args.get(1).copied() else {
                emit_command_output(app, "Usage: /pet dock <left|right>");
                return Ok(CommandResult::Handled);
            };
            if let Some(dock) = parse_pet_dock(raw) {
                settings.dock = dock;
                app.set_pet_settings(settings.clone())?;
                emit_command_output(app, render_pet_status(&settings));
            } else {
                emit_command_output(app, "Usage: /pet dock <left|right>");
            }
        }
        "speed" => {
            let Some(raw) = args.get(1).copied() else {
                emit_command_output(app, "Usage: /pet speed <ms>");
                return Ok(CommandResult::Handled);
            };
            match raw.trim().parse::<u64>() {
                Ok(ms) => {
                    settings.tick_ms = ms;
                    app.set_pet_settings(settings.clone())?;
                    emit_command_output(app, render_pet_status(&settings));
                }
                Err(_) => emit_command_output(app, "Usage: /pet speed <ms>"),
            }
        }
        _ => emit_command_output(
            app,
            "Usage: /pet [status|on|off|toggle|wake|tuck|list|set <species>|mood <mood>|dock <left|right>|speed <ms>]",
        ),
    }

    Ok(CommandResult::Handled)
}

fn handle_toolsets_command(app: &mut App) -> Result<CommandResult, AgentError> {
    if app.config.platform_toolsets.is_empty() {
        emit_command_output(app, "No explicit platform toolsets configured.");
        return Ok(CommandResult::Handled);
    }
    let mut rows: Vec<_> = app.config.platform_toolsets.iter().collect();
    rows.sort_by(|a, b| a.0.cmp(b.0));
    let mut out = String::from("Configured toolsets by platform:\n");
    for (platform, toolsets) in rows {
        let _ = writeln!(out, "  - {:<10} {}", platform, toolsets.join(", "));
    }
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

include!("skills_memory_cron.rs");
