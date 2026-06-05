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
        CommandInfo {
            name: "/new",
            aliases: &[],
            description: "Start a new conversation",
            usage: "/new",
        },
        CommandInfo {
            name: "/reset",
            aliases: &["/clear"],
            description: "Reset the current session",
            usage: "/reset",
        },
        CommandInfo {
            name: "/model",
            aliases: &[],
            description: "Show or switch the LLM model",
            usage: "/model [name]",
        },
        CommandInfo {
            name: "/personality",
            aliases: &["/persona"],
            description: "Show or switch personality",
            usage: "/personality [name]",
        },
        CommandInfo {
            name: "/retry",
            aliases: &[],
            description: "Retry the last message",
            usage: "/retry",
        },
        CommandInfo {
            name: "/undo",
            aliases: &[],
            description: "Undo the last exchange",
            usage: "/undo",
        },
        CommandInfo {
            name: "/compress",
            aliases: &[],
            description: "Compress conversation context",
            usage: "/compress",
        },
        CommandInfo {
            name: "/usage",
            aliases: &["/cost"],
            description: "Show token usage and cost",
            usage: "/usage",
        },
        CommandInfo {
            name: "/stop",
            aliases: &["/cancel"],
            description: "Stop the running agent",
            usage: "/stop",
        },
        CommandInfo {
            name: "/background",
            aliases: &["/bg"],
            description: "Run a task in the background",
            usage: "/background <prompt>",
        },
        CommandInfo {
            name: "/btw",
            aliases: &[],
            description: "Side conversation without context",
            usage: "/btw <prompt>",
        },
        CommandInfo {
            name: "/reasoning",
            aliases: &["/think"],
            description: "Toggle reasoning display",
            usage: "/reasoning",
        },
        CommandInfo {
            name: "/fast",
            aliases: &[],
            description: "Switch to fast model",
            usage: "/fast",
        },
        CommandInfo {
            name: "/verbose",
            aliases: &[],
            description: "Toggle verbose output",
            usage: "/verbose",
        },
        CommandInfo {
            name: "/yolo",
            aliases: &[],
            description: "Toggle auto-approve mode",
            usage: "/yolo",
        },
        CommandInfo {
            name: "/sethome",
            aliases: &[],
            description: "Set the working directory",
            usage: "/sethome <path>",
        },
        CommandInfo {
            name: "/status",
            aliases: &[],
            description: "Show current status",
            usage: "/status",
        },
        CommandInfo {
            name: "/approve",
            aliases: &[],
            description: "Authorize a user id for DM access (admin only)",
            usage: "/approve <user_id>",
        },
        CommandInfo {
            name: "/deny",
            aliases: &[],
            description: "Revoke a user id from DM access (admin only)",
            usage: "/deny <user_id>",
        },
        CommandInfo {
            name: "/reload_mcp",
            aliases: &["/mcp_reload"],
            description: "Reload MCP tool/server registrations",
            usage: "/reload_mcp",
        },
        CommandInfo {
            name: "/provider",
            aliases: &[],
            description: "Show or switch provider",
            usage: "/provider [name]",
        },
        CommandInfo {
            name: "/profile",
            aliases: &[],
            description: "Show or switch profile",
            usage: "/profile [name]",
        },
        CommandInfo {
            name: "/branch",
            aliases: &[],
            description: "Show or switch branch context",
            usage: "/branch [name]",
        },
        CommandInfo {
            name: "/rollback",
            aliases: &[],
            description: "Rollback N latest messages (default 2)",
            usage: "/rollback [steps]",
        },
        CommandInfo {
            name: "/update",
            aliases: &[],
            description: "Check for updates",
            usage: "/update",
        },
        CommandInfo {
            name: "/tools",
            aliases: &[],
            description: "List, enable, or disable tools",
            usage: "/tools [list|enable|disable] [name]",
        },
        CommandInfo {
            name: "/sessions",
            aliases: &[],
            description: "List or switch sessions",
            usage: "/sessions [id]",
        },
        CommandInfo {
            name: "/budget",
            aliases: &[],
            description: "Show or set usage budget",
            usage: "/budget [amount]",
        },
        CommandInfo {
            name: "/insights",
            aliases: &[],
            description: "Show conversation insights",
            usage: "/insights",
        },
        CommandInfo {
            name: "/help",
            aliases: &["/commands"],
            description: "Show this help message",
            usage: "/help",
        },
    ]
}

fn normalize_command_args(args: &str) -> String {
    args.replace("\u{2014}\u{2014}", "--")
        .replace('\u{2014}', "--")
        .replace('\u{2013}', "-")
}

/// Returns true when the gateway can handle this slash command without the agent loop.
pub fn is_known_gateway_command(input: &str) -> bool {
    !matches!(handle_command(input), GatewayCommandResult::Unknown(_))
}

// ---------------------------------------------------------------------------
// Batch command parsing
// ---------------------------------------------------------------------------

/// How a command behaves in a batch (multi-command) message context.
///
/// Used by [`parse_batch_commands`] to decide whether a multi-line message
/// containing several slash commands can be dispatched automatically.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BatchCommandClass {
    /// Can be dispatched in parallel without touching session state.
    /// Examples: `/background`, `/btw`.
    FireAndForget,
    /// Stateless query — safe to run sequentially, each sends its own reply.
    /// Examples: `/status`, `/usage`, `/help`.
    ReadOnly,
    /// Mutates session or agent state.  Must NOT appear in a batch.
    /// Examples: `/model`, `/reset`, `/new`, `/yolo`, `/rollback`.
    SessionMutation,
    /// Interrupt or access-control action.  Must NOT appear in a batch.
    /// Examples: `/stop`, `/approve`, `/deny`, `/retry`, `/undo`.
    Control,
}

/// A single command parsed from a multi-line batch message.
#[derive(Debug, Clone)]
pub struct BatchedCommand {
    /// Canonical lower-case command name without the leading `/`.
    pub name: String,
    /// Everything after the command keyword (iOS dashes normalised).
    pub args: String,
    /// Batch execution class.
    pub class: BatchCommandClass,
}

/// Classify a raw (lower-cased, alias-expanded) command name into its batch class.
///
/// Conservative: unknown or ambiguous commands default to `SessionMutation`
/// so they are never silently batched.
pub fn classify_batch_class(name: &str) -> BatchCommandClass {
    // Normalise aliases to canonical names first.
    let canonical = match name {
        "bg"         => "background",
        "clear"      => "reset",
        "cancel"     => "stop",
        "cost"       => "usage",
        "persona"    => "personality",
        "think"      => "reasoning",
        "commands"   => "help",
        "mcp_reload" => "reload_mcp",
        other        => other,
    };
    match canonical {
        // ── FireAndForget ──────────────────────────────────────────────────
        "background" | "btw" => BatchCommandClass::FireAndForget,

        // ── Control ────────────────────────────────────────────────────────
        "stop" | "approve" | "deny" | "retry" | "undo" => BatchCommandClass::Control,

        // ── ReadOnly ───────────────────────────────────────────────────────
        // Only commands whose behaviour is *always* read-only regardless of args.
        "status" | "usage" | "insights" | "help" => BatchCommandClass::ReadOnly,

        // ── SessionMutation (everything else that the gateway handles) ─────
        "new" | "reset" | "model" | "personality" | "compress" | "verbose"
        | "yolo" | "sethome" | "reload_mcp" | "provider" | "profile"
        | "branch" | "rollback" | "update" | "fast" | "reasoning"
        | "tools" | "sessions" | "budget" => BatchCommandClass::SessionMutation,

        // Unknown / not a gateway command → conservative
        _ => BatchCommandClass::SessionMutation,
    }
}

/// Parse a multi-line message into individual [`BatchedCommand`]s.
///
/// Returns a non-empty `Vec` only when **2 or more** slash commands are found.
/// Returns an empty `Vec` when there is 0 or 1 command — caller falls through
/// to the normal single-command path.
///
/// Parsing rules
/// - Split text into lines.
/// - A line whose first non-whitespace character is `/` starts a new command
///   (first word = command name, rest = beginning of args).
/// - Subsequent lines that do NOT start a new command are continuation lines
///   appended to the current command's args (useful for multi-line prompts
///   like `/background <first line>\n<more context>`).
/// - Lines that appear before the first slash command are ignored.
/// - Empty prompts are silently discarded.
pub fn parse_batch_commands(text: &str) -> Vec<BatchedCommand> {
    let mut commands: Vec<BatchedCommand> = Vec::new();
    let mut cur_name: Option<String> = None;
    let mut cur_args: Option<String> = None;

    let flush = |name: Option<String>, args: Option<String>, out: &mut Vec<BatchedCommand>| {
        if let (Some(n), Some(a)) = (name, args) {
            let args_trimmed = a.trim().to_string();
            // Discard fire-and-forget commands whose prompt is empty — they have
            // no useful work to do and would just error downstream.
            if n == "background" || n == "bg" || n == "btw" {
                if args_trimmed.is_empty() {
                    return;
                }
            }
            let class = classify_batch_class(&n);
            out.push(BatchedCommand {
                name: n,
                args: args_trimmed,
                class,
            });
        }
    };

    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.starts_with('/') {
            // Flush the previous command (if any).
            flush(cur_name.take(), cur_args.take(), &mut commands);

            // Parse the new command.
            let parts: Vec<&str> = line.splitn(2, ' ').collect();
            let raw_cmd = parts[0][1..].to_lowercase(); // strip leading /
            // Reject file paths (contain '/') and empty names.
            if raw_cmd.is_empty() || raw_cmd.contains('/') {
                cur_name = None;
                cur_args = None;
                continue;
            }
            let raw_args = parts.get(1).copied().unwrap_or("");
            cur_name = Some(raw_cmd);
            cur_args = Some(normalize_command_args(raw_args));
        } else if let Some(ref mut args_buf) = cur_args {
            // Continuation line for the current command's args.
            if !line.is_empty() {
                args_buf.push('\n');
                args_buf.push_str(line);
            }
        }
        // Lines before any slash command are silently ignored.
    }

    // Flush the last command.
    flush(cur_name, cur_args, &mut commands);

    if commands.len() >= 2 {
        commands
    } else {
        Vec::new()
    }
}

/// Parse and dispatch a gateway slash command.
pub fn handle_command(input: &str) -> GatewayCommandResult {
    let trimmed = input.trim();
    if !trimmed.starts_with('/') {
        return GatewayCommandResult::Unknown(format!("Not a command: {}", trimmed));
    }

    let parts: Vec<&str> = trimmed.splitn(2, ' ').collect();
    let cmd = parts[0].to_lowercase();
    let args = normalize_command_args(parts.get(1).map(|s| s.trim()).unwrap_or(""));

    match cmd.as_str() {
        "/new" => GatewayCommandResult::ResetSession("🆕 New conversation started.".to_string()),
        "/reset" | "/clear" => GatewayCommandResult::ResetSession("🔄 Session reset.".to_string()),
        "/model" => {
            if args.is_empty() {
                GatewayCommandResult::Reply(
                    "Current model shown in status. Use /model <name> to switch.".to_string(),
                )
            } else {
                GatewayCommandResult::SwitchModel {
                    model: args.clone(),
                    reply: format!("🔀 Model switched to: {}", args),
                }
            }
        }
        "/personality" | "/persona" => {
            if args.is_empty() {
                GatewayCommandResult::Reply("Use /personality <name> to switch.".to_string())
            } else {
                GatewayCommandResult::SwitchPersonality {
                    name: args.clone(),
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
                    user_id: args.clone(),
                }
            }
        }
        "/deny" => {
            if args.is_empty() {
                GatewayCommandResult::Reply("Usage: /deny <user_id>".to_string())
            } else {
                GatewayCommandResult::DenyUser {
                    user_id: args.clone(),
                }
            }
        }
        "/compress" => GatewayCommandResult::CompressContext("📦 Context compressed.".to_string()),
        "/usage" | "/cost" => {
            GatewayCommandResult::ShowUsage("Usage statistics will be shown.".to_string())
        }
        "/stop" | "/cancel" => GatewayCommandResult::StopAgent("⏹ Agent stopped.".to_string()),
        "/background" | "/bg" => {
            if args.is_empty() {
                GatewayCommandResult::Reply("Usage: /background <prompt>".to_string())
            } else {
                GatewayCommandResult::BackgroundTask {
                    prompt: args.clone(),
                }
            }
        }
        "/btw" => {
            if args.is_empty() {
                GatewayCommandResult::Reply("Usage: /btw <prompt>".to_string())
            } else {
                GatewayCommandResult::BtwTask {
                    prompt: args.clone(),
                }
            }
        }
        "/reasoning" | "/think" => {
            GatewayCommandResult::ToggleReasoning("🧠 Reasoning display toggled.".to_string())
        }
        "/fast" => GatewayCommandResult::SwitchFast("⚡ Switched to fast model.".to_string()),
        "/verbose" => GatewayCommandResult::ToggleVerbose("📝 Verbose mode toggled.".to_string()),
        "/yolo" => GatewayCommandResult::ToggleYolo(
            "🤠 YOLO mode toggled. Auto-approving all actions.".to_string(),
        ),
        "/sethome" => {
            if args.is_empty() {
                GatewayCommandResult::Reply("Usage: /sethome <path>".to_string())
            } else {
                GatewayCommandResult::SetHome {
                    path: args.clone(),
                    reply: format!("🏠 Home directory set to: {}", args),
                }
            }
        }
        "/status" => {
            GatewayCommandResult::ShowStatus("Status information will be shown.".to_string())
        }
        "/reload_mcp" | "/mcp_reload" => GatewayCommandResult::ReloadMcp,
        "/provider" => {
            if args.is_empty() {
                GatewayCommandResult::Reply(
                    "Current provider shown in status. Use /provider <name> to switch.".to_string(),
                )
            } else {
                GatewayCommandResult::SwitchProvider {
                    provider: args.clone(),
                    reply: format!("🔌 Provider switched to: {}", args),
                }
            }
        }
        "/profile" => {
            if args.is_empty() {
                GatewayCommandResult::Reply(
                    "Current profile shown in status. Use /profile <name> to switch.".to_string(),
                )
            } else {
                GatewayCommandResult::SwitchProfile {
                    profile: args.clone(),
                    reply: format!("👤 Profile switched to: {}", args),
                }
            }
        }
        "/branch" => {
            if args.is_empty() {
                GatewayCommandResult::SwitchBranch { branch: None }
            } else {
                GatewayCommandResult::SwitchBranch {
                    branch: Some(args.clone()),
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
                    "Usage: /tools [list] [filter] | /tools enable <name> | /tools disable <name>"
                        .to_string(),
                ),
                [only] => GatewayCommandResult::ListTools {
                    filter: Some((*only).to_string()),
                },
                _ => GatewayCommandResult::Reply(
                    "Usage: /tools [list] [filter] | /tools enable <name> | /tools disable <name>"
                        .to_string(),
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
                        "Usage: /budget [amount] — amount must be a non-negative number."
                            .to_string(),
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
                help.push_str(&format!(
                    "  `{}` — {}\n",
                    cmd_info.usage, cmd_info.description
                ));
            }
            GatewayCommandResult::ShowHelp(help)
        }
        _ => GatewayCommandResult::Unknown(format!(
            "Unknown command: {}. Type /help for available commands.",
            cmd
        )),
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
    fn test_model_switch_normalizes_ios_unicode_dashes() {
        let em_dash = '\u{2014}';
        let en_dash = '\u{2013}';

        let em_input = format!("/model glm-4.7 {}provider zai", em_dash);
        match handle_command(&em_input) {
            GatewayCommandResult::SwitchModel { model, .. } => {
                assert_eq!(model, "glm-4.7 --provider zai");
            }
            other => panic!("Expected SwitchModel for em dash input, got {:?}", other),
        }

        let en_input = format!("/model glm-4.7 {}provider zai", en_dash);
        match handle_command(&en_input) {
            GatewayCommandResult::SwitchModel { model, .. } => {
                assert_eq!(model, "glm-4.7 -provider zai");
            }
            other => panic!("Expected SwitchModel for en dash input, got {:?}", other),
        }
    }

    #[test]
    fn test_is_known_gateway_command() {
        assert!(is_known_gateway_command("/new"));
        assert!(is_known_gateway_command("/status"));
        assert!(!is_known_gateway_command("/xyz123"));
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
            GatewayCommandResult::ListTools { filter } => {
                assert_eq!(filter.as_deref(), Some("grep"))
            }
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
            GatewayCommandResult::ListTools { filter } => {
                assert_eq!(filter.as_deref(), Some("my_filter"))
            }
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
            GatewayCommandResult::SwitchSession { session_id } => {
                assert_eq!(session_id, "multi word id")
            }
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

    // -----------------------------------------------------------------------
    // BatchCommandClass / classify_batch_class tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_classify_fire_and_forget() {
        assert_eq!(classify_batch_class("background"), BatchCommandClass::FireAndForget);
        assert_eq!(classify_batch_class("bg"), BatchCommandClass::FireAndForget);
        assert_eq!(classify_batch_class("btw"), BatchCommandClass::FireAndForget);
    }

    #[test]
    fn test_classify_control() {
        for cmd in &["stop", "cancel", "approve", "deny", "retry", "undo"] {
            assert_eq!(
                classify_batch_class(cmd),
                BatchCommandClass::Control,
                "/{} should be Control",
                cmd
            );
        }
    }

    #[test]
    fn test_classify_readonly() {
        for cmd in &["status", "usage", "cost", "insights", "help", "commands"] {
            assert_eq!(
                classify_batch_class(cmd),
                BatchCommandClass::ReadOnly,
                "/{} should be ReadOnly",
                cmd
            );
        }
    }

    #[test]
    fn test_classify_session_mutation() {
        for cmd in &[
            "new", "reset", "clear", "model", "personality", "persona",
            "compress", "verbose", "yolo", "sethome", "reload_mcp", "mcp_reload",
            "provider", "profile", "branch", "rollback", "update", "fast",
            "reasoning", "think", "tools", "sessions", "budget",
        ] {
            assert_eq!(
                classify_batch_class(cmd),
                BatchCommandClass::SessionMutation,
                "/{} should be SessionMutation",
                cmd
            );
        }
    }

    #[test]
    fn test_classify_unknown_is_conservative() {
        assert_eq!(classify_batch_class("xyzzy"), BatchCommandClass::SessionMutation);
    }

    // -----------------------------------------------------------------------
    // parse_batch_commands tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_batch_single_returns_empty() {
        assert!(parse_batch_commands("").is_empty());
        assert!(parse_batch_commands("/background single task").is_empty());
        assert!(parse_batch_commands("/status").is_empty());
    }

    #[test]
    fn test_batch_two_background_tasks() {
        let text = "/background task one\n/background task two";
        let cmds = parse_batch_commands(text);
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0].name, "background");
        assert_eq!(cmds[0].args, "task one");
        assert_eq!(cmds[0].class, BatchCommandClass::FireAndForget);
        assert_eq!(cmds[1].args, "task two");
    }

    #[test]
    fn test_batch_three_mixed_ff() {
        let text = "/background 写邮件\n/background 设定提醒\n/btw 现在几点";
        let cmds = parse_batch_commands(text);
        assert_eq!(cmds.len(), 3);
        assert_eq!(cmds[0].args, "写邮件");
        assert_eq!(cmds[1].args, "设定提醒");
        assert_eq!(cmds[2].name, "btw");
        assert_eq!(cmds[2].args, "现在几点");
        assert!(cmds.iter().all(|c| c.class == BatchCommandClass::FireAndForget));
    }

    #[test]
    fn test_batch_bg_alias_normalised() {
        let text = "/bg task A\n/bg task B";
        let cmds = parse_batch_commands(text);
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0].name, "bg");
        assert_eq!(cmds[0].class, BatchCommandClass::FireAndForget);
    }

    #[test]
    fn test_batch_multiline_prompt_continuation() {
        let text = "/background first line\nmore context here\n/background another task";
        let cmds = parse_batch_commands(text);
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0].args, "first line\nmore context here");
        assert_eq!(cmds[1].args, "another task");
    }

    #[test]
    fn test_batch_readonly_pair() {
        let text = "/status\n/usage";
        let cmds = parse_batch_commands(text);
        assert_eq!(cmds.len(), 2);
        assert_eq!(cmds[0].name, "status");
        assert_eq!(cmds[0].class, BatchCommandClass::ReadOnly);
        assert_eq!(cmds[1].name, "usage");
        assert_eq!(cmds[1].class, BatchCommandClass::ReadOnly);
    }

    #[test]
    fn test_batch_contains_session_mutation() {
        let text = "/background task\n/model gpt-4";
        let cmds = parse_batch_commands(text);
        assert_eq!(cmds.len(), 2);
        assert!(cmds.iter().any(|c| c.class == BatchCommandClass::SessionMutation));
    }

    #[test]
    fn test_batch_contains_control() {
        let text = "/background task\n/stop";
        let cmds = parse_batch_commands(text);
        assert_eq!(cmds.len(), 2);
        assert!(cmds.iter().any(|c| c.class == BatchCommandClass::Control));
    }

    #[test]
    fn test_batch_empty_ff_prompt_discarded() {
        // /background with no args should be dropped; only 1 valid command → empty
        let text = "/background\n/background real task";
        let cmds = parse_batch_commands(text);
        // Only "real task" survives; count < 2 → empty
        assert!(cmds.is_empty());
    }

    #[test]
    fn test_batch_ios_dash_normalisation_in_args() {
        let em = '\u{2014}';
        let text = format!("/background task {}provider openai\n/background another", em);
        let cmds = parse_batch_commands(&text);
        assert_eq!(cmds.len(), 2);
        assert!(cmds[0].args.contains("--provider"), "em-dash should be normalised to --");
    }

    #[test]
    fn test_batch_real_world_three_background() {
        let text = "/background 帮我写一封邮件介绍产品\n\
                   /background 帮我定个5分钟后的提醒\n\
                   /background 分析桌面下的所有文件";
        let cmds = parse_batch_commands(text);
        assert_eq!(cmds.len(), 3);
        assert!(cmds.iter().all(|c| c.class == BatchCommandClass::FireAndForget));
        assert_eq!(cmds[0].args, "帮我写一封邮件介绍产品");
        assert_eq!(cmds[1].args, "帮我定个5分钟后的提醒");
        assert_eq!(cmds[2].args, "分析桌面下的所有文件");
    }
}
