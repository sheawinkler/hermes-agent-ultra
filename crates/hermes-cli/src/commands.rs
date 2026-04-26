//! Slash command handler (Requirement 9.2).
//!
//! Defines and dispatches all supported `/` commands in the interactive
//! REPL, and provides auto-completion suggestions.

use std::process::Stdio;
use std::sync::Arc;

use hermes_core::AgentError;
use regex::Regex;

use crate::app::App;
use crate::model_switch::{curated_provider_slugs, normalize_provider_model, provider_model_ids};

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
    (
        "/model",
        "Show current model, set directly, or pick provider/model interactively",
    ),
    (
        "/personality",
        "Show current personality, list built-ins, or switch mode",
    ),
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
    (
        "/policy",
        "Policy lifecycle (needs HERMES_POLICY_ADMIN_TOKEN, same as HTTP X-Hermes-Policy-Admin)",
    ),
    ("/help", "Show help for available commands"),
    ("/quit", "Quit the application"),
    ("/exit", "Alias for /quit"),
];

const DEFAULT_SKILL_TAPS: &[&str] = &["https://github.com/MiniMax-AI/cli"];

fn read_skill_taps(path: &std::path::Path) -> Vec<String> {
    if !path.exists() {
        return Vec::new();
    }
    let content = std::fs::read_to_string(path).unwrap_or_else(|_| "[]".to_string());
    serde_json::from_str(&content).unwrap_or_default()
}

fn merged_skill_taps(custom_taps: &[String]) -> Vec<String> {
    let mut merged: Vec<String> = Vec::new();
    for tap in DEFAULT_SKILL_TAPS {
        merged.push((*tap).to_string());
    }
    for tap in custom_taps {
        if !merged.iter().any(|existing| existing == tap) {
            merged.push(tap.clone());
        }
    }
    merged
}

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
        "/model" => handle_model_command(app, args).await,
        "/personality" => handle_personality_command(app, args),
        "/skills" => handle_skills_command(app),
        "/tools" => handle_tools_command(app),
        "/config" => handle_config_command(app, args),
        "/compress" => handle_compress_command(app),
        "/usage" => handle_usage_command(app),
        "/stop" => handle_stop_command(app),
        "/status" => handle_status_command(app),
        "/save" => handle_save_command(app, args),
        "/load" => handle_load_command(app, args),
        "/background" => handle_background_command(app, args),
        "/verbose" => handle_verbose_command(app),
        "/yolo" => handle_yolo_command(app),
        "/reasoning" => handle_reasoning_command(app),
        "/policy" => handle_policy_command(app, args),
        "/help" => {
            print_help();
            Ok(CommandResult::Handled)
        }
        "/quit" | "/exit" => {
            println!("Goodbye!");
            Ok(CommandResult::Quit)
        }
        _ => {
            println!(
                "Unknown command: {}. Type /help for available commands.",
                cmd
            );
            Ok(CommandResult::Handled)
        }
    }
}

// ---------------------------------------------------------------------------
// Individual command handlers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
enum ModelSwitchRequest {
    PickProviderThenModel,
    PickModelFromProvider(String),
    SetDirect(String),
}

fn parse_model_switch_request(args: &[&str], known_providers: &[&str]) -> ModelSwitchRequest {
    if args.is_empty() {
        return ModelSwitchRequest::PickProviderThenModel;
    }
    let raw = args.join(" ");
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return ModelSwitchRequest::PickProviderThenModel;
    }
    if trimmed.contains(':') {
        return ModelSwitchRequest::SetDirect(trimmed.to_string());
    }
    if known_providers
        .iter()
        .any(|p| p.eq_ignore_ascii_case(trimmed))
    {
        return ModelSwitchRequest::PickModelFromProvider(trimmed.to_ascii_lowercase());
    }
    ModelSwitchRequest::SetDirect(trimmed.to_string())
}

fn split_provider_model(provider_model: &str) -> (&str, &str) {
    provider_model
        .split_once(':')
        .unwrap_or(("openai", provider_model))
}

async fn pick_model_for_provider(
    app: &mut App,
    provider: &str,
    current_model: &str,
) -> Result<bool, AgentError> {
    let models = provider_model_ids(provider).await;
    if models.is_empty() {
        println!("No models available for provider '{}'.", provider);
        return Ok(false);
    }

    let (_, current_model_id) = split_provider_model(current_model);
    let default_index = models
        .iter()
        .position(|m| m.eq_ignore_ascii_case(current_model_id))
        .unwrap_or(0);
    let labels: Vec<String> = models.clone();
    let title = format!("Select {} model ({} available)", provider, labels.len());
    let pick = crate::curses_select(&title, &labels, default_index);
    if !pick.confirmed || pick.index >= models.len() {
        println!("Model switch cancelled.");
        return Ok(false);
    }
    let provider_model = format!("{}:{}", provider, models[pick.index].trim());
    app.switch_model(&provider_model);
    println!("Model switched to: {}", provider_model);
    Ok(true)
}

async fn handle_model_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let known_providers = curated_provider_slugs();
    match parse_model_switch_request(args, &known_providers) {
        ModelSwitchRequest::SetDirect(raw) => {
            let provider_model = normalize_provider_model(&raw)?;
            app.switch_model(&provider_model);
            println!("Model switched to: {}", provider_model);
        }
        ModelSwitchRequest::PickModelFromProvider(provider) => {
            let current_model = app.current_model.clone();
            pick_model_for_provider(app, &provider, &current_model).await?;
        }
        ModelSwitchRequest::PickProviderThenModel => {
            println!("Current model: {}", app.current_model);
            let providers: Vec<String> = known_providers.iter().map(|p| (*p).to_string()).collect();
            if providers.is_empty() {
                println!("No providers are registered for selection.");
                return Ok(CommandResult::Handled);
            }
            let (current_provider, _) = split_provider_model(&app.current_model);
            let default_provider_index = providers
                .iter()
                .position(|p| p.eq_ignore_ascii_case(current_provider))
                .unwrap_or(0);
            let provider_pick =
                crate::curses_select("Select provider", &providers, default_provider_index);
            if !provider_pick.confirmed || provider_pick.index >= providers.len() {
                println!("Model switch cancelled.");
                return Ok(CommandResult::Handled);
            }
            let provider = providers[provider_pick.index].as_str();
            let current_model = app.current_model.clone();
            pick_model_for_provider(app, provider, &current_model).await?;
        }
    }
    Ok(CommandResult::Handled)
}

fn handle_personality_command(app: &mut App, args: &[&str]) -> Result<CommandResult, AgentError> {
    let builtin = hermes_agent::builtin_personality_names();
    let builtin_descriptions = hermes_agent::builtin_personality_descriptions();
    let print_builtin = || {
        println!("Built-in personalities:");
        for (name, usage) in builtin_descriptions {
            println!("  - {:<14} {}", name, usage);
        }
    };
    if args.is_empty() {
        match &app.current_personality {
            Some(p) => println!("Current personality: {}", p),
            None => println!("No personality set"),
        }
        print_builtin();
    } else if args.len() == 1 && args[0].eq_ignore_ascii_case("list") {
        print_builtin();
    } else {
        let name = args.join(" ");
        app.switch_personality(&name);
        println!("Personality switched to: {}", name);
        if !name.contains(char::is_whitespace)
            && !name.eq_ignore_ascii_case("default")
            && !builtin.iter().any(|n| n.eq_ignore_ascii_case(&name))
        {
            println!(
                "Note: '{}' is not built-in. Hermes will look for personalities/{}.md or treat inline text as compatibility mode.",
                name, name
            );
        }
    }
    Ok(CommandResult::Handled)
}

fn handle_skills_command(_app: &mut App) -> Result<CommandResult, AgentError> {
    let skills_dir = hermes_config::hermes_home().join("skills");
    if !skills_dir.exists() {
        println!(
            "No skills directory found at {}. Run `hermes setup` first.",
            skills_dir.display()
        );
        return Ok(CommandResult::Handled);
    }

    let mut skills: Vec<(String, String)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&skills_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            let skill_md = path.join("SKILL.md");
            if !path.is_dir() || !skill_md.exists() {
                continue;
            }
            let name = path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let title = std::fs::read_to_string(&skill_md)
                .ok()
                .and_then(|c| {
                    c.lines()
                        .find(|l| l.starts_with('#'))
                        .map(|l| l.trim_start_matches('#').trim().to_string())
                })
                .unwrap_or_else(|| "(no description)".to_string());
            skills.push((name, title));
        }
    }
    skills.sort_by(|a, b| a.0.cmp(&b.0));

    if skills.is_empty() {
        println!("No installed skills found in {}.", skills_dir.display());
        println!("Install skills with `hermes skills install <name>`.");
    } else {
        println!("Installed skills ({}):", skills.len());
        for (name, title) in &skills {
            println!("  • {} — {}", name, title);
        }
        println!("\nUse `hermes skills inspect <name>` for details.");
    }
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
                println!("Unknown config action '{}'. Use 'get' or 'set'.", args[0]);
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
    app.messages
        .push(hermes_core::Message::system(summary_text));
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

fn handle_stop_command(app: &mut App) -> Result<CommandResult, AgentError> {
    app.interrupt_controller.interrupt(None);
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

    let json =
        serde_json::to_string_pretty(&data).map_err(|e| AgentError::Config(e.to_string()))?;
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
            let role_str = msg.get("role").and_then(|r| r.as_str()).unwrap_or("User");
            let content_str = msg.get("content").and_then(|c| c.as_str()).unwrap_or("");
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
    let job_id = format!(
        "{}-{}",
        chrono::Utc::now().format("%Y%m%d%H%M%S"),
        uuid::Uuid::new_v4().simple()
    );
    let jobs_dir = hermes_config::hermes_home().join("background_jobs");
    std::fs::create_dir_all(&jobs_dir).map_err(|e| {
        AgentError::Io(format!(
            "Failed to create background job directory {}: {}",
            jobs_dir.display(),
            e
        ))
    })?;
    let status_path = jobs_dir.join(format!("{}.json", job_id));
    let log_path = jobs_dir.join(format!("{}.log", job_id));

    let status = serde_json::json!({
        "id": job_id,
        "task": task,
        "status": "queued",
        "created_at": chrono::Utc::now().to_rfc3339(),
        "started_at": serde_json::Value::Null,
        "finished_at": serde_json::Value::Null,
        "exit_code": serde_json::Value::Null,
        "log_path": log_path,
    });
    std::fs::write(
        &status_path,
        serde_json::to_string_pretty(&status).unwrap_or_else(|_| "{}".to_string()),
    )
    .map_err(|e| AgentError::Io(format!("Failed to write background status: {}", e)))?;

    let task_for_run = task.clone();
    let status_path_for_run = status_path.clone();
    let log_path_for_run = log_path.clone();
    tokio::spawn(async move {
        let started = chrono::Utc::now().to_rfc3339();
        let mut queued = read_json_map(&status_path_for_run);
        queued.insert(
            "status".to_string(),
            serde_json::Value::String("running".into()),
        );
        queued.insert(
            "started_at".to_string(),
            serde_json::Value::String(started.clone()),
        );
        let _ = write_json_map(&status_path_for_run, &queued);

        let exe = match std::env::current_exe() {
            Ok(p) => p,
            Err(e) => {
                let mut failed = queued.clone();
                failed.insert("status".into(), serde_json::Value::String("failed".into()));
                failed.insert(
                    "finished_at".into(),
                    serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
                );
                failed.insert(
                    "error".into(),
                    serde_json::Value::String(format!("current_exe: {}", e)),
                );
                let _ = write_json_map(&status_path_for_run, &failed);
                return;
            }
        };

        let mut cmd = tokio::process::Command::new(exe);
        cmd.arg("chat")
            .arg("--query")
            .arg(task_for_run)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Ok(home) = std::env::var("HERMES_HOME") {
            cmd.env("HERMES_HOME", home);
        }

        let out = cmd.output().await;
        match out {
            Ok(output) => {
                let exit = output.status.code().unwrap_or(-1);
                let stdout = String::from_utf8_lossy(&output.stdout);
                let stderr = String::from_utf8_lossy(&output.stderr);
                let log = format!(
                    "task: {}\nstarted_at: {}\nfinished_at: {}\nexit_code: {}\n\n[stdout]\n{}\n\n[stderr]\n{}\n",
                    queued
                        .get("task")
                        .and_then(|v| v.as_str())
                        .unwrap_or(""),
                    started,
                    chrono::Utc::now().to_rfc3339(),
                    exit,
                    stdout,
                    stderr
                );
                let _ = std::fs::write(&log_path_for_run, log);

                let mut done = queued.clone();
                done.insert(
                    "status".into(),
                    serde_json::Value::String(if output.status.success() {
                        "completed".into()
                    } else {
                        "failed".into()
                    }),
                );
                done.insert(
                    "finished_at".into(),
                    serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
                );
                done.insert("exit_code".into(), serde_json::json!(exit));
                let _ = write_json_map(&status_path_for_run, &done);
            }
            Err(e) => {
                let mut failed = queued.clone();
                failed.insert("status".into(), serde_json::Value::String("failed".into()));
                failed.insert(
                    "finished_at".into(),
                    serde_json::Value::String(chrono::Utc::now().to_rfc3339()),
                );
                failed.insert(
                    "error".into(),
                    serde_json::Value::String(format!("spawn/output failed: {}", e)),
                );
                let _ = write_json_map(&status_path_for_run, &failed);
            }
        }
    });

    println!("[Background task queued: \"{}\"]", task);
    println!("Job ID: {}", status["id"].as_str().unwrap_or("unknown"));
    println!("Status: {}", status_path.display());
    println!("Logs:   {}", log_path.display());
    println!("This task runs in a detached `hermes chat --query ...` process.");

    Ok(CommandResult::Handled)
}

fn read_json_map(path: &std::path::Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default()
}

fn write_json_map(
    path: &std::path::Path,
    map: &serde_json::Map<String, serde_json::Value>,
) -> Result<(), std::io::Error> {
    let content = serde_json::to_string_pretty(&serde_json::Value::Object(map.clone()))
        .unwrap_or_else(|_| "{}".to_string());
    std::fs::write(path, content)
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

fn handle_policy_command(_app: &mut App, _args: &[&str]) -> Result<CommandResult, AgentError> {
    println!(
        "The adaptive `/policy` CLI was removed — Hermes Python has no equivalent policy store."
    );
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

// ---------------------------------------------------------------------------
// CLI subcommand handlers (dispatched from main.rs)
// ---------------------------------------------------------------------------

/// Handle `hermes chat [--query ...] [--preload-skill ...] [--yolo]`.
pub async fn handle_cli_chat(
    query: Option<String>,
    preload_skill: Option<String>,
    yolo: bool,
) -> Result<(), hermes_core::AgentError> {
    use crate::runtime_tool_wiring::{wire_cron_scheduler_backend, wire_stdio_clarify_backend};
    use crate::terminal_backend::build_terminal_backend;
    use crate::tool_preview::{build_tool_preview_from_value, tool_emoji};
    use hermes_config::load_config;
    use hermes_core::MessageRole;
    use hermes_cron::cron_scheduler_for_data_dir;
    use hermes_skills::{FileSkillStore, SkillManager};
    use hermes_tools::ToolRegistry;

    if let Some(skill) = &preload_skill {
        println!("[Preloading skill: {}]", skill);
    }
    if yolo {
        println!("[YOLO mode: tool confirmations disabled]");
    }

    let mut config =
        load_config(None).map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;

    if yolo {
        config.approval.require_approval = false;
    }

    let current_model = config.model.clone().unwrap_or_else(|| "gpt-4o".to_string());

    let tool_registry = Arc::new(ToolRegistry::new());
    let terminal_backend = build_terminal_backend(&config);
    let skill_store = Arc::new(FileSkillStore::new(FileSkillStore::default_dir()));
    let skill_provider: Arc<dyn hermes_core::SkillProvider> =
        Arc::new(SkillManager::new(skill_store));
    hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);
    wire_stdio_clarify_backend(&tool_registry);
    let cron_data_dir = hermes_config::cron_dir();
    std::fs::create_dir_all(&cron_data_dir)
        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
    let cron_scheduler = Arc::new(cron_scheduler_for_data_dir(cron_data_dir));
    cron_scheduler
        .load_persisted_jobs()
        .await
        .map_err(|e| hermes_core::AgentError::Config(format!("cron load: {e}")))?;
    cron_scheduler.start().await;
    wire_cron_scheduler_backend(&tool_registry, cron_scheduler);
    let tool_schemas =
        crate::platform_toolsets::resolve_platform_tool_schemas(&config, "cli", &tool_registry);
    let agent_tool_registry = Arc::new(crate::app::bridge_tool_registry(&tool_registry));

    let agent_config = crate::app::build_agent_config(&config, &current_model);
    let provider = crate::app::build_provider(&config, &current_model);

    let on_tool_start: Box<dyn Fn(&str, &serde_json::Value) + Send + Sync> =
        Box::new(move |name: &str, args: &serde_json::Value| {
            let emoji = tool_emoji(name);
            let preview = build_tool_preview_from_value(name, args, 56).unwrap_or_default();
            if preview.is_empty() {
                println!("┊ {emoji} {name}");
            } else {
                println!("┊ {emoji} {name:<16} {preview}");
            }
        });
    let on_tool_complete: Box<dyn Fn(&str, &str) + Send + Sync> =
        Box::new(move |name: &str, result: &str| {
            let mut snippet: String = result.trim().chars().take(96).collect();
            if result.trim().chars().count() > 96 {
                snippet.push_str("...");
            }
            let emoji = tool_emoji(name);
            if snippet.is_empty() {
                println!("┊ {emoji} {name:<16} done");
            } else {
                println!("┊ {emoji} {name:<16} done: {snippet}");
            }
        });
    let callbacks = hermes_agent::AgentCallbacks {
        on_tool_start: Some(on_tool_start),
        on_tool_complete: Some(on_tool_complete),
        ..Default::default()
    };
    let agent = hermes_agent::AgentLoop::new(agent_config, agent_tool_registry, provider)
        .with_callbacks(callbacks);

    match query {
        Some(q) => {
            let messages = vec![hermes_core::Message::user(&q)];
            let result = agent.run(messages, Some(tool_schemas)).await?;

            let reply = result
                .messages
                .iter()
                .rev()
                .find_map(|m| {
                    if m.role == MessageRole::Assistant {
                        m.content.clone()
                    } else {
                        None
                    }
                })
                .unwrap_or_else(|| "(no assistant reply)".to_string());
            println!("{}", reply);
        }
        None => {
            println!("Starting interactive chat session...");
            println!("(Use `hermes` for the default interactive TUI)");
        }
    }
    Ok(())
}

/// Handle `hermes skills [action] [name] [--extra ...]`.
pub async fn handle_cli_skills(
    action: Option<String>,
    name: Option<String>,
    extra: Option<String>,
) -> Result<(), hermes_core::AgentError> {
    let skills_dir = hermes_config::hermes_home().join("skills");

    match action.as_deref().unwrap_or("list") {
        "list" => {
            if !skills_dir.exists() {
                println!(
                    "No skills directory found at {}. Run `hermes setup` first.",
                    skills_dir.display()
                );
                return Ok(());
            }
            let mut count = 0u32;
            println!("Installed skills ({}):", skills_dir.display());
            if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    let skill_md = path.join("SKILL.md");
                    if path.is_dir() && skill_md.exists() {
                        let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
                        let first_line = std::fs::read_to_string(&skill_md)
                            .ok()
                            .and_then(|c| {
                                c.lines()
                                    .find(|l| l.starts_with('#'))
                                    .map(|l| l.trim_start_matches('#').trim().to_string())
                            })
                            .unwrap_or_else(|| "(no description)".to_string());
                        println!("  • {} — {}", dir_name, first_line);
                        count += 1;
                    }
                }
            }
            if count == 0 {
                println!("  (no skills installed)");
            }
        }
        "browse" => {
            if !skills_dir.exists() {
                println!("No skills directory found.");
                return Ok(());
            }
            println!("Skills Browser");
            println!("==============\n");
            let mut categories: std::collections::HashMap<String, Vec<(String, String)>> =
                std::collections::HashMap::new();
            if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    let skill_md = path.join("SKILL.md");
                    if path.is_dir() && skill_md.exists() {
                        let dir_name = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        let content = std::fs::read_to_string(&skill_md).unwrap_or_default();
                        let first_line = content
                            .lines()
                            .find(|l| l.starts_with('#'))
                            .map(|l| l.trim_start_matches('#').trim().to_string())
                            .unwrap_or_else(|| "(no description)".to_string());
                        let category = path
                            .parent()
                            .and_then(|p| p.file_name())
                            .map(|n| n.to_string_lossy().to_string())
                            .unwrap_or_else(|| "general".to_string());
                        categories
                            .entry(category)
                            .or_default()
                            .push((dir_name, first_line));
                    }
                }
            }
            for (category, skills) in &categories {
                println!("[{}]", category);
                for (name, desc) in skills {
                    println!("  • {} — {}", name, desc);
                }
                println!();
            }
            if categories.is_empty() {
                println!("  (no skills installed)");
            }
        }
        "search" => {
            let query = name.unwrap_or_default();
            if query.is_empty() {
                println!("Usage: hermes skills search <query>");
                return Ok(());
            }
            println!("Searching Skills Hub for: \"{}\"...", query);
            match reqwest::Client::new()
                .get("https://skills.hermes.run/api/search")
                .query(&[("q", &query)])
                .timeout(std::time::Duration::from_secs(10))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(data) = resp.json::<serde_json::Value>().await {
                        if let Some(results) = data.get("results").and_then(|r| r.as_array()) {
                            if results.is_empty() {
                                println!("No skills found matching \"{}\".", query);
                            } else {
                                println!("Found {} skill(s):", results.len());
                                for skill in results {
                                    let name =
                                        skill.get("name").and_then(|n| n.as_str()).unwrap_or("?");
                                    let desc = skill
                                        .get("description")
                                        .and_then(|d| d.as_str())
                                        .unwrap_or("");
                                    let version = skill
                                        .get("version")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("?");
                                    println!("  • {} (v{}) — {}", name, version, desc);
                                }
                                println!("\nInstall with: hermes skills install <name>");
                            }
                        } else {
                            println!("Unexpected response format from Skills Hub.");
                        }
                    }
                }
                Ok(resp) => {
                    println!("Skills Hub returned status {}", resp.status());
                }
                Err(e) => {
                    println!("Could not reach Skills Hub: {}", e);
                    println!("Check https://hermes.run/skills for manual browsing.");
                }
            }
        }
        "install" => {
            let skill_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing skill name. Usage: hermes skills install <name>".into(),
                )
            })?;
            println!("Installing skill: {}", skill_name);
            let target = skills_dir.join(&skill_name);
            std::fs::create_dir_all(&target).map_err(|e| {
                hermes_core::AgentError::Io(format!("Failed to create skill dir: {}", e))
            })?;
            let skill_md = target.join("SKILL.md");
            if !skill_md.exists() {
                std::fs::write(
                    &skill_md,
                    format!(
                        "# {}\n\nInstalled via CLI. Replace with actual skill content.\n",
                        skill_name
                    ),
                )
                .map_err(|e| {
                    hermes_core::AgentError::Io(format!("Failed to write SKILL.md: {}", e))
                })?;
            }
            println!("Skill '{}' installed to {}", skill_name, target.display());
        }
        "reset" => {
            let skill_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing skill name. Usage: hermes skills reset <name>".into(),
                )
            })?;
            let target = skills_dir.join(&skill_name);
            if target.exists() {
                std::fs::remove_dir_all(&target).map_err(|e| {
                    hermes_core::AgentError::Io(format!("Failed to remove skill dir: {}", e))
                })?;
            }
            std::fs::create_dir_all(&target).map_err(|e| {
                hermes_core::AgentError::Io(format!("Failed to create skill dir: {}", e))
            })?;
            std::fs::write(
                target.join("SKILL.md"),
                format!(
                    "# {}\n\nReset by CLI. Replace with canonical skill contents.\n",
                    skill_name
                ),
            )
            .map_err(|e| hermes_core::AgentError::Io(format!("Failed to write SKILL.md: {}", e)))?;
            println!("Skill '{}' reset at {}", skill_name, target.display());
        }
        "subscribe" => {
            let source = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing source. Usage: hermes skills subscribe <name-or-url>".into(),
                )
            })?;
            std::fs::create_dir_all(&skills_dir)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            let subscriptions_path = skills_dir.join("subscriptions.json");
            let mut subscriptions: Vec<serde_json::Value> = if subscriptions_path.exists() {
                let raw = std::fs::read_to_string(&subscriptions_path)
                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                serde_json::from_str(&raw).unwrap_or_default()
            } else {
                Vec::new()
            };
            let normalized = source.trim().to_string();
            if normalized.is_empty() {
                return Err(hermes_core::AgentError::Config(
                    "skills subscribe: source cannot be empty".into(),
                ));
            }
            let exists = subscriptions.iter().any(|item| {
                item.get("source")
                    .and_then(|v| v.as_str())
                    .map(|s| s == normalized)
                    .unwrap_or(false)
            });
            if exists {
                println!("Skill subscription already exists: {}", normalized);
                return Ok(());
            }
            subscriptions.push(serde_json::json!({
                "source": normalized,
                "added_at": chrono::Utc::now().to_rfc3339(),
                "options": extra.as_deref().unwrap_or(""),
            }));
            std::fs::write(
                &subscriptions_path,
                serde_json::to_string_pretty(&subscriptions)
                    .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?,
            )
            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            println!(
                "Subscribed to skill source '{}'. Registry: {}",
                source,
                subscriptions_path.display()
            );
        }
        "inspect" => {
            let skill_name = name.unwrap_or_default();
            let skill_md = skills_dir.join(&skill_name).join("SKILL.md");
            if skill_md.exists() {
                let content = std::fs::read_to_string(&skill_md)
                    .map_err(|e| hermes_core::AgentError::Io(format!("Read error: {}", e)))?;
                println!("{}", content);
            } else {
                println!("Skill '{}' not found at {}", skill_name, skill_md.display());
            }
        }
        "uninstall" => {
            let skill_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing skill name. Usage: hermes skills uninstall <name>".into(),
                )
            })?;
            let target = skills_dir.join(&skill_name);
            if target.exists() {
                std::fs::remove_dir_all(&target).map_err(|e| {
                    hermes_core::AgentError::Io(format!("Failed to remove skill: {}", e))
                })?;
                println!("Skill '{}' uninstalled.", skill_name);
            } else {
                println!("Skill '{}' not found.", skill_name);
            }
        }
        "check" => {
            let skill_name = name.unwrap_or_default();
            if skill_name.is_empty() {
                println!("Checking all installed skills...");
                let mut ok = 0u32;
                let mut issues = 0u32;
                if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                    for entry in entries.filter_map(|e| e.ok()) {
                        let path = entry.path();
                        if !path.is_dir() {
                            continue;
                        }
                        let dir_name = path
                            .file_name()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .to_string();
                        let skill_md = path.join("SKILL.md");
                        if !skill_md.exists() {
                            println!("  ✗ {} — missing SKILL.md", dir_name);
                            issues += 1;
                        } else {
                            let content = std::fs::read_to_string(&skill_md).unwrap_or_default();
                            if content.trim().is_empty() {
                                println!("  ⚠ {} — SKILL.md is empty", dir_name);
                                issues += 1;
                            } else {
                                println!("  ✓ {}", dir_name);
                                ok += 1;
                            }
                        }
                    }
                }
                println!("\n{} healthy, {} with issues.", ok, issues);
            } else {
                let skill_path = skills_dir.join(&skill_name);
                let skill_md = skill_path.join("SKILL.md");
                if !skill_path.exists() {
                    println!("Skill '{}' not found.", skill_name);
                } else if !skill_md.exists() {
                    println!("Skill '{}': MISSING SKILL.md", skill_name);
                } else {
                    let content = std::fs::read_to_string(&skill_md).unwrap_or_default();
                    let lines = content.lines().count();
                    let has_frontmatter = content.starts_with("---");
                    println!("Skill '{}': OK", skill_name);
                    println!("  Path: {}", skill_path.display());
                    println!("  SKILL.md: {} lines", lines);
                    println!(
                        "  Frontmatter: {}",
                        if has_frontmatter { "yes" } else { "no" }
                    );
                }
            }
        }
        "update" => {
            println!("Checking for skill updates...\n");
            if !skills_dir.exists() {
                println!("No skills installed.");
                return Ok(());
            }

            let apply_updates = extra.as_deref() == Some("--apply");

            // Collect installed skills with their local versions
            struct LocalSkill {
                name: String,
                version: String,
            }
            let mut installed: Vec<LocalSkill> = Vec::new();

            if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    let skill_md = path.join("SKILL.md");
                    if !skill_md.exists() {
                        continue;
                    }

                    let dir_name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let content = std::fs::read_to_string(&skill_md).unwrap_or_default();
                    let (fm, _body) = hermes_tools::tools::skill_utils::parse_frontmatter(&content);
                    let version = fm
                        .get("version")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();

                    installed.push(LocalSkill {
                        name: dir_name,
                        version,
                    });
                }
            }

            if installed.is_empty() {
                println!("No skills installed.");
                return Ok(());
            }

            println!(
                "{:30} {:>12} {:>12}   {}",
                "Skill", "Local", "Hub", "Status"
            );
            println!("{}", "-".repeat(75));

            let client = reqwest::Client::new();
            let mut updates_available: Vec<(String, String)> = Vec::new();

            for skill in &installed {
                // Query Hub for latest version
                let hub_url = format!(
                    "https://agentskills.io/api/v1/skills/{}/versions",
                    skill.name
                );
                let hub_result = client
                    .get(&hub_url)
                    .timeout(std::time::Duration::from_secs(10))
                    .send()
                    .await;

                match hub_result {
                    Ok(resp) if resp.status().is_success() => {
                        if let Ok(data) = resp.json::<serde_json::Value>().await {
                            let latest = data
                                .get("latest")
                                .and_then(|v| v.as_str())
                                .unwrap_or("unknown");

                            let status = if skill.version == "unknown" || latest == "unknown" {
                                "unknown".to_string()
                            } else {
                                match hermes_skills::compare_versions(&skill.version, latest) {
                                    std::cmp::Ordering::Less => {
                                        updates_available
                                            .push((skill.name.clone(), latest.to_string()));
                                        "⬆ update available".to_string()
                                    }
                                    std::cmp::Ordering::Equal => "✓ up-to-date".to_string(),
                                    std::cmp::Ordering::Greater => "⚠ local is newer".to_string(),
                                }
                            };
                            println!(
                                "{:30} {:>12} {:>12}   {}",
                                skill.name, skill.version, latest, status
                            );
                        }
                    }
                    Ok(resp) if resp.status() == reqwest::StatusCode::NOT_FOUND => {
                        println!(
                            "{:30} {:>12} {:>12}   {}",
                            skill.name, skill.version, "-", "not on hub"
                        );
                    }
                    _ => {
                        println!(
                            "{:30} {:>12} {:>12}   {}",
                            skill.name, skill.version, "?", "hub unreachable"
                        );
                    }
                }
            }

            println!();
            if updates_available.is_empty() {
                println!("All skills are up to date.");
            } else {
                println!("{} update(s) available.", updates_available.len());

                if apply_updates {
                    println!("\nApplying updates...");
                    for (skill_name, new_version) in &updates_available {
                        let download_url = format!(
                            "https://agentskills.io/api/v1/skills/{}/download?version={}",
                            skill_name, new_version
                        );
                        match client
                            .get(&download_url)
                            .timeout(std::time::Duration::from_secs(30))
                            .send()
                            .await
                        {
                            Ok(resp) if resp.status().is_success() => {
                                if let Ok(bytes) = resp.bytes().await {
                                    let dec = flate2::read::GzDecoder::new(&bytes[..]);
                                    let mut archive = tar::Archive::new(dec);
                                    if archive.unpack(&skills_dir).is_ok() {
                                        println!("  ✓ {} updated to v{}", skill_name, new_version);
                                    } else {
                                        println!("  ✗ {} — failed to extract archive", skill_name);
                                    }
                                }
                            }
                            _ => {
                                println!("  ✗ {} — download failed", skill_name);
                            }
                        }
                    }
                } else {
                    println!("Run `hermes skills update --apply` to install updates.");
                }
            }
        }
        "publish" => {
            let skill_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing skill name. Usage: hermes skills publish <name>".into(),
                )
            })?;
            let skill_path = skills_dir.join(&skill_name);
            if !skill_path.exists() {
                return Err(hermes_core::AgentError::Config(format!(
                    "Skill '{}' not found.",
                    skill_name
                )));
            }
            println!("Publishing skill '{}' to Skills Hub...", skill_name);
            println!("  Source: {}", skill_path.display());

            let skill_md = skill_path.join("SKILL.md");
            if !skill_md.exists() {
                println!("  ✗ Missing SKILL.md — required for publishing.");
                return Ok(());
            }

            let content = std::fs::read_to_string(&skill_md)
                .map_err(|e| hermes_core::AgentError::Io(format!("Read error: {}", e)))?;
            let (frontmatter, _body) =
                hermes_tools::tools::skill_utils::parse_frontmatter(&content);

            let fm_name = frontmatter.get("name").and_then(|v| v.as_str());
            let fm_version = frontmatter.get("version").and_then(|v| v.as_str());
            let fm_desc = frontmatter.get("description").and_then(|v| v.as_str());
            let fm_category = frontmatter.get("category").and_then(|v| v.as_str());

            if fm_name.is_none()
                || fm_version.is_none()
                || fm_desc.is_none()
                || fm_category.is_none()
            {
                println!(
                    "  ✗ SKILL.md frontmatter must include: name, version, description, category"
                );
                let mut missing = Vec::new();
                if fm_name.is_none() {
                    missing.push("name");
                }
                if fm_version.is_none() {
                    missing.push("version");
                }
                if fm_desc.is_none() {
                    missing.push("description");
                }
                if fm_category.is_none() {
                    missing.push("category");
                }
                println!("    Missing: {}", missing.join(", "));
                return Ok(());
            }

            let publish_name = fm_name.unwrap();
            let publish_version = fm_version.unwrap();
            let publish_desc = fm_desc.unwrap();
            let publish_category = fm_category.unwrap();
            println!(
                "  ✓ name={}, version={}, category={}",
                publish_name, publish_version, publish_category
            );
            println!("  ✓ description: {}", publish_desc);

            // Package skill directory into a tarball in memory
            let mut tar_buf = Vec::new();
            {
                let enc =
                    flate2::write::GzEncoder::new(&mut tar_buf, flate2::Compression::default());
                let mut tar_builder = tar::Builder::new(enc);
                tar_builder
                    .append_dir_all(&skill_name, &skill_path)
                    .map_err(|e| hermes_core::AgentError::Io(format!("Tar error: {}", e)))?;
                tar_builder
                    .finish()
                    .map_err(|e| hermes_core::AgentError::Io(format!("Tar finish error: {}", e)))?;
            }
            println!("  ✓ Packaged {} bytes", tar_buf.len());

            // Read hub token
            let token_path = hermes_config::hermes_home().join("hub_token");
            if !token_path.exists() {
                println!("  ✗ No hub token found at {}", token_path.display());
                println!("    Run `hermes login hub` to authenticate with Skills Hub.");
                return Ok(());
            }
            let hub_token = std::fs::read_to_string(&token_path)
                .map_err(|e| hermes_core::AgentError::Io(format!("Token read error: {}", e)))?
                .trim()
                .to_string();

            // Build metadata JSON
            let metadata = serde_json::json!({
                "name": publish_name,
                "version": publish_version,
                "description": publish_desc,
                "category": publish_category,
            });

            // Upload to Skills Hub API via multipart
            let tarball_part = reqwest::multipart::Part::bytes(tar_buf)
                .file_name(format!("{}-{}.tar.gz", publish_name, publish_version))
                .mime_str("application/gzip")
                .unwrap();
            let metadata_part = reqwest::multipart::Part::text(metadata.to_string())
                .mime_str("application/json")
                .unwrap();
            let form = reqwest::multipart::Form::new()
                .part("tarball", tarball_part)
                .part("metadata", metadata_part);

            println!("  Uploading to Skills Hub...");
            match reqwest::Client::new()
                .post("https://agentskills.io/api/v1/skills")
                .bearer_auth(&hub_token)
                .multipart(form)
                .timeout(std::time::Duration::from_secs(60))
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    let url = format!("https://agentskills.io/skills/{}", publish_name);
                    println!("  ✓ Published successfully!");
                    println!("  URL: {}", url);
                }
                Ok(resp) if resp.status() == reqwest::StatusCode::CONFLICT => {
                    println!(
                        "  ✗ Version {} already exists on Skills Hub.",
                        publish_version
                    );
                    println!("    Bump the version in SKILL.md frontmatter and try again.");
                }
                Ok(resp) if resp.status() == reqwest::StatusCode::UNAUTHORIZED => {
                    println!("  ✗ Unauthorized. Hub token may be expired.");
                    println!("    Run `hermes login hub` to re-authenticate.");
                }
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    println!("  ✗ Upload failed (HTTP {}): {}", status, body);
                }
                Err(e) => {
                    println!("  ✗ Could not reach Skills Hub: {}", e);
                }
            }
        }
        "snapshot" => {
            let sub = name.as_deref().unwrap_or("export");
            match sub {
                "export" => {
                    let output = extra.unwrap_or_else(|| {
                        format!(
                            "skills-snapshot-{}.tar.gz",
                            chrono::Utc::now().format("%Y%m%d-%H%M%S")
                        )
                    });
                    println!("Exporting skills snapshot to: {}", output);
                    if !skills_dir.exists() {
                        println!("No skills directory found.");
                        return Ok(());
                    }
                    // Create a tar.gz archive of skills directory
                    let tar_gz = std::fs::File::create(&output).map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to create archive: {}", e))
                    })?;
                    let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
                    let mut tar = tar::Builder::new(enc);
                    tar.append_dir_all("skills", &skills_dir).map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to archive: {}", e))
                    })?;
                    tar.finish().map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to finalize archive: {}", e))
                    })?;
                    println!("Snapshot exported to: {}", output);
                }
                "import" => {
                    let input = extra.ok_or_else(|| {
                        hermes_core::AgentError::Config(
                            "Missing snapshot path. Usage: hermes skills snapshot import <path>"
                                .into(),
                        )
                    })?;
                    println!("Importing skills snapshot from: {}", input);
                    let tar_gz = std::fs::File::open(&input).map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to open archive: {}", e))
                    })?;
                    let dec = flate2::read::GzDecoder::new(tar_gz);
                    let mut archive = tar::Archive::new(dec);
                    std::fs::create_dir_all(&skills_dir).map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to create skills dir: {}", e))
                    })?;
                    archive.unpack(hermes_config::hermes_home()).map_err(|e| {
                        hermes_core::AgentError::Io(format!("Failed to extract archive: {}", e))
                    })?;
                    println!("Snapshot imported successfully.");
                }
                _ => {
                    println!("Usage: hermes skills snapshot export|import [path]");
                }
            }
        }
        "tap" => {
            let sub = name.as_deref().unwrap_or("list");
            let taps_file = hermes_config::hermes_home().join("skill_taps.json");
            match sub {
                "list" => {
                    let taps = merged_skill_taps(&read_skill_taps(&taps_file));
                    if taps.is_empty() {
                        println!("No skill taps configured.");
                    } else {
                        println!("Skill taps:");
                        for tap in &taps {
                            println!("  • {}", tap);
                        }
                    }
                }
                "add" => {
                    let url = extra.ok_or_else(|| {
                        hermes_core::AgentError::Config(
                            "Missing tap URL. Usage: hermes skills tap add <url>".into(),
                        )
                    })?;
                    let mut taps: Vec<String> = read_skill_taps(&taps_file);
                    if merged_skill_taps(&taps).contains(&url) {
                        println!("Tap already exists: {}", url);
                    } else {
                        taps.push(url.clone());
                        let json = serde_json::to_string_pretty(&taps)
                            .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
                        std::fs::write(&taps_file, json)
                            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                        println!("Added tap: {}", url);
                    }
                }
                "remove" => {
                    let url = extra.ok_or_else(|| {
                        hermes_core::AgentError::Config(
                            "Missing tap URL. Usage: hermes skills tap remove <url>".into(),
                        )
                    })?;
                    if DEFAULT_SKILL_TAPS
                        .iter()
                        .any(|default_tap| default_tap == &url.as_str())
                    {
                        println!("Tap '{}' is a built-in default and cannot be removed.", url);
                        println!(
                            "Add custom taps with `hermes skills tap add <url>`; defaults remain active."
                        );
                        return Ok(());
                    }

                    let mut taps: Vec<String> = read_skill_taps(&taps_file);
                    let before_len = taps.len();
                    taps.retain(|t| t != &url);
                    if taps.len() < before_len {
                        let json = serde_json::to_string_pretty(&taps)
                            .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
                        std::fs::write(&taps_file, json)
                            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                        println!("Removed tap: {}", url);
                    } else {
                        println!("Tap not found: {}", url);
                    }
                }
                _ => {
                    println!("Usage: hermes skills tap list|add|remove [url]");
                }
            }
        }
        "config" => {
            let skill_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing skill name. Usage: hermes skills config <name> [key] [value]".into(),
                )
            })?;
            let config_file = skills_dir.join(&skill_name).join("config.json");
            if let Some(key) = extra {
                // Set or get a config key
                let parts: Vec<&str> = key.splitn(2, '=').collect();
                if parts.len() == 2 {
                    let mut config: serde_json::Value = if config_file.exists() {
                        let c = std::fs::read_to_string(&config_file)
                            .unwrap_or_else(|_| "{}".to_string());
                        serde_json::from_str(&c).unwrap_or(serde_json::json!({}))
                    } else {
                        serde_json::json!({})
                    };
                    config[parts[0]] = serde_json::Value::String(parts[1].to_string());
                    let json = serde_json::to_string_pretty(&config)
                        .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
                    std::fs::write(&config_file, json)
                        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                    println!("Set {} = {} for skill '{}'", parts[0], parts[1], skill_name);
                } else {
                    // Get value
                    if config_file.exists() {
                        let c = std::fs::read_to_string(&config_file)
                            .unwrap_or_else(|_| "{}".to_string());
                        let config: serde_json::Value =
                            serde_json::from_str(&c).unwrap_or(serde_json::json!({}));
                        match config.get(&key) {
                            Some(v) => println!("{} = {}", key, v),
                            None => println!("Key '{}' not found in skill config.", key),
                        }
                    } else {
                        println!("No config for skill '{}'.", skill_name);
                    }
                }
            } else {
                // Show all config
                if config_file.exists() {
                    let content = std::fs::read_to_string(&config_file)
                        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                    println!("Config for skill '{}':", skill_name);
                    println!("{}", content);
                } else {
                    println!("No config for skill '{}'.", skill_name);
                }
            }
        }
        "audit" => {
            println!("Security audit of installed skills");
            println!("==================================\n");
            if !skills_dir.exists() {
                println!("No skills installed.");
                return Ok(());
            }

            struct AuditFinding {
                file: String,
                pattern: String,
                severity: &'static str, // "warning" or "critical"
            }

            let shell_injection_patterns: &[(&str, &str)] = &[
                (
                    r"(?i)\b(rm\s+-rf|mkfs|dd\s+if=)",
                    "Shell command injection (destructive command)",
                ),
                (r"(?i)(:\(\)\{.*;\}|fork\s+bomb)", "Fork bomb pattern"),
                (r"(?i)\b(sudo\s+|su\s+-\s)", "Privilege escalation attempt"),
                (
                    r"(?i)(export\s+PATH|PATH\s*=\s*/)",
                    "PATH environment manipulation",
                ),
                (
                    r"(?i)chmod\s+[0-7]*777",
                    "Overly permissive file permissions",
                ),
                (r"(?i)\beval\s*\(", "Dynamic code evaluation (eval)"),
                (r"(?i)\bexec\s*\(", "Dynamic code execution (exec)"),
                (
                    r"(?i)(os\.system|subprocess\.call|subprocess\.run|subprocess\.Popen)",
                    "Subprocess execution",
                ),
            ];

            let path_traversal_patterns: &[(&str, &str)] =
                &[(r"\.\.[\\/]", "Path traversal (../)")];

            let network_patterns: &[(&str, &str)] = &[
                (r"(?i)://127\.0\.0\.1", "Internal network URL (127.0.0.1)"),
                (r"(?i)://localhost", "Internal network URL (localhost)"),
                (
                    r"(?i)://10\.\d+\.\d+\.\d+",
                    "Internal network URL (10.x.x.x)",
                ),
                (
                    r"(?i)://192\.168\.\d+\.\d+",
                    "Internal network URL (192.168.x.x)",
                ),
                (r"(?i)://0\.0\.0\.0", "Internal network URL (0.0.0.0)"),
                (r"(?i)://\[::1\]", "Internal network URL (::1)"),
            ];

            let credential_patterns: &[(&str, &str)] = &[
                (
                    r#"(?i)(password\s*=\s*['"][^'"]{3,}['"])"#,
                    "Hardcoded password",
                ),
                (
                    r#"(?i)(api[_-]?key\s*=\s*['"][^'"]{3,}['"])"#,
                    "Hardcoded API key",
                ),
                (
                    r#"(?i)(secret\s*=\s*['"][^'"]{3,}['"])"#,
                    "Hardcoded secret",
                ),
                (r"(?i)(sk-[a-zA-Z0-9]{20,})", "Exposed API key (sk-...)"),
                (r"(?i)(ghp_[a-zA-Z0-9]{30,})", "Exposed GitHub PAT"),
            ];

            let base64_suspicious: &[(&str, &str)] = &[
                (
                    r"(?i)(base64[._-]?decode|atob)\s*\(",
                    "Base64 decode invocation (potential obfuscation)",
                ),
                (
                    r"[A-Za-z0-9+/]{100,}={0,2}",
                    "Long base64-encoded content (potential obfuscation)",
                ),
            ];

            let mut total = 0u32;
            let mut total_warnings = 0u32;
            let mut total_critical = 0u32;

            fn scan_dir_recursive(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
                if let Ok(entries) = std::fs::read_dir(dir) {
                    for entry in entries.filter_map(|e| e.ok()) {
                        let p = entry.path();
                        if p.is_dir() {
                            scan_dir_recursive(&p, files);
                        } else if p.is_file() {
                            files.push(p);
                        }
                    }
                }
            }

            if let Ok(entries) = std::fs::read_dir(&skills_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    total += 1;
                    let dir_name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .to_string();
                    let mut findings: Vec<AuditFinding> = Vec::new();

                    let mut all_files = Vec::new();
                    scan_dir_recursive(&path, &mut all_files);

                    for fp in &all_files {
                        let Ok(content) = std::fs::read_to_string(fp) else {
                            continue;
                        };
                        let fname = fp
                            .strip_prefix(&path)
                            .unwrap_or(fp)
                            .to_string_lossy()
                            .to_string();

                        // Shell injection (critical)
                        for (pat, desc) in shell_injection_patterns {
                            if let Ok(re) = Regex::new(pat) {
                                if re.is_match(&content) {
                                    findings.push(AuditFinding {
                                        file: fname.clone(),
                                        pattern: desc.to_string(),
                                        severity: "critical",
                                    });
                                }
                            }
                        }

                        // Path traversal (critical)
                        for (pat, desc) in path_traversal_patterns {
                            if let Ok(re) = Regex::new(pat) {
                                if re.is_match(&content) {
                                    findings.push(AuditFinding {
                                        file: fname.clone(),
                                        pattern: desc.to_string(),
                                        severity: "critical",
                                    });
                                }
                            }
                        }

                        // Internal network URLs (warning)
                        for (pat, desc) in network_patterns {
                            if let Ok(re) = Regex::new(pat) {
                                if re.is_match(&content) {
                                    findings.push(AuditFinding {
                                        file: fname.clone(),
                                        pattern: desc.to_string(),
                                        severity: "warning",
                                    });
                                }
                            }
                        }

                        // Credential patterns (critical)
                        for (pat, desc) in credential_patterns {
                            if let Ok(re) = Regex::new(pat) {
                                if re.is_match(&content) {
                                    findings.push(AuditFinding {
                                        file: fname.clone(),
                                        pattern: desc.to_string(),
                                        severity: "critical",
                                    });
                                }
                            }
                        }

                        // Base64 suspicious (warning)
                        for (pat, desc) in base64_suspicious {
                            if let Ok(re) = Regex::new(pat) {
                                if re.is_match(&content) {
                                    findings.push(AuditFinding {
                                        file: fname.clone(),
                                        pattern: desc.to_string(),
                                        severity: "warning",
                                    });
                                }
                            }
                        }
                    }

                    if findings.is_empty() {
                        println!("  ✓ {} — clean", dir_name);
                    } else {
                        let crit_count =
                            findings.iter().filter(|f| f.severity == "critical").count();
                        let warn_count =
                            findings.iter().filter(|f| f.severity == "warning").count();
                        total_critical += crit_count as u32;
                        total_warnings += warn_count as u32;

                        let icon = if crit_count > 0 { "✗" } else { "⚠" };
                        println!(
                            "  {} {} — {} critical, {} warning(s):",
                            icon, dir_name, crit_count, warn_count
                        );
                        for f in &findings {
                            let sev_icon = if f.severity == "critical" {
                                "CRIT"
                            } else {
                                "WARN"
                            };
                            println!("    [{}] {} — {}", sev_icon, f.file, f.pattern);
                        }
                    }
                }
            }

            println!("\n{}", "=".repeat(50));
            println!("Audited {} skill(s)", total);
            println!("  Critical: {}", total_critical);
            println!("  Warnings: {}", total_warnings);
            if total_critical == 0 && total_warnings == 0 {
                println!("  Status:   All clear ✓");
            } else if total_critical > 0 {
                println!("  Status:   Action required — review critical findings");
            } else {
                println!("  Status:   Review recommended");
            }
        }
        other => {
            println!("Skills action '{}' is not recognized.", other);
            println!("Available actions: list, browse, search, install, inspect, uninstall, check, update, publish, snapshot, tap, config, audit");
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Plugin security (remote Git installs)
// ---------------------------------------------------------------------------

fn default_git_host_allowlist() -> Vec<&'static str> {
    vec![
        "github.com",
        "www.github.com",
        "raw.githubusercontent.com",
        "gitlab.com",
        "www.gitlab.com",
        "codeberg.org",
        "www.codeberg.org",
        "gitea.com",
        "bitbucket.org",
    ]
}

fn plugin_git_host_allowed(url: &str, allow_untrusted: bool) -> bool {
    if allow_untrusted {
        return true;
    }
    let extra = std::env::var("HERMES_PLUGIN_GIT_EXTRA_HOSTS").unwrap_or_default();
    let mut hosts: Vec<String> = default_git_host_allowlist()
        .into_iter()
        .map(|s| s.to_string())
        .collect();
    for part in extra.split(',') {
        let p = part.trim();
        if !p.is_empty() {
            hosts.push(p.to_lowercase());
        }
    }
    let lower = url.to_lowercase();
    let host_part = if lower.contains("://") {
        lower.split("://").nth(1).unwrap_or("")
    } else if lower.starts_with("git@") {
        lower
            .trim_start_matches("git@")
            .split(':')
            .next()
            .unwrap_or("")
    } else {
        return false;
    };
    let host = host_part
        .split('/')
        .next()
        .unwrap_or(host_part)
        .split('@')
        .last()
        .unwrap_or(host_part);
    let host = host.split(':').next().unwrap_or(host).to_lowercase();
    hosts
        .iter()
        .any(|h| host == *h || host.ends_with(&format!(".{}", h)))
}

fn short_sha(sha: &str) -> String {
    sha.chars().take(8).collect()
}

/// Static scan of a cloned plugin tree: risky patterns in scripts/config.
fn scan_plugin_security(root: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    let manifest = root.join("plugin.yaml");
    if manifest.exists() {
        if let Ok(text) = std::fs::read_to_string(&manifest) {
            if text.contains("post_install") || text.contains("postInstall") {
                out.push(
                    "plugin.yaml declares post_install / postInstall — review before running the plugin"
                        .into(),
                );
            }
            if Regex::new(r"(?i)curl\s+[^|\n]*\|\s*(ba)?sh")
                .ok()
                .and_then(|re| re.find(&text))
                .is_some()
            {
                out.push("plugin.yaml references curl|sh style install — high risk".into());
            }
        }
    }

    let risky_file_patterns: &[(&str, &[(&str, &str)])] = &[(
        r"\.(sh|bash|zsh|py|rb|ps1|fish)$",
        &[
            (r"(?i)\bcurl\s+[^|\n]*\|\s*(ba)?sh", "curl piped to shell"),
            (r"(?i)\bwget\s+[^|\n]*\|\s*(ba)?sh", "wget piped to shell"),
            (r"(?i)\beval\s*\(", "eval("),
            (r"(?i)\bexec\s*\(", "exec("),
            (r"(?i)(base64[._-]?decode|atob)\s*\(", "base64 decode"),
            (r"(?i)\brm\s+-rf\s+/", "rm -rf on absolute path"),
        ],
    )];

    fn walk(dir: &std::path::Path, files: &mut Vec<std::path::PathBuf>) {
        let name = dir.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if dir.is_dir() && (name == ".git" || name == "target" || name == "node_modules") {
            return;
        }
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, files);
                } else if p.is_file() {
                    files.push(p);
                }
            }
        }
    }

    let mut files = Vec::new();
    walk(root, &mut files);

    for fp in files {
        let fname = fp.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if fname == ".DS_Store" {
            continue;
        }
        let rel = fp.strip_prefix(root).unwrap_or(&fp).display().to_string();
        let Ok(content) = std::fs::read_to_string(&fp) else {
            continue;
        };
        for (ext_re, rules) in risky_file_patterns {
            if let Ok(re_ext) = Regex::new(ext_re) {
                if !re_ext.is_match(fname) {
                    continue;
                }
                for (pat, label) in *rules {
                    if let Ok(re) = Regex::new(pat) {
                        if re.is_match(&content) {
                            out.push(format!("{}: {}", rel, label));
                        }
                    }
                }
            }
        }
    }

    out.sort();
    out.dedup();
    out
}

async fn git_checkout_ref(repo_dir: &std::path::Path, git_ref: &str) -> Result<(), String> {
    let dir = repo_dir.to_string_lossy().to_string();
    let fetch = tokio::process::Command::new("git")
        .args(["-C", &dir, "fetch", "--depth", "1", "origin", git_ref])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !fetch.status.success() {
        let err = String::from_utf8_lossy(&fetch.stderr);
        return Err(format!("git fetch origin {}: {}", git_ref, err.trim()));
    }
    let co = tokio::process::Command::new("git")
        .args(["-C", &dir, "checkout", git_ref])
        .output()
        .await
        .map_err(|e| e.to_string())?;
    if !co.status.success() {
        let err = String::from_utf8_lossy(&co.stderr);
        return Err(format!("git checkout {}: {}", git_ref, err.trim()));
    }
    Ok(())
}

/// Handle `hermes plugins [action] [name]`.
pub async fn handle_cli_plugins(
    action: Option<String>,
    name: Option<String>,
    git_ref: Option<String>,
    allow_untrusted_git_host: bool,
) -> Result<(), hermes_core::AgentError> {
    let plugins_dir = hermes_config::hermes_home().join("plugins");

    match action.as_deref().unwrap_or("list") {
        "list" => {
            if !plugins_dir.exists() {
                println!("No plugins directory found at {}", plugins_dir.display());
                return Ok(());
            }
            let mut count = 0u32;
            println!("Installed plugins ({}):", plugins_dir.display());
            if let Ok(entries) = std::fs::read_dir(&plugins_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    let manifest = path.join("plugin.yaml");
                    if path.is_dir() && manifest.exists() {
                        let dir_name = path.file_name().unwrap_or_default().to_string_lossy();
                        let disabled_marker = path.join(".disabled");
                        let status = if disabled_marker.exists() {
                            "disabled"
                        } else {
                            "enabled"
                        };
                        println!("  • {} [{}]", dir_name, status);
                        count += 1;
                    }
                }
            }
            if count == 0 {
                println!("  (no plugins installed)");
            }
        }
        "enable" => {
            let plugin_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing plugin name. Usage: hermes plugins enable <name>".into(),
                )
            })?;
            let disabled_marker = plugins_dir.join(&plugin_name).join(".disabled");
            if disabled_marker.exists() {
                std::fs::remove_file(&disabled_marker).map_err(|e| {
                    hermes_core::AgentError::Io(format!("Failed to enable plugin: {}", e))
                })?;
                println!("Plugin '{}' enabled.", plugin_name);
            } else {
                println!(
                    "Plugin '{}' is already enabled (or not installed).",
                    plugin_name
                );
            }
        }
        "disable" => {
            let plugin_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing plugin name. Usage: hermes plugins disable <name>".into(),
                )
            })?;
            let plugin_dir = plugins_dir.join(&plugin_name);
            if !plugin_dir.exists() {
                println!("Plugin '{}' not found.", plugin_name);
                return Ok(());
            }
            let disabled_marker = plugin_dir.join(".disabled");
            std::fs::write(&disabled_marker, "").map_err(|e| {
                hermes_core::AgentError::Io(format!("Failed to disable plugin: {}", e))
            })?;
            println!("Plugin '{}' disabled.", plugin_name);
        }
        "install" => {
            let plugin_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing plugin name. Usage: hermes plugins install <name|url>".into(),
                )
            })?;
            println!("Installing plugin: {}...", plugin_name);

            let is_git_url = plugin_name.starts_with("http://")
                || plugin_name.starts_with("https://")
                || plugin_name.starts_with("git@");

            if is_git_url {
                if !plugin_git_host_allowed(&plugin_name, allow_untrusted_git_host) {
                    println!(
                        "  ✗ Git host is not on the default allow-list (github.com, gitlab.com, codeberg.org, …)."
                    );
                    println!(
                        "    Set comma-separated HERMES_PLUGIN_GIT_EXTRA_HOSTS or pass --allow-untrusted-git-host after you trust the source."
                    );
                    return Ok(());
                }
                // Extract repo name from URL for target directory
                let repo_name = plugin_name
                    .trim_end_matches('/')
                    .trim_end_matches(".git")
                    .rsplit('/')
                    .next()
                    .unwrap_or("unknown-plugin")
                    .to_string();

                // Also handle git@ SSH URLs like git@github.com:user/repo.git
                let repo_name = if repo_name.contains(':') {
                    repo_name
                        .rsplit(':')
                        .next()
                        .unwrap_or(&repo_name)
                        .trim_end_matches(".git")
                        .rsplit('/')
                        .next()
                        .unwrap_or(&repo_name)
                        .to_string()
                } else {
                    repo_name
                };

                let target = plugins_dir.join(&repo_name);
                if target.exists() {
                    println!(
                        "Plugin '{}' is already installed at {}",
                        repo_name,
                        target.display()
                    );
                    return Ok(());
                }

                std::fs::create_dir_all(&plugins_dir).map_err(|e| {
                    hermes_core::AgentError::Io(format!("Failed to create plugins dir: {}", e))
                })?;

                println!("  Cloning {} ...", plugin_name);
                let output = tokio::process::Command::new("git")
                    .args([
                        "clone",
                        "--depth",
                        "1",
                        &plugin_name,
                        &target.to_string_lossy(),
                    ])
                    .output()
                    .await
                    .map_err(|e| hermes_core::AgentError::Io(format!("git clone failed: {}", e)))?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    println!("  ✗ git clone failed: {}", stderr.trim());
                    return Ok(());
                }

                if let Some(gr) = git_ref.as_deref() {
                    println!("  Checking out ref: {} ...", gr);
                    if let Err(e) = git_checkout_ref(&target, gr).await {
                        println!("  ✗ {}", e);
                        let _ = std::fs::remove_dir_all(&target);
                        return Ok(());
                    }
                }

                // Verify plugin.yaml exists
                let manifest_path = target.join("plugin.yaml");
                if !manifest_path.exists() {
                    println!("  ✗ No plugin.yaml found in cloned repository.");
                    println!("    Removing {}...", target.display());
                    let _ = std::fs::remove_dir_all(&target);
                    return Ok(());
                }

                // Parse and display plugin info
                let manifest_content = std::fs::read_to_string(&manifest_path)
                    .map_err(|e| hermes_core::AgentError::Io(format!("Read error: {}", e)))?;
                let manifest: serde_json::Value =
                    serde_yaml::from_str(&manifest_content).unwrap_or(serde_json::json!({}));

                let p_name = manifest
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&repo_name);
                let p_version = manifest
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let p_desc = manifest
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                // Security scan of cloned files
                let suspicious = scan_plugin_security(&target);
                let hard_block = suspicious.iter().any(|s| {
                    s.contains("curl piped to shell")
                        || s.contains("wget piped to shell")
                        || s.contains("curl|sh style install")
                });
                if hard_block && !allow_untrusted_git_host {
                    println!("\n  ✗ High-risk install patterns detected — clone removed.");
                    for warning in &suspicious {
                        println!("    - {}", warning);
                    }
                    println!(
                        "\n  If you reviewed the code manually, re-run with --allow-untrusted-git-host."
                    );
                    let _ = std::fs::remove_dir_all(&target);
                    return Ok(());
                }
                if !suspicious.is_empty() {
                    println!("\n  ⚠ Security warnings found ({}):", suspicious.len());
                    for warning in &suspicious {
                        println!("    - {}", warning);
                    }
                    println!("\n  Review the warnings above before enabling this plugin.");
                }

                println!("  ✓ Plugin installed successfully!");
                println!("    Name:        {}", p_name);
                println!("    Version:     {}", p_version);
                println!("    Description: {}", p_desc);
                println!("    Path:        {}", target.display());
            } else if plugin_name.starts_with("gh:") || plugin_name.contains('/') {
                // Convert gh:user/repo or user/repo to a GitHub HTTPS URL
                let repo_path = plugin_name.trim_start_matches("gh:");
                let git_url = format!("https://github.com/{}.git", repo_path);
                let repo_name = repo_path.rsplit('/').next().unwrap_or("unknown-plugin");
                let target = plugins_dir.join(repo_name);
                if target.exists() {
                    println!("Plugin '{}' is already installed.", repo_name);
                    return Ok(());
                }

                std::fs::create_dir_all(&plugins_dir).map_err(|e| {
                    hermes_core::AgentError::Io(format!("Failed to create plugins dir: {}", e))
                })?;

                println!("  Cloning from GitHub: {}", git_url);
                let output = tokio::process::Command::new("git")
                    .args(["clone", "--depth", "1", &git_url, &target.to_string_lossy()])
                    .output()
                    .await
                    .map_err(|e| hermes_core::AgentError::Io(format!("git clone failed: {}", e)))?;

                if !output.status.success() {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    println!("  ✗ git clone failed: {}", stderr.trim());
                    return Ok(());
                }

                if let Some(gr) = git_ref.as_deref() {
                    println!("  Checking out ref: {} ...", gr);
                    if let Err(e) = git_checkout_ref(&target, gr).await {
                        println!("  ✗ {}", e);
                        let _ = std::fs::remove_dir_all(&target);
                        return Ok(());
                    }
                }

                let manifest_path = target.join("plugin.yaml");
                if !manifest_path.exists() {
                    println!("  ✗ No plugin.yaml found in cloned repository.");
                    let _ = std::fs::remove_dir_all(&target);
                    return Ok(());
                }

                let manifest_content = std::fs::read_to_string(&manifest_path).unwrap_or_default();
                let manifest: serde_json::Value =
                    serde_yaml::from_str(&manifest_content).unwrap_or(serde_json::json!({}));

                let p_name = manifest
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or(repo_name);
                let p_version = manifest
                    .get("version")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                let p_desc = manifest
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");

                let suspicious = scan_plugin_security(&target);
                let hard_block = suspicious.iter().any(|s| {
                    s.contains("curl piped to shell")
                        || s.contains("wget piped to shell")
                        || s.contains("curl|sh style install")
                });
                if hard_block && !allow_untrusted_git_host {
                    println!("\n  ✗ High-risk install patterns detected — clone removed.");
                    for warning in &suspicious {
                        println!("    - {}", warning);
                    }
                    println!(
                        "\n  If you reviewed the code manually, re-run with --allow-untrusted-git-host."
                    );
                    let _ = std::fs::remove_dir_all(&target);
                    return Ok(());
                }
                if !suspicious.is_empty() {
                    println!("\n  ⚠ Security warnings found ({}):", suspicious.len());
                    for warning in &suspicious {
                        println!("    - {}", warning);
                    }
                }

                println!("  ✓ Plugin installed successfully!");
                println!("    Name:        {}", p_name);
                println!("    Version:     {}", p_version);
                println!("    Description: {}", p_desc);
                println!("    Path:        {}", target.display());
            } else {
                let target = plugins_dir.join(&plugin_name);
                if target.exists() {
                    println!("Plugin '{}' is already installed.", plugin_name);
                    return Ok(());
                }
                // Registry lookup
                println!("  Looking up '{}' in plugin registry...", plugin_name);
                match reqwest::Client::new()
                    .get(&format!(
                        "https://plugins.hermes.run/api/v1/{}",
                        plugin_name
                    ))
                    .timeout(std::time::Duration::from_secs(10))
                    .send()
                    .await
                {
                    Ok(resp) if resp.status().is_success() => {
                        if let Ok(data) = resp.json::<serde_json::Value>().await {
                            let version = data
                                .get("version")
                                .and_then(|v| v.as_str())
                                .unwrap_or("latest");
                            let git_url = data.get("git_url").and_then(|v| v.as_str());
                            println!("  Found {} v{}", plugin_name, version);

                            if let Some(url) = git_url {
                                if !plugin_git_host_allowed(url, allow_untrusted_git_host) {
                                    println!("  ✗ Registry git_url host is not allow-listed. Use --allow-untrusted-git-host or HERMES_PLUGIN_GIT_EXTRA_HOSTS.");
                                    return Ok(());
                                }
                                std::fs::create_dir_all(&plugins_dir)
                                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;

                                let output = tokio::process::Command::new("git")
                                    .args(["clone", "--depth", "1", url, &target.to_string_lossy()])
                                    .output()
                                    .await
                                    .map_err(|e| {
                                        hermes_core::AgentError::Io(format!(
                                            "git clone failed: {}",
                                            e
                                        ))
                                    })?;

                                if output.status.success() {
                                    if let Some(gr) = git_ref.as_deref() {
                                        println!("  Checking out ref: {} ...", gr);
                                        if let Err(e) = git_checkout_ref(&target, gr).await {
                                            println!("  ✗ {}", e);
                                            let _ = std::fs::remove_dir_all(&target);
                                            return Ok(());
                                        }
                                    }
                                    let suspicious = scan_plugin_security(&target);
                                    let hard_block = suspicious.iter().any(|s| {
                                        s.contains("curl piped to shell")
                                            || s.contains("wget piped to shell")
                                            || s.contains("curl|sh style install")
                                    });
                                    if hard_block && !allow_untrusted_git_host {
                                        println!("  ✗ High-risk patterns — removed clone.");
                                        let _ = std::fs::remove_dir_all(&target);
                                        return Ok(());
                                    }
                                    if !suspicious.is_empty() {
                                        println!("  ⚠ Security warnings: {}", suspicious.len());
                                        for w in &suspicious {
                                            println!("    - {}", w);
                                        }
                                    }
                                    println!(
                                        "  ✓ Plugin '{}' v{} installed.",
                                        plugin_name, version
                                    );
                                } else {
                                    let stderr = String::from_utf8_lossy(&output.stderr);
                                    println!("  ✗ Clone failed: {}", stderr.trim());
                                }
                            } else {
                                println!("  No git_url in registry response. Cannot install.");
                            }
                        }
                    }
                    _ => {
                        println!("  Plugin '{}' not found in registry.", plugin_name);
                        println!("  Try installing from a URL or GitHub repo instead:");
                        println!("    hermes plugins install https://github.com/user/repo");
                        println!("    hermes plugins install gh:user/repo");
                    }
                }
            }
        }
        "remove" | "uninstall" => {
            let plugin_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing plugin name. Usage: hermes plugins remove <name>".into(),
                )
            })?;
            let target = plugins_dir.join(&plugin_name);
            if target.exists() {
                std::fs::remove_dir_all(&target).map_err(|e| {
                    hermes_core::AgentError::Io(format!("Failed to remove plugin: {}", e))
                })?;
                println!("Plugin '{}' removed.", plugin_name);
            } else {
                println!("Plugin '{}' not found.", plugin_name);
            }
        }
        "update" => {
            let plugin_name = name.as_deref();
            let mut checked = 0u32;
            let mut updated = 0u32;
            if !plugins_dir.exists() {
                println!("No plugins installed.");
                return Ok(());
            }
            if let Ok(entries) = std::fs::read_dir(&plugins_dir) {
                for entry in entries.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if !path.is_dir() {
                        continue;
                    }
                    let dir_name = path
                        .file_name()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    if let Some(target) = plugin_name {
                        if dir_name != target {
                            continue;
                        }
                    }
                    let manifest = path.join("plugin.yaml");
                    if manifest.exists() {
                        checked += 1;
                        println!("  Checking updates for '{}'...", dir_name);

                        let git_dir = path.join(".git");
                        if !git_dir.exists() {
                            println!("    Skipped: plugin is not a git checkout.");
                            continue;
                        }

                        let path_s = path.to_string_lossy().to_string();
                        let before = tokio::process::Command::new("git")
                            .args(["-C", &path_s, "rev-parse", "HEAD"])
                            .output()
                            .await
                            .map_err(|e| {
                                hermes_core::AgentError::Io(format!(
                                    "git rev-parse failed for {}: {}",
                                    dir_name, e
                                ))
                            })?;
                        if !before.status.success() {
                            let stderr = String::from_utf8_lossy(&before.stderr);
                            println!(
                                "    Skipped: cannot read current revision ({})",
                                stderr.trim()
                            );
                            continue;
                        }
                        let before_sha = String::from_utf8_lossy(&before.stdout).trim().to_string();

                        let pull = tokio::process::Command::new("git")
                            .args(["-C", &path_s, "pull", "--ff-only"])
                            .output()
                            .await
                            .map_err(|e| {
                                hermes_core::AgentError::Io(format!(
                                    "git pull failed for {}: {}",
                                    dir_name, e
                                ))
                            })?;

                        if !pull.status.success() {
                            let stderr = String::from_utf8_lossy(&pull.stderr);
                            println!("    Update failed: {}", stderr.trim());
                            continue;
                        }

                        let after = tokio::process::Command::new("git")
                            .args(["-C", &path_s, "rev-parse", "HEAD"])
                            .output()
                            .await
                            .map_err(|e| {
                                hermes_core::AgentError::Io(format!(
                                    "git rev-parse failed for {} after update: {}",
                                    dir_name, e
                                ))
                            })?;
                        if !after.status.success() {
                            let stderr = String::from_utf8_lossy(&after.stderr);
                            println!(
                                "    Updated but could not read final revision ({})",
                                stderr.trim()
                            );
                            continue;
                        }
                        let after_sha = String::from_utf8_lossy(&after.stdout).trim().to_string();

                        if before_sha == after_sha {
                            println!("    Up to date ({})", short_sha(&after_sha));
                        } else {
                            updated += 1;
                            println!(
                                "    Updated: {} -> {}",
                                short_sha(&before_sha),
                                short_sha(&after_sha)
                            );
                        }
                    }
                }
            }
            if checked == 0 {
                if let Some(n) = plugin_name {
                    println!("Plugin '{}' not found.", n);
                } else {
                    println!("No plugins to update.");
                }
            } else {
                println!("Checked {} plugin(s); updated {}.", checked, updated);
            }
        }
        "inspect" | "info" => {
            let plugin_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing plugin name. Usage: hermes plugins inspect <name>".into(),
                )
            })?;
            let target = plugins_dir.join(&plugin_name);
            if !target.exists() {
                println!("Plugin '{}' not found.", plugin_name);
                return Ok(());
            }
            let manifest_path = target.join("plugin.yaml");
            if manifest_path.exists() {
                let content = std::fs::read_to_string(&manifest_path)
                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                println!("Plugin: {}", plugin_name);
                println!("Path:   {}", target.display());
                let disabled = target.join(".disabled").exists();
                println!("Status: {}", if disabled { "disabled" } else { "enabled" });
                println!("\n--- plugin.yaml ---");
                println!("{}", content);
            } else {
                println!("Plugin '{}' has no plugin.yaml manifest.", plugin_name);
            }
        }
        other => {
            println!("Plugins action '{}' is not recognized.", other);
            println!("Available: list, install, remove, enable, disable, update, inspect");
        }
    }
    Ok(())
}

/// Handle `hermes memory [action]`.
pub async fn handle_cli_memory(
    action: Option<String>,
    target: Option<String>,
    yes: bool,
) -> Result<(), hermes_core::AgentError> {
    let hermes_home = hermes_config::hermes_home();
    let memories_dir = hermes_home.join("memories");
    let memory_md = memories_dir.join("MEMORY.md");
    let user_md = memories_dir.join("USER.md");
    let legacy_memory_db = hermes_home.join("memory.db");
    let disabled_marker = hermes_home.join(".memory_disabled");

    match action.as_deref().unwrap_or("status") {
        "status" => {
            if disabled_marker.exists() {
                println!("Memory provider: disabled");
                println!("  Marker: {}", disabled_marker.display());
                println!("Run `hermes memory setup` to re-enable.");
                return Ok(());
            }

            if memory_md.exists() || user_md.exists() {
                let mem_size = std::fs::metadata(&memory_md).map(|m| m.len()).unwrap_or(0);
                let user_size = std::fs::metadata(&user_md).map(|m| m.len()).unwrap_or(0);
                println!("Memory provider: files (MEMORY.md + USER.md)");
                println!("  Directory: {}", memories_dir.display());
                println!(
                    "  MEMORY.md: {} ({:.1} KB)",
                    memory_md.display(),
                    mem_size as f64 / 1024.0
                );
                println!(
                    "  USER.md:   {} ({:.1} KB)",
                    user_md.display(),
                    user_size as f64 / 1024.0
                );
                if legacy_memory_db.exists() {
                    println!(
                        "  Legacy file detected (unused by current memory backend): {}",
                        legacy_memory_db.display()
                    );
                }
            } else if legacy_memory_db.exists() {
                let size = std::fs::metadata(&legacy_memory_db)
                    .map(|m| m.len())
                    .unwrap_or(0);
                println!("Memory provider: legacy sqlite artifact only");
                println!("  File: {}", legacy_memory_db.display());
                println!("  Size: {} KB", size / 1024);
                println!("Run `hermes memory setup` to initialize the current file backend.");
            } else {
                println!("Memory provider: not configured");
                println!("Run `hermes memory setup` to initialize.");
            }
        }
        "setup" => {
            println!("Memory Provider Setup");
            println!("---------------------");
            std::fs::create_dir_all(&memories_dir)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            if !memory_md.exists() {
                std::fs::write(
                    &memory_md,
                    "# Hermes MEMORY\n\nStore durable assistant memory entries here.\n",
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            }
            if !user_md.exists() {
                std::fs::write(
                    &user_md,
                    "# Hermes USER\n\nStore durable user profile entries here.\n",
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            }
            if disabled_marker.exists() {
                let _ = std::fs::remove_file(&disabled_marker);
            }
            println!("Initialized file memory backend.");
            println!("  MEMORY.md: {}", memory_md.display());
            println!("  USER.md:   {}", user_md.display());
            println!("Memory is enabled for subsequent sessions.");
        }
        "off" => {
            std::fs::create_dir_all(&hermes_home)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            std::fs::write(
                &disabled_marker,
                format!(
                    "disabled_at={}\nreason=hermes memory off\n",
                    chrono::Utc::now().to_rfc3339()
                ),
            )
            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            println!("Memory provider disabled.");
            println!("  Marker: {}", disabled_marker.display());
            println!("Run `hermes memory setup` to re-enable.");
        }
        "reset" => {
            if !yes {
                return Err(hermes_core::AgentError::Config(
                    "memory reset requires confirmation flag: use `hermes memory reset [all|memory|user] -y`"
                        .into(),
                ));
            }
            let reset_target = target
                .as_deref()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .unwrap_or("all")
                .to_ascii_lowercase();
            let reset_memory = reset_target == "all" || reset_target == "memory";
            let reset_user = reset_target == "all" || reset_target == "user";
            if !reset_memory && !reset_user {
                return Err(hermes_core::AgentError::Config(format!(
                    "Unknown memory reset target '{}'. Use all|memory|user",
                    reset_target
                )));
            }
            std::fs::create_dir_all(&memories_dir)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            if reset_memory && memory_md.exists() {
                let _ = std::fs::remove_file(&memory_md);
            }
            if reset_user && user_md.exists() {
                let _ = std::fs::remove_file(&user_md);
            }
            if reset_target == "all" && legacy_memory_db.exists() {
                let _ = std::fs::remove_file(&legacy_memory_db);
            }
            if disabled_marker.exists() {
                let _ = std::fs::remove_file(&disabled_marker);
            }
            if reset_memory {
                std::fs::write(
                    &memory_md,
                    "# Hermes MEMORY\n\nStore durable assistant memory entries here.\n",
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            }
            if reset_user {
                std::fs::write(
                    &user_md,
                    "# Hermes USER\n\nStore durable user profile entries here.\n",
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            }
            println!(
                "Memory reset complete (target={}). MEMORY.md={} USER.md={}",
                reset_target,
                if memory_md.exists() {
                    "present"
                } else {
                    "absent"
                },
                if user_md.exists() {
                    "present"
                } else {
                    "absent"
                }
            );
        }
        other => {
            println!("Unknown memory action '{}'.", other);
            println!("Available actions: status, setup, off, reset");
        }
    }
    Ok(())
}

/// Handle `hermes mcp [action] [--server ...]`.
pub async fn handle_cli_mcp(
    action: Option<String>,
    name: Option<String>,
    server: Option<String>,
    url: Option<String>,
    command: Option<String>,
) -> Result<(), hermes_core::AgentError> {
    let config_dir = hermes_config::hermes_home();
    let mcp_config_path = config_dir.join("mcp_servers.json");
    let mcp_auth_path = config_dir.join("mcp_auth.json");
    let selected = name.clone().or(server.clone());

    match action.as_deref().unwrap_or("list") {
        "list" => {
            if !mcp_config_path.exists() {
                println!("No MCP servers configured ({})", mcp_config_path.display());
                println!("Add one with `hermes mcp add --server <name-or-url>`.");
                return Ok(());
            }
            let content = std::fs::read_to_string(&mcp_config_path)
                .map_err(|e| hermes_core::AgentError::Io(format!("Read error: {}", e)))?;
            let servers: serde_json::Value =
                serde_json::from_str(&content).unwrap_or(serde_json::json!({}));
            if let Some(obj) = servers.as_object() {
                if obj.is_empty() {
                    println!("No MCP servers configured.");
                } else {
                    println!("MCP servers ({}):", mcp_config_path.display());
                    for (name, cfg) in obj {
                        let url = cfg.get("url").and_then(|v| v.as_str()).unwrap_or("(stdio)");
                        println!("  • {} — {}", name, url);
                    }
                }
            }
        }
        "add" => {
            let (entry_name, entry) = if let Some(name) =
                name.as_deref().map(str::trim).filter(|s| !s.is_empty())
            {
                let entry = if let Some(url) = url.clone().filter(|v| !v.trim().is_empty()) {
                    serde_json::json!({"url": url, "enabled": true})
                } else if let Some(command) = command.clone().filter(|v| !v.trim().is_empty()) {
                    serde_json::json!({"command": command, "enabled": true})
                } else {
                    return Err(hermes_core::AgentError::Config(
                        "mcp add with positional name requires --url or --command".into(),
                    ));
                };
                (name.to_string(), entry)
            } else {
                let srv = server
                    .as_deref()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .ok_or_else(|| {
                        hermes_core::AgentError::Config(
                            "Missing server. Usage: hermes mcp add <name> --url <url> | --command <cmd> (legacy: --server <name-or-url>)".into(),
                        )
                    })?;
                (
                    srv.to_string(),
                    if srv.starts_with("http://") || srv.starts_with("https://") {
                        serde_json::json!({"url": srv, "enabled": true})
                    } else {
                        serde_json::json!({"url": srv, "enabled": true})
                    },
                )
            };
            println!("Adding MCP server: {}", entry_name);
            let mut servers: serde_json::Value = if mcp_config_path.exists() {
                let content = std::fs::read_to_string(&mcp_config_path)
                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                serde_json::from_str(&content).unwrap_or(serde_json::json!({}))
            } else {
                serde_json::json!({})
            };
            if let Some(obj) = servers.as_object_mut() {
                obj.insert(entry_name.clone(), entry);
            }
            let json = serde_json::to_string_pretty(&servers)
                .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
            std::fs::write(&mcp_config_path, json)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            println!(
                "MCP server '{}' added to {}",
                entry_name,
                mcp_config_path.display()
            );
        }
        "remove" => {
            let srv = selected.clone().ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing server name. Usage: hermes mcp remove <name>".into(),
                )
            })?;
            if !mcp_config_path.exists() {
                println!("No MCP config to modify.");
                return Ok(());
            }
            let content = std::fs::read_to_string(&mcp_config_path)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            let mut servers: serde_json::Value =
                serde_json::from_str(&content).unwrap_or(serde_json::json!({}));
            if let Some(obj) = servers.as_object_mut() {
                if obj.remove(&srv).is_some() {
                    let json = serde_json::to_string_pretty(&servers)
                        .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
                    std::fs::write(&mcp_config_path, json)
                        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                    println!("MCP server '{}' removed.", srv);
                    if mcp_auth_path.exists() {
                        let raw = std::fs::read_to_string(&mcp_auth_path).unwrap_or_default();
                        let mut auth: serde_json::Value =
                            serde_json::from_str(&raw).unwrap_or(serde_json::json!({}));
                        if let Some(auth_obj) = auth.as_object_mut() {
                            auth_obj.remove(&srv);
                            let out = serde_json::to_string_pretty(&auth).unwrap_or_default();
                            let _ = std::fs::write(&mcp_auth_path, out);
                        }
                    }
                } else {
                    println!("MCP server '{}' not found.", srv);
                }
            }
        }
        "serve" => {
            use hermes_skills::{FileSkillStore, SkillManager};
            use hermes_tools::ToolRegistry;

            eprintln!("Starting Hermes as MCP server on stdio...");

            let config = hermes_config::load_config(None)
                .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
            let tool_registry = Arc::new(ToolRegistry::new());
            let terminal_backend = crate::terminal_backend::build_terminal_backend(&config);
            let skill_store = Arc::new(FileSkillStore::new(FileSkillStore::default_dir()));
            let skill_provider: Arc<dyn hermes_core::SkillProvider> =
                Arc::new(SkillManager::new(skill_store));
            hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);

            let mcp_server = hermes_mcp::McpServer::new(tool_registry);
            let transport = Box::new(hermes_mcp::ServerStdioTransport::new());
            mcp_server
                .start(transport)
                .await
                .map_err(|e| hermes_core::AgentError::Io(format!("MCP server error: {}", e)))?;
        }
        "test" => {
            let srv = selected.clone().ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing server name. Usage: hermes mcp test <name>".into(),
                )
            })?;
            println!("Testing MCP server: {}...", srv);
            if !mcp_config_path.exists() {
                println!("No MCP config found.");
                return Ok(());
            }
            let content = std::fs::read_to_string(&mcp_config_path)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            let servers: serde_json::Value =
                serde_json::from_str(&content).unwrap_or(serde_json::json!({}));
            match servers.get(&srv) {
                Some(cfg) => {
                    let url = cfg.get("url").and_then(|v| v.as_str()).unwrap_or("(stdio)");
                    let enabled = cfg.get("enabled").and_then(|v| v.as_bool()).unwrap_or(true);
                    println!("  Server: {}", srv);
                    println!("  URL: {}", url);
                    println!("  Enabled: {}", enabled);
                    if url.starts_with("http") {
                        match reqwest::Client::new()
                            .get(url)
                            .timeout(std::time::Duration::from_secs(5))
                            .send()
                            .await
                        {
                            Ok(resp) => println!("  Status: {} (reachable)", resp.status()),
                            Err(e) => println!("  Status: unreachable ({})", e),
                        }
                    } else {
                        println!("  Status: stdio transport (not testable via HTTP)");
                    }
                }
                None => println!("Server '{}' not found in MCP config.", srv),
            }
        }
        "configure" => {
            let srv = selected.clone().ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing server name. Usage: hermes mcp configure <name>".into(),
                )
            })?;
            if !mcp_config_path.exists() {
                println!("No MCP config found. Add a server first with `hermes mcp add`.");
                return Ok(());
            }
            let content = std::fs::read_to_string(&mcp_config_path)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            let servers: serde_json::Value =
                serde_json::from_str(&content).unwrap_or(serde_json::json!({}));
            match servers.get(&srv) {
                Some(cfg) => {
                    println!("Current config for '{}':", srv);
                    println!("{}", serde_json::to_string_pretty(cfg).unwrap_or_default());
                    println!("\nEdit {} to modify settings.", mcp_config_path.display());
                }
                None => println!("Server '{}' not found.", srv),
            }
        }
        "login" => {
            let srv = selected.clone().ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing server name. Usage: hermes mcp login <name>".into(),
                )
            })?;
            if !mcp_config_path.exists() {
                return Err(hermes_core::AgentError::Config(format!(
                    "No MCP config found at {}",
                    mcp_config_path.display()
                )));
            }
            let configured = std::fs::read_to_string(&mcp_config_path)
                .ok()
                .and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
                .and_then(|v| v.get(&srv).cloned())
                .is_some();
            if !configured {
                return Err(hermes_core::AgentError::Config(format!(
                    "MCP server '{}' is not configured",
                    srv
                )));
            }

            let env_key = format!("MCP_{}_TOKEN", srv.to_uppercase().replace('-', "_"));
            let token_from_env = std::env::var(&env_key)
                .ok()
                .map(|v| v.trim().to_string())
                .filter(|v| !v.is_empty());
            let token = if let Some(v) = token_from_env {
                v
            } else {
                use std::io::{self, Write};
                print!("Token for '{}': ", srv);
                let _ = io::stdout().flush();
                let mut buf = String::new();
                io::stdin()
                    .read_line(&mut buf)
                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                buf.trim().to_string()
            };
            if token.is_empty() {
                return Err(hermes_core::AgentError::Config(
                    "Empty token; aborting mcp login".into(),
                ));
            }
            let mut auth: serde_json::Value = if mcp_auth_path.exists() {
                let raw = std::fs::read_to_string(&mcp_auth_path)
                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                serde_json::from_str(&raw).unwrap_or(serde_json::json!({}))
            } else {
                serde_json::json!({})
            };
            if let Some(obj) = auth.as_object_mut() {
                obj.insert(
                    srv.clone(),
                    serde_json::json!({
                        "token": token,
                        "updated_at": chrono::Utc::now().to_rfc3339(),
                    }),
                );
            }
            std::fs::write(
                &mcp_auth_path,
                serde_json::to_string_pretty(&auth)
                    .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?,
            )
            .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            println!(
                "Stored MCP auth token for '{}' in {}",
                srv,
                mcp_auth_path.display()
            );
        }
        other => {
            println!("MCP action '{}' is not recognized.", other);
            println!("Available actions: list, add, remove, serve, test, configure, login");
        }
    }
    Ok(())
}

/// Handle `hermes sessions [action] [--id ...] [--name ...]`.
pub async fn handle_cli_sessions(
    action: Option<String>,
    id: Option<String>,
    name: Option<String>,
) -> Result<(), hermes_core::AgentError> {
    let sessions_dir = hermes_config::hermes_home().join("sessions");

    match action.as_deref().unwrap_or("list") {
        "list" => {
            if !sessions_dir.exists() {
                println!("No sessions directory found.");
                return Ok(());
            }
            let mut entries: Vec<(String, u64)> = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&sessions_dir) {
                for entry in rd.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if path.extension().map(|e| e == "json").unwrap_or(false) {
                        let stem = path
                            .file_stem()
                            .unwrap_or_default()
                            .to_string_lossy()
                            .into_owned();
                        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
                        entries.push((stem, size));
                    }
                }
            }
            if entries.is_empty() {
                println!("No saved sessions.");
            } else {
                println!("Saved sessions ({}):", entries.len());
                for (name, size) in &entries {
                    println!("  • {} ({} bytes)", name, size);
                }
            }
        }
        "export" => {
            let session_id = id.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing session ID. Usage: hermes sessions export --id <id>".into(),
                )
            })?;
            let path = sessions_dir.join(format!("{}.json", session_id));
            if !path.exists() {
                println!("Session '{}' not found.", session_id);
                return Ok(());
            }
            let content = std::fs::read_to_string(&path)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            println!("{}", content);
        }
        "delete" => {
            let session_id = id.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing session ID. Usage: hermes sessions delete --id <id>".into(),
                )
            })?;
            let path = sessions_dir.join(format!("{}.json", session_id));
            if path.exists() {
                std::fs::remove_file(&path)
                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                println!("Session '{}' deleted.", session_id);
            } else {
                println!("Session '{}' not found.", session_id);
            }
        }
        "stats" => {
            if !sessions_dir.exists() {
                println!("No sessions directory.");
                return Ok(());
            }
            let mut total_files = 0u32;
            let mut total_size = 0u64;
            if let Ok(rd) = std::fs::read_dir(&sessions_dir) {
                for entry in rd.filter_map(|e| e.ok()) {
                    if entry
                        .path()
                        .extension()
                        .map(|e| e == "json")
                        .unwrap_or(false)
                    {
                        total_files += 1;
                        total_size += std::fs::metadata(entry.path())
                            .map(|m| m.len())
                            .unwrap_or(0);
                    }
                }
            }
            println!("Session statistics:");
            println!("  Total sessions: {}", total_files);
            println!("  Total size:     {} KB", total_size / 1024);
            println!("  Directory:      {}", sessions_dir.display());
        }
        "prune" => {
            let max_age_days: u64 = name
                .as_deref()
                .and_then(|s| s.parse::<u64>().ok())
                .unwrap_or(30);
            println!("Pruning sessions older than {} days...", max_age_days);
            if !sessions_dir.exists() {
                println!("No sessions directory.");
                return Ok(());
            }
            let cutoff = std::time::SystemTime::now()
                .checked_sub(std::time::Duration::from_secs(max_age_days * 86400))
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            let mut pruned = 0u32;
            if let Ok(rd) = std::fs::read_dir(&sessions_dir) {
                for entry in rd.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if !path.extension().map(|e| e == "json").unwrap_or(false) {
                        continue;
                    }
                    if let Ok(meta) = std::fs::metadata(&path) {
                        let modified = meta.modified().unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                        if modified < cutoff {
                            if std::fs::remove_file(&path).is_ok() {
                                let name = path.file_stem().unwrap_or_default().to_string_lossy();
                                println!("  Pruned: {}", name);
                                pruned += 1;
                            }
                        }
                    }
                }
            }
            println!("Pruned {} session(s).", pruned);
        }
        "rename" => {
            let session_id = id.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing session ID. Usage: hermes sessions rename --id <id> --name <new>"
                        .into(),
                )
            })?;
            let new_name = name.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing new name. Usage: hermes sessions rename --id <id> --name <new>".into(),
                )
            })?;
            let old_path = sessions_dir.join(format!("{}.json", session_id));
            let new_path = sessions_dir.join(format!("{}.json", new_name));
            if !old_path.exists() {
                println!("Session '{}' not found.", session_id);
                return Ok(());
            }
            std::fs::rename(&old_path, &new_path)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            println!("Session renamed: {} -> {}", session_id, new_name);
        }
        "browse" => {
            if !sessions_dir.exists() {
                println!("No sessions directory found.");
                return Ok(());
            }
            println!("Session Browser");
            println!("===============\n");
            let mut entries: Vec<(String, u64, std::time::SystemTime, usize)> = Vec::new();
            if let Ok(rd) = std::fs::read_dir(&sessions_dir) {
                for entry in rd.filter_map(|e| e.ok()) {
                    let path = entry.path();
                    if !path.extension().map(|e| e == "json").unwrap_or(false) {
                        continue;
                    }
                    let stem = path
                        .file_stem()
                        .unwrap_or_default()
                        .to_string_lossy()
                        .into_owned();
                    let meta = std::fs::metadata(&path);
                    let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
                    let modified = meta
                        .as_ref()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    let msg_count = std::fs::read_to_string(&path)
                        .ok()
                        .and_then(|c| serde_json::from_str::<serde_json::Value>(&c).ok())
                        .and_then(|v| {
                            v.get("messages")
                                .and_then(|m| m.as_array())
                                .map(|a| a.len())
                        })
                        .unwrap_or(0);
                    entries.push((stem, size, modified, msg_count));
                }
            }
            entries.sort_by(|a, b| b.2.cmp(&a.2));
            if entries.is_empty() {
                println!("No sessions found.");
            } else {
                println!(
                    "{:3} {:30} {:>8} {:>6}  {}",
                    "#", "Session ID", "Size", "Msgs", "Modified"
                );
                println!("{}", "-".repeat(75));
                for (idx, (name, size, modified, msgs)) in entries.iter().enumerate() {
                    let age = modified.elapsed().unwrap_or_default();
                    let age_str = if age.as_secs() < 3600 {
                        format!("{}m ago", age.as_secs() / 60)
                    } else if age.as_secs() < 86400 {
                        format!("{}h ago", age.as_secs() / 3600)
                    } else {
                        format!("{}d ago", age.as_secs() / 86400)
                    };
                    println!(
                        "{:3} {:30} {:>6}KB {:>6}  {}",
                        idx + 1,
                        &name[..name.len().min(30)],
                        size / 1024,
                        msgs,
                        age_str,
                    );
                }
                println!("\nUse `hermes sessions export --id <id>` to view a session.");
            }
        }
        other => {
            println!("Sessions action '{}' is not recognized.", other);
            println!("Available actions: list, export, delete, prune, stats, rename, browse");
        }
    }
    Ok(())
}

/// Handle `hermes insights [--days N] [--source ...]`.
pub async fn handle_cli_insights(
    days: u32,
    source: Option<String>,
) -> Result<(), hermes_core::AgentError> {
    println!("Usage Insights (last {} days)", days);
    println!("=============================");
    if let Some(src) = &source {
        println!("Filter: source={}\n", src);
    }
    let sessions_dir = hermes_config::hermes_home().join("sessions");
    if !sessions_dir.exists() {
        println!("No sessions directory found.");
        return Ok(());
    }

    let cutoff = std::time::SystemTime::now()
        .checked_sub(std::time::Duration::from_secs(u64::from(days) * 86400))
        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);

    let mut total_sessions = 0u32;
    let mut total_messages = 0u64;
    let mut total_input_tokens = 0u64;
    let mut total_output_tokens = 0u64;
    let mut total_cost_cents = 0.0f64;
    let mut models_used: std::collections::HashMap<String, u32> = std::collections::HashMap::new();
    let mut daily_counts: std::collections::BTreeMap<String, u32> =
        std::collections::BTreeMap::new();

    if let Ok(rd) = std::fs::read_dir(&sessions_dir) {
        for entry in rd.filter_map(|e| e.ok()) {
            let path = entry.path();
            if !path.extension().map(|e| e == "json").unwrap_or(false) {
                continue;
            }
            let meta = std::fs::metadata(&path);
            let modified = meta
                .as_ref()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            if modified < cutoff {
                continue;
            }

            if let Ok(content) = std::fs::read_to_string(&path) {
                if let Ok(data) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(src_filter) = &source {
                        let session_source = data
                            .get("source")
                            .and_then(|s| s.as_str())
                            .unwrap_or("unknown");
                        if session_source != src_filter.as_str() {
                            continue;
                        }
                    }

                    total_sessions += 1;

                    if let Some(msgs) = data.get("messages").and_then(|m| m.as_array()) {
                        total_messages += msgs.len() as u64;
                    }

                    if let Some(usage) = data.get("usage") {
                        total_input_tokens += usage
                            .get("input_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        total_output_tokens += usage
                            .get("output_tokens")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(0);
                        total_cost_cents +=
                            usage.get("cost").and_then(|v| v.as_f64()).unwrap_or(0.0);
                    }

                    if let Some(model) = data.get("model").and_then(|m| m.as_str()) {
                        *models_used.entry(model.to_string()).or_insert(0) += 1;
                    }

                    let dur = modified
                        .duration_since(std::time::SystemTime::UNIX_EPOCH)
                        .unwrap_or_default();
                    let secs = dur.as_secs();
                    let day_secs = secs - (secs % 86400);
                    let day_key = format!("{}", day_secs / 86400);
                    *daily_counts.entry(day_key).or_insert(0) += 1;
                }
            }
        }
    }

    println!("Sessions:       {}", total_sessions);
    println!("Messages:       {}", total_messages);
    println!("Input tokens:   {}", total_input_tokens);
    println!("Output tokens:  {}", total_output_tokens);
    let total_tokens = total_input_tokens + total_output_tokens;
    println!("Total tokens:   {}", total_tokens);
    if total_cost_cents > 0.0 {
        println!("Estimated cost: ${:.4}", total_cost_cents / 100.0);
    }

    if !models_used.is_empty() {
        println!("\nModels Used:");
        let mut model_vec: Vec<_> = models_used.into_iter().collect();
        model_vec.sort_by(|a, b| b.1.cmp(&a.1));
        for (model, count) in &model_vec {
            println!("  {:30} {:>5} session(s)", model, count);
        }
    }

    if total_sessions > 0 {
        println!("\nAverages per session:");
        println!(
            "  Messages: {:.1}",
            total_messages as f64 / total_sessions as f64
        );
        println!(
            "  Tokens:   {:.0}",
            total_tokens as f64 / total_sessions as f64
        );
    }

    Ok(())
}

/// Handle `hermes login [provider]`.
pub async fn handle_cli_login(provider: Option<String>) -> Result<(), hermes_core::AgentError> {
    let provider = provider.unwrap_or_else(|| "openai".to_string());
    let creds_dir = hermes_config::hermes_home().join("credentials");
    std::fs::create_dir_all(&creds_dir).map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;

    println!("Login to: {}", provider);
    println!("----------{}", "-".repeat(provider.len()));

    match provider.as_str() {
        "openai" => {
            let env_key = std::env::var("HERMES_OPENAI_API_KEY")
                .ok()
                .or_else(|| std::env::var("OPENAI_API_KEY").ok());
            if let Some(key) = env_key {
                let masked = if key.len() > 8 {
                    format!("{}...{}", &key[..4], &key[key.len() - 4..])
                } else {
                    "****".to_string()
                };
                println!(
                    "Found HERMES_OPENAI_API_KEY/OPENAI_API_KEY in environment: {}",
                    masked
                );
                let cred_file = creds_dir.join("openai.json");
                let cred = serde_json::json!({
                    "provider": "openai",
                    "api_key_masked": masked,
                    "stored_at": chrono::Utc::now().to_rfc3339(),
                    "source": "env",
                });
                std::fs::write(
                    &cred_file,
                    serde_json::to_string_pretty(&cred).unwrap_or_default(),
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                println!("Credential reference stored at {}", cred_file.display());
            } else {
                println!("No HERMES_OPENAI_API_KEY/OPENAI_API_KEY found in environment.");
                println!("Set it with: export HERMES_OPENAI_API_KEY=sk-...");
                println!("Or use: hermes config set openai_api_key <key>");
            }
        }
        "anthropic" => {
            let env_key = std::env::var("ANTHROPIC_API_KEY").ok();
            if let Some(key) = env_key {
                let masked = if key.len() > 8 {
                    format!("{}...{}", &key[..4], &key[key.len() - 4..])
                } else {
                    "****".to_string()
                };
                println!("Found ANTHROPIC_API_KEY in environment: {}", masked);
                let cred_file = creds_dir.join("anthropic.json");
                let cred = serde_json::json!({
                    "provider": "anthropic",
                    "api_key_masked": masked,
                    "stored_at": chrono::Utc::now().to_rfc3339(),
                    "source": "env",
                });
                std::fs::write(
                    &cred_file,
                    serde_json::to_string_pretty(&cred).unwrap_or_default(),
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                println!("Credential reference stored at {}", cred_file.display());
            } else {
                println!("No ANTHROPIC_API_KEY found in environment.");
                println!("Set it with: export ANTHROPIC_API_KEY=sk-ant-...");
            }
        }
        other => {
            let env_var = format!("{}_API_KEY", other.to_uppercase().replace('-', "_"));
            let env_key = std::env::var(&env_var).ok();
            if let Some(key) = env_key {
                let masked = if key.len() > 8 {
                    format!("{}...{}", &key[..4], &key[key.len() - 4..])
                } else {
                    "****".to_string()
                };
                println!("Found {} in environment: {}", env_var, masked);
                let cred_file = creds_dir.join(format!("{}.json", other));
                let cred = serde_json::json!({
                    "provider": other,
                    "api_key_masked": masked,
                    "stored_at": chrono::Utc::now().to_rfc3339(),
                    "source": "env",
                });
                std::fs::write(
                    &cred_file,
                    serde_json::to_string_pretty(&cred).unwrap_or_default(),
                )
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                println!("Credential reference stored.");
            } else {
                println!("No {} found in environment.", env_var);
                println!("Set it with: export {}=<your-key>", env_var);
            }
        }
    }
    Ok(())
}

/// Handle `hermes logout [provider]`.
pub async fn handle_cli_logout(provider: Option<String>) -> Result<(), hermes_core::AgentError> {
    let creds_dir = hermes_config::hermes_home().join("credentials");

    match provider.as_deref() {
        Some(p) => {
            let cred_file = creds_dir.join(format!("{}.json", p));
            if cred_file.exists() {
                std::fs::remove_file(&cred_file)
                    .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
                println!("Logged out from '{}'. Credential reference removed.", p);
            } else {
                println!("No stored credentials for '{}'.", p);
            }
            println!(
                "Note: Environment variables (e.g. {}_API_KEY) are not affected.",
                p.to_uppercase().replace('-', "_")
            );
        }
        None => {
            if creds_dir.exists() {
                let mut removed = 0u32;
                if let Ok(rd) = std::fs::read_dir(&creds_dir) {
                    for entry in rd.filter_map(|e| e.ok()) {
                        let path = entry.path();
                        if path.extension().map(|e| e == "json").unwrap_or(false) {
                            if std::fs::remove_file(&path).is_ok() {
                                let name = path.file_stem().unwrap_or_default().to_string_lossy();
                                println!("  Removed credential: {}", name);
                                removed += 1;
                            }
                        }
                    }
                }
                if removed == 0 {
                    println!("No stored credentials to remove.");
                } else {
                    println!("Logged out from {} provider(s).", removed);
                }
            } else {
                println!("No credentials directory found.");
            }
            println!("Note: Environment variables are not affected.");
        }
    }
    Ok(())
}

/// Handle `hermes whatsapp [action]`.
pub async fn handle_cli_whatsapp(action: Option<String>) -> Result<(), hermes_core::AgentError> {
    match action.as_deref().unwrap_or("status") {
        "setup" => {
            whatsapp_setup().await?;
        }
        "status" => {
            whatsapp_status().await?;
        }
        "qr" => {
            whatsapp_qr().await?;
        }
        other => {
            println!("WhatsApp action '{}' is not recognized.", other);
            println!("Available actions: setup, status, qr");
        }
    }
    Ok(())
}

/// Interactive setup: collect credentials, persist to config.yaml, verify.
async fn whatsapp_setup() -> Result<(), hermes_core::AgentError> {
    use std::io::{self, BufRead, Write};

    println!("WhatsApp Cloud API Setup");
    println!("========================\n");
    println!("You will need credentials from the Meta developer dashboard:");
    println!("  https://developers.facebook.com/apps/\n");

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    print!("Phone Number ID: ");
    stdout.flush().ok();
    let phone_number_id = stdin
        .lock()
        .lines()
        .next()
        .and_then(|l| l.ok())
        .unwrap_or_default()
        .trim()
        .to_string();

    if phone_number_id.is_empty() {
        println!("Aborted: phone number ID is required.");
        return Ok(());
    }

    print!("Business Account ID: ");
    stdout.flush().ok();
    let business_account_id = stdin
        .lock()
        .lines()
        .next()
        .and_then(|l| l.ok())
        .unwrap_or_default()
        .trim()
        .to_string();

    if business_account_id.is_empty() {
        println!("Aborted: business account ID is required.");
        return Ok(());
    }

    print!("Access Token: ");
    stdout.flush().ok();
    let access_token = stdin
        .lock()
        .lines()
        .next()
        .and_then(|l| l.ok())
        .unwrap_or_default()
        .trim()
        .to_string();

    if access_token.is_empty() {
        println!("Aborted: access token is required.");
        return Ok(());
    }

    println!("\nVerifying token against WhatsApp Cloud API...");
    let url = format!(
        "https://graph.facebook.com/v21.0/{}/messages",
        phone_number_id
    );
    let client = reqwest::Client::new();
    match client
        .get(&url)
        .bearer_auth(&access_token)
        .timeout(std::time::Duration::from_secs(10))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            if status.is_success() || status.as_u16() == 400 {
                // 400 means the endpoint is reachable (POST required for actual messages)
                println!("  API reachable (HTTP {}).", status);
            } else if status.as_u16() == 401 || status.as_u16() == 403 {
                println!("  Warning: API returned {} — token may be invalid.", status);
                println!("  Saving anyway; you can re-run setup later.");
            } else {
                println!("  API returned HTTP {}. Saving config anyway.", status);
            }
        }
        Err(e) => {
            println!("  Could not reach API: {}", e);
            println!("  Saving config anyway — verify network connectivity.");
        }
    }

    let config_path = hermes_config::hermes_home().join("config.yaml");
    let mut config: serde_yaml::Value = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path)
            .map_err(|e| hermes_core::AgentError::Io(format!("Read error: {}", e)))?;
        serde_yaml::from_str(&content).unwrap_or(serde_yaml::Value::Mapping(Default::default()))
    } else {
        serde_yaml::Value::Mapping(Default::default())
    };

    let platforms = config
        .as_mapping_mut()
        .unwrap()
        .entry(serde_yaml::Value::String("platforms".into()))
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));

    let wa = platforms
        .as_mapping_mut()
        .unwrap()
        .entry(serde_yaml::Value::String("whatsapp".into()))
        .or_insert_with(|| serde_yaml::Value::Mapping(Default::default()));

    let wa_map = wa.as_mapping_mut().unwrap();
    wa_map.insert(
        serde_yaml::Value::String("phone_number_id".into()),
        serde_yaml::Value::String(phone_number_id.clone()),
    );
    wa_map.insert(
        serde_yaml::Value::String("business_account_id".into()),
        serde_yaml::Value::String(business_account_id),
    );
    wa_map.insert(
        serde_yaml::Value::String("access_token".into()),
        serde_yaml::Value::String(access_token),
    );
    wa_map.insert(
        serde_yaml::Value::String("enabled".into()),
        serde_yaml::Value::Bool(true),
    );

    let yaml_str = serde_yaml::to_string(&config)
        .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
    std::fs::create_dir_all(hermes_config::hermes_home())
        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
    std::fs::write(&config_path, &yaml_str)
        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;

    println!(
        "\nWhatsApp configuration saved to {}",
        config_path.display()
    );
    println!("Phone Number ID: {}", phone_number_id);
    println!("\nRun `hermes whatsapp status` to verify.");
    Ok(())
}

/// Check whether WhatsApp is configured and verify connectivity.
async fn whatsapp_status() -> Result<(), hermes_core::AgentError> {
    let config_path = hermes_config::hermes_home().join("config.yaml");
    if !config_path.exists() {
        println!("WhatsApp: not configured");
        println!("Run `hermes whatsapp setup` to configure.");
        return Ok(());
    }

    let content = std::fs::read_to_string(&config_path)
        .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
    let config: serde_yaml::Value =
        serde_yaml::from_str(&content).unwrap_or(serde_yaml::Value::Mapping(Default::default()));

    let wa = config.get("platforms").and_then(|p| p.get("whatsapp"));

    match wa {
        None => {
            println!("WhatsApp: not configured");
            println!("Run `hermes whatsapp setup` to configure.");
        }
        Some(wa_cfg) => {
            let phone_id = wa_cfg
                .get("phone_number_id")
                .and_then(|v| v.as_str())
                .unwrap_or("(not set)");
            let enabled = wa_cfg
                .get("enabled")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let has_token = wa_cfg
                .get("access_token")
                .and_then(|v| v.as_str())
                .map(|t| !t.is_empty())
                .unwrap_or(false);

            println!("WhatsApp Status");
            println!("---------------");
            println!("  Configured:     yes");
            println!("  Enabled:        {}", enabled);
            println!("  Phone Number ID: {}", phone_id);
            println!(
                "  Access Token:   {}",
                if has_token { "present" } else { "missing" }
            );

            if has_token {
                let token = wa_cfg.get("access_token").unwrap().as_str().unwrap();
                let url = format!("https://graph.facebook.com/v21.0/{}/messages", phone_id);
                print!("  API Connectivity: ");
                match reqwest::Client::new()
                    .get(&url)
                    .bearer_auth(token)
                    .timeout(std::time::Duration::from_secs(10))
                    .send()
                    .await
                {
                    Ok(resp) => println!("reachable (HTTP {})", resp.status()),
                    Err(e) => println!("unreachable ({})", e),
                }
            }
        }
    }
    Ok(())
}

/// Connect to local bridge, fetch QR data, and render in terminal.
async fn whatsapp_qr() -> Result<(), hermes_core::AgentError> {
    let config_path = hermes_config::hermes_home().join("config.yaml");
    let bridge_url = if config_path.exists() {
        let content = std::fs::read_to_string(&config_path).unwrap_or_default();
        let config: serde_yaml::Value = serde_yaml::from_str(&content)
            .unwrap_or(serde_yaml::Value::Mapping(Default::default()));
        config
            .get("platforms")
            .and_then(|p| p.get("whatsapp"))
            .and_then(|w| w.get("bridge_url"))
            .and_then(|u| u.as_str())
            .unwrap_or("http://localhost:3000")
            .to_string()
    } else {
        "http://localhost:3000".to_string()
    };

    let qr_url = format!("{}/qr", bridge_url);
    println!("Fetching QR code from {}...", qr_url);

    match reqwest::Client::new()
        .get(&qr_url)
        .timeout(std::time::Duration::from_secs(15))
        .send()
        .await
    {
        Ok(resp) if resp.status().is_success() => {
            let body = resp
                .text()
                .await
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;

            let qr_data = if let Ok(json) = serde_json::from_str::<serde_json::Value>(&body) {
                json.get("qr")
                    .or_else(|| json.get("data"))
                    .and_then(|v| v.as_str())
                    .unwrap_or(&body)
                    .to_string()
            } else {
                body
            };

            println!();
            render_qr_to_terminal(&qr_data);
            println!();
            println!("Scan this QR code with WhatsApp on your phone:");
            println!("  WhatsApp → Settings → Linked Devices → Link a Device");
        }
        Ok(resp) => {
            println!(
                "Bridge returned HTTP {}. Is the bridge server running?",
                resp.status()
            );
            println!("Start it with: npx hermes-whatsapp-bridge");
        }
        Err(e) => {
            println!("Could not connect to bridge at {}: {}", bridge_url, e);
            println!("\nMake sure the WhatsApp Web bridge is running:");
            println!("  npx hermes-whatsapp-bridge");
            println!("  # or: docker run -p 3000:3000 hermes/whatsapp-bridge");
        }
    }
    Ok(())
}

/// Render QR data as Unicode block art in the terminal.
///
/// Uses a simple bit-encoding approach: each character in the input
/// string controls whether a "module" is dark or light. Two rows are
/// packed into one terminal line using half-block characters.
fn render_qr_to_terminal(data: &str) {
    // Determine a square side length from the data
    let len = data.len();
    let side = (len as f64).sqrt().ceil() as usize;
    if side == 0 {
        println!("(empty QR data)");
        return;
    }

    let bytes = data.as_bytes();

    // Dark module = odd byte value, light = even (simple heuristic)
    let is_dark = |row: usize, col: usize| -> bool {
        let idx = row * side + col;
        if idx < bytes.len() {
            bytes[idx] % 2 == 1
        } else {
            false
        }
    };

    // Print using half-block characters: each terminal row encodes two QR rows.
    // ▀ = top dark, bottom light | ▄ = top light, bottom dark
    // █ = both dark              | ' ' = both light
    let mut row = 0;
    while row < side {
        let mut line = String::new();
        for col in 0..side {
            let top = is_dark(row, col);
            let bottom = if row + 1 < side {
                is_dark(row + 1, col)
            } else {
                false
            };
            line.push(match (top, bottom) {
                (true, true) => '█',
                (true, false) => '▀',
                (false, true) => '▄',
                (false, false) => ' ',
            });
        }
        println!("  {}", line);
        row += 2;
    }
}

/// Handle `hermes pairing [action] [--device-id ...]`.
pub async fn handle_cli_pairing(
    action: Option<String>,
    device_id: Option<String>,
) -> Result<(), hermes_core::AgentError> {
    use crate::pairing_store::{PairingStatus, PairingStore};

    let store = PairingStore::open_default();

    match action.as_deref().unwrap_or("list") {
        "list" => {
            let devices = store.list().map_err(|e| hermes_core::AgentError::Io(e))?;
            if devices.is_empty() {
                println!("No paired devices.");
                println!("  Store: {}", PairingStore::default_path().display());
            } else {
                println!("Paired devices ({}):", devices.len());
                println!(
                    "  {:20} {:10} {:12} {}",
                    "Device ID", "Status", "Last Seen", "Name"
                );
                println!("  {}", "-".repeat(60));
                for d in &devices {
                    let last_seen = d.last_seen.as_deref().unwrap_or("never");
                    let name = d.name.as_deref().unwrap_or("(unnamed)");
                    let status_icon = match d.status {
                        PairingStatus::Pending => "⏳",
                        PairingStatus::Approved => "✓",
                        PairingStatus::Revoked => "✗",
                    };
                    println!(
                        "  {:20} {} {:8} {:12} {}",
                        d.device_id, status_icon, d.status, last_seen, name
                    );
                }
            }
        }
        "approve" => {
            let did = device_id.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing --device-id. Usage: hermes pairing approve --device-id <id>".into(),
                )
            })?;
            match store.approve(&did) {
                Ok(dev) => {
                    println!("Device '{}' approved.", dev.device_id);
                    if let Some(secret) = &dev.shared_secret {
                        println!("  Shared secret: {}", secret);
                        println!("  (Store this securely — it will not be shown again)");
                    }
                }
                Err(e) => println!("Failed to approve device: {}", e),
            }
        }
        "revoke" => {
            let did = device_id.ok_or_else(|| {
                hermes_core::AgentError::Config(
                    "Missing --device-id. Usage: hermes pairing revoke --device-id <id>".into(),
                )
            })?;
            match store.revoke(&did) {
                Ok(dev) => {
                    println!("Device '{}' revoked.", dev.device_id);
                    println!("  The device will no longer be able to connect.");
                }
                Err(e) => println!("Failed to revoke device: {}", e),
            }
        }
        "clear-pending" => match store.clear_pending() {
            Ok(count) => {
                if count == 0 {
                    println!("No pending pairing requests to clear.");
                } else {
                    println!("Cleared {} pending pairing request(s).", count);
                }
            }
            Err(e) => println!("Failed to clear pending requests: {}", e),
        },
        other => {
            println!("Pairing action '{}' is not recognized.", other);
            println!("Available actions: list, approve, revoke, clear-pending");
        }
    }
    Ok(())
}

/// Handle `hermes claw [action]`.
pub async fn handle_cli_claw(action: Option<String>) -> Result<(), hermes_core::AgentError> {
    match action.as_deref().unwrap_or("status") {
        "migrate" => {
            claw_migrate_cmd()?;
        }
        "cleanup" => {
            claw_cleanup_cmd()?;
        }
        "status" => {
            claw_status_cmd();
        }
        other => {
            println!("Claw action '{}' is not recognized.", other);
            println!("Available actions: migrate, cleanup, status");
        }
    }
    Ok(())
}

/// Check for legacy OpenClaw artefacts and report findings.
fn claw_status_cmd() {
    use crate::claw_migrate::find_openclaw_dir;

    println!("OpenClaw Legacy Status");
    println!("======================\n");

    let home = dirs::home_dir();

    match find_openclaw_dir(None) {
        Some(dir) => {
            println!("  OpenClaw directory: {} (found)", dir.display());

            let config_yaml = dir.join("config.yaml");
            let sessions_dir = dir.join("sessions");
            let env_file = dir.join(".env");
            let skills_dir = dir.join("skills");

            println!(
                "  config.yaml:       {}",
                if config_yaml.exists() {
                    "present"
                } else {
                    "not found"
                }
            );
            println!(
                "  .env:              {}",
                if env_file.exists() {
                    "present"
                } else {
                    "not found"
                }
            );
            println!(
                "  skills/:           {}",
                if skills_dir.is_dir() {
                    "present"
                } else {
                    "not found"
                }
            );

            if sessions_dir.is_dir() {
                let count = std::fs::read_dir(&sessions_dir)
                    .map(|rd| rd.filter_map(|e| e.ok()).count())
                    .unwrap_or(0);
                println!("  sessions/:         {} file(s)", count);
            } else {
                println!("  sessions/:         not found");
            }

            println!("\n  Run `hermes claw migrate` to import into Hermes.");
            println!("  Run `hermes claw cleanup` to remove legacy files.");
        }
        None => {
            println!("  No OpenClaw directory found.");
            if let Some(h) = &home {
                println!(
                    "  Checked: ~/.openclaw, ~/.clawdbot, ~/.moldbot under {}",
                    h.display()
                );
            }
            println!("\n  Nothing to migrate.");
        }
    }

    // Also check for PATH entries in shell configs
    if let Some(h) = &home {
        let shell_files = [".bashrc", ".zshrc", ".profile", ".bash_profile"];
        let mut found_refs = Vec::new();
        for f in &shell_files {
            let path = h.join(f);
            if let Ok(content) = std::fs::read_to_string(&path) {
                if content.contains("openclaw") || content.contains("clawdbot") {
                    found_refs.push(f.to_string());
                }
            }
        }
        if !found_refs.is_empty() {
            println!("\n  Shell config references found:");
            for f in &found_refs {
                println!("    ~/{}", f);
            }
        }
    }
}

/// Run the full migration using `claw_migrate::run_migration`.
fn claw_migrate_cmd() -> Result<(), hermes_core::AgentError> {
    use crate::claw_migrate::{find_openclaw_dir, run_migration, MigrateOptions};

    println!("OpenClaw → Hermes Migration");
    println!("===========================\n");

    let source_dir = find_openclaw_dir(None);
    if source_dir.is_none() {
        println!("No OpenClaw directory found. Nothing to migrate.");
        return Ok(());
    }
    let source_dir = source_dir.unwrap();
    println!("Source: {}", source_dir.display());
    println!("Target: {}\n", hermes_config::hermes_home().display());

    // Also copy sessions if they exist
    let src_sessions = source_dir.join("sessions");
    let dst_sessions = hermes_config::hermes_home().join("sessions");
    let mut session_count = 0usize;

    if src_sessions.is_dir() {
        std::fs::create_dir_all(&dst_sessions).map_err(|e| {
            hermes_core::AgentError::Io(format!("Failed to create sessions dir: {}", e))
        })?;
        if let Ok(entries) = std::fs::read_dir(&src_sessions) {
            for entry in entries.flatten() {
                let src = entry.path();
                let dst = dst_sessions.join(entry.file_name());
                if src.is_file() && !dst.exists() {
                    if std::fs::copy(&src, &dst).is_ok() {
                        session_count += 1;
                    }
                }
            }
        }
    }

    let options = MigrateOptions {
        source: Some(source_dir),
        dry_run: false,
        preset: "full".to_string(),
        overwrite: false,
    };

    let result = run_migration(&options);

    if !result.migrated.is_empty() {
        println!("Migrated:");
        for item in &result.migrated {
            let src = item
                .source
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let dst = item
                .destination
                .as_ref()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let extra = item.reason.as_deref().unwrap_or("");
            println!("  ✓ {} → {} {}", src, dst, extra);
        }
    }

    if !result.skipped.is_empty() {
        println!("Skipped:");
        for item in &result.skipped {
            let reason = item.reason.as_deref().unwrap_or("");
            println!("  ⊘ {} — {}", item.kind, reason);
        }
    }

    if !result.errors.is_empty() {
        println!("Errors:");
        for item in &result.errors {
            let reason = item.reason.as_deref().unwrap_or("unknown error");
            println!("  ✗ {} — {}", item.kind, reason);
        }
    }

    if session_count > 0 {
        println!("\nSessions copied: {}", session_count);
    }

    let total = result.migrated.len() + session_count;
    println!(
        "\nMigration complete: {} item(s) migrated, {} skipped, {} error(s).",
        total,
        result.skipped.len(),
        result.errors.len()
    );

    Ok(())
}

/// Remove legacy OpenClaw files after confirmation.
fn claw_cleanup_cmd() -> Result<(), hermes_core::AgentError> {
    use crate::claw_migrate::find_openclaw_dir;
    use std::io::{self, BufRead, Write};

    let source_dir = find_openclaw_dir(None);
    if source_dir.is_none() {
        println!("No OpenClaw directory found. Nothing to clean up.");
        return Ok(());
    }
    let source_dir = source_dir.unwrap();

    println!("OpenClaw Cleanup");
    println!("================\n");
    println!("The following will be PERMANENTLY deleted:");
    println!("  Directory: {}", source_dir.display());

    // Count contents
    let file_count = count_files_recursive(&source_dir);
    println!("  Contains:  ~{} file(s)\n", file_count);

    // Check shell configs
    let home = dirs::home_dir();
    let shell_files = [".bashrc", ".zshrc", ".profile", ".bash_profile"];
    let mut affected_shells: Vec<String> = Vec::new();
    if let Some(h) = &home {
        for f in &shell_files {
            let path = h.join(f);
            if let Ok(content) = std::fs::read_to_string(&path) {
                if content.contains("openclaw") || content.contains("clawdbot") {
                    affected_shells.push(f.to_string());
                    println!("  Shell config: ~/{} (contains openclaw references)", f);
                }
            }
        }
    }

    print!("\nProceed with cleanup? [y/N]: ");
    io::stdout().flush().ok();
    let answer = io::stdin()
        .lock()
        .lines()
        .next()
        .and_then(|l| l.ok())
        .unwrap_or_default();

    if !matches!(answer.trim().to_lowercase().as_str(), "y" | "yes") {
        println!("Cleanup cancelled.");
        return Ok(());
    }

    // Remove the directory
    match std::fs::remove_dir_all(&source_dir) {
        Ok(_) => println!("  ✓ Removed {}", source_dir.display()),
        Err(e) => println!("  ✗ Failed to remove {}: {}", source_dir.display(), e),
    }

    // Clean shell configs
    if let Some(h) = &home {
        for f in &affected_shells {
            let path = h.join(f);
            if let Ok(content) = std::fs::read_to_string(&path) {
                let cleaned: Vec<&str> = content
                    .lines()
                    .filter(|line| {
                        let lower = line.to_lowercase();
                        !lower.contains("openclaw") && !lower.contains("clawdbot")
                    })
                    .collect();
                let new_content = cleaned.join("\n") + "\n";
                match std::fs::write(&path, new_content) {
                    Ok(_) => println!("  ✓ Cleaned ~/{}", f),
                    Err(e) => println!("  ✗ Failed to clean ~/{}: {}", f, e),
                }
            }
        }
    }

    println!("\nCleanup complete.");
    Ok(())
}

/// Recursively count files in a directory.
fn count_files_recursive(dir: &std::path::Path) -> usize {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                count += count_files_recursive(&path);
            } else {
                count += 1;
            }
        }
    }
    count
}

fn acp_history_to_messages(
    history: &[serde_json::Value],
    fallback_user_text: &str,
) -> Vec<hermes_core::Message> {
    let mut messages = Vec::new();

    for item in history {
        let role = item.get("role").and_then(|v| v.as_str()).unwrap_or("");
        let content = item
            .get("content")
            .or_else(|| item.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();

        match role {
            "system" if !content.is_empty() => messages.push(hermes_core::Message::system(content)),
            "user" if !content.is_empty() => messages.push(hermes_core::Message::user(content)),
            "assistant" => {
                if let Some(tool_calls_val) = item.get("tool_calls") {
                    if let Ok(tool_calls) =
                        serde_json::from_value::<Vec<hermes_core::ToolCall>>(tool_calls_val.clone())
                    {
                        let assistant = hermes_core::Message::assistant_with_tool_calls(
                            if content.is_empty() {
                                None
                            } else {
                                Some(content)
                            },
                            tool_calls,
                        );
                        messages.push(assistant);
                        continue;
                    }
                }
                if !content.is_empty() {
                    messages.push(hermes_core::Message::assistant(content));
                }
            }
            "tool" if !content.is_empty() => {
                let tool_call_id = item
                    .get("tool_call_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("tool_call");
                messages.push(hermes_core::Message::tool_result(tool_call_id, content));
            }
            _ => {}
        }
    }

    let has_user_tail = messages
        .last()
        .map(|m| matches!(m.role, hermes_core::MessageRole::User))
        .unwrap_or(false);
    if !has_user_tail && !fallback_user_text.trim().is_empty() {
        messages.push(hermes_core::Message::user(fallback_user_text));
    }

    messages
}

struct CliAcpPromptExecutor {
    config: Arc<hermes_config::GatewayConfig>,
    tool_registry: Arc<hermes_tools::ToolRegistry>,
    tool_schemas: Vec<hermes_core::ToolSchema>,
}

#[async_trait::async_trait]
impl hermes_acp::AcpPromptExecutor for CliAcpPromptExecutor {
    async fn execute_prompt(
        &self,
        session: &hermes_acp::SessionState,
        user_text: &str,
        history: &[serde_json::Value],
    ) -> Result<hermes_acp::PromptExecutionOutput, String> {
        let model = session
            .model
            .clone()
            .or_else(|| self.config.model.clone())
            .unwrap_or_else(|| "gpt-4o".to_string());

        let provider = crate::app::build_provider(&self.config, &model);
        let mut agent_config = crate::app::build_agent_config(&self.config, &model);
        agent_config.session_id = Some(session.session_id.clone());

        let agent_tools = Arc::new(crate::app::bridge_tool_registry(&self.tool_registry));
        let agent = hermes_agent::AgentLoop::new(agent_config, agent_tools, provider);
        let messages = acp_history_to_messages(history, user_text);

        let result = agent
            .run(messages, Some(self.tool_schemas.clone()))
            .await
            .map_err(|e| e.to_string())?;
        let response_text = result
            .messages
            .iter()
            .rev()
            .find(|m| matches!(m.role, hermes_core::MessageRole::Assistant))
            .and_then(|m| m.content.clone())
            .unwrap_or_default();

        let usage = result.usage.map(|u| hermes_acp::Usage {
            input_tokens: u.prompt_tokens,
            output_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
            thought_tokens: None,
            cached_read_tokens: None,
        });

        Ok(hermes_acp::PromptExecutionOutput {
            response_text,
            usage,
            total_turns: Some(result.total_turns),
        })
    }
}

/// Handle `hermes acp [action]`.
pub async fn handle_cli_acp(action: Option<String>) -> Result<(), hermes_core::AgentError> {
    match action.as_deref().unwrap_or("status") {
        "start" => {
            let config = hermes_config::load_config(None)
                .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;

            let model = config.model.clone().unwrap_or_else(|| "gpt-4o".to_string());
            let max_turns = config.max_turns as usize;

            println!(
                "Starting ACP server (model={}, max_turns={})...",
                model, max_turns
            );

            let tool_registry = Arc::new(hermes_tools::ToolRegistry::new());
            let terminal_backend = crate::terminal_backend::build_terminal_backend(&config);
            let skill_store = Arc::new(hermes_skills::FileSkillStore::new(
                hermes_skills::FileSkillStore::default_dir(),
            ));
            let skill_provider: Arc<dyn hermes_core::SkillProvider> =
                Arc::new(hermes_skills::SkillManager::new(skill_store));
            hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);
            crate::runtime_tool_wiring::wire_stdio_clarify_backend(&tool_registry);
            let cron_data_dir = hermes_config::cron_dir();
            std::fs::create_dir_all(&cron_data_dir)
                .map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;
            let cron_scheduler = Arc::new(hermes_cron::cron_scheduler_for_data_dir(cron_data_dir));
            cron_scheduler
                .load_persisted_jobs()
                .await
                .map_err(|e| hermes_core::AgentError::Config(format!("cron load: {e}")))?;
            cron_scheduler.start().await;
            crate::runtime_tool_wiring::wire_cron_scheduler_backend(&tool_registry, cron_scheduler);
            let tool_schemas = crate::platform_toolsets::resolve_platform_tool_schemas(
                &config,
                "cli",
                &tool_registry,
            );

            let prompt_executor = Arc::new(CliAcpPromptExecutor {
                config: Arc::new(config.clone()),
                tool_registry,
                tool_schemas,
            });

            let session_manager = Arc::new(hermes_acp::SessionManager::new());
            let event_sink = Arc::new(hermes_acp::EventSink::default());
            let permission_store = Arc::new(hermes_acp::PermissionStore::new());
            let handler = Arc::new(
                hermes_acp::HermesAcpHandler::new(
                    session_manager.clone(),
                    event_sink.clone(),
                    permission_store.clone(),
                )
                .with_prompt_executor(prompt_executor),
            );
            let server = hermes_acp::AcpServer::with_components(
                handler,
                session_manager,
                event_sink,
                permission_store,
            );

            server
                .run()
                .await
                .map_err(|e| hermes_core::AgentError::Io(format!("ACP server error: {}", e)))?;
        }
        "status" => {
            println!("ACP server: not running");
            println!("ACP runs as a stdio JSON-RPC server in the foreground.");
            println!("Start with `hermes acp start`.");
        }
        "stop" => {
            println!("ACP stop is not a separate command in stdio mode.");
            println!("If running, stop it by closing the parent process or sending Ctrl+C.");
        }
        "restart" => {
            println!("ACP restart in stdio mode is equivalent to stop + start.");
            println!("Use:");
            println!("  1) Stop the current process (Ctrl+C)");
            println!("  2) Run `hermes acp start`");
        }
        other => {
            println!("Unknown ACP action '{}'.", other);
            println!("Available actions: start, status, stop, restart");
        }
    }
    Ok(())
}

/// Handle `hermes backup [output]`.
pub async fn handle_cli_backup(output: Option<String>) -> Result<(), hermes_core::AgentError> {
    let hermes_dir = hermes_config::hermes_home();
    if !hermes_dir.exists() {
        println!(
            "Hermes home directory not found at {}",
            hermes_dir.display()
        );
        return Ok(());
    }
    let out = output.unwrap_or_else(|| {
        format!(
            "hermes-backup-{}.tar.gz",
            chrono::Utc::now().format("%Y%m%d-%H%M%S")
        )
    });
    println!("Backing up {} -> {}", hermes_dir.display(), out);

    let tar_gz = std::fs::File::create(&out)
        .map_err(|e| hermes_core::AgentError::Io(format!("Cannot create {}: {}", out, e)))?;
    let enc = flate2::write::GzEncoder::new(tar_gz, flate2::Compression::default());
    let mut tar = tar::Builder::new(enc);
    tar.append_dir_all("hermes", &hermes_dir)
        .map_err(|e| hermes_core::AgentError::Io(format!("Tar error: {}", e)))?;
    tar.finish()
        .map_err(|e| hermes_core::AgentError::Io(format!("Tar finish error: {}", e)))?;

    let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
    println!("Backup complete: {} ({} KB)", out, size / 1024);
    Ok(())
}

/// Handle `hermes import <path>`.
pub async fn handle_cli_import(path: String) -> Result<(), hermes_core::AgentError> {
    let src = std::path::Path::new(&path);
    if !src.exists() {
        return Err(hermes_core::AgentError::Io(format!(
            "Backup archive not found: {}",
            path
        )));
    }
    println!("Importing configuration from: {}", path);

    let hermes_dir = hermes_config::hermes_home();
    std::fs::create_dir_all(&hermes_dir).map_err(|e| hermes_core::AgentError::Io(e.to_string()))?;

    let file = std::fs::File::open(src)
        .map_err(|e| hermes_core::AgentError::Io(format!("Cannot open {}: {}", path, e)))?;
    let dec = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(dec);
    archive
        .unpack(&hermes_dir)
        .map_err(|e| hermes_core::AgentError::Io(format!("Extract error: {}", e)))?;

    println!(
        "Import complete. Files restored to {}",
        hermes_dir.display()
    );
    Ok(())
}

/// Handle `hermes version`.
pub fn handle_cli_version() -> Result<(), hermes_core::AgentError> {
    println!("hermes {}", env!("CARGO_PKG_VERSION"));
    Ok(())
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

    #[test]
    fn test_default_skill_tap_present_in_merged_list() {
        let merged = merged_skill_taps(&[]);
        assert!(merged
            .iter()
            .any(|tap| tap == "https://github.com/MiniMax-AI/cli"));
    }

    #[test]
    fn test_merged_skill_taps_deduplicates_default() {
        let merged = merged_skill_taps(&vec!["https://github.com/MiniMax-AI/cli".to_string()]);
        assert_eq!(
            merged
                .iter()
                .filter(|tap| tap.as_str() == "https://github.com/MiniMax-AI/cli")
                .count(),
            1
        );
    }

    #[test]
    fn parse_model_switch_request_picks_provider_when_empty() {
        let providers = vec!["openai", "nous", "anthropic"];
        let req = parse_model_switch_request(&[], &providers);
        assert_eq!(req, ModelSwitchRequest::PickProviderThenModel);
    }

    #[test]
    fn parse_model_switch_request_uses_provider_picker_for_provider_arg() {
        let providers = vec!["openai", "nous", "anthropic"];
        let req = parse_model_switch_request(&["NOUS"], &providers);
        assert_eq!(
            req,
            ModelSwitchRequest::PickModelFromProvider("nous".to_string())
        );
    }

    #[test]
    fn parse_model_switch_request_accepts_direct_provider_model() {
        let providers = vec!["openai", "nous", "anthropic"];
        let req = parse_model_switch_request(&["openai:gpt-4o"], &providers);
        assert_eq!(
            req,
            ModelSwitchRequest::SetDirect("openai:gpt-4o".to_string())
        );
    }

    #[test]
    fn parse_model_switch_request_keeps_bare_model_as_direct() {
        let providers = vec!["openai", "nous", "anthropic"];
        let req = parse_model_switch_request(&["gpt-4o"], &providers);
        assert_eq!(req, ModelSwitchRequest::SetDirect("gpt-4o".to_string()));
    }
}
