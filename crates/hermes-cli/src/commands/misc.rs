//! Miscellaneous slash command handlers (extracted from `mod.rs`).
//!
//! Small/medium `/` commands that don't warrant their own module.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono;
use hermes_agent;
use hermes_config::LlmProviderConfig;
use hermes_core::AgentError;
use hermes_core::MessageRole;
use hermes_skills;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::alpha_runtime::utility_terms_from_contract;
use crate::commands::background;
use crate::commands::compress;
use crate::commands::model::{
    gemini_thinking_level_for_effort, openai_reasoning_effort_for_level, resolve_provider_key,
    split_provider_model,
};
use crate::commands::session;
use crate::commands::{CommandResult, emit_command_output, truncate_chars, yes_no};
use crate::model_switch::{curated_provider_slugs, provider_catalog_entries};
use crate::{App, env_vars};

// ---------------------------------------------------------------------------
// /toolcards
// ---------------------------------------------------------------------------

pub(crate) fn handle_toolcards_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let action = args.first().copied().unwrap_or("help");
    let msg = match action {
        "export" => {
            "Tool-card export is handled by the interactive TUI modal loop. In TUI, run `/toolcards export` to write `~/.hermes-agent-ultra/logs/toolcards-export.txt`.".to_string()
        }
        _ => "Tool-card controls:\n  /toolcards export   Export current tool-card transcript".to_string(),
    };
    emit_command_output(app, msg);
    Ok(CommandResult::Handled)
}

// ---------------------------------------------------------------------------
// /personality
// ---------------------------------------------------------------------------

pub(crate) fn handle_personality_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let builtin = hermes_agent::builtin_personality_names();
    let builtin_descriptions = hermes_agent::builtin_personality_descriptions();
    if args.is_empty() {
        emit_command_output(
            app,
            super::format_personality_catalog(
                app.current_personality.as_deref(),
                builtin_descriptions,
            ),
        );
    } else if args.len() == 1 && args[0].eq_ignore_ascii_case("list") {
        emit_command_output(
            app,
            super::format_personality_catalog(
                app.current_personality.as_deref(),
                builtin_descriptions,
            ),
        );
    } else {
        let name = args.join(" ");
        app.switch_personality(&name);
        let mut response = format!("Switched personality to `{}`.", name);
        if !name.contains(char::is_whitespace)
            && !name.eq_ignore_ascii_case("default")
            && !builtin.iter().any(|n| n.eq_ignore_ascii_case(&name))
        {
            response.push_str(&format!(
                "\n\nNote: `{}` is not built-in. Hermes will look for `personalities/{}.md` or treat inline text as compatibility mode.",
                name, name,
            ));
        }
        emit_command_output(app, response);
    }
    Ok(CommandResult::Handled)
}

// ---------------------------------------------------------------------------
// /curator
// ---------------------------------------------------------------------------

pub(crate) async fn handle_curator_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let skills_dir = hermes_config::hermes_home().join("skills");
    let curator_config = curator_config_from_app(app);

    let sub = args.first().map(|s| s.to_lowercase()).unwrap_or_default();

    match sub.as_str() {
        "status" | "" => {
            let rows = hermes_skills::agent_created_report(&skills_dir);
            let state = hermes_skills::load_curator_state(&skills_dir);

            if rows.is_empty() {
                let mut out = String::from("No agent-created skills found.\n\n");
                out.push_str(&format!(
                    "curator: {}\n",
                    curator_status_label(&curator_config, &state)
                ));
                out.push_str(&format!(
                    "  interval: every {}h\n",
                    curator_config.interval_hours
                ));
                out.push_str(&format!(
                    "  stale after: {}d\n",
                    curator_config.stale_after_days
                ));
                out.push_str(&format!(
                    "  archive after: {}d\n",
                    curator_config.archive_after_days
                ));
                if let Some(countdown) = next_run_countdown(&state, &curator_config) {
                    out.push_str(&format!("  next run eligible: {}\n", countdown));
                }
                out.push_str(
                    "\nSkills created by the agent during background review will appear here.",
                );
                emit_command_output(app, &out);
            } else {
                let mut out = format!("## Agent-created skills ({})\n\n", rows.len());

                // Most active top 5
                let mut sorted_by_activity: Vec<_> = rows.iter().collect();
                sorted_by_activity.sort_by_key(|r| -(r.activity_count as i64));
                let top_active: Vec<_> = sorted_by_activity.iter().take(5).collect();
                if !top_active.is_empty() {
                    out.push_str("**Most active:**\n");
                    for row in &top_active {
                        let pin_mark = if row.pinned { "📌 " } else { "  " };
                        let _ = writeln!(
                            out,
                            "{}`{}` activity={} state={}",
                            pin_mark, row.name, row.activity_count, row.state
                        );
                    }
                    out.push('\n');
                }

                // All rows (with latest_active sort)
                out.push_str("### All agent-created skills\n\n");
                for row in &rows {
                    let pin_mark = if row.pinned { "📌 " } else { "  " };
                    let _ = writeln!(
                        out,
                        "{}`{}` activity={} state={}",
                        pin_mark, row.name, row.activity_count, row.state
                    );
                }

                out.push('\n');
                out.push_str(&format!(
                    "curator: {} interval: every {}h\n",
                    curator_status_label(&curator_config, &state),
                    curator_config.interval_hours
                ));
                if let Some(countdown) = next_run_countdown(&state, &curator_config) {
                    out.push_str(&format!("next run eligible: {}\n", countdown));
                }

                out.push_str(
                    "\nUse `/curator run` to run the curator manually.\nUse `/curator history` to view run history.",
                );
                emit_command_output(app, out.trim_end());
            }
        }
        "run" => {
            let dry_run = args
                .get(1)
                .is_some_and(|s| s.eq_ignore_ascii_case("--dry-run"));
            let before_state = hermes_skills::load_curator_state(&skills_dir);
            if dry_run {
                let result =
                    hermes_skills::apply_automatic_transitions(&skills_dir, &curator_config);
                let report_text = format!(
                    "Curator dry-run: checked={} stale={} archived={} reactivated={}",
                    result.checked, result.marked_stale, result.archived, result.reactivated
                );
                emit_command_output(app, report_text);
                return Ok(CommandResult::Handled);
            }

            // Run the curator
            let result = hermes_skills::apply_automatic_transitions(&skills_dir, &curator_config);
            let report_text = format!(
                "Curator run: checked={} stale={} archived={} reactivated={}",
                result.checked, result.marked_stale, result.archived, result.reactivated
            );
            emit_command_output(app, report_text);
            let after_state = hermes_skills::load_curator_state(&skills_dir);
            // Detect if a backup was created during curator run (state changed)
            if before_state.last_run_at != after_state.last_run_at {
                emit_command_output(app, "\n[Curator state updated]");
            }
        }
        "history" => {
            let state = hermes_skills::load_curator_state(&skills_dir);
            if state.run_count == 0 {
                emit_command_output(app, "No curator run history yet.");
            } else {
                let mut out = String::from("Curator run history\n\n");
                let _ = writeln!(out, "run_count: {}", state.run_count);
                if let Some(ref last) = state.last_run_at {
                    let _ = writeln!(out, "last_run_at: {}", last);
                }
                if let Some(ref summary) = state.last_run_summary {
                    let _ = writeln!(out, "last_summary: {}", truncate_chars(summary, 160));
                }
                emit_command_output(app, out.trim_end());
            }
        }
        "backup" => {
            let sub = args.get(1).map(|s| s.to_ascii_lowercase());
            match sub.as_deref() {
                Some("create") | None => match backup_skills(&skills_dir) {
                    Ok(path) => {
                        emit_command_output(app, format!("Backup created at {}", path.display()));
                    }
                    Err(e) => {
                        emit_command_output(app, format!("Backup failed: {}", e));
                    }
                },
                Some("list") => match list_backups(&skills_dir) {
                    Ok(backups) => {
                        if backups.is_empty() {
                            emit_command_output(app, "No curator backups found.");
                        } else {
                            let mut out = String::from("Curator backups\n");
                            for (name, _) in &backups {
                                let _ = writeln!(out, "- {}", name);
                            }
                            emit_command_output(app, out.trim_end());
                        }
                    }
                    Err(e) => {
                        emit_command_output(app, format!("Failed to list backups: {}", e));
                    }
                },
                Some("rollback") => {
                    let Some(backup_name) = args.get(2) else {
                        emit_command_output(app, "Usage: /curator backup rollback <backup-name>");
                        return Ok(CommandResult::Handled);
                    };
                    match rollback_skills(&skills_dir, backup_name) {
                        Ok(()) => {
                            emit_command_output(
                                app,
                                format!("Rolled back to backup `{}`.", backup_name),
                            );
                        }
                        Err(e) => {
                            emit_command_output(app, format!("Rollback failed: {}", e));
                        }
                    }
                }
                Some(other) => {
                    emit_command_output(
                        app,
                        format!(
                            "Unknown backup subcommand '{}'. Use create, list, or rollback.",
                            other
                        ),
                    );
                }
            }
        }
        other => {
            emit_command_output(
                app,
                format!(
                    "Unknown curator subcommand '{}'. Try: status, run, history, backup.",
                    other
                ),
            );
        }
    }
    Ok(CommandResult::Handled)
}

fn curator_config_from_app(app: &App) -> hermes_skills::CuratorConfig {
    let gc = &app.config.curator;
    hermes_skills::CuratorConfig {
        enabled: gc.enabled,
        interval_hours: gc.interval_hours,
        min_idle_hours: gc.min_idle_hours,
        stale_after_days: gc.stale_after_days,
        archive_after_days: gc.archive_after_days,
        prune_builtins: gc.prune_builtins,
    }
}

fn curator_status_label(
    config: &hermes_skills::CuratorConfig,
    state: &hermes_skills::CuratorState,
) -> &'static str {
    if state.paused {
        "PAUSED"
    } else if config.enabled {
        "ENABLED"
    } else {
        "DISABLED"
    }
}

fn next_run_countdown(
    state: &hermes_skills::CuratorState,
    config: &hermes_skills::CuratorConfig,
) -> Option<String> {
    if !config.enabled || state.paused {
        return None;
    }
    let last = state.last_run_at.as_ref()?;
    let last_dt: chrono::DateTime<chrono::Utc> = last.parse().ok()?;
    let interval = chrono::Duration::seconds((config.interval_hours * 3600) as i64);
    let eligible = last_dt + interval;
    let now = chrono::Utc::now();
    if now >= eligible {
        Some("now".to_string())
    } else {
        let remaining = eligible - now;
        let hours = remaining.num_hours();
        let mins = (remaining.num_minutes() % 60).abs();
        if hours > 0 {
            Some(format!("in ~{}h {}m", hours, mins))
        } else {
            Some(format!("in ~{}m", mins))
        }
    }
}

#[allow(dead_code)]
fn build_curator_run_report(
    record: &hermes_skills::CuratorRunRecord,
    model: Option<String>,
    provider: Option<String>,
) -> hermes_skills::CuratorRunReport {
    let before_count = 0u64;
    let after_count = 0u64;
    let consolidated_count = 0u64;
    let pruned_count = 0u64;
    let transitions = record.auto_transitions.checked
        + record.auto_transitions.marked_stale
        + record.auto_transitions.archived
        + record.auto_transitions.reactivated;
    let tool_calls_total = record
        .llm_review
        .as_ref()
        .map_or(0, |r| r.tool_calls.len() as u64);

    hermes_skills::CuratorRunReport {
        started_at: record.started_at.clone(),
        duration_seconds: record.duration_seconds,
        model: model.or_else(|| record.model.clone()),
        provider: provider.or_else(|| record.provider.clone()),
        dry_run: record.dry_run,
        auto_transitions: record.auto_transitions.clone(),
        counts: hermes_skills::CuratorRunCounts {
            before: before_count,
            after: after_count,
            delta: (after_count as i64) - (before_count as i64),
            archived_this_run: record.auto_transitions.archived,
            consolidated_this_run: consolidated_count,
            pruned_this_run: pruned_count,
            state_transitions: transitions,
            tool_calls_total,
        },
        consolidated: vec![],
        pruned: vec![],
        tool_calls: record
            .llm_review
            .as_ref()
            .map_or(vec![], |r| r.tool_calls.clone()),
        llm_error: None,
    }
}

#[allow(dead_code)]
fn build_curator_run_report_from_transitions(
    result: &hermes_skills::TransitionResult,
) -> hermes_skills::CuratorRunReport {
    let transitions = result.checked + result.marked_stale + result.archived + result.reactivated;
    hermes_skills::CuratorRunReport {
        started_at: chrono::Utc::now().to_rfc3339(),
        duration_seconds: 0.0,
        model: None,
        provider: None,
        dry_run: false,
        auto_transitions: result.clone(),
        counts: hermes_skills::CuratorRunCounts {
            before: 0,
            after: 0,
            delta: 0,
            archived_this_run: result.archived,
            consolidated_this_run: 0,
            pruned_this_run: 0,
            state_transitions: transitions,
            tool_calls_total: 0,
        },
        consolidated: vec![],
        pruned: vec![],
        tool_calls: vec![],
        llm_error: None,
    }
}

fn backup_skills(skills_dir: &std::path::Path) -> Result<std::path::PathBuf, std::io::Error> {
    let backup_root = skills_dir.join(".curator_backups");
    std::fs::create_dir_all(&backup_root)?;
    let ts = chrono::Utc::now().format("%Y%m%d-%H%M%S").to_string();
    let backup_dir = backup_root.join(&ts);

    if backup_dir.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("backup directory already exists: {}", backup_dir.display()),
        ));
    }

    std::fs::create_dir_all(&backup_dir)?;
    for entry in std::fs::read_dir(skills_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == ".curator_backups"
            || name_str == ".archive"
            || name_str.starts_with(".curator_state")
        {
            continue;
        }
        let dest = backup_dir.join(&name);
        if entry.path().is_dir() {
            copy_dir_recursive(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), &dest)?;
        }
    }

    tracing::info!("curator: backup created at {}", backup_dir.display());
    Ok(backup_dir)
}

fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> Result<(), std::io::Error> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dest = dst.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir_recursive(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), &dest)?;
        }
    }
    Ok(())
}

fn list_backups(
    skills_dir: &std::path::Path,
) -> Result<Vec<(String, std::path::PathBuf)>, std::io::Error> {
    let backup_root = skills_dir.join(".curator_backups");
    if !backup_root.exists() {
        return Ok(vec![]);
    }
    let mut backups = Vec::new();
    for entry in std::fs::read_dir(&backup_root)? {
        let entry = entry?;
        if entry.path().is_dir() {
            let name = entry.file_name().to_string_lossy().to_string();
            backups.push((name, entry.path()));
        }
    }
    backups.sort_by(|a, b| b.0.cmp(&a.0));
    Ok(backups)
}

fn rollback_skills(skills_dir: &std::path::Path, backup_name: &str) -> Result<(), std::io::Error> {
    let backup_dir = skills_dir.join(".curator_backups").join(backup_name);
    if !backup_dir.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("backup not found: {}", backup_name),
        ));
    }

    for entry in std::fs::read_dir(skills_dir)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str == ".curator_backups"
            || name_str == ".archive"
            || name_str.starts_with(".curator_state")
        {
            continue;
        }
        if entry.path().is_dir() {
            std::fs::remove_dir_all(entry.path())?;
        } else {
            std::fs::remove_file(entry.path())?;
        }
    }

    for entry in std::fs::read_dir(&backup_dir)? {
        let entry = entry?;
        let dest = skills_dir.join(entry.file_name());
        if entry.path().is_dir() {
            copy_dir_recursive(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), &dest)?;
        }
    }

    tracing::info!("curator: rolled back to backup {}", backup_name);
    Ok(())
}

// ---------------------------------------------------------------------------
// /tools
// ---------------------------------------------------------------------------

pub(crate) fn handle_tools_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    if args
        .first()
        .is_some_and(|sub| sub.eq_ignore_ascii_case("trust"))
    {
        let counters = app.tool_registry.policy_counters();
        let tools = app.tool_registry.list_tools();
        let mut risk: Vec<(String, i32, String)> = tools
            .iter()
            .map(|tool| {
                let mut score = 100i32;
                if !tool.env_deps.is_empty() {
                    score -= 15;
                }
                if matches!(
                    tool.name.as_str(),
                    "terminal" | "execute_code" | "shell_exec" | "bash" | "python_exec"
                ) {
                    score -= 35;
                }
                if tool.toolset.eq_ignore_ascii_case("network")
                    || tool.name.contains("webhook")
                    || tool.name.contains("http")
                {
                    score -= 20;
                }
                if tool.name.contains("secrets")
                    || tool.name.contains("token")
                    || tool.name.contains("oauth")
                {
                    score -= 25;
                }
                score = score.clamp(0, 100);
                let tier = if score >= 80 {
                    "low-risk"
                } else if score >= 55 {
                    "moderate-risk"
                } else {
                    "high-risk"
                };
                (tool.name.clone(), score, tier.to_string())
            })
            .collect();
        risk.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        let mut out = String::new();
        out.push_str("Tool trust scorecard (heuristic)\n");
        out.push_str("--------------------------------\n");
        let _ = writeln!(
            out,
            "policy_counters: allow={} deny={} audit_only={} simulate={} would_block={}",
            counters.allow,
            counters.deny,
            counters.audit_only,
            counters.simulate,
            counters.would_block
        );
        let _ = writeln!(out, "registered_tools={}", risk.len());
        for (name, score, tier) in risk.iter().take(20) {
            let _ = writeln!(out, "- {name:<28} score={score:>3} tier={tier}");
        }
        out.push_str("\nUse `/ops status` and `/raw trace verify` for live enforcement + trace integrity signals.");
        emit_command_output(app, out.trim_end());
        return Ok(CommandResult::Handled);
    }

    let tools = app.tool_registry.list_tools();
    if tools.is_empty() {
        emit_command_output(app, "No tools registered.");
    } else {
        let mut out = format!("Registered tools ({}):\n", tools.len());
        for tool in &tools {
            out.push_str(&format!("- `{}` — {}\n", tool.name, tool.description));
        }
        out.push_str("\n\nUse `/tools trust` for a risk/score summary.");
        emit_command_output(app, out.trim_end());
    }
    Ok(CommandResult::Handled)
}

// ---------------------------------------------------------------------------
// /config
// ---------------------------------------------------------------------------

pub(crate) fn handle_config_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        let config_json = serde_json::to_string_pretty(&*app.config)
            .unwrap_or_else(|e| format!("<serialization error: {}>", e));
        emit_command_output(app, config_json);
    } else {
        match args[0] {
            "get" => {
                if args.len() < 2 {
                    emit_command_output(app, "Usage: /config get <key>");
                } else {
                    let key = args[1];
                    let value = get_config_value(app, key);
                    match value {
                        Some(v) => emit_command_output(app, format!("{} = {}", key, v)),
                        None => emit_command_output(
                            app,
                            format!("Key '{}' not found in configuration.", key),
                        ),
                    }
                }
            }
            "set" => {
                if args.len() < 3 {
                    emit_command_output(app, "Usage: /config set <key> <value>");
                } else {
                    let key = args[1];
                    let value = args[2..].join(" ");
                    if set_config_value(app, key, &value) {
                        emit_command_output(app, format!("Set {} = {}", key, value));
                    } else {
                        emit_command_output(app, format!("Unknown configuration key: {}", key));
                    }
                }
            }
            _ => {
                emit_command_output(
                    app,
                    format!("Unknown config action '{}'. Use 'get' or 'set'.", args[0]),
                );
            }
        }
    }
    Ok(CommandResult::Handled)
}

fn get_config_value(app: &App, key: &str) -> Option<String> {
    match key {
        "model" => app.config.model.clone(),
        "personality" => app.config.personality.clone(),
        "max_turns" => Some(app.config.max_turns.to_string()),
        "system_prompt" => app.config.system_prompt.clone(),
        _ => None,
    }
}

fn set_config_value(app: &mut App, key: &str, value: &str) -> bool {
    match key {
        "model" => {
            app.config = Arc::new({
                let mut cfg = (*app.config).clone();
                cfg.model = Some(value.to_string());
                cfg
            });
            app.switch_model(value);
            true
        }
        "personality" => {
            app.config = Arc::new({
                let mut cfg = (*app.config).clone();
                cfg.personality = Some(value.to_string());
                cfg
            });
            app.switch_personality(value);
            true
        }
        "max_turns" => {
            if let Ok(turns) = value.parse::<u32>() {
                app.config = Arc::new({
                    let mut cfg = (*app.config).clone();
                    cfg.max_turns = turns;
                    cfg
                });
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// /usage
// ---------------------------------------------------------------------------

pub(crate) fn handle_usage_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let display = app.agent.session_usage_display();
    let mut body = hermes_agent::format_usage_command_text(&display);
    if display.calls == 0 {
        let estimated_tokens: usize = app
            .messages
            .iter()
            .map(|m| m.content.as_ref().map_or(0, |c| c.len()) / 4)
            .sum();
        body.push_str(&format!(
            "\n\n(Transcript heuristic ~{} tokens — no provider usage yet.)",
            estimated_tokens
        ));
    }
    emit_command_output(app, body);
    Ok(CommandResult::Handled)
}

// ---------------------------------------------------------------------------
// /stop
// ---------------------------------------------------------------------------

pub(crate) fn handle_stop_command(app: &mut App) -> Result<CommandResult, AgentError> {
    app.interrupt_controller.interrupt(None);
    emit_command_output(
        app,
        "[Stopping current agent execution]\nAgent execution halted. You can continue typing or use /retry.",
    );
    Ok(CommandResult::Handled)
}

// ---------------------------------------------------------------------------
// /status
// ---------------------------------------------------------------------------

pub(crate) fn handle_status_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let msg_count = app.messages.len();
    let turns = app
        .messages
        .iter()
        .filter(|m| m.role == MessageRole::User)
        .count();
    let usage = app.agent.session_usage_metrics();
    let token_line = if usage.api_calls > 0 {
        format!(
            "  Session tokens: {} total ({} in / {} out, {} API calls)",
            usage.total_tokens, usage.input_tokens, usage.output_tokens, usage.api_calls
        )
    } else {
        let estimated_tokens: usize = app
            .messages
            .iter()
            .map(|m| m.content.as_ref().map_or(0, |c| c.len()) / 4)
            .sum();
        format!("  Est. tokens:   ~{} (no API calls yet)", estimated_tokens)
    };

    emit_command_output(
        app,
        format!(
            "Session Status\n  ID:            {}\n  Model:         {}\n  Personality:   {}\n  Turns:         {}\n  Messages:      {}\n{}\n  Max turns:     {}",
            app.session_id,
            app.current_model,
            app.current_personality.as_deref().unwrap_or("(none)"),
            turns,
            msg_count,
            token_line,
            app.config.max_turns
        ),
    );
    Ok(CommandResult::Handled)
}

// ---------------------------------------------------------------------------
// /about
// ---------------------------------------------------------------------------

pub(crate) fn discover_repo_root_for_about() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("HERMES_REPO_ROOT") {
        let path = PathBuf::from(explicit.trim());
        if path.exists() {
            return Some(path);
        }
    }

    let mut probes: Vec<PathBuf> = Vec::new();
    if let Ok(cwd) = std::env::current_dir() {
        probes.push(cwd);
    }
    probes.push(PathBuf::from(env!("CARGO_MANIFEST_DIR")));

    for probe in probes {
        for candidate in probe.ancestors() {
            if candidate.join("docs/parity").exists() && candidate.join("README.md").exists() {
                return Some(candidate.to_path_buf());
            }
        }
    }
    None
}

pub(crate) fn read_json_file(path: &Path) -> Option<serde_json::Value> {
    let raw = std::fs::read_to_string(path).ok()?;
    serde_json::from_str::<serde_json::Value>(&raw).ok()
}

fn json_value_at_path<'a>(
    value: &'a serde_json::Value,
    path: &[&str],
) -> Option<&'a serde_json::Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn json_str_at_path(value: &serde_json::Value, path: &[&str]) -> Option<String> {
    json_value_at_path(value, path)?
        .as_str()
        .map(|s| s.to_string())
}

fn json_u64_at_path(value: &serde_json::Value, path: &[&str]) -> Option<u64> {
    json_value_at_path(value, path)?.as_u64()
}

fn latest_upstream_sync_report(report_dir: &Path) -> Option<PathBuf> {
    let mut reports: Vec<PathBuf> = std::fs::read_dir(report_dir)
        .ok()?
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let name = path.file_name()?.to_string_lossy();
            if name.starts_with("upstream-sync-") && name.ends_with(".txt") {
                Some(path)
            } else {
                None
            }
        })
        .collect();
    reports.sort();
    reports.into_iter().last()
}

fn parse_sync_report_metadata(path: &Path) -> (HashMap<String, String>, usize) {
    let mut meta = HashMap::new();
    let mut pending_commit_lines = 0usize;
    let raw = std::fs::read_to_string(path).unwrap_or_default();

    let mut in_pending_section = false;
    let mut in_pending_block = false;
    for line in raw.lines() {
        let trimmed = line.trim();
        if !in_pending_section {
            if trimmed.starts_with("## Pending Upstream Commits") {
                in_pending_section = true;
                continue;
            }
            if let Some((k, v)) = line.split_once(':') {
                let key = k.trim();
                if !key.is_empty()
                    && key
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
                {
                    meta.insert(key.to_string(), v.trim().to_string());
                }
            }
            continue;
        }

        if trimmed == "```" {
            if !in_pending_block {
                in_pending_block = true;
            } else {
                break;
            }
            continue;
        }
        if in_pending_block && !trimmed.is_empty() {
            pending_commit_lines = pending_commit_lines.saturating_add(1);
        }
    }

    (meta, pending_commit_lines)
}

pub(crate) fn handle_about_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let mut out = String::new();
    let _ = writeln!(out, "Hermes Agent Ultra — About");
    let _ = writeln!(out, "  Version:         {}", env!("CARGO_PKG_VERSION"));
    let _ = writeln!(out, "  Session model:   {}", app.current_model);
    let _ = writeln!(
        out,
        "  Personality:     {}",
        app.current_personality.as_deref().unwrap_or("(none)")
    );
    if let Ok(exe) = std::env::current_exe() {
        let _ = writeln!(out, "  Binary:          {}", exe.display());
    }
    if let Ok(cwd) = std::env::current_dir() {
        let _ = writeln!(out, "  Current dir:     {}", cwd.display());
    }

    let raw_mode = app.tool_registry.raw_mode_state();
    let policy_mode = std::env::var("HERMES_TOOL_POLICY_MODE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "enforce".to_string());
    let policy_preset = std::env::var("HERMES_TOOL_POLICY_PRESET")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .unwrap_or_else(|| "off".to_string());

    let has_contextlattice_mcp = app.config.mcp_servers.iter().any(|entry| {
        let name_hit = entry.name.to_ascii_lowercase().contains("contextlattice");
        let url_hit = entry
            .url
            .as_ref()
            .map(|u| u.to_ascii_lowercase().contains("contextlattice"))
            .unwrap_or(false);
        name_hit || url_hit
    });

    let _ = writeln!(out);
    let _ = writeln!(out, "Enabled Ultra Features:");
    let _ = writeln!(
        out,
        "  - RTK raw-mode: enabled={} once={}",
        yes_no(raw_mode.enabled),
        yes_no(raw_mode.once_pending)
    );
    let _ = writeln!(
        out,
        "  - Tool policy: mode={} preset={}",
        policy_mode, policy_preset
    );
    let _ = writeln!(
        out,
        "  - Code indexing: {} (max_files={}, max_symbols={})",
        yes_no(app.config.agent.code_index_enabled),
        app.config.agent.code_index_max_files,
        app.config.agent.code_index_max_symbols
    );
    let _ = writeln!(
        out,
        "  - LSP context injection: {} (max_chars={})",
        yes_no(app.config.agent.lsp_context_enabled),
        app.config.agent.lsp_context_max_chars
    );
    let _ = writeln!(
        out,
        "  - Background review loop: {}",
        yes_no(app.config.agent.background_review_enabled)
    );
    let _ = writeln!(out, "  - Multi-registry skills: yes");
    let _ = writeln!(out, "  - Skill security scanning: yes");
    let _ = writeln!(
        out,
        "  - ContextLattice MCP configured: {}",
        yes_no(has_contextlattice_mcp)
    );

    if let Some(repo_root) = discover_repo_root_for_about() {
        let report_dir = repo_root.join(".sync-reports");
        let workstream_path = repo_root.join("docs/parity/workstream-status.json");
        let queue_path = repo_root.join("docs/parity/upstream-missing-queue.json");
        let proof_path = repo_root.join("docs/parity/global-parity-proof.json");

        let mut upstream_ref = String::from("unknown");
        let mut upstream_sha = String::from("unknown");
        let mut workstream_generated = String::from("unknown");
        if let Some(workstream) = read_json_file(&workstream_path) {
            if let Some(v) = json_str_at_path(&workstream, &["upstream_ref"]) {
                upstream_ref = v;
            }
            if let Some(v) = json_str_at_path(&workstream, &["upstream_sha"]) {
                upstream_sha = v;
            }
            if let Some(v) = json_str_at_path(&workstream, &["generated_at_utc"]) {
                workstream_generated = v;
            }
        }

        let mut queue_pending = 0u64;
        let mut queue_ported = 0u64;
        let mut queue_superseded = 0u64;
        if let Some(queue) = read_json_file(&queue_path) {
            queue_pending =
                json_u64_at_path(&queue, &["summary", "by_disposition", "pending"]).unwrap_or(0);
            queue_ported =
                json_u64_at_path(&queue, &["summary", "by_disposition", "ported"]).unwrap_or(0);
            queue_superseded =
                json_u64_at_path(&queue, &["summary", "by_disposition", "superseded"]).unwrap_or(0);
        }

        let mut release_gate_pass = String::from("unknown");
        let mut ci_gate_pass = String::from("unknown");
        if let Some(proof) = read_json_file(&proof_path) {
            if let Some(v) =
                json_value_at_path(&proof, &["release_gate", "pass"]).and_then(|v| v.as_bool())
            {
                release_gate_pass = yes_no(v).to_string();
            }
            if let Some(v) =
                json_value_at_path(&proof, &["ci_gate", "pass"]).and_then(|v| v.as_bool())
            {
                ci_gate_pass = yes_no(v).to_string();
            }
        }

        let mut latest_report_name = String::from("none");
        let mut latest_origin_sha = String::from("unknown");
        let mut latest_upstream_sha = String::from("unknown");
        let mut latest_timestamp = String::from("unknown");
        let mut latest_pending_count = 0usize;
        if let Some(report_path) = latest_upstream_sync_report(&report_dir) {
            latest_report_name = report_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_else(|| report_path.display().to_string());
            let (meta, pending_count) = parse_sync_report_metadata(&report_path);
            latest_pending_count = pending_count;
            if let Some(v) = meta.get("origin_sha") {
                latest_origin_sha = v.clone();
            }
            if let Some(v) = meta.get("upstream_sha") {
                latest_upstream_sha = v.clone();
            }
            if let Some(v) = meta.get("timestamp_utc") {
                latest_timestamp = v.clone();
            }
        }

        let _ = writeln!(out);
        let _ = writeln!(out, "Parity Snapshot:");
        let _ = writeln!(out, "  - Repo root: {}", repo_root.display());
        let _ = writeln!(out, "  - Upstream ref: {}", upstream_ref);
        let _ = writeln!(out, "  - Upstream sha: {}", upstream_sha);
        let _ = writeln!(
            out,
            "  - Workstream report generated_at: {}",
            workstream_generated
        );
        let _ = writeln!(
            out,
            "  - Queue (pending/ported/superseded): {}/{}/{}",
            queue_pending, queue_ported, queue_superseded
        );
        let _ = writeln!(
            out,
            "  - Gate status (release/ci): {}/{}",
            release_gate_pass, ci_gate_pass
        );
        let _ = writeln!(out, "  - Latest sync report: {}", latest_report_name);
        let _ = writeln!(out, "  - Latest sync timestamp_utc: {}", latest_timestamp);
        let _ = writeln!(out, "  - Latest report origin_sha: {}", latest_origin_sha);
        let _ = writeln!(
            out,
            "  - Latest report upstream_sha: {}",
            latest_upstream_sha
        );
        let _ = writeln!(
            out,
            "  - Pending upstream commits in latest report: {}",
            latest_pending_count
        );
    } else {
        let _ = writeln!(out);
        let _ = writeln!(
            out,
            "Parity Snapshot: unavailable (run from a source checkout to load docs/parity + .sync-reports)."
        );
    }

    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

// ---------------------------------------------------------------------------
// Trigger triage types & helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) enum TriggerTriageDecision {
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
pub(crate) struct TriggerTriageAssessment {
    pub(crate) source: String,
    pub(crate) payload: String,
    pub(crate) severity: i32,
    pub(crate) decision: TriggerTriageDecision,
    pub(crate) requires_approval: bool,
    pub(crate) reasons: Vec<String>,
}

fn trigger_triage_mode() -> String {
    std::env::var("HERMES_TRIGGER_TRIAGE_MODE")
        .ok()
        .filter(|v| !v.trim().is_empty())
        .map(|v| v.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "off".to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct TriggerTriageLearningEntry {
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

pub(crate) fn trigger_triage_learning_state_path() -> PathBuf {
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

pub(crate) fn triage_learning_bias(source: &str, payload: &str) -> (i32, Vec<String>) {
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

pub(crate) fn evaluate_trigger_triage(source: &str, payload: &str) -> TriggerTriageAssessment {
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

pub(crate) fn append_triage_learning_feedback(
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
    ranked.sort_by(|a, b| b.1.cmp(&a.1));
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

// ---------------------------------------------------------------------------
// Subconscious types & helpers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct SubconsciousTask {
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
pub(crate) struct SubconsciousQueueState {
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

pub(crate) fn save_subconscious_state(state: &SubconsciousQueueState) -> Result<(), AgentError> {
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

#[cfg(test)]
pub(crate) fn subconscious_test_high_risk_state() -> SubconsciousQueueState {
    let now = chrono::Utc::now().to_rfc3339();
    SubconsciousQueueState {
        tasks: vec![SubconsciousTask {
            id: "sc-risky".to_string(),
            source: "test".to_string(),
            prompt: "rotate key and deploy to prod".to_string(),
            score: 4.2,
            risk: "high".to_string(),
            requires_approval: false,
            status: "pending".to_string(),
            job_id: None,
            created_at: now.clone(),
            updated_at: now,
        }],
    }
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

// ---------------------------------------------------------------------------
// /subconscious
// ---------------------------------------------------------------------------

pub(crate) fn handle_subconscious_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
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
                Uuid::new_v4()
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
                let job = background::queue_background_job(&task.prompt)?;
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
                    if dry_run { "Dry-run" } else { "Executed" },
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
            let token = args
                .get(1)
                .copied()
                .unwrap_or("status")
                .to_ascii_lowercase();
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
                    env_vars::remove_var("HERMES_SUBCONSCIOUS_PROFILE");
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
                    env_vars::set_var("HERMES_SUBCONSCIOUS_PROFILE", next.as_str());
                    emit_command_output(
                        app,
                        format!("Subconscious profile set to {}.", next.as_str()),
                    );
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

// ---------------------------------------------------------------------------
// /triage
// ---------------------------------------------------------------------------

pub(crate) fn handle_trigger_triage_command(
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
                emit_command_output(app, "Usage: /triage feedback <source> <outcome> <payload>");
                return Ok(CommandResult::Handled);
            };
            let Some(outcome) = args.get(2).copied() else {
                emit_command_output(app, "Usage: /triage feedback <source> <outcome> <payload>");
                return Ok(CommandResult::Handled);
            };
            let payload = args.get(3..).unwrap_or(&[]).join(" ").trim().to_string();
            if payload.is_empty() {
                emit_command_output(app, "Usage: /triage feedback <source> <outcome> <payload>");
                return Ok(CommandResult::Handled);
            }
            let assessment = evaluate_trigger_triage(source, &payload);
            let entry = append_triage_learning_feedback(source, &payload, outcome, &assessment)?;
            let (bias_now, _) = triage_learning_bias(source, &payload);
            emit_command_output(
                app,
                format!(
                    "Recorded triage feedback.\nsource={} outcome={} delta={:+} decision={} severity={}\nsource_bias_now={:+}",
                    entry.source,
                    entry.outcome,
                    entry.bias_delta,
                    entry.decision,
                    entry.severity,
                    bias_now
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
                            Uuid::new_v4()
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
                        let job = background::queue_background_job(payload.trim())?;
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

// ---------------------------------------------------------------------------
// /verbose
// ---------------------------------------------------------------------------

pub(crate) fn handle_verbose_command(app: &mut App) -> Result<CommandResult, AgentError> {
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

// ---------------------------------------------------------------------------
// /yolo
// ---------------------------------------------------------------------------

pub(crate) fn handle_yolo_command(app: &mut App) -> Result<CommandResult, AgentError> {
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

// ---------------------------------------------------------------------------
// Reasoning helpers
// ---------------------------------------------------------------------------

fn reasoning_display_flag() -> &'static std::sync::atomic::AtomicBool {
    static SHOW_REASONING: std::sync::atomic::AtomicBool =
        std::sync::atomic::AtomicBool::new(false);
    &SHOW_REASONING
}

fn set_reasoning_display(enabled: bool) {
    reasoning_display_flag().store(enabled, std::sync::atomic::Ordering::Relaxed);
}

fn toggle_reasoning_display() -> bool {
    let prev = reasoning_display_flag().fetch_xor(true, std::sync::atomic::Ordering::Relaxed);
    !prev
}

fn reasoning_display_enabled() -> bool {
    reasoning_display_flag().load(std::sync::atomic::Ordering::Relaxed)
}

pub(crate) fn parse_reasoning_effort(raw: &str) -> Result<Option<&'static str>, AgentError> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "minimal" | "min" => Ok(Some("minimal")),
        "low" => Ok(Some("low")),
        "medium" | "med" => Ok(Some("medium")),
        "high" => Ok(Some("high")),
        "xhigh" | "max" => Ok(Some("xhigh")),
        "auto" | "default" | "clear" | "reset" | "none" => Ok(None),
        other => Err(AgentError::Config(format!(
            "Unknown reasoning effort '{}'. Use one of: minimal, low, medium, high, xhigh, auto.",
            other
        ))),
    }
}

fn set_provider_reasoning_effort(
    cfg: &mut hermes_config::GatewayConfig,
    provider: &str,
    effort: Option<&str>,
) {
    let provider_key = resolve_provider_key(cfg, provider);
    let provider_cfg = cfg
        .llm_providers
        .entry(provider_key.clone())
        .or_insert_with(LlmProviderConfig::default);

    let mut body_map = provider_cfg
        .extra_body
        .take()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();

    match effort {
        Some(level) => {
            body_map.remove("reasoning_effort");
            let mut reasoning_obj = body_map
                .get("reasoning")
                .and_then(|v| v.as_object().cloned())
                .unwrap_or_default();
            let mapped_reasoning = openai_reasoning_effort_for_level(level);
            reasoning_obj.insert(
                "effort".to_string(),
                serde_json::Value::String(mapped_reasoning.to_string()),
            );
            body_map.insert(
                "reasoning".to_string(),
                serde_json::Value::Object(reasoning_obj),
            );

            if provider_key.contains("gemini") || provider_key == "google" {
                let level_mapped = gemini_thinking_level_for_effort(level);
                let mut google_obj = body_map
                    .get("google")
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default();
                let mut thinking_cfg = google_obj
                    .get("thinking_config")
                    .and_then(|v| v.as_object().cloned())
                    .unwrap_or_default();
                thinking_cfg.insert(
                    "thinking_level".to_string(),
                    serde_json::Value::String(level_mapped.to_string()),
                );
                google_obj.insert(
                    "thinking_config".to_string(),
                    serde_json::Value::Object(thinking_cfg.clone()),
                );
                body_map.insert("google".to_string(), serde_json::Value::Object(google_obj));
                body_map.insert(
                    "thinking_config".to_string(),
                    serde_json::Value::Object(thinking_cfg),
                );
            }
        }
        None => {
            body_map.remove("reasoning_effort");
            if let Some(reasoning_obj) = body_map
                .get_mut("reasoning")
                .and_then(|value| value.as_object_mut())
            {
                reasoning_obj.remove("effort");
                if reasoning_obj.is_empty() {
                    body_map.remove("reasoning");
                }
            }
            body_map.remove("thinking_config");
            if let Some(google_obj) = body_map
                .get_mut("google")
                .and_then(|value| value.as_object_mut())
            {
                google_obj.remove("thinking_config");
                if google_obj.is_empty() {
                    body_map.remove("google");
                }
            }
        }
    }

    provider_cfg.extra_body = if body_map.is_empty() {
        None
    } else {
        Some(serde_json::Value::Object(body_map))
    };
}

fn provider_reasoning_effort(cfg: &hermes_config::GatewayConfig, provider: &str) -> Option<String> {
    let provider_key = resolve_provider_key(cfg, provider);
    cfg.llm_providers
        .get(&provider_key)
        .and_then(|entry| entry.extra_body.as_ref())
        .and_then(|body| {
            body.get("reasoning")
                .and_then(|value| value.get("effort"))
                .and_then(|value| value.as_str())
                .map(ToString::to_string)
                .or_else(|| {
                    body.get("reasoning_effort")
                        .and_then(|value| value.as_str())
                        .map(ToString::to_string)
                })
        })
}

// ---------------------------------------------------------------------------
// /reasoning
// ---------------------------------------------------------------------------

pub(crate) fn handle_reasoning_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        let enabled = toggle_reasoning_display();
        if enabled {
            emit_command_output(
                app,
                "Reasoning display: ON — model reasoning will be shown.",
            );
        } else {
            emit_command_output(
                app,
                "Reasoning display: OFF — model reasoning will be hidden.",
            );
        }
        return Ok(CommandResult::Handled);
    }

    match args[0].trim().to_ascii_lowercase().as_str() {
        "status" => {
            let (provider, _) = split_provider_model(&app.current_model);
            let effort = provider_reasoning_effort(&app.config, provider)
                .unwrap_or_else(|| "auto".to_string());
            emit_command_output(
                app,
                format!(
                    "Reasoning status\n- display: {}\n- effort: {}\n- provider: {}",
                    if reasoning_display_enabled() {
                        "ON"
                    } else {
                        "OFF"
                    },
                    effort,
                    provider
                ),
            );
        }
        "toggle" => {
            let enabled = toggle_reasoning_display();
            emit_command_output(
                app,
                format!(
                    "Reasoning display: {} — model reasoning will be {}.",
                    if enabled { "ON" } else { "OFF" },
                    if enabled { "shown" } else { "hidden" }
                ),
            );
        }
        "on" | "show" => {
            set_reasoning_display(true);
            emit_command_output(
                app,
                "Reasoning display: ON — model reasoning will be shown.",
            );
        }
        "off" | "hide" => {
            set_reasoning_display(false);
            emit_command_output(
                app,
                "Reasoning display: OFF — model reasoning will be hidden.",
            );
        }
        "set" | "level" | "effort" => {
            if args.len() < 2 {
                emit_command_output(
                    app,
                    "Usage: /reasoning set <minimal|low|medium|high|xhigh|auto>",
                );
                return Ok(CommandResult::Handled);
            }
            let effort = parse_reasoning_effort(args[1])?;
            let provider = split_provider_model(&app.current_model).0.to_string();
            let current_model = app.current_model.clone();
            app.config = Arc::new({
                let mut cfg = (*app.config).clone();
                set_provider_reasoning_effort(&mut cfg, &provider, effort);
                cfg
            });
            app.switch_model(&current_model);
            let effort_label = effort.unwrap_or("auto");
            emit_command_output(
                app,
                format!(
                    "Reasoning effort set to `{}` for provider `{}` (model `{}`).",
                    effort_label, provider, current_model
                ),
            );
        }
        "help" => {
            emit_command_output(
                app,
                "Reasoning controls:\n\
                 - /reasoning                 Toggle reasoning display\n\
                 - /reasoning status          Show display + effort state\n\
                 - /reasoning on|off          Explicitly show/hide reasoning\n\
                 - /reasoning set <level>     Set provider reasoning effort\n\
                 Levels: minimal, low, medium, high, xhigh, auto",
            );
        }
        shorthand => {
            let effort = parse_reasoning_effort(shorthand)?;
            let provider = split_provider_model(&app.current_model).0.to_string();
            let current_model = app.current_model.clone();
            app.config = Arc::new({
                let mut cfg = (*app.config).clone();
                set_provider_reasoning_effort(&mut cfg, &provider, effort);
                cfg
            });
            app.switch_model(&current_model);
            emit_command_output(
                app,
                format!(
                    "Reasoning effort set to `{}` for provider `{}` (model `{}`).",
                    effort.unwrap_or("auto"),
                    provider,
                    current_model
                ),
            );
        }
    }
    Ok(CommandResult::Handled)
}

// ---------------------------------------------------------------------------
// /raw
// ---------------------------------------------------------------------------

pub(crate) fn handle_raw_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    if args
        .first()
        .is_some_and(|sub| sub.eq_ignore_ascii_case("trace"))
    {
        let replay_path = super::replay_log_path_for_session(&app.session_id);
        let sub = args.get(1).map(|s| s.trim().to_ascii_lowercase());
        match sub.as_deref() {
            None | Some("status") => {
                emit_command_output(
                    app,
                    format!(
                        "Replay trace: {}{}\nSession: {}\nPath: {}\nUsage: /raw trace [on|off|toggle|status|tail [N]|focus <trace-id> [N]|graph [N]|verify|export [N] [PATH]|path]",
                        if replay_enabled_runtime() {
                            "ON"
                        } else {
                            "OFF"
                        },
                        if replay_path.exists() {
                            ""
                        } else {
                            " (no log yet)"
                        },
                        app.session_id,
                        replay_path.display()
                    ),
                );
            }
            Some("path") => {
                emit_command_output(app, format!("Replay path: {}", replay_path.display()));
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
                            "Replay log not found for current session yet: {}",
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
                            "Replay log not found for current session yet: {}",
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
                            "Replay log not found for current session yet: {}",
                            replay_path.display()
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
                let rendered = render_replay_trace_graph(&replay_path, limit)?;
                emit_command_output(app, rendered);
            }
            Some("verify") => {
                if !replay_path.exists() {
                    emit_command_output(
                        app,
                        format!(
                            "Replay log not found for current session yet: {}",
                            replay_path.display()
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
                let (entries, parse_errors, chain_breaks) =
                    super::replay_trace_integrity(&replay_path)?;
                let ok = parse_errors == 0 && chain_breaks == 0;
                emit_command_output(
                    app,
                    format!(
                        "Replay integrity: {}\nentries: {}\nparse_errors: {}\nchain_breaks: {}\npath: {}",
                        if ok { "PASS" } else { "FAIL" },
                        entries,
                        parse_errors,
                        chain_breaks,
                        replay_path.display()
                    ),
                );
            }
            Some("export") => {
                let limit = args
                    .get(2)
                    .and_then(|raw| raw.trim().parse::<usize>().ok())
                    .unwrap_or(100)
                    .clamp(1, 1000);
                let output_path = args.get(3).map(PathBuf::from).unwrap_or_else(|| {
                    hermes_config::hermes_home()
                        .join("logs")
                        .join("replay")
                        .join("exports")
                        .join(format!("{}-tail.json", app.session_id))
                });
                if !replay_path.exists() {
                    emit_command_output(
                        app,
                        format!(
                            "Replay log not found for current session yet: {}",
                            replay_path.display()
                        ),
                    );
                    return Ok(CommandResult::Handled);
                }
                let written = export_replay_trace_json(&replay_path, limit, &output_path)?;
                emit_command_output(
                    app,
                    format!(
                        "Replay export written.\nrows: {}\nsource: {}\noutput: {}",
                        written,
                        replay_path.display(),
                        output_path.display()
                    ),
                );
            }
            Some("on") | Some("off") | Some("toggle") => {
                let next = match sub.as_deref().unwrap_or("status") {
                    "on" => true,
                    "off" => false,
                    "toggle" => !replay_enabled_runtime(),
                    _ => replay_enabled_runtime(),
                };
                env_vars::set_var("HERMES_REPLAY_ENABLED", if next { "1" } else { "0" });
                emit_command_output(
                    app,
                    format!(
                        "Replay trace mode: {}.\nThis applies to new turns in the current process.",
                        if next { "ON" } else { "OFF" }
                    ),
                );
            }
            Some("help") | Some("--help") | Some("-h") => emit_command_output(
                app,
                "Replay trace controls:\n  /raw trace status              Show enabled state + current log path\n  /raw trace on|off              Enable or disable deterministic replay trace logs\n  /raw trace toggle              Toggle replay trace logs\n  /raw trace tail [N]            Show latest trace events with lineage hashes\n  /raw trace focus <id> [N]      Filter replay rows by trace_id\n  /raw trace graph [N]           Show lineage edges for recent rows\n  /raw trace verify              Validate replay hash-chain integrity\n  /raw trace export [N] [PATH]   Export tail events to JSON\n  /raw trace path                Show trace log file for current session",
            ),
            _ => emit_command_output(
                app,
                "Usage: /raw trace [on|off|toggle|status|tail [N]|focus <trace-id> [N]|graph [N]|verify|export [N] [PATH]|path]",
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
                "RTK raw mode: {}{}\nDual logs: {}\nReplay trace: {}\nUsage: /raw [on|off|toggle|once|status|trace]",
                if state.enabled { "ON" } else { "OFF" },
                if state.once_pending {
                    " (one-shot pending)"
                } else {
                    ""
                },
                log_dir.display(),
                if replay_enabled_runtime() {
                    "ON"
                } else {
                    "OFF"
                }
            ),
        );
        return Ok(CommandResult::Handled);
    }

    match args[0].trim().to_ascii_lowercase().as_str() {
        "help" => emit_command_output(
            app,
            "RTK raw controls:\n  /raw status        Show current mode + log path\n  /raw on            Disable output filtering for all tool calls\n  /raw off           Re-enable RTK output filtering\n  /raw toggle        Toggle global raw mode\n  /raw once          Raw pass-through for next tool call only\n  /raw trace ...     Deterministic replay trace controls",
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
            env_vars::set_var("HERMES_RTK_RAW", if next { "1" } else { "0" });
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

pub(crate) fn replay_enabled_runtime() -> bool {
    std::env::var("HERMES_REPLAY_ENABLED")
        .ok()
        .is_some_and(|v| {
            matches!(
                v.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
}

fn render_replay_trace_tail(path: &Path, limit: usize) -> Result<String, AgentError> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        AgentError::Io(format!(
            "Failed to read replay log {}: {}",
            path.display(),
            e
        ))
    })?;
    let lines: Vec<&str> = raw
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(limit)
        .collect();
    let mut out = format!("Replay trace tail ({} lines)\n", lines.len());
    for line in lines.iter().rev() {
        let _ = writeln!(out, "{}", line);
    }
    Ok(out)
}

fn replay_entries(path: &Path, limit: usize) -> Result<Vec<serde_json::Value>, AgentError> {
    let raw = std::fs::read_to_string(path).map_err(|e| {
        AgentError::Io(format!(
            "Failed to read replay log {}: {}",
            path.display(),
            e
        ))
    })?;
    Ok(raw
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(limit)
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
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
                .is_some_and(|id| id.contains(trace_filter))
        })
        .collect();
    let mut out = format!(
        "Replay trace focus ({} rows match `{}`)\n",
        filtered.len(),
        trace_filter
    );
    for row in &filtered {
        let seq = row.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
        let event = row.get("event").and_then(|v| v.as_str()).unwrap_or("?");
        let preview = row
            .get("payload")
            .map(|v| truncate_chars(&v.to_string(), 120))
            .unwrap_or_default();
        let _ = writeln!(out, "#{} [{}] {} {}", seq, event, trace_filter, preview);
    }
    Ok(out)
}

fn render_replay_trace_graph(path: &Path, limit: usize) -> Result<String, AgentError> {
    let rows = replay_entries(path, limit)?;
    if rows.is_empty() {
        return Ok("Replay graph: no entries in current window.".to_string());
    }
    let mut out = String::new();
    let _ = writeln!(out, "Replay lineage graph");
    let _ = writeln!(out, "--------------------");
    let _ = writeln!(out, "window={} path={}", rows.len(), path.display());
    for row in rows {
        let seq = row.get("seq").and_then(|v| v.as_u64()).unwrap_or(0);
        let event = row.get("event").and_then(|v| v.as_str()).unwrap_or("?");
        let tid = row.get("trace_id").and_then(|v| v.as_str()).unwrap_or("?");
        let prev = row
            .get("prev_hash")
            .and_then(|v| v.as_str())
            .unwrap_or("seed");
        let curr = row
            .get("event_hash")
            .and_then(|v| v.as_str())
            .unwrap_or("?");
        let _ = writeln!(
            out,
            "#{} [{}] tid={} {} → {}",
            seq,
            event,
            tid,
            truncate_chars(prev, 16),
            truncate_chars(curr, 16)
        );
    }
    Ok(out)
}

fn export_replay_trace_json(
    replay_path: &Path,
    limit: usize,
    output_path: &Path,
) -> Result<usize, AgentError> {
    let raw = std::fs::read_to_string(replay_path).map_err(|e| {
        AgentError::Io(format!(
            "Failed to read replay log {}: {}",
            replay_path.display(),
            e
        ))
    })?;
    let rows: Vec<serde_json::Value> = raw
        .lines()
        .rev()
        .filter(|l| !l.trim().is_empty())
        .take(limit)
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .collect();
    let count = rows.len();
    let export = serde_json::json!({ "rows": rows });
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("Failed to create {}: {}", parent.display(), e)))?;
    }
    std::fs::write(
        output_path,
        serde_json::to_string_pretty(&export)
            .map_err(|e| AgentError::Io(format!("Failed to serialize export: {}", e)))?,
    )
    .map_err(|e| AgentError::Io(format!("Failed to write {}: {}", output_path.display(), e)))?;
    Ok(count)
}

// ---------------------------------------------------------------------------
// /history
// ---------------------------------------------------------------------------

pub(crate) fn handle_history_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let sp = session::session_db(app);
    let db_messages = if sp.ensure_db().is_ok() && !app.session_id.is_empty() {
        sp.load_session(&app.session_id).ok()
    } else {
        None
    };

    let transcript: Vec<_> = db_messages
        .as_ref()
        .filter(|m| !m.is_empty())
        .cloned()
        .unwrap_or_else(|| app.transcript_messages());

    if transcript.is_empty() {
        emit_command_output(app, "No conversation history yet.");
        return Ok(CommandResult::Handled);
    }

    let source_note = if db_messages.is_some() {
        " (from state.db)"
    } else {
        ""
    };
    let mut out = format!("Recent conversation history{source_note}:\n");
    for (idx, msg) in transcript.iter().enumerate().rev().take(12).rev() {
        let role = match msg.role {
            MessageRole::User => "USER",
            MessageRole::Assistant => "HERMES",
            MessageRole::System => "SYSTEM",
            MessageRole::Tool => "TOOL",
        };
        let preview =
            hermes_agent::session_persistence::decode_content_preview(msg.content.as_deref());
        let preview = preview.lines().next().unwrap_or("").trim();
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

// ---------------------------------------------------------------------------
// /recap
// ---------------------------------------------------------------------------

pub(crate) fn handle_recap_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
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
            MessageRole::User => user_msgs += 1,
            MessageRole::Assistant => assistant_msgs += 1,
            MessageRole::Tool => tool_msgs += 1,
            MessageRole::System => system_msgs += 1,
        }
        tool_call_count += msg.tool_calls.as_ref().map(|c| c.len()).unwrap_or(0);
        char_count += msg.content.as_deref().map(str::len).unwrap_or(0);
    }

    let latest_user = window
        .iter()
        .rev()
        .find(|m| matches!(m.role, MessageRole::User))
        .and_then(|m| m.content.as_deref())
        .map(|c| truncate_chars(c.trim(), 120))
        .unwrap_or_else(|| "(none)".to_string());
    let latest_assistant = window
        .iter()
        .rev()
        .find(|m| matches!(m.role, MessageRole::Assistant))
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

// ---------------------------------------------------------------------------
// /context
// ---------------------------------------------------------------------------

pub(crate) async fn handle_context_command(
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
                    MessageRole::User => "USER",
                    MessageRole::Assistant => "HERMES",
                    MessageRole::Tool => "TOOL",
                    MessageRole::System => "SYSTEM",
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
            return compress::handle_compress_command(app, &[]).await;
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

// ---------------------------------------------------------------------------
// /provider
// ---------------------------------------------------------------------------

pub(crate) async fn handle_provider_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let providers = curated_provider_slugs();
    if providers.is_empty() {
        emit_command_output(app, "No providers registered.");
        return Ok(CommandResult::Handled);
    }
    let entries = provider_catalog_entries(&providers, 4).await;
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

// ---------------------------------------------------------------------------
// /runbook
// ---------------------------------------------------------------------------

pub(crate) fn handle_runbook_command(
    app: &mut App,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    let action = args.first().copied().unwrap_or("list").to_ascii_lowercase();
    if action == "list" || action == "status" {
        emit_command_output(
            app,
            "Runbooks\n- auth-refresh: provider auth/session rejected\n- model-not-found: catalog drift / unknown model\n- contextlattice-connect: local memory integration bootstrap\n- tool-policy-deny: blocked by policy or sandbox profile\n- stream-finalization: stream done but transcript not finalized\n\nUse `/runbook show <name>`.",
        );
        return Ok(CommandResult::Handled);
    }
    if action == "show" {
        let Some(name) = args.get(1).map(|v| v.to_ascii_lowercase()) else {
            emit_command_output(app, "Usage: /runbook show <name>");
            return Ok(CommandResult::Handled);
        };
        let body = match name.as_str() {
            "auth-refresh" => {
                "Runbook: auth-refresh\n1) `/auth status`\n2) `/auth refresh`\n3) retry prompt\n4) if still failing, run `/model` and confirm provider/model pair is valid for your account."
            }
            "model-not-found" => {
                "Runbook: model-not-found\n1) `/model` and select a valid catalog model\n2) retry request\n3) if provider alias was stale, run `/auth verify` and re-check."
            }
            "contextlattice-connect" => {
                "Runbook: contextlattice-connect\n1) ensure contextlattice tools are registered via `/tools`\n2) ask agent to run `contextlattice_search` first (not shell command `contextlattice`)\n3) checkpoint verified integration via `contextlattice_write`."
            }
            "tool-policy-deny" => {
                "Runbook: tool-policy-deny\n1) inspect denial reason in tool card `[remediation]` section\n2) remove secret-like args from inline command payload\n3) retry with safer params or approved tool route (`/tools`)."
            }
            "stream-finalization" => {
                "Runbook: stream-finalization\n1) wait for final transcript writeback (status shows `Finalizing response…`)\n2) avoid submitting a new prompt until finalization completes\n3) if UI appears stale, use Ctrl+G to refresh and jump latest."
            }
            _ => {
                emit_command_output(
                    app,
                    format!(
                        "Unknown runbook `{}`. Use `/runbook list` for available entries.",
                        name
                    ),
                );
                return Ok(CommandResult::Handled);
            }
        };
        emit_command_output(app, body);
        return Ok(CommandResult::Handled);
    }
    emit_command_output(app, "Usage: /runbook [list|show <name>]");
    Ok(CommandResult::Handled)
}
