//! Slash-command catalog rendering and misc catalog handlers.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::io::Write as _;
use std::path::PathBuf;

use hermes_core::AgentError;

use super::autocomplete::{SLASH_COMMANDS, help_for};
use super::{CommandResult, emit_command_output};
use crate::app::App;

pub(crate) fn provider_health_snapshot(provider: &str) -> &'static str {
    match provider.trim().to_ascii_lowercase().as_str() {
        "nous" | "google-gemini-cli" | "gemini-cli" | "gemini-oauth" | "qwen-oauth" => {
            "oauth-capable"
        }
        "openai" | "anthropic" | "openrouter" => "api-key/session",
        _ => "unknown",
    }
}

pub(crate) fn detect_repo_root_from_cwd() -> Option<PathBuf> {
    let cwd = std::env::current_dir().ok()?;
    for candidate in cwd.ancestors() {
        if candidate.join(".git").exists() {
            return Some(candidate.to_path_buf());
        }
    }
    None
}

pub(crate) fn handle_experiment_command(
    host: &mut impl crate::app::SlashCommandHost,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        let active = super::objective::current_session_steer(host)
            .filter(|value| value.to_ascii_lowercase().starts_with("experiment: "))
            .map(|value| value.trim_start_matches("Experiment: ").to_string())
            .unwrap_or_else(|| "(none)".to_string());
        emit_command_output(
            host,
            format!(
                "Experiment steering: {}\nUsage: /experiment <label or instruction> | /experiment clear",
                active
            ),
        );
        return Ok(CommandResult::Handled);
    }
    if args[0].eq_ignore_ascii_case("clear") {
        let active = super::objective::current_session_steer(host)
            .map(|value| value.to_ascii_lowercase().starts_with("experiment: "))
            .unwrap_or(false);
        if active {
            super::objective::set_session_steer(host, None);
            emit_command_output(host, "Cleared experiment steering context.");
        } else {
            emit_command_output(
                host,
                "No experiment steering context active. Use `/experiment <instruction>`.",
            );
        }
        return Ok(CommandResult::Handled);
    }
    let hint = args.join(" ").trim().to_string();
    if hint.is_empty() {
        emit_command_output(
            host,
            "Usage: /experiment <label or instruction> | /experiment clear",
        );
        return Ok(CommandResult::Handled);
    }
    let steer = format!("Experiment: {hint}");
    super::objective::set_session_steer(host, Some(steer.clone()));
    emit_command_output(
        host,
        format!(
            "Experiment steering applied.\n{}\nUse `/model` to switch variants, then `/retry` to re-run the last turn.",
            steer
        ),
    );
    Ok(CommandResult::Handled)
}

pub(crate) fn feedback_log_path() -> PathBuf {
    hermes_config::hermes_home()
        .join("logs")
        .join("feedback.ndjson")
}

pub(crate) fn handle_feedback_command(
    host: &mut impl crate::app::SlashCommandHost,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        emit_command_output(
            host,
            "Usage: /feedback <note>\nStores a local feedback record at ~/.hermes-agent-ultra/logs/feedback.ndjson.",
        );
        return Ok(CommandResult::Handled);
    }
    let note = args.join(" ").trim().to_string();
    if note.is_empty() {
        emit_command_output(host, "Usage: /feedback <note>");
        return Ok(CommandResult::Handled);
    }
    let path = feedback_log_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("Failed to create {}: {}", parent.display(), e)))?;
    }
    let record = serde_json::json!({
        "at": chrono::Utc::now().to_rfc3339(),
        "session_id": host.session_id(),
        "model": host.current_model(),
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
    emit_command_output(host, format!("Feedback captured in {}", path.display()));
    Ok(CommandResult::Handled)
}

pub(crate) fn handle_restart_command(
    host: &mut impl crate::app::SlashCommandHost,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let preserve_model = args.first().is_some_and(|v| {
        matches!(
            v.to_ascii_lowercase().as_str(),
            "keep-model" | "--keep-model"
        )
    });
    let previous_model = host.current_model().to_string();
    host.new_session();
    if preserve_model && !previous_model.eq_ignore_ascii_case(host.current_model()) {
        host.switch_model(&previous_model);
    }
    emit_command_output(
        host,
        format!(
            "Session restarted.\n  new_session_id: {}\n  model: {}",
            host.session_id(),
            host.current_model()
        ),
    );
    Ok(CommandResult::Handled)
}

pub(crate) async fn handle_update_command(
    host: &mut impl crate::app::SlashCommandHost,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
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
    if !check_only {
        let _ = writeln!(out, "\nTo perform the update, exit and run: hermes update");
    }
    emit_command_output(host, out.trim_end());
    Ok(CommandResult::Handled)
}

#[derive(Debug, Clone, Copy)]
struct CommandCatalogSection {
    title: &'static str,
    hint: &'static str,
    commands: &'static [&'static str],
}

const COMMAND_CATALOG_SECTIONS: &[CommandCatalogSection] = &[
    CommandCatalogSection {
        title: "Core Session",
        hint: "Session lifecycle, snapshots, rollback, and queue controls",
        commands: &[
            "/new",
            "/reset",
            "/retry",
            "/undo",
            "/history",
            "/recap",
            "/context",
            "/title",
            "/branch",
            "/timetravel",
            "/snapshot",
            "/rollback",
            "/queue",
            "/background",
            "/save",
            "/load",
            "/resume",
            "/sessions",
        ],
    },
    CommandCatalogSection {
        title: "Model/Auth",
        hint: "Provider, model, auth, and reasoning controls",
        commands: &[
            "/model",
            "/provider",
            "/auth",
            "/reasoning",
            "/gquota",
            "/qos",
            "/boot",
            "/walkthrough",
        ],
    },
    CommandCatalogSection {
        title: "Objective/Planning",
        hint: "Mission steering, objectives, planning, and simulation",
        commands: &[
            "/objective",
            "/goal",
            "/subgoal",
            "/plan",
            "/plan-mode",
            "/ask",
            "/steer",
            "/btw",
            "/simulate",
            "/specpatch",
            "/quorum",
            "/mission",
            "/autopilot",
            "/triage",
            "/subconscious",
        ],
    },
    CommandCatalogSection {
        title: "Tools/Skills/Integrations",
        hint: "Skills, tools, MCP, gateway adapters, and integration health",
        commands: &[
            "/skills",
            "/tools",
            "/toolcards",
            "/toolsets",
            "/plugins",
            "/mcp",
            "/platforms",
            "/integrations",
            "/reload",
            "/reload-mcp",
            "/runbook",
            "/ops",
            "/telemetry",
            "/dashboard",
        ],
    },
    CommandCatalogSection {
        title: "UX/Views",
        hint: "TUI surface controls and visibility toggles",
        commands: &[
            "/skin",
            "/voice",
            "/pet",
            "/image",
            "/mouse",
            "/verbose",
            "/statusbar",
            "/raw",
            "/redraw",
            "/copy",
            "/paste",
            "/commands",
            "/help",
            "/quit",
        ],
    },
];

fn command_catalog_matches_filter(command: &str, description: &str, query: &str) -> bool {
    let q = query.trim().to_ascii_lowercase();
    if q.is_empty() {
        return true;
    }
    let cmd = command.to_ascii_lowercase();
    let desc = description.to_ascii_lowercase();
    cmd.contains(&q) || desc.contains(&q.trim_start_matches('/'))
}

fn render_command_catalog(filter: Option<&str>) -> String {
    let query = filter.unwrap_or("").trim();
    let mut seen = HashSet::new();
    let mut out = String::new();
    out.push_str("Hermes Agent Ultra — Slash Command Palette\n");
    out.push_str("==========================================\n");
    if query.is_empty() {
        out.push_str(
            "Tip: type `/` in the composer to open completions and use arrows/Tab/Enter.\n",
        );
        out.push_str("Scoped search: `/commands <term>` (example: `/commands auth`).\n");
    } else {
        let _ = writeln!(out, "Filter: `{}`", query);
    }
    out.push('\n');

    for section in COMMAND_CATALOG_SECTIONS {
        let mut rendered = 0usize;
        for command in section.commands {
            let Some(description) = help_for(command) else {
                continue;
            };
            if !command_catalog_matches_filter(command, description, query) {
                continue;
            }
            if rendered == 0 {
                let _ = writeln!(out, "## {}\n{}\n", section.title, section.hint);
            }
            let _ = writeln!(out, "- `{:<16}` {}", command, description);
            seen.insert(*command);
            rendered += 1;
        }
        if rendered > 0 {
            out.push('\n');
        }
    }

    let mut extras = Vec::new();
    for (command, description) in SLASH_COMMANDS {
        if seen.contains(command) {
            continue;
        }
        if command_catalog_matches_filter(command, description, query) {
            extras.push((*command, *description));
        }
    }
    if !extras.is_empty() {
        out.push_str("## Other\nCommands that are available but not in the primary sections.\n\n");
        extras.sort_by(|a, b| a.0.cmp(b.0));
        for (command, description) in extras {
            let _ = writeln!(out, "- `{:<16}` {}", command, description);
        }
        out.push('\n');
    }
    out.push_str("You can also type plain text to send a normal chat message.");
    out
}

pub(crate) fn handle_commands_catalog_command(
    host: &mut impl crate::app::SlashCommandHost,
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
    emit_command_output(host, render_command_catalog(query.as_deref()));
    Ok(CommandResult::Handled)
}

pub(crate) fn print_help(host: &mut impl crate::app::SlashCommandHost) {
    emit_command_output(host, render_command_catalog(None));
}
