//! Runtime UI slash commands (profile, skin, pet, clipboard, mouse).

use std::fmt::Write as _;

use hermes_core::{AgentError, MessageRole};

use crate::app::{App, PetDock, PetSettings};
use crate::commands::{CommandResult, emit_command_output, truncate_chars};
use crate::skin_engine::{BUILTIN_SKINS, canonical_skin_name};

pub(crate) fn handle_profile_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let home = hermes_config::hermes_home();
    let selected = app.config.profile.current.as_deref().unwrap_or("default");
    let mut out = String::new();
    let _ = writeln!(out, "Active profile: {}", selected);
    let _ = writeln!(out, "Hermes home: {}", home.display());
    let _ = writeln!(out, "Session id: {}", app.session_id);
    emit_command_output(app, out.trim_end());
    Ok(CommandResult::Handled)
}

pub(crate) fn handle_runtime_ui_mode_command(
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
                    crate::env_vars::set_var("HERMES_THEME", canonical);
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

pub(crate) fn handle_pet_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
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
                            if settings.enabled {
                                "enabled"
                            } else {
                                "hidden"
                            },
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

pub(crate) fn handle_redraw_command(app: &mut App) -> Result<CommandResult, AgentError> {
    app.push_ui_assistant("↻ Repaint pulse requested.");
    emit_command_output(
        app,
        "Repaint pulse sent.\nIf the screen still looks stale: press Ctrl+L (lane toggle) or resize the terminal once.",
    );
    Ok(CommandResult::Handled)
}

pub(crate) fn handle_paste_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
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

pub(crate) fn handle_copy_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let maybe_text = app.transcript_messages().into_iter().rev().find_map(|msg| {
        if msg.role != MessageRole::Assistant {
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

pub(crate) fn handle_statusbar_command(app: &mut App) -> Result<CommandResult, AgentError> {
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

pub(crate) fn handle_mouse_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.len() >= 2 && args[0].eq_ignore_ascii_case("set") {
        match parse_toggle_arg(args.get(1).copied(), app.mouse_enabled()) {
            Ok(next) => {
                app.set_mouse_enabled(next);
                crate::env_vars::set_var("HERMES_TUI_MOUSE", if next { "1" } else { "0" });
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
            crate::env_vars::set_var("HERMES_TUI_MOUSE", if next { "1" } else { "0" });
            emit_command_output(
                app,
                format!("Mouse interactions: {}", if next { "ON" } else { "OFF" }),
            );
        }
        Err(usage) => emit_command_output(app, usage),
    }
    Ok(CommandResult::Handled)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_toggle_arg_supports_status_and_explicit_values() {
        assert_eq!(parse_toggle_arg(None, true).expect("toggle"), false);
        assert_eq!(
            parse_toggle_arg(Some("toggle"), false).expect("toggle"),
            true
        );
        assert_eq!(parse_toggle_arg(Some("on"), false).expect("on"), true);
        assert_eq!(parse_toggle_arg(Some("off"), true).expect("off"), false);
        assert!(parse_toggle_arg(Some("bad-value"), true).is_err());
    }

    #[test]
    fn parse_pet_species_and_mood_validate_catalog_entries() {
        assert_eq!(parse_pet_species("fox").as_deref(), Some("fox"));
        assert!(parse_pet_species("dragon").is_none());
        assert_eq!(parse_pet_mood("ready").as_deref(), Some("ready"));
        assert!(parse_pet_mood("sleeping-beauty").is_none());
    }

    #[test]
    fn parse_pet_dock_accepts_left_or_right() {
        assert_eq!(parse_pet_dock("left"), Some(PetDock::Left));
        assert_eq!(parse_pet_dock("right"), Some(PetDock::Right));
        assert_eq!(parse_pet_dock("center"), None);
    }
}
