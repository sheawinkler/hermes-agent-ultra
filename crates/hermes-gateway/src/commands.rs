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
    /// List, enable, or disable tools.
    ListTools { filter: Option<String> },
    /// Enable a specific tool.
    EnableTool { name: String },
    /// Disable a specific tool.
    DisableTool { name: String },
    /// List or switch sessions.
    ListSessions,
    /// Switch to a specific session.
    SwitchSession { session_id: String },
    /// Show or set usage budget.
    ShowBudget { new_budget: Option<f64> },
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
    /// Approve a user for DM access.
    ApproveUser { user_id: String },
    /// Deny and revoke a user from DM access.
    DenyUser { user_id: String },
    /// Reload MCP server registry/state.
    ReloadMcp,
    /// Switch active provider.
    SwitchProvider { provider: String, reply: String },
    /// Switch active profile.
    SwitchProfile { profile: String, reply: String },
    /// Show or switch current branch.
    SwitchBranch { branch: Option<String> },
    /// Rollback conversation messages.
    Rollback { steps: usize },
    /// Check for updates.
    CheckUpdate,
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
        CommandInfo { name: "/approve", aliases: &[], description: "Authorize a user id for DM access (admin only)", usage: "/approve <user_id>" },
        CommandInfo { name: "/deny", aliases: &[], description: "Revoke a user id from DM access (admin only)", usage: "/deny <user_id>" },
        CommandInfo { name: "/reload_mcp", aliases: &["/mcp_reload"], description: "Reload MCP tool/server registrations", usage: "/reload_mcp" },
        CommandInfo { name: "/provider", aliases: &[], description: "Show or switch provider", usage: "/provider [name]" },
        CommandInfo { name: "/profile", aliases: &[], description: "Show or switch profile", usage: "/profile [name]" },
        CommandInfo { name: "/branch", aliases: &[], description: "Show or switch branch context", usage: "/branch [name]" },
        CommandInfo { name: "/rollback", aliases: &[], description: "Rollback N latest messages (default 2)", usage: "/rollback [steps]" },
        CommandInfo { name: "/update", aliases: &[], description: "Check for updates", usage: "/update" },
        CommandInfo { name: "/tools", aliases: &[], description: "List, enable, or disable tools", usage: "/tools [list|enable|disable] [name]" },
        CommandInfo { name: "/sessions", aliases: &[], description: "List or switch sessions", usage: "/sessions [id]" },
        CommandInfo { name: "/budget", aliases: &[], description: "Show or set usage budget", usage: "/budget [amount]" },
        CommandInfo { name: "/insights", aliases: &[], description: "Show conversation insights", usage: "/insights" },
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
        "/approve" => {
            if args.is_empty() {
                GatewayCommandResult::Reply("Usage: /approve <user_id>".to_string())
            } else {
                GatewayCommandResult::ApproveUser {
                    user_id: args.to_string(),
                }
            }
        }
        "/deny" => {
            if args.is_empty() {
                GatewayCommandResult::Reply("Usage: /deny <user_id>".to_string())
            } else {
                GatewayCommandResult::DenyUser {
                    user_id: args.to_string(),
                }
            }
        }
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
        "/reload_mcp" | "/mcp_reload" => GatewayCommandResult::ReloadMcp,
        "/provider" => {
            if args.is_empty() {
                GatewayCommandResult::Reply("Current provider shown in status. Use /provider <name> to switch.".to_string())
            } else {
                GatewayCommandResult::SwitchProvider {
                    provider: args.to_string(),
                    reply: format!("🔌 Provider switched to: {}", args),
                }
            }
        }
        "/profile" => {
            if args.is_empty() {
                GatewayCommandResult::Reply("Current profile shown in status. Use /profile <name> to switch.".to_string())
            } else {
                GatewayCommandResult::SwitchProfile {
                    profile: args.to_string(),
                    reply: format!("👤 Profile switched to: {}", args),
                }
            }
        }
        "/branch" => {
            if args.is_empty() {
                GatewayCommandResult::SwitchBranch { branch: None }
            } else {
                GatewayCommandResult::SwitchBranch {
                    branch: Some(args.to_string()),
                }
            }
        }
        "/rollback" => {
            if args.is_empty() {
                GatewayCommandResult::Rollback { steps: 2 }
            } else {
                let parsed = args.parse::<usize>().unwrap_or(2).max(1);
                GatewayCommandResult::Rollback { steps: parsed }
            }
        }
        "/update" => GatewayCommandResult::CheckUpdate,
        "/tools" => {
            let tokens: Vec<&str> = args.split_whitespace().collect();
            match tokens.as_slice() {
                [] => GatewayCommandResult::ListTools { filter: None },
                ["list", rest @ ..] => GatewayCommandResult::ListTools {
                    filter: if rest.is_empty() {
                        None
                    } else {
                        Some(rest.join(" "))
                    },
                },
                ["enable", rest @ ..] if !rest.is_empty() => GatewayCommandResult::EnableTool {
                    name: rest.join(" "),
                },
                ["disable", rest @ ..] if !rest.is_empty() => GatewayCommandResult::DisableTool {
                    name: rest.join(" "),
                },
                ["enable"] | ["disable"] => GatewayCommandResult::Reply(
                    "Usage: /tools [list] [filter] | /tools enable <name> | /tools disable <name>".to_string(),
                ),
                [only] => GatewayCommandResult::ListTools {
                    filter: Some((*only).to_string()),
                },
                _ => GatewayCommandResult::Reply(
                    "Usage: /tools [list] [filter] | /tools enable <name> | /tools disable <name>".to_string(),
                ),
            }
        }
        "/sessions" => {
            if args.is_empty() {
                GatewayCommandResult::ListSessions
            } else {
                GatewayCommandResult::SwitchSession {
                    session_id: args.to_string(),
                }
            }
        }
        "/budget" => {
            if args.is_empty() {
                GatewayCommandResult::ShowBudget { new_budget: None }
            } else {
                match args.trim().parse::<f64>() {
                    Ok(v) if v.is_finite() && v >= 0.0 => GatewayCommandResult::ShowBudget {
                        new_budget: Some(v),
                    },
                    _ => GatewayCommandResult::Reply(
                        "Usage: /budget [amount] — amount must be a non-negative number.".to_string(),
                    ),
                }
            }
        }
        "/insights" => GatewayCommandResult::ShowInsights(
            "📌 Conversation insights will be shown here.".to_string(),
        ),
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

    #[test]
    fn test_admin_commands() {
        match handle_command("/approve alice") {
            GatewayCommandResult::ApproveUser { user_id } => assert_eq!(user_id, "alice"),
            other => panic!("Expected ApproveUser, got {:?}", other),
        }
        match handle_command("/deny bob") {
            GatewayCommandResult::DenyUser { user_id } => assert_eq!(user_id, "bob"),
            other => panic!("Expected DenyUser, got {:?}", other),
        }
    }

    #[test]
    fn test_profile_provider_and_rollback_commands() {
        match handle_command("/provider openai") {
            GatewayCommandResult::SwitchProvider { provider, .. } => assert_eq!(provider, "openai"),
            other => panic!("Expected SwitchProvider, got {:?}", other),
        }
        match handle_command("/profile prod") {
            GatewayCommandResult::SwitchProfile { profile, .. } => assert_eq!(profile, "prod"),
            other => panic!("Expected SwitchProfile, got {:?}", other),
        }
        match handle_command("/rollback 5") {
            GatewayCommandResult::Rollback { steps } => assert_eq!(steps, 5),
            other => panic!("Expected Rollback, got {:?}", other),
        }
    }

    #[test]
    fn test_tools_list_enable_disable() {
        match handle_command("/tools") {
            GatewayCommandResult::ListTools { filter } => assert!(filter.is_none()),
            other => panic!("Expected ListTools, got {:?}", other),
        }
        match handle_command("/tools list") {
            GatewayCommandResult::ListTools { filter } => assert!(filter.is_none()),
            other => panic!("Expected ListTools, got {:?}", other),
        }
        match handle_command("/tools list grep") {
            GatewayCommandResult::ListTools { filter } => assert_eq!(filter.as_deref(), Some("grep")),
            other => panic!("Expected ListTools with filter, got {:?}", other),
        }
        match handle_command("/tools enable fs_read") {
            GatewayCommandResult::EnableTool { name } => assert_eq!(name, "fs_read"),
            other => panic!("Expected EnableTool, got {:?}", other),
        }
        match handle_command("/tools disable shell") {
            GatewayCommandResult::DisableTool { name } => assert_eq!(name, "shell"),
            other => panic!("Expected DisableTool, got {:?}", other),
        }
        match handle_command("/tools enable") {
            GatewayCommandResult::Reply(msg) => assert!(msg.contains("Usage")),
            other => panic!("Expected Reply for bare enable, got {:?}", other),
        }
        match handle_command("/tools my_filter") {
            GatewayCommandResult::ListTools { filter } => assert_eq!(filter.as_deref(), Some("my_filter")),
            other => panic!("Expected ListTools with shorthand filter, got {:?}", other),
        }
    }

    #[test]
    fn test_sessions_and_budget_and_insights() {
        match handle_command("/sessions") {
            GatewayCommandResult::ListSessions => {}
            other => panic!("Expected ListSessions, got {:?}", other),
        }
        match handle_command("/sessions abc-123") {
            GatewayCommandResult::SwitchSession { session_id } => assert_eq!(session_id, "abc-123"),
            other => panic!("Expected SwitchSession, got {:?}", other),
        }
        match handle_command("/sessions multi word id") {
            GatewayCommandResult::SwitchSession { session_id } => assert_eq!(session_id, "multi word id"),
            other => panic!("Expected SwitchSession, got {:?}", other),
        }
        match handle_command("/budget") {
            GatewayCommandResult::ShowBudget { new_budget } => assert!(new_budget.is_none()),
            other => panic!("Expected ShowBudget (show), got {:?}", other),
        }
        match handle_command("/budget 12.5") {
            GatewayCommandResult::ShowBudget { new_budget } => assert_eq!(new_budget, Some(12.5)),
            other => panic!("Expected ShowBudget (set), got {:?}", other),
        }
        match handle_command("/budget -1") {
            GatewayCommandResult::Reply(msg) => assert!(msg.contains("Usage")),
            other => panic!("Expected Reply for invalid budget, got {:?}", other),
        }
        match handle_command("/budget nan") {
            GatewayCommandResult::Reply(msg) => assert!(msg.contains("Usage")),
            other => panic!("Expected Reply for invalid budget, got {:?}", other),
        }
        match handle_command("/insights") {
            GatewayCommandResult::ShowInsights(s) => assert!(!s.is_empty()),
            other => panic!("Expected ShowInsights, got {:?}", other),
        }
    }
}
