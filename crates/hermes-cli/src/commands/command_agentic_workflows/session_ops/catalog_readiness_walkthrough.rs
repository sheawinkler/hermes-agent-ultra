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
