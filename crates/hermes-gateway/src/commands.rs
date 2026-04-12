//! Gateway slash command handler.
//!
//! Processes user-issued slash commands from messaging platforms
//! (Telegram, Discord, Slack, etc.) and returns responses or actions.

/// Result of processing a gateway slash command.
#[derive(Debug, Clone)]
pub enum GatewayCommandResult {
    /// Send a text reply to the user.
    Reply(String),
    /// Reset the session and send a reply.
    ResetSession(String),
    /// Switch the model and send a reply.
    SwitchModel { model: String, reply: String },
    /// Switch the personality and send a reply.
    SwitchPersonality { name: String, reply: String },
    /// Stop the currently running agent task.
    StopAgent(String),
    /// Show usage statistics.
    ShowUsage(String),
    /// Compress the conversation context.
    CompressContext(String),
    /// Show insights about the conversation.
    ShowInsights(String),
    /// Toggle verbose mode.
    ToggleVerbose(String),
    /// Toggle YOLO (auto-approve) mode.
    ToggleYolo(String),
    /// Set the home/working directory.
    SetHome { path: String, reply: String },
    /// Show status information.
    ShowStatus(String),
    /// Show help text.
    ShowHelp(String),
    /// Trigger background task.
    BackgroundTask { prompt: String },
    /// BTW (side conversation) task.
    BtwTask { prompt: String },
    /// Toggle reasoning display.
    ToggleReasoning(String),
    /// Switch to fast model.
    SwitchFast(String),
    /// Retry the last message.
    Retry,
    /// Undo the last exchange.
    Undo,
    /// Unknown command.
    Unknown(String),
    /// No-op (command handled internally).
    Noop,
}

/// Metadata about a slash command.
pub struct CommandInfo {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub usage: &'static str,
}

/// All registered gateway slash commands.
pub fn all_commands() -> Vec<CommandInfo> {
    vec![
        CommandInfo { name: "/new", aliases: &[], description: "Start a new conversation", usage: "/new" },
        CommandInfo { name: "/reset", aliases: &["/clear"], description: "Reset the current session", usage: "/reset" },
        CommandInfo { name: "/model", aliases: &[], description: "Show or switch the LLM model", usage: "/model [name]" },
        CommandInfo { name: "/personality", aliases: &["/persona"], description: "Show or switch personality", usage: "/personality [name]" },
        CommandInfo { name: "/retry", aliases: &[], description: "Retry the last message", usage: "/retry" },
        CommandInfo { name: "/undo", aliases: &[], description: "Undo the last exchange", usage: "/undo" },
        CommandInfo { name: "/compress", aliases: &[], description: "Compress conversation context", usage: "/compress" },
        CommandInfo { name: "/usage", aliases: &["/cost"], description: "Show token usage and cost", usage: "/usage" },
        CommandInfo { name: "/stop", aliases: &["/cancel"], description: "Stop the running agent", usage: "/stop" },
        CommandInfo { name: "/background", aliases: &["/bg"], description: "Run a task in the background", usage: "/background <prompt>" },
        CommandInfo { name: "/btw", aliases: &[], description: "Side conversation without context", usage: "/btw <prompt>" },
        CommandInfo { name: "/reasoning", aliases: &["/think"], description: "Toggle reasoning display", usage: "/reasoning" },
        CommandInfo { name: "/fast", aliases: &[], description: "Switch to fast model", usage: "/fast" },
        CommandInfo { name: "/verbose", aliases: &[], description: "Toggle verbose output", usage: "/verbose" },
        CommandInfo { name: "/yolo", aliases: &[], description: "Toggle auto-approve mode", usage: "/yolo" },
        CommandInfo { name: "/sethome", aliases: &[], description: "Set the working directory", usage: "/sethome <path>" },
        CommandInfo { name: "/status", aliases: &[], description: "Show current status", usage: "/status" },
        CommandInfo { name: "/help", aliases: &["/commands"], description: "Show this help message", usage: "/help" },
    ]
}

/// Parse and dispatch a gateway slash command.
pub fn handle_command(input: &str) -> GatewayCommandResult {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return GatewayCommandResult::Unknown(format!("Not a command: {}", trimmed));
    }

    let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
    let cmd = parts[0].to_lowercase();
    let args = parts.get(1).map(|s| s.trim()).unwrap_or("");

    match cmd.as_str() {
        "/new" => GatewayCommandResult::ResetSession("🆕 New conversation started.".to_string()),
        "/reset" | "/clear" => GatewayCommandResult::ResetSession("🔄 Session reset.".to_string()),
        "/model" => {
            if args.is_empty() {
                GatewayCommandResult::Reply("Current model shown in status. Use /model <name> to switch.".to_string())
            } else {
                GatewayCommandResult::SwitchModel {
                    model: args.to_string(),
                    reply: format!("🔀 Model switched to: {}", args),
                }
            }
        }
        "/personality" | "/persona" => {
            if args.is_empty() {
                GatewayCommandResult::Reply("Use /personality <name> to switch.".to_string())
            } else {
                GatewayCommandResult::SwitchPersonality {
                    name: args.to_string(),
                    reply: format!("🎭 Personality switched to: {}", args),
                }
            }
        }
        "/retry" => GatewayCommandResult::Retry,
        "/undo" => GatewayCommandResult::Undo,
        "/compress" => GatewayCommandResult::CompressContext("📦 Context compressed.".to_string()),
        "/usage" | "/cost" => GatewayCommandResult::ShowUsage("Usage statistics will be shown.".to_string()),
        "/stop" | "/cancel" => GatewayCommandResult::StopAgent("⏹ Agent stopped.".to_string()),
        "/background" | "/bg" => {
            if args.is_empty() {
                GatewayCommandResult::Reply("Usage: /background <prompt>".to_string())
            } else {
                GatewayCommandResult::BackgroundTask { prompt: args.to_string() }
            }
        }
        "/btw" => {
            if args.is_empty() {
                GatewayCommandResult::Reply("Usage: /btw <prompt>".to_string())
            } else {
                GatewayCommandResult::BtwTask { prompt: args.to_string() }
            }
        }
        "/reasoning" | "/think" => GatewayCommandResult::ToggleReasoning("🧠 Reasoning display toggled.".to_string()),
        "/fast" => GatewayCommandResult::SwitchFast("⚡ Switched to fast model.".to_string()),
        "/verbose" => GatewayCommandResult::ToggleVerbose("📝 Verbose mode toggled.".to_string()),
        "/yolo" => GatewayCommandResult::ToggleYolo("🤠 YOLO mode toggled. Auto-approving all actions.".to_string()),
        "/sethome" => {
            if args.is_empty() {
                GatewayCommandResult::Reply("Usage: /sethome <path>".to_string())
            } else {
                GatewayCommandResult::SetHome {
                    path: args.to_string(),
                    reply: format!("🏠 Home directory set to: {}", args),
                }
            }
        }
        "/status" => GatewayCommandResult::ShowStatus("Status information will be shown.".to_string()),
        "/help" | "/commands" => {
            let mut help = String::from("📖 **Available Commands:**\n\n");
            for cmd_info in all_commands() {
                help.push_str(&format!("  `{}` — {}\n", cmd_info.usage, cmd_info.description));
            }
            GatewayCommandResult::ShowHelp(help)
        }
        _ => GatewayCommandResult::Unknown(format!("Unknown command: {}. Type /help for available commands.", cmd)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_new_command() {
        match handle_command("/new") {
            GatewayCommandResult::ResetSession(_) => {}
            other => panic!("Expected ResetSession, got {:?}", other),
        }
    }

    #[test]
    fn test_model_switch() {
        match handle_command("/model gpt-4o") {
            GatewayCommandResult::SwitchModel { model, .. } => {
                assert_eq!(model, "gpt-4o");
            }
            other => panic!("Expected SwitchModel, got {:?}", other),
        }
    }

    #[test]
    fn test_help_command() {
        match handle_command("/help") {
            GatewayCommandResult::ShowHelp(text) => {
                assert!(text.contains("Available Commands"));
            }
            other => panic!("Expected ShowHelp, got {:?}", other),
        }
    }

    #[test]
    fn test_unknown_command() {
        match handle_command("/xyz123") {
            GatewayCommandResult::Unknown(_) => {}
            other => panic!("Expected Unknown, got {:?}", other),
        }
    }

    #[test]
    fn test_background_without_args() {
        match handle_command("/background") {
            GatewayCommandResult::Reply(msg) => {
                assert!(msg.contains("Usage"));
            }
            other => panic!("Expected Reply, got {:?}", other),
        }
    }

    #[test]
    fn test_background_with_args() {
        match handle_command("/background check disk space") {
            GatewayCommandResult::BackgroundTask { prompt } => {
                assert_eq!(prompt, "check disk space");
            }
            other => panic!("Expected BackgroundTask, got {:?}", other),
        }
    }

    #[test]
    fn test_case_insensitive() {
        match handle_command("/NEW") {
            GatewayCommandResult::ResetSession(_) => {}
            other => panic!("Expected ResetSession, got {:?}", other),
        }
    }

    #[test]
    fn test_alias_commands() {
        match handle_command("/clear") {
            GatewayCommandResult::ResetSession(_) => {}
            other => panic!("Expected ResetSession for /clear, got {:?}", other),
        }
        match handle_command("/cancel") {
            GatewayCommandResult::StopAgent(_) => {}
            other => panic!("Expected StopAgent for /cancel, got {:?}", other),
        }
    }
}
