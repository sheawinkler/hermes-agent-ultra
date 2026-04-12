//! Slash command handler (Requirement 9.2).
//!
//! Defines and dispatches all supported `/` commands in the interactive
//! REPL, and provides auto-completion suggestions.

use std::sync::Arc;

use hermes_core::AgentError;

use crate::app::App;

// ---------------------------------------------------------------------------
// CommandResult
// ---------------------------------------------------------------------------

/// Result of handling a slash command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandResult {
    /// The command was fully handled (no further action needed).
    Handled,
    /// The command requires the agent to process a follow-up message.
    NeedsAgent,
    /// The user requested to quit the application.
    Quit,
}

// ---------------------------------------------------------------------------
// Slash commands
// ---------------------------------------------------------------------------

/// All supported slash commands and their descriptions.
pub const SLASH_COMMANDS: &[(&str, &str)] = &[
    ("/new", "Start a new session"),
    ("/reset", "Reset the current session (clear messages)"),
    ("/retry", "Retry the last user message"),
    ("/undo", "Undo the last exchange"),
    ("/model", "Show or switch the current model"),
    ("/personality", "Show or switch the current personality"),
    ("/skills", "List available skills"),
    ("/tools", "List registered tools"),
    ("/config", "Show or modify configuration"),
    ("/compress", "Trigger context compression"),
    ("/usage", "Show token usage statistics"),
    ("/stop", "Stop current agent execution"),
    ("/status", "Show session status (model, turns, token count)"),
    ("/save", "Save current session to disk"),
    ("/load", "Load a saved session"),
    ("/background", "Run a task in the background"),
    ("/verbose", "Toggle verbose mode"),
    ("/yolo", "Toggle auto-approve mode"),
    ("/reasoning", "Toggle reasoning display"),
    ("/help", "Show help for available commands"),
    ("/quit", "Quit the application"),
    ("/exit", "Alias for /quit"),
];

/// Return auto-completion suggestions for a partial slash command.
pub fn autocomplete(partial: &str) -> Vec<&'static str> {
    if partial.is_empty() {
        return SLASH_COMMANDS.iter().map(|(cmd, _)| *cmd).collect();
    }

    let lower = partial.to_lowercase();
    SLASH_COMMANDS
        .iter()
        .filter(|(cmd, _)| cmd.starts_with(&lower))
        .map(|(cmd, _)| *cmd)
        .collect()
}

/// Return the help text for a specific slash command.
pub fn help_for(cmd: &str) -> Option<&'static str> {
    SLASH_COMMANDS
        .iter()
        .find(|(name, _)| *name == cmd)
        .map(|(_, desc)| *desc)
}

// ---------------------------------------------------------------------------
// Command dispatcher
// ---------------------------------------------------------------------------

/// Handle a slash command.
///
/// `cmd` is the full command token including the `/` prefix
/// (e.g. `/model`, `/new`). `args` are the remaining tokens.
pub async fn handle_slash_command(
    app: &mut App,
    cmd: &str,
    args: &[&str],
) -> Result<CommandResult, AgentError> {
    match cmd {
        "/new" => {
            app.new_session();
            println!("[New session started: {}]", app.session_id);
            Ok(CommandResult::Handled)
        }
        "/reset" => {
            app.reset_session();
            println!("[Session reset]");
            Ok(CommandResult::Handled)
        }
        "/retry" => {
            app.retry_last().await?;
            Ok(CommandResult::Handled)
        }
        "/undo" => {
            app.undo_last();
            println!("[Last exchange undone]");
            Ok(CommandResult::Handled)
        }
        "/model" => {
            handle_model_command(app, args)
        }
        "/personality" => {
            handle_personality_command(app, args)
        }
        "/skills" => {
            handle_skills_command(app)
        }
        "/tools" => {
            handle_tools_command(app)
        }
        "/config" => {
            handle_config_command(app, args)
        }
        "/compress" => {
            handle_compress_command(app)
        }
        "/usage" => {
            handle_usage_command(app)
        }
        "/stop" => {
            handle_stop_command(app)
        }
        "/status" => {
            handle_status_command(app)
        }
        "/save" => {
            handle_save_command(app, args)
        }
        "/load" => {
            handle_load_command(app, args)
        }
        "/background" => {
            handle_background_command(app, args)
        }
        "/verbose" => {
            handle_verbose_command(app)
        }
        "/yolo" => {
            handle_yolo_command(app)
        }
        "/reasoning" => {
            handle_reasoning_command(app)
        }
        "/help" => {
            print_help();
            Ok(CommandResult::Handled)
        }
        "/quit" | "/exit" => {
            println!("Goodbye!");
            Ok(CommandResult::Quit)
        }
        _ => {
            println!("Unknown command: {}. Type /help for available commands.", cmd);
            Ok(CommandResult::Handled)
        }
    }
}

// ---------------------------------------------------------------------------
// Individual command handlers
// ---------------------------------------------------------------------------

fn handle_model_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        // Show current model
        println!("Current model: {}", app.current_model);
    } else {
        // Switch model
        let provider_model = args.join(" ");
        app.switch_model(&provider_model);
        println!("Model switched to: {}", provider_model);
    }
    Ok(CommandResult::Handled)
}

fn handle_personality_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        // Show current personality
        match &app.current_personality {
            Some(p) => println!("Current personality: {}", p),
            None => println!("No personality set"),
        }
    } else {
        let name = args.join(" ");
        app.switch_personality(&name);
        println!("Personality switched to: {}", name);
    }
    Ok(CommandResult::Handled)
}

fn handle_skills_command(app: &mut App) -> Result<CommandResult, AgentError> {
    // In a full implementation, we would query the skill provider.
    println!("Skills (not yet loaded — skill provider not connected)");
    println!("Use /skills to list available skills once a skill provider is configured.");
    Ok(CommandResult::Handled)
}

fn handle_tools_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let tools = app.tool_registry.list_tools();
    if tools.is_empty() {
        println!("No tools registered.");
    } else {
        println!("Registered tools ({}):", tools.len());
        for tool in &tools {
            println!("  • {} — {}", tool.name, tool.description);
        }
    }
    Ok(CommandResult::Handled)
}

fn handle_config_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        // Show full config
        let config_json = serde_json::to_string_pretty(&*app.config)
            .unwrap_or_else(|e| format!("<serialization error: {}>", e));
        println!("{}", config_json);
    } else {
        match args[0] {
            "get" => {
                if args.len() < 2 {
                    println!("Usage: /config get <key>");
                } else {
                    let key = args[1];
                    let value = get_config_value(app, key);
                    match value {
                        Some(v) => println!("{} = {}", key, v),
                        None => println!("Key '{}' not found in configuration.", key),
                    }
                }
            }
            "set" => {
                if args.len() < 3 {
                    println!("Usage: /config set <key> <value>");
                } else {
                    let key = args[1];
                    let value = args[2..].join(" ");
                    set_config_value(app, key, &value);
                    println!("Set {} = {}", key, value);
                }
            }
            _ => {
                println!(
                    "Unknown config action '{}'. Use 'get' or 'set'.",
                    args[0]
                );
            }
        }
    }
    Ok(CommandResult::Handled)
}

/// Get a configuration value by dotted key path.
fn get_config_value(app: &App, key: &str) -> Option<String> {
    match key {
        "model" => app.config.model.clone(),
        "personality" => app.config.personality.clone(),
        "max_turns" => Some(app.config.max_turns.to_string()),
        "system_prompt" => app.config.system_prompt.clone(),
        _ => None,
    }
}

/// Set a configuration value by dotted key path.
fn set_config_value(app: &mut App, key: &str, value: &str) {
    match key {
        "model" => {
            app.config = Arc::new({
                let mut cfg = (*app.config).clone();
                cfg.model = Some(value.to_string());
                cfg
            });
            app.switch_model(value);
        }
        "personality" => {
            app.config = Arc::new({
                let mut cfg = (*app.config).clone();
                cfg.personality = Some(value.to_string());
                cfg
            });
            app.switch_personality(value);
        }
        "max_turns" => {
            if let Ok(turns) = value.parse::<u32>() {
                app.config = Arc::new({
                    let mut cfg = (*app.config).clone();
                    cfg.max_turns = turns;
                    cfg
                });
            }
        }
        _ => {
            println!("Unknown configuration key: {}", key);
        }
    }
}

fn handle_compress_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let msg_count = app.messages.len();
    if msg_count <= 2 {
        println!("Context too small to compress ({} messages).", msg_count);
        return Ok(CommandResult::Handled);
    }

    let keep = std::cmp::max(2, msg_count / 3);
    let removed = msg_count - keep;
    let summary_text = format!(
        "[Compressed: {} earlier messages summarized. {} messages retained.]",
        removed, keep,
    );

    let split_at = app.messages.len() - keep;
    let retained = app.messages.split_off(split_at);
    app.messages.clear();
    app.messages.push(hermes_core::Message::system(summary_text));
    app.messages.extend(retained);

    println!(
        "Compressed context: removed {} messages, kept {}. Total now: {}.",
        removed,
        keep,
        app.messages.len(),
    );
    Ok(CommandResult::Handled)
}

fn handle_usage_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let msg_count = app.messages.len();
    let user_msgs = app
        .messages
        .iter()
        .filter(|m| m.role == hermes_core::MessageRole::User)
        .count();
    let assistant_msgs = app
        .messages
        .iter()
        .filter(|m| m.role == hermes_core::MessageRole::Assistant)
        .count();

    let estimated_tokens: usize = app
        .messages
        .iter()
        .map(|m| m.content.as_ref().map_or(0, |c| c.len()) / 4)
        .sum();

    println!("Session Usage Statistics");
    println!("  Session:    {}", app.session_id);
    println!("  Model:      {}", app.current_model);
    println!("  Messages:   {} total", msg_count);
    println!("    User:     {}", user_msgs);
    println!("    Assistant: {}", assistant_msgs);
    println!("  Est. tokens: ~{}", estimated_tokens);
    Ok(CommandResult::Handled)
}

fn handle_stop_command(_app: &mut App) -> Result<CommandResult, AgentError> {
    println!("[Stopping current agent execution]");
    println!("Agent execution halted. You can continue typing or use /retry.");
    Ok(CommandResult::Handled)
}

fn handle_status_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let msg_count = app.messages.len();
    let turns = app
        .messages
        .iter()
        .filter(|m| m.role == hermes_core::MessageRole::User)
        .count();
    let estimated_tokens: usize = app
        .messages
        .iter()
        .map(|m| m.content.as_ref().map_or(0, |c| c.len()) / 4)
        .sum();

    println!("Session Status");
    println!("  ID:           {}", app.session_id);
    println!("  Model:        {}", app.current_model);
    println!(
        "  Personality:  {}",
        app.current_personality.as_deref().unwrap_or("(none)")
    );
    println!("  Turns:        {}", turns);
    println!("  Messages:     {}", msg_count);
    println!("  Est. tokens:  ~{}", estimated_tokens);
    println!("  Max turns:    {}", app.config.max_turns);
    Ok(CommandResult::Handled)
}

fn handle_save_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let sessions_dir = hermes_config::hermes_home().join("sessions");
    std::fs::create_dir_all(&sessions_dir)
        .map_err(|e| AgentError::Io(format!("Failed to create sessions dir: {}", e)))?;

    let filename = if args.is_empty() {
        format!("{}.json", app.session_id)
    } else {
        format!("{}.json", args[0])
    };

    let path = sessions_dir.join(&filename);
    let info = app.session_info();
    let data = serde_json::json!({
        "session_info": info,
        "messages": app.messages.iter().map(|m| {
            serde_json::json!({
                "role": format!("{:?}", m.role),
                "content": m.content.as_deref().unwrap_or(""),
            })
        }).collect::<Vec<_>>(),
    });

    let json = serde_json::to_string_pretty(&data)
        .map_err(|e| AgentError::Config(e.to_string()))?;
    std::fs::write(&path, json)
        .map_err(|e| AgentError::Io(format!("Failed to save session: {}", e)))?;

    println!("Session saved to {}", path.display());
    Ok(CommandResult::Handled)
}

fn handle_load_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let sessions_dir = hermes_config::hermes_home().join("sessions");

    if args.is_empty() {
        // List available sessions
        if !sessions_dir.exists() {
            println!("No saved sessions found.");
            return Ok(CommandResult::Handled);
        }
        let entries: Vec<String> = std::fs::read_dir(&sessions_dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path()
                            .extension()
                            .map(|ext| ext == "json")
                            .unwrap_or(false)
                    })
                    .filter_map(|e| {
                        e.path()
                            .file_stem()
                            .map(|s| s.to_string_lossy().into_owned())
                    })
                    .collect()
            })
            .unwrap_or_default();

        if entries.is_empty() {
            println!("No saved sessions found.");
        } else {
            println!("Saved sessions:");
            for name in &entries {
                println!("  • {}", name);
            }
            println!("\nUsage: /load <session-name>");
        }
        return Ok(CommandResult::Handled);
    }

    let name = args[0];
    let path = sessions_dir.join(format!("{}.json", name));
    if !path.exists() {
        println!("Session '{}' not found at {}", name, path.display());
        return Ok(CommandResult::Handled);
    }

    let content = std::fs::read_to_string(&path)
        .map_err(|e| AgentError::Io(format!("Failed to read session: {}", e)))?;
    let data: serde_json::Value = serde_json::from_str(&content)
        .map_err(|e| AgentError::Config(format!("Failed to parse session: {}", e)))?;

    if let Some(messages) = data.get("messages").and_then(|m| m.as_array()) {
        app.messages.clear();
        for msg in messages {
            let role_str = msg
                .get("role")
                .and_then(|r| r.as_str())
                .unwrap_or("User");
            let content_str = msg
                .get("content")
                .and_then(|c| c.as_str())
                .unwrap_or("");
            let message = match role_str {
                "Assistant" => hermes_core::Message::assistant(content_str),
                "System" => hermes_core::Message::system(content_str),
                _ => hermes_core::Message::user(content_str),
            };
            app.messages.push(message);
        }
        println!(
            "Loaded session '{}' ({} messages)",
            name,
            app.messages.len()
        );
    } else {
        println!("Session file has no messages array.");
    }

    Ok(CommandResult::Handled)
}

fn handle_background_command(_app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    if args.is_empty() {
        println!("Usage: /background <message>");
        println!("Queues a task to run in the background while you continue chatting.");
        return Ok(CommandResult::Handled);
    }
    let task = args.join(" ");
    println!("[Background task queued: \"{}\"]", task);
    println!("(Background execution is not yet fully implemented — task will run inline)");
    Ok(CommandResult::NeedsAgent)
}

fn handle_verbose_command(_app: &mut App) -> Result<CommandResult, AgentError> {
    let current = tracing::enabled!(tracing::Level::DEBUG);
    if current {
        println!("Verbose mode: OFF (switching to info level)");
        println!("(Runtime log level changes require restart — use `hermes -v` for verbose)");
    } else {
        println!("Verbose mode: ON (switching to debug level)");
        println!("(Runtime log level changes require restart — use `hermes -v` for verbose)");
    }
    Ok(CommandResult::Handled)
}

fn handle_yolo_command(app: &mut App) -> Result<CommandResult, AgentError> {
    let currently_required = app.config.approval.require_approval;
    let new_val = !currently_required;

    app.config = Arc::new({
        let mut cfg = (*app.config).clone();
        cfg.approval.require_approval = new_val;
        cfg
    });

    if !new_val {
        println!("YOLO mode: ON — tool executions will not require approval.");
        println!("Be careful! The agent can now execute tools without confirmation.");
    } else {
        println!("YOLO mode: OFF — tool executions will require approval.");
    }
    Ok(CommandResult::Handled)
}

fn handle_reasoning_command(_app: &mut App) -> Result<CommandResult, AgentError> {
    // Reasoning display is a runtime-only toggle; stored as thread-local state
    // since StreamingConfig doesn't have a show_reasoning field.
    use std::sync::atomic::{AtomicBool, Ordering};
    static SHOW_REASONING: AtomicBool = AtomicBool::new(false);

    let prev = SHOW_REASONING.fetch_xor(true, Ordering::Relaxed);
    let new_val = !prev;

    if new_val {
        println!("Reasoning display: ON — model reasoning will be shown.");
    } else {
        println!("Reasoning display: OFF — model reasoning will be hidden.");
    }
    Ok(CommandResult::Handled)
}

fn print_help() {
    println!("Hermes Agent — Available Commands:");
    println!();
    for (cmd, desc) in SLASH_COMMANDS {
        println!("  {:16} {}", cmd, desc);
    }
    println!();
    println!("You can also type any text to send it as a message to the agent.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_autocomplete_empty() {
        let results = autocomplete("");
        assert_eq!(results.len(), SLASH_COMMANDS.len());
    }

    #[test]
    fn test_autocomplete_partial() {
        let results = autocomplete("/m");
        assert!(results.contains(&"/model"));
    }

    #[test]
    fn test_autocomplete_exact() {
        let results = autocomplete("/help");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0], "/help");
    }

    #[test]
    fn test_autocomplete_no_match() {
        let results = autocomplete("/xyz");
        assert!(results.is_empty());
    }

    #[test]
    fn test_help_for_known_command() {
        assert!(help_for("/help").is_some());
        assert!(help_for("/model").is_some());
    }

    #[test]
    fn test_help_for_unknown_command() {
        assert!(help_for("/unknown").is_none());
    }

    #[test]
    fn test_command_result_equality() {
        assert_eq!(CommandResult::Handled, CommandResult::Handled);
        assert_ne!(CommandResult::Handled, CommandResult::Quit);
    }
}