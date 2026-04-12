//! Hermes Agent — binary entry point.
//!
//! Initializes logging, parses CLI arguments, and dispatches to the
//! appropriate subcommand handler.

use clap::Parser;
use clap::CommandFactory;
use clap_complete::{generate, Shell as CompletionShell};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use hermes_cli::cli::{Cli, CliCommand};
use hermes_cli::App;
use hermes_cli::app::{
    bridge_tool_registry, build_agent_config, build_provider, provider_api_key_from_env,
};
use tokio::sync::broadcast;
use hermes_config::{
    apply_user_config_patch, gateway_pid_path_in, hermes_home, load_config, load_user_config_file,
    save_config_yaml, state_dir, user_config_field_display, validate_config, ConfigError,
    PlatformConfig,
};
use hermes_core::AgentError;
use hermes_core::{MessageRole, StreamChunk};
use hermes_core::PlatformAdapter;
use hermes_agent::AgentLoop;
use hermes_auth::{AuthManager, FileTokenStore, OAuthCredential};
use hermes_cron::{
    cron_scheduler_for_data_dir, CronCompletionEvent, CronError, CronRunner, CronScheduler,
    FileJobPersistence,
};
use hermes_gateway::{Gateway, GatewayRuntimeContext, SessionManager, DmManager};
use hermes_gateway::gateway::IncomingMessage as GatewayIncomingMessage;
use hermes_gateway::gateway::GatewayConfig as RuntimeGatewayConfig;
use hermes_gateway::platforms::telegram::{TelegramAdapter, TelegramConfig};
use hermes_telemetry::init_telemetry_from_env;
use hermes_environments::LocalBackend;
use hermes_skills::{FileSkillStore, SkillManager};
use hermes_tools::ToolRegistry;
#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Initialize tracing
    init_tracing(cli.verbose);

    tracing::debug!("Hermes Agent starting");

    let result = match cli.effective_command() {
        CliCommand::Hermes => run_interactive(cli).await,
        CliCommand::Chat { query, preload_skill, yolo } => {
            hermes_cli::commands::handle_cli_chat(query, preload_skill, yolo).await
        }
        CliCommand::Model { provider_model } => run_model(cli, provider_model).await,
        CliCommand::Tools { action } => run_tools(cli, action).await,
        CliCommand::Config { action, key, value } => run_config(cli, action, key, value).await,
        CliCommand::Gateway { action } => run_gateway(cli, action).await,
        CliCommand::Setup => run_setup().await,
        CliCommand::Doctor => run_doctor(cli).await,
        CliCommand::Update => run_update().await,
        CliCommand::Status => run_status(cli).await,
        CliCommand::Logs { lines, follow } => run_logs(cli, lines, follow).await,
        CliCommand::Profile { action, name } => run_profile(cli, action, name).await,
        CliCommand::Auth { action, provider } => run_auth(cli, action, provider).await,
        CliCommand::Skills { action, name, extra } => {
            hermes_cli::commands::handle_cli_skills(action, name, extra).await
        }
        CliCommand::Plugins {
            action,
            name,
            git_ref,
            allow_untrusted_git_host,
        } => {
            hermes_cli::commands::handle_cli_plugins(
                action,
                name,
                git_ref,
                allow_untrusted_git_host,
            )
            .await
        }
        CliCommand::Memory { action } => {
            hermes_cli::commands::handle_cli_memory(action).await
        }
        CliCommand::Mcp { action, server } => {
            hermes_cli::commands::handle_cli_mcp(action, server).await
        }
        CliCommand::Sessions { action, id, name } => {
            hermes_cli::commands::handle_cli_sessions(action, id, name).await
        }
        CliCommand::Insights { days, source } => {
            hermes_cli::commands::handle_cli_insights(days, source).await
        }
        CliCommand::Login { provider } => {
            hermes_cli::commands::handle_cli_login(provider).await
        }
        CliCommand::Logout { provider } => {
            hermes_cli::commands::handle_cli_logout(provider).await
        }
        CliCommand::Whatsapp { action } => {
            hermes_cli::commands::handle_cli_whatsapp(action).await
        }
        CliCommand::Pairing { action, device_id } => {
            hermes_cli::commands::handle_cli_pairing(action, device_id).await
        }
        CliCommand::Claw { action } => {
            hermes_cli::commands::handle_cli_claw(action).await
        }
        CliCommand::Acp { action } => {
            hermes_cli::commands::handle_cli_acp(action).await
        }
        CliCommand::Backup { output } => {
            hermes_cli::commands::handle_cli_backup(output).await
        }
        CliCommand::Import { path } => {
            hermes_cli::commands::handle_cli_import(path).await
        }
        CliCommand::Version => hermes_cli::commands::handle_cli_version(),
        CliCommand::Cron {
            action,
            id,
            schedule,
            prompt,
        } => run_cron(cli, action, id, schedule, prompt).await,
        CliCommand::Webhook { action, url, id } => run_webhook(cli, action, url, id).await,
        CliCommand::Dump { session, output } => run_dump(cli, session, output).await,
        CliCommand::Completion { shell } => run_completion(shell),
        CliCommand::Uninstall { yes } => run_uninstall(yes).await,
    };

    if let Err(e) = result {
        eprintln!("Error: {}", e);
        std::process::exit(1);
    }
}

/// Initialize the tracing subscriber with env filter.
fn init_tracing(verbose: bool) {
    let default = if verbose { "debug" } else { "info" };
    init_telemetry_from_env("hermes-cli", default);
}

/// Run the interactive REPL (default command).
async fn run_interactive(cli: Cli) -> Result<(), AgentError> {
    let app = App::new(cli).await?;
    hermes_cli::tui::run(app).await
}

/// Handle `hermes model [provider:model]`.
async fn run_model(cli: Cli, provider_model: Option<String>) -> Result<(), AgentError> {
    let config = load_config(cli.config_dir.as_deref())
        .map_err(|e| AgentError::Config(e.to_string()))?;

    match provider_model {
        Some(pm) => {
            println!("Model switched to: {}", pm);
            println!("(To persist, run: hermes config set model {})", pm);
        }
        None => {
            let current = config.model.as_deref().unwrap_or("gpt-4o");
            println!("Current model: {}", current);

            // List known providers
            println!("\nAvailable providers:");
            println!("  openai       — OpenAI (gpt-4o, gpt-4o-mini, ...)");
            println!("  anthropic    — Anthropic (claude-3-5-sonnet, claude-3-opus, ...)");
            println!("  openrouter   — OpenRouter (multi-provider routing)");
            println!("\nUsage: hermes model <provider>:<model>");
        }
    }
    Ok(())
}

/// Handle `hermes tools [action]`.
async fn run_tools(cli: Cli, action: Option<String>) -> Result<(), AgentError> {
    let registry = hermes_tools::ToolRegistry::new();
    let tools = registry.list_tools();

    match action.as_deref() {
        None | Some("list") => {
            if tools.is_empty() {
                println!("No tools registered (tools are loaded at runtime).");
                println!("\nBuilt-in tool categories:");
                let categories = [
                    "web", "terminal", "file", "browser", "vision", "image_gen",
                    "skills", "memory", "session_search", "todo", "clarify",
                    "code_execution", "delegation", "cronjob", "messaging",
                    "homeassistant",
                ];
                for cat in &categories {
                    println!("  • {}", cat);
                }
            } else {
                println!("Registered tools ({}):", tools.len());
                for tool in &tools {
                    println!("  • {} — {}", tool.name, tool.description);
                }
            }
        }
        Some(other) => {
            println!("Unknown tools action: {}. Use 'list'.", other);
        }
    }
    Ok(())
}

/// Handle `hermes config [action] [key] [value]`.
async fn run_config(
    cli: Cli,
    action: Option<String>,
    key: Option<String>,
    value: Option<String>,
) -> Result<(), AgentError> {
    let config = load_config(cli.config_dir.as_deref())
        .map_err(|e| AgentError::Config(e.to_string()))?;

    match action.as_deref() {
        None => {
            // Show full config as JSON
            let json = serde_json::to_string_pretty(&config)
                .map_err(|e| AgentError::Config(e.to_string()))?;
            println!("{}", json);
        }
        Some("get") => {
            let key = key.ok_or_else(|| AgentError::Config("Missing key. Usage: hermes config get <key>".into()))?;
            match user_config_field_display(&config, &key) {
                Ok(s) => println!("{}", s),
                Err(ConfigError::NotFound(_)) => println!("Unknown config key: {}", key),
                Err(e) => return Err(AgentError::Config(e.to_string())),
            }
        }
        Some("set") => {
            let key = key.ok_or_else(|| AgentError::Config("Missing key. Usage: hermes config set <key> <value>".into()))?;
            let value = value.ok_or_else(|| AgentError::Config("Missing value. Usage: hermes config set <key> <value>".into()))?;
            let base: PathBuf = cli
                .config_dir
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(hermes_home);
            let cfg_path = base.join("config.yaml");
            let mut disk = load_user_config_file(&cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;
            apply_user_config_patch(&mut disk, &key, &value).map_err(|e| AgentError::Config(e.to_string()))?;
            validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
            save_config_yaml(&cfg_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;
            println!("Saved {} = {} -> {}", key, value, cfg_path.display());
        }
        Some("show") => {
            let json = serde_json::to_string_pretty(&config)
                .map_err(|e| AgentError::Config(e.to_string()))?;
            println!("{}", json);
        }
        Some("path") => {
            let base: PathBuf = cli
                .config_dir
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(hermes_home);
            let cfg_path = base.join("config.yaml");
            println!("{}", cfg_path.display());
        }
        Some("env-path") => {
            let env_path = hermes_home().join(".env");
            println!("{}", env_path.display());
            if env_path.exists() {
                println!("(exists)");
            } else {
                println!("(not found — create it to set environment overrides)");
            }
        }
        Some("check") | Some("validate") => {
            println!("Validating configuration...");
            match validate_config(&config) {
                Ok(()) => println!("Configuration is valid. ✓"),
                Err(e) => println!("Configuration error: {}", e),
            }
        }
        Some("edit") => {
            let base: PathBuf = cli
                .config_dir
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(hermes_home);
            let cfg_path = base.join("config.yaml");
            let editor = std::env::var("EDITOR").unwrap_or_else(|_| "vi".into());
            println!("Opening {} with {}...", cfg_path.display(), editor);
            let status = std::process::Command::new(&editor)
                .arg(&cfg_path)
                .status();
            match status {
                Ok(s) if s.success() => println!("Config saved."),
                Ok(s) => println!("Editor exited with: {}", s),
                Err(e) => println!("Could not launch editor '{}': {}", editor, e),
            }
        }
        Some("migrate") => {
            println!("Config Migration");
            println!("----------------");
            let base: PathBuf = cli
                .config_dir
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(hermes_home);
            let old_json = base.join("config.json");
            let new_yaml = base.join("config.yaml");
            if old_json.exists() && !new_yaml.exists() {
                println!("Found legacy config.json — converting to config.yaml...");
                match std::fs::read_to_string(&old_json) {
                    Ok(content) => {
                        if let Ok(val) = serde_json::from_str::<serde_json::Value>(&content) {
                            match serde_yaml::to_string(&val) {
                                Ok(yaml) => {
                                    std::fs::write(&new_yaml, &yaml)
                                        .map_err(|e| AgentError::Io(e.to_string()))?;
                                    println!("Migrated config.json -> config.yaml");
                                    println!("The old config.json was preserved.");
                                }
                                Err(e) => println!("YAML conversion error: {}", e),
                            }
                        } else {
                            println!("Could not parse config.json as JSON.");
                        }
                    }
                    Err(e) => println!("Could not read config.json: {}", e),
                }
            } else if new_yaml.exists() {
                println!("config.yaml already exists. No migration needed.");
            } else {
                println!("No legacy config.json found. Nothing to migrate.");
            }
        }
        Some(other) => {
            println!("Unknown config action: '{}'.", other);
            println!("Available: show, get, set, path, env-path, check, edit, migrate");
        }
    }
    Ok(())
}

/// Config/state root shared by CLI, `hermes gateway`, cron, and `webhooks.json`.
fn hermes_state_root(cli: &Cli) -> PathBuf {
    state_dir(cli.config_dir.as_deref().map(Path::new))
}

fn gateway_pid_path_for_cli(cli: &Cli) -> PathBuf {
    gateway_pid_path_in(hermes_state_root(cli))
}

fn read_gateway_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

#[cfg(unix)]
fn gateway_pid_is_alive(pid: u32) -> bool {
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[cfg(not(unix))]
fn gateway_pid_is_alive(_pid: u32) -> bool {
    false
}

#[cfg(unix)]
fn gateway_pid_terminate(pid: u32) -> std::io::Result<()> {
    let r = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
    if r == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn gateway_pid_terminate(_pid: u32) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "gateway stop is not supported on this platform",
    ))
}

/// Handle `hermes gateway [action]`.
async fn run_gateway(cli: Cli, action: Option<String>) -> Result<(), AgentError> {
    let config = load_config(cli.config_dir.as_deref())
        .map_err(|e| AgentError::Config(e.to_string()))?;

    match action.as_deref() {
        Some("setup") => {
            println!("Gateway setup wizard");
            println!("--------------------");
            println!("Edit config.yaml and enable platforms under `platforms:`");
            println!("Then run `hermes gateway start`.");
        }
        None | Some("start") => {
            println!("Starting Hermes Gateway...");

            // List enabled platforms
            let enabled: Vec<&String> = config
                .platforms
                .iter()
                .filter(|(_, pc)| pc.enabled)
                .map(|(name, _)| name)
                .collect();

            if enabled.is_empty() {
                println!(
                    "Note: no chat platforms enabled in config.yaml — gateway still runs cron + HTTP webhooks."
                );
            }

            let pid_path = gateway_pid_path_for_cli(&cli);
            if let Some(pid) = read_gateway_pid(&pid_path) {
                if gateway_pid_is_alive(pid) {
                    println!(
                        "Gateway already appears to be running (PID {}, file {}). Stop it first or remove a stale PID file.",
                        pid,
                        pid_path.display()
                    );
                    return Ok(());
                }
                let _ = std::fs::remove_file(&pid_path);
            }

            if !enabled.is_empty() {
                println!(
                    "Enabled platforms: {}",
                    enabled.iter().map(|s| s.as_str()).collect::<Vec<_>>().join(", ")
                );
            }

            // Build gateway runtime and context-aware message handler.
            let runtime_gateway_config = RuntimeGatewayConfig {
                streaming_enabled: config.streaming.enabled,
                ..RuntimeGatewayConfig::default()
            };
            let session_manager = Arc::new(SessionManager::new(config.session.clone()));
            let dm_manager = DmManager::with_pair_behavior();
            let gateway = Arc::new(Gateway::new(
                session_manager,
                dm_manager,
                runtime_gateway_config,
            ));

            let tool_registry = Arc::new(ToolRegistry::new());
            let terminal_backend: Arc<dyn hermes_core::TerminalBackend> =
                Arc::new(LocalBackend::default());
            let skill_store = Arc::new(FileSkillStore::new(FileSkillStore::default_dir()));
            let skill_provider: Arc<dyn hermes_core::SkillProvider> =
                Arc::new(SkillManager::new(skill_store));
            hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);
            let agent_registry = Arc::new(bridge_tool_registry(&tool_registry));
            let agent_tools_for_msg = agent_registry.clone();
            let agent_tools_for_stream = agent_registry.clone();
            let agent_tools_for_cron = agent_registry.clone();
            let config_arc = Arc::new(config.clone());
            let config_arc_stream = config_arc.clone();
            gateway
                .set_message_handler_with_context(Arc::new(move |messages, ctx| {
                    let config = config_arc.clone();
                    let agent_tools = agent_tools_for_msg.clone();
                    Box::pin(async move {
                        let agent = build_agent_for_gateway_context(config.as_ref(), &ctx, agent_tools);
                        let result = agent
                            .run(messages, None)
                            .await
                            .map_err(|e| hermes_gateway::GatewayError::Platform(e.to_string()))?;
                        Ok(extract_last_assistant_reply(&result.messages))
                    })
                }))
                .await;
            gateway
                .set_streaming_handler_with_context(Arc::new(move |messages, ctx, on_chunk| {
                    let config = config_arc_stream.clone();
                    let agent_tools = agent_tools_for_stream.clone();
                    Box::pin(async move {
                        let agent = build_agent_for_gateway_context(config.as_ref(), &ctx, agent_tools);
                        let emit = on_chunk.clone();
                        let stream_cb: Box<dyn Fn(StreamChunk) + Send + Sync> =
                            Box::new(move |chunk: StreamChunk| {
                                if let Some(delta) = chunk.delta {
                                    if let Some(text) = delta.content {
                                        emit(text);
                                    }
                                }
                            });

                        let result = agent
                            .run_stream(messages, None, Some(stream_cb))
                            .await
                            .map_err(|e| hermes_gateway::GatewayError::Platform(e.to_string()))?;
                        Ok(extract_last_assistant_reply(&result.messages))
                    })
                }))
                .await;

            // Cron: same on-disk dir as `hermes cron` + real LLM/tools as the gateway agent.
            let cron_dir = hermes_state_root(&cli).join("cron");
            std::fs::create_dir_all(&cron_dir).map_err(|e| {
                AgentError::Io(format!("cron dir {}: {}", cron_dir.display(), e))
            })?;
            let default_model = config
                .model
                .clone()
                .unwrap_or_else(|| "gpt-4o".to_string());
            let cron_persistence = Arc::new(FileJobPersistence::with_dir(cron_dir.clone()));
            let cron_llm = build_provider(&config, &default_model);
            let cron_runner = Arc::new(CronRunner::new(cron_llm, agent_tools_for_cron));
            let mut cron_scheduler = CronScheduler::new(cron_persistence, cron_runner);
            let (cron_tx, cron_rx) = broadcast::channel::<CronCompletionEvent>(64);
            cron_scheduler.set_completion_broadcast(cron_tx);
            cron_scheduler
                .load_persisted_jobs()
                .await
                .map_err(|e| AgentError::Config(format!("cron load: {e}")))?;
            cron_scheduler.start().await;
            let cron_scheduler = Arc::new(cron_scheduler);
            let webhooks_path = hermes_state_root(&cli).join("webhooks.json");
            tracing::info!(
                cron_dir = %cron_dir.display(),
                webhooks = %webhooks_path.display(),
                "gateway cron scheduler + HTTP webhook fan-out"
            );
            println!(
                "Cron jobs: {}  |  Webhook registry: {}",
                cron_dir.display(),
                webhooks_path.display()
            );

            let mut sidecar_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
            let webhooks_path_clone = webhooks_path.clone();
            sidecar_tasks.push(tokio::spawn(async move {
                run_cron_webhook_delivery_loop(cron_rx, webhooks_path_clone).await;
            }));

            if let Some(platform_cfg) = config.platforms.get("telegram") {
                if platform_cfg.enabled {
                    if let Some(token) = platform_cfg.token.clone().filter(|t| !t.trim().is_empty()) {
                        let telegram_config = build_telegram_config(platform_cfg, token);
                        let telegram_adapter = Arc::new(TelegramAdapter::new(telegram_config)?);
                        gateway
                            .register_adapter("telegram", telegram_adapter.clone())
                            .await;
                        let gw_clone = gateway.clone();
                        sidecar_tasks.push(tokio::spawn(async move {
                            run_telegram_poll_loop(gw_clone, telegram_adapter).await;
                        }));
                    } else {
                        println!("Telegram is enabled but token is missing; skipping telegram adapter.");
                    }
                }
            }

            if gateway.adapter_names().await.is_empty() {
                println!(
                    "No chat adapters started (e.g. missing Telegram token). Cron + webhooks still active."
                );
            }

            gateway.start_all().await?;
            let own_pid = std::process::id();
            std::fs::write(&pid_path, format!("{}\n", own_pid)).map_err(|e| {
                AgentError::Io(format!("failed to write {}: {}", pid_path.display(), e))
            })?;
            println!("Gateway runtime initialized with context-aware model/provider routing.");
            println!("Gateway is ready. Press Ctrl+C to stop.");
            // Keep gateway alive for future adapter/event wiring.
            // Wait for Ctrl+C
            tokio::signal::ctrl_c()
                .await
                .map_err(|e| AgentError::Io(format!("Failed to listen for Ctrl+C: {}", e)))?;

            println!("\nShutting down gateway...");
            cron_scheduler.stop().await;
            gateway.stop_all().await?;
            let _ = std::fs::remove_file(&pid_path);
            for task in sidecar_tasks {
                task.abort();
            }
            println!("Gateway stopped.");
        }
        Some("status") => {
            let pid_path = gateway_pid_path_for_cli(&cli);
            match std::fs::read_to_string(&pid_path) {
                Ok(raw) => match raw.trim().parse::<u32>() {
                    Ok(pid) if gateway_pid_is_alive(pid) => {
                        println!(
                            "Gateway status: running (PID {}, file {})",
                            pid,
                            pid_path.display()
                        );
                    }
                    Ok(pid) => {
                        println!(
                            "Gateway status: not running (stale PID {} in {})",
                            pid,
                            pid_path.display()
                        );
                    }
                    Err(_) => {
                        println!(
                            "Gateway status: invalid PID file at {}",
                            pid_path.display()
                        );
                    }
                },
                Err(_) => {
                    println!("Gateway status: not running (no PID file; start with `hermes gateway start`)");
                }
            }
        }
        Some("stop") => {
            let pid_path = gateway_pid_path_for_cli(&cli);
            let Some(pid) = read_gateway_pid(&pid_path) else {
                println!("Gateway stop: no PID file (nothing to stop).");
                return Ok(());
            };
            if !gateway_pid_is_alive(pid) {
                let _ = std::fs::remove_file(&pid_path);
                println!(
                    "Gateway stop: process {} not running; removed stale PID file {}.",
                    pid,
                    pid_path.display()
                );
                return Ok(());
            }
            match gateway_pid_terminate(pid) {
                Ok(()) => {
                    println!("Sent SIGTERM to gateway PID {}.", pid);
                    let _ = std::fs::remove_file(&pid_path);
                    println!("Removed {}.", pid_path.display());
                }
                Err(e) => println!("Gateway stop: failed to signal PID {}: {}", pid, e),
            }
        }
        Some(other) => {
            println!("Unknown gateway action: {}. Use 'start', 'stop', or 'status'.", other);
        }
    }
    Ok(())
}

fn resolve_model_for_gateway(default_model: &str, ctx: &GatewayRuntimeContext) -> String {
    if let Some(model) = &ctx.model {
        if model.contains(':') {
            return model.clone();
        }
        if let Some(provider) = &ctx.provider {
            return format!("{}:{}", provider, model);
        }
        return model.clone();
    }

    if let Some(provider) = &ctx.provider {
        if default_model.contains(':') {
            if let Some((_, model_part)) = default_model.split_once(':') {
                return format!("{}:{}", provider, model_part);
            }
        }
        return format!("{}:{}", provider, default_model);
    }

    default_model.to_string()
}

fn build_agent_for_gateway_context(
    config: &hermes_config::GatewayConfig,
    ctx: &GatewayRuntimeContext,
    agent_tools: Arc<hermes_agent::agent_loop::ToolRegistry>,
) -> AgentLoop {
    let effective_model = resolve_model_for_gateway(config.model.as_deref().unwrap_or("gpt-4o"), ctx);
    let provider = build_provider(config, &effective_model);
    let mut agent_config = build_agent_config(config, &effective_model);
    if let Some(personality) = ctx.personality.clone() {
        agent_config.personality = Some(personality);
    }
    AgentLoop::new(agent_config, agent_tools, provider)
}

fn extract_last_assistant_reply(messages: &[hermes_core::Message]) -> String {
    messages
        .iter()
        .rev()
        .find_map(|m| {
            if m.role == MessageRole::Assistant {
                m.content.clone()
            } else {
                None
            }
        })
        .unwrap_or_else(|| "(no assistant reply)".to_string())
}

fn build_telegram_config(
    platform_cfg: &hermes_config::platform::PlatformConfig,
    token: String,
) -> TelegramConfig {
    let polling = platform_cfg
        .extra
        .get("polling")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);
    let parse_markdown = platform_cfg
        .extra
        .get("parse_markdown")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let parse_html = platform_cfg
        .extra
        .get("parse_html")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let poll_timeout = platform_cfg
        .extra
        .get("poll_timeout")
        .and_then(|v| v.as_u64())
        .unwrap_or(30);

    TelegramConfig {
        token,
        webhook_url: platform_cfg.webhook_url.clone(),
        polling,
        proxy: Default::default(),
        parse_markdown,
        parse_html,
        poll_timeout,
        bot_username: None,
    }
}

async fn run_telegram_poll_loop(gateway: Arc<Gateway>, adapter: Arc<TelegramAdapter>) {
    loop {
        if !adapter.is_running() {
            break;
        }

        match adapter.get_updates().await {
            Ok(updates) => {
                for update in updates {
                    let Some(msg) = TelegramAdapter::parse_update(&update) else {
                        continue;
                    };

                    let text = msg.text.unwrap_or_else(|| {
                        if msg.is_voice {
                            "[voice message]".to_string()
                        } else if msg.is_photo {
                            "[photo message]".to_string()
                        } else {
                            "[unsupported message]".to_string()
                        }
                    });
                    let user_id = msg
                        .user_id
                        .map(|id| id.to_string())
                        .or(msg.username)
                        .unwrap_or_else(|| "unknown".to_string());
                    let incoming = GatewayIncomingMessage {
                        platform: "telegram".to_string(),
                        chat_id: msg.chat_id.to_string(),
                        user_id,
                        text,
                        message_id: Some(msg.message_id.to_string()),
                        is_dm: msg.chat_id > 0,
                    };

                    if let Err(err) = gateway.route_message(&incoming).await {
                        tracing::warn!("Failed to route telegram message: {}", err);
                    }
                }
            }
            Err(err) => {
                tracing::warn!("Telegram polling error: {}", err);
                tokio::time::sleep(std::time::Duration::from_millis(800)).await;
            }
        }
    }
}

/// Default auth provider: CLI arg, then `HERMES_AUTH_DEFAULT_PROVIDER`, then `openai`.
///
/// Set `HERMES_AUTH_DEFAULT_PROVIDER=telegram` if you primarily use the Telegram gateway.
fn resolve_auth_provider(provider: Option<String>) -> String {
    provider
        .filter(|s| !s.trim().is_empty())
        .or_else(|| {
            std::env::var("HERMES_AUTH_DEFAULT_PROVIDER")
                .ok()
                .filter(|s| !s.trim().is_empty())
        })
        .unwrap_or_else(|| "openai".to_string())
}

async fn telegram_bot_token_from_env_or_prompt() -> Result<String, AgentError> {
    if let Ok(t) = std::env::var("TELEGRAM_BOT_TOKEN") {
        let t = t.trim().to_string();
        if !t.is_empty() {
            return Ok(t);
        }
    }
    let line = tokio::task::spawn_blocking(|| {
        use std::io::{self, Write};
        print!("Enter Telegram bot token (from @BotFather): ");
        let _ = io::stdout().flush();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).map(|_| buf)
    })
    .await
    .map_err(|e| AgentError::Io(format!("telegram token prompt: {e}")))?
    .map_err(|e| AgentError::Io(format!("stdin: {e}")))?;
    let t = line.trim().to_string();
    if t.is_empty() {
        return Err(AgentError::Config(
            "Telegram bot token cannot be empty (set TELEGRAM_BOT_TOKEN or paste token)"
                .into(),
        ));
    }
    Ok(t)
}

async fn run_auth(cli: Cli, action: Option<String>, provider: Option<String>) -> Result<(), AgentError> {
    let provider = resolve_auth_provider(provider);
    let auth_store_path = hermes_home()
        .join("auth")
        .join("tokens.json");
    let token_store = FileTokenStore::new(auth_store_path).await?;
    let manager = AuthManager::new(token_store.clone());
    match action.as_deref().unwrap_or("status") {
        "login" => {
            if provider == "telegram" {
                let token = telegram_bot_token_from_env_or_prompt().await?;
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let tg = disk
                    .platforms
                    .entry("telegram".to_string())
                    .or_insert_with(PlatformConfig::default);
                tg.token = Some(token);
                tg.enabled = true;
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "Telegram: token saved and platform enabled in {}",
                    cfg_path.display()
                );
                return Ok(());
            }
            if provider == "copilot" || provider == "github-copilot" {
                let access_token = hermes_cli::copilot_auth::start_copilot_device_flow().await?;
                manager
                    .save_credential(OAuthCredential {
                        provider: "copilot".to_string(),
                        access_token,
                        refresh_token: None,
                        token_type: "bearer".to_string(),
                        scope: None,
                        expires_at: None,
                    })
                    .await?;
                println!("GitHub device login complete; credential saved as provider 'copilot'.");
                println!("Ensure GITHUB_COPILOT_TOKEN is set for the agent (see printed instructions above).");
                return Ok(());
            }

            let access_token = resolve_llm_login_token(&cli, &provider).await?;
            manager
                .save_credential(OAuthCredential {
                    provider: provider.clone(),
                    access_token,
                    refresh_token: None,
                    token_type: "bearer".to_string(),
                    scope: None,
                    expires_at: None,
                })
                .await?;
            let msg = hermes_cli::auth::login(&provider).await?;
            println!("{}", msg);
        }
        "logout" => {
            if provider == "telegram" {
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                if let Some(tg) = disk.platforms.get_mut("telegram") {
                    tg.token = None;
                    tg.enabled = false;
                }
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "Telegram: token cleared and platform disabled in {}",
                    cfg_path.display()
                );
                return Ok(());
            }
            let msg = hermes_cli::auth::logout(&provider).await?;
            token_store.remove(&provider).await?;
            println!("{} (removed credential for provider: {})", msg, provider);
        }
        _ => {
            if provider == "telegram" {
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let (has, en) = disk
                    .platforms
                    .get("telegram")
                    .map(|p| {
                        (
                            p.token
                                .as_deref()
                                .map(|t| !t.trim().is_empty())
                                .unwrap_or(false),
                            p.enabled,
                        )
                    })
                    .unwrap_or((false, false));
                println!(
                    "Telegram ({}): token_present={} enabled={}",
                    cfg_path.display(),
                    has,
                    en
                );
                return Ok(());
            }
            let has_token = manager.get_access_token(&provider).await?.is_some();
            println!(
                "Auth status: provider='{}', credential_present={}",
                provider, has_token
            );
        }
    }
    Ok(())
}

fn cron_cli_error(e: CronError) -> AgentError {
    AgentError::Config(e.to_string())
}

async fn run_cron(
    cli: Cli,
    action: Option<String>,
    id: Option<String>,
    schedule: Option<String>,
    prompt: Option<String>,
) -> Result<(), AgentError> {
    let data_dir = hermes_state_root(&cli).join("cron");
    std::fs::create_dir_all(&data_dir)
        .map_err(|e| AgentError::Io(format!("cron dir {}: {}", data_dir.display(), e)))?;
    let sched = cron_scheduler_for_data_dir(data_dir.clone());

    match action.as_deref().unwrap_or("list") {
        "list" => {
            let jobs = sched.list_jobs().await;
            if jobs.is_empty() {
                println!("(no cron jobs in {})", data_dir.display());
                return Ok(());
            }
            println!("Cron jobs ({}):", data_dir.display());
            for j in jobs {
                let snippet: String = j.prompt.chars().take(48).collect();
                println!(
                    "  {}  [{}]  {:?}  next_run={:?}  {}",
                    j.id, j.schedule, j.status, j.next_run, snippet
                );
            }
        }
        "create" => {
            let schedule = schedule.unwrap_or_else(|| "0 * * * *".to_string());
            let prompt = prompt.ok_or_else(|| {
                AgentError::Config("cron create: use --prompt \"...\"".into())
            })?;
            let job = hermes_cron::CronJob::new(schedule, prompt);
            let jid = sched.create_job(job).await.map_err(cron_cli_error)?;
            println!("Created cron job id={} (persisted under {})", jid, data_dir.display());
        }
        "delete" | "pause" | "resume" | "run" | "history" => {
            let act = action.as_deref().unwrap_or("cron");
            let jid = id
                .filter(|s| !s.is_empty())
                .ok_or_else(|| AgentError::Config(format!("{}: use --id <job-id>", act)))?;
            match act {
                "delete" => {
                    sched.remove_job(&jid).await.map_err(cron_cli_error)?;
                    println!("Deleted job {}", jid);
                }
                "pause" => {
                    sched.pause_job(&jid).await.map_err(cron_cli_error)?;
                    println!("Paused job {}", jid);
                }
                "resume" => {
                    sched.resume_job(&jid).await.map_err(cron_cli_error)?;
                    println!("Resumed job {}", jid);
                }
                "run" => {
                    let result = sched.run_job(&jid).await.map_err(cron_cli_error)?;
                    let json = serde_json::to_string_pretty(&result)
                        .unwrap_or_else(|_| format!("{result:#?}"));
                    println!("{}", json);
                }
                "history" => {
                    let job = sched
                        .get_job(&jid)
                        .await
                        .ok_or_else(|| AgentError::Config(format!("unknown job id: {}", jid)))?;
                    let json = serde_json::to_string_pretty(&job)
                        .map_err(|e| AgentError::Config(e.to_string()))?;
                    println!("{}", json);
                }
                _ => {
                    return Err(AgentError::Config(format!(
                        "internal: unexpected cron action '{}'",
                        act
                    )));
                }
            }
        }
        other => {
            return Err(AgentError::Config(format!(
                "Unknown cron action: {} (use list|create|delete|pause|resume|run|history)",
                other
            )));
        }
    }
    Ok(())
}

fn webhook_store_path(cli: &Cli) -> PathBuf {
    hermes_state_root(&cli).join("webhooks.json")
}

async fn prompt_line(prompt: impl Into<String>) -> Result<String, AgentError> {
    let prompt = prompt.into();
    let line = tokio::task::spawn_blocking(move || {
        use std::io::{self, Write};
        print!("{}", prompt);
        let _ = io::stdout().flush();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).map(|_| buf)
    })
    .await
    .map_err(|e| AgentError::Io(format!("stdin task: {}", e)))?
    .map_err(|e| AgentError::Io(format!("stdin: {}", e)))?;
    Ok(line.trim().to_string())
}

/// Resolve API key for `hermes auth login <provider>`: env → merged config → stdin.
async fn resolve_llm_login_token(cli: &Cli, provider: &str) -> Result<String, AgentError> {
    if let Some(k) = provider_api_key_from_env(provider) {
        return Ok(k);
    }
    let cfg = load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;
    if let Some(k) = cfg
        .llm_providers
        .get(provider)
        .and_then(|c| c.api_key.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        return Ok(k.to_string());
    }
    let fallback_var = format!("{}_API_KEY", provider.to_uppercase().replace('-', "_"));
    let msg = format!(
        "No API key in env or config for provider '{}'.\n\
         Set {} (see `hermes config set llm.{}.api_key ...`) or paste key now: ",
        provider, fallback_var, provider
    );
    let pasted = prompt_line(msg).await?;
    if pasted.is_empty() {
        return Err(AgentError::Config(format!(
            "Missing API key for provider '{}'",
            provider
        )));
    }
    Ok(pasted)
}

async fn run_webhook(
    cli: Cli,
    action: Option<String>,
    url: Option<String>,
    id: Option<String>,
) -> Result<(), AgentError> {
    let path = webhook_store_path(&cli);
    let mut store = hermes_cli::webhook_delivery::load_webhook_store(&path)?;

    match action.as_deref().unwrap_or("list") {
        "list" => {
            if store.webhooks.is_empty() {
                println!("(no webhooks in {})", path.display());
                return Ok(());
            }
            println!("Webhooks ({}):", path.display());
            for w in &store.webhooks {
                println!("  {}  {}  {}", w.id, w.url, w.created_at);
            }
        }
        "add" => {
            let url = url
                .filter(|u| !u.is_empty())
                .ok_or_else(|| AgentError::Config("webhook add: use --url https://...".into()))?;
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(AgentError::Config(
                    "webhook URL must start with http:// or https://".into(),
                ));
            }
            let rec = hermes_cli::webhook_delivery::WebhookRecord {
                id: uuid::Uuid::new_v4().to_string(),
                url: url.clone(),
                created_at: chrono::Utc::now().to_rfc3339(),
            };
            store.webhooks.push(rec.clone());
            hermes_cli::webhook_delivery::save_webhook_store(&path, &store)?;
            println!("Added webhook {} -> {}", rec.id, rec.url);
        }
        "remove" => {
            let before = store.webhooks.len();
            if let Some(rid) = id.filter(|s| !s.is_empty()) {
                store.webhooks.retain(|w| w.id != rid);
            } else if let Some(u) = url.filter(|s| !s.is_empty()) {
                store.webhooks.retain(|w| w.url != u);
            } else {
                return Err(AgentError::Config(
                    "webhook remove: use --id <id> or --url <exact-url>".into(),
                ));
            }
            if store.webhooks.len() == before {
                println!("No matching webhook removed.");
            } else {
                hermes_cli::webhook_delivery::save_webhook_store(&path, &store)?;
                println!("Updated {}", path.display());
            }
        }
        other => {
            return Err(AgentError::Config(format!(
                "Unknown webhook action: {} (use list|add|remove)",
                other
            )));
        }
    }
    Ok(())
}

/// POST each [`CronCompletionEvent`] to every URL in `webhooks.json` (same file as `hermes webhook`).
async fn run_cron_webhook_delivery_loop(
    mut rx: broadcast::Receiver<CronCompletionEvent>,
    webhooks_json: PathBuf,
) {
    use tokio::sync::broadcast::error::RecvError;

    let client = match hermes_cli::webhook_delivery::webhook_http_client() {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("cron webhooks: HTTP client build failed: {e}");
            return;
        }
    };

    loop {
        let ev = match rx.recv().await {
            Ok(ev) => ev,
            Err(RecvError::Lagged(n)) => {
                tracing::debug!(n, "cron webhook receiver lagged; skipped messages");
                continue;
            }
            Err(RecvError::Closed) => break,
        };

        if let Err(e) =
            hermes_cli::webhook_delivery::deliver_cron_completion_to_webhooks(&webhooks_json, &ev, &client).await
        {
            tracing::warn!("cron webhook delivery: {e}");
        }
    }
}

async fn run_dump(cli: Cli, session: Option<String>, output: Option<String>) -> Result<(), AgentError> {
    let home = cli
        .config_dir
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(hermes_config::hermes_home);
    let sessions_dir = home.join("sessions");
    let session = session.unwrap_or_else(|| "latest".to_string());
    let out = output.unwrap_or_else(|| format!("{}.dump.json", session));
    let payload = serde_json::json!({
        "session": session,
        "source_dir": sessions_dir,
        "note": "Session export scaffold"
    });
    std::fs::write(&out, serde_json::to_string_pretty(&payload).unwrap_or_default())
        .map_err(|e| AgentError::Io(format!("Failed to write dump: {}", e)))?;
    println!("Wrote dump to {}", out);
    Ok(())
}

fn run_completion(shell: Option<String>) -> Result<(), AgentError> {
    let mut cmd = Cli::command();
    let sh = match shell.as_deref().unwrap_or("zsh") {
        "bash" => CompletionShell::Bash,
        "fish" => CompletionShell::Fish,
        "powershell" => CompletionShell::PowerShell,
        "elvish" => CompletionShell::Elvish,
        _ => CompletionShell::Zsh,
    };
    generate(sh, &mut cmd, "hermes", &mut std::io::stdout());
    Ok(())
}

async fn run_uninstall(yes: bool) -> Result<(), AgentError> {
    let home = hermes_config::hermes_home();
    if !yes {
        println!("Uninstall is destructive. Re-run with `hermes uninstall --yes`.");
        return Ok(());
    }
    if home.exists() {
        std::fs::remove_dir_all(&home)
            .map_err(|e| AgentError::Io(format!("Failed to remove {}: {}", home.display(), e)))?;
        println!("Removed {}", home.display());
    } else {
        println!("Nothing to uninstall.");
    }
    Ok(())
}

/// Handle `hermes setup`.
async fn run_setup() -> Result<(), AgentError> {
    use std::io::{self, BufRead, Write};

    println!("Hermes Agent — Setup Wizard");
    println!("===========================\n");

    let config_dir = hermes_config::hermes_home();
    println!("Config directory: {}", config_dir.display());

    // 1. Create directory structure
    let subdirs = ["profiles", "sessions", "logs", "skills"];
    for dir in [config_dir.clone()]
        .into_iter()
        .chain(subdirs.iter().map(|d| config_dir.join(d)))
    {
        if dir.exists() {
            println!("  ✓ {} exists", dir.display());
        } else {
            std::fs::create_dir_all(&dir)
                .map_err(|e| AgentError::Io(format!("Failed to create {}: {}", dir.display(), e)))?;
            println!("  ✓ Created {}", dir.display());
        }
    }

    let config_path = config_dir.join("config.yaml");
    let stdin = io::stdin();
    let mut reader = stdin.lock();

    // 2. Prompt for API key
    print!("\nOpenAI API key (leave blank to skip): ");
    io::stdout().flush().ok();
    let mut api_key = String::new();
    reader.read_line(&mut api_key).ok();
    let api_key = api_key.trim().to_string();

    // 3. Prompt for model
    println!("\nAvailable models:");
    println!("  1) openai:gpt-4o          (recommended)");
    println!("  2) openai:gpt-4o-mini     (fast & cheap)");
    println!("  3) anthropic:claude-3-5-sonnet");
    println!("  4) openrouter:auto        (multi-provider)");
    print!("Choose model [1]: ");
    io::stdout().flush().ok();
    let mut model_choice = String::new();
    reader.read_line(&mut model_choice).ok();
    let model = match model_choice.trim() {
        "2" => "openai:gpt-4o-mini",
        "3" => "anthropic:claude-3-5-sonnet",
        "4" => "openrouter:auto",
        _ => "openai:gpt-4o",
    };

    // 4. Prompt for personality
    print!("\nPersonality (default, concise, creative, technical) [default]: ");
    io::stdout().flush().ok();
    let mut personality = String::new();
    reader.read_line(&mut personality).ok();
    let personality = personality.trim();
    let personality = if personality.is_empty() { "default" } else { personality };

    // 5. Write config.yaml
    if config_path.exists() {
        print!("\nconfig.yaml already exists. Overwrite? [y/N]: ");
        io::stdout().flush().ok();
        let mut answer = String::new();
        reader.read_line(&mut answer).ok();
        if !answer.trim().eq_ignore_ascii_case("y") {
            println!("Keeping existing config.yaml.");
            println!("\nSetup complete! Run `hermes` to start an interactive session.");
            return Ok(());
        }
    }

    let mut config_content = String::from("# Hermes Agent Configuration\n\n");
    config_content.push_str(&format!("model: {}\n", model));
    config_content.push_str(&format!("personality: {}\n", personality));
    config_content.push_str("max_turns: 50\n\n");

    if !api_key.is_empty() {
        config_content.push_str("llm_providers:\n");
        config_content.push_str("  openai:\n");
        config_content.push_str(&format!("    api_key: {}\n", api_key));
    }

    std::fs::write(&config_path, &config_content)
        .map_err(|e| AgentError::Io(format!("Failed to write config: {}", e)))?;
    println!("\n  ✓ Wrote config.yaml");

    // 6. Write default profile
    let default_profile = config_dir.join("profiles").join("default.yaml");
    if !default_profile.exists() {
        let profile_content = format!(
            "# Default Hermes Profile\nname: default\nmodel: {}\npersonality: {}\n",
            model, personality,
        );
        std::fs::write(&default_profile, profile_content)
            .map_err(|e| AgentError::Io(format!("Failed to write profile: {}", e)))?;
        println!("  ✓ Created default profile");
    }

    println!("\nSetup complete! Run `hermes` to start an interactive session.");
    println!("Run `hermes doctor` to check system requirements.");
    Ok(())
}

/// Handle `hermes doctor`.
async fn run_doctor(cli: Cli) -> Result<(), AgentError> {
    println!("Hermes Agent — System Check");
    println!("===========================\n");

    // Check config
    let config_dir = hermes_config::hermes_home();
    print!("Config directory ({})... ", config_dir.display());
    if config_dir.exists() {
        println!("✓");
    } else {
        println!("✗ (run `hermes setup`)");
    }

    // Check config.yaml
    let config_path = config_dir.join("config.yaml");
    print!("config.yaml... ");
    if config_path.exists() {
        println!("✓");
    } else {
        println!("✗ (run `hermes setup`)");
    }

    // Check API keys via environment
    let api_checks = [
        ("OPENAI_API_KEY", "OpenAI"),
        ("ANTHROPIC_API_KEY", "Anthropic"),
        ("OPENROUTER_API_KEY", "OpenRouter"),
        ("EXA_API_KEY", "Exa (web search)"),
        ("FIRECRAWL_API_KEY", "Firecrawl (web extract)"),
    ];

    println!("\nAPI Keys:");
    for (env_var, name) in &api_checks {
        print!("  {} ({})... ", name, env_var);
        if std::env::var(env_var).is_ok() {
            println!("✓");
        } else {
            println!("✗ (not set)");
        }
    }

    // Check external tools
    println!("\nExternal tools:");
    let tool_checks = [
        ("docker", "Docker"),
        ("ssh", "SSH"),
        ("git", "Git"),
    ];

    for (cmd, name) in &tool_checks {
        print!("  {}... ", name);
        match tokio::process::Command::new("which")
            .arg(cmd)
            .output()
            .await
        {
            Ok(output) if output.status.success() => println!("✓"),
            _ => println!("✗ (not found)"),
        }
    }

    // Try loading config
    println!("\nConfiguration:");
    print!("  Loading config... ");
    match load_config(cli.config_dir.as_deref()) {
        Ok(config) => {
            println!("✓");
            println!("  Model: {}", config.model.as_deref().unwrap_or("(default)"));
            println!("  Max turns: {}", config.max_turns);
            let platform_count = config.platforms.iter().filter(|(_, p)| p.enabled).count();
            println!("  Enabled platforms: {}", platform_count);
        }
        Err(e) => {
            println!("✗ ({})", e);
        }
    }

    println!("\nDone.");
    Ok(())
}

/// Handle `hermes update`.
async fn run_update() -> Result<(), AgentError> {
    println!("Hermes Agent v{}", env!("CARGO_PKG_VERSION"));
    println!("{}", hermes_cli::update::check_for_updates().await?);
    Ok(())
}

/// Handle `hermes status`.
async fn run_status(cli: Cli) -> Result<(), AgentError> {
    println!("Hermes Agent — Status");
    println!("=====================\n");

    println!("Version: {}", env!("CARGO_PKG_VERSION"));

    let config = load_config(cli.config_dir.as_deref())
        .map_err(|e| AgentError::Config(e.to_string()))?;

    println!("Model:   {}", config.model.as_deref().unwrap_or("(default: gpt-4o)"));
    println!(
        "Personality: {}",
        config.personality.as_deref().unwrap_or("(none)")
    );
    println!("Max turns: {}", config.max_turns);

    let enabled_platforms: Vec<&String> = config
        .platforms
        .iter()
        .filter(|(_, pc)| pc.enabled)
        .map(|(name, _)| name)
        .collect();
    if enabled_platforms.is_empty() {
        println!("Platforms: (none enabled)");
    } else {
        println!(
            "Platforms: {}",
            enabled_platforms
                .iter()
                .map(|s| s.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }

    let config_dir = hermes_config::hermes_home();
    println!("\nConfig dir: {}", config_dir.display());

    // Check for active sessions
    let sessions_dir = config_dir.join("sessions");
    if sessions_dir.exists() {
        let count = std::fs::read_dir(&sessions_dir)
            .map(|entries| entries.filter_map(|e| e.ok()).count())
            .unwrap_or(0);
        println!("Saved sessions: {}", count);
    }

    // Check for profiles
    let profiles_dir = config_dir.join("profiles");
    if profiles_dir.exists() {
        let profiles: Vec<String> = std::fs::read_dir(&profiles_dir)
            .map(|entries| {
                entries
                    .filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path()
                            .extension()
                            .map(|ext| ext == "yaml" || ext == "yml")
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
        if profiles.is_empty() {
            println!("Profiles: (none)");
        } else {
            println!("Profiles: {}", profiles.join(", "));
        }
    }

    Ok(())
}

/// Handle `hermes logs`.
async fn run_logs(cli: Cli, lines: u32, follow: bool) -> Result<(), AgentError> {
    let config_dir = cli
        .config_dir
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(hermes_config::hermes_home);
    let log_file = config_dir.join("logs").join("hermes.log");

    if !log_file.exists() {
        println!("No log file found at: {}", log_file.display());
        println!("Logs are written here during interactive sessions.");
        return Ok(());
    }

    if follow {
        println!("Tailing {}... (Ctrl+C to stop)\n", log_file.display());
        let mut child = tokio::process::Command::new("tail")
            .args(["-f", "-n", &lines.to_string()])
            .arg(&log_file)
            .spawn()
            .map_err(|e| AgentError::Io(format!("Failed to tail log file: {}", e)))?;

        tokio::select! {
            status = child.wait() => {
                match status {
                    Ok(s) if !s.success() => {
                        eprintln!("tail exited with status: {}", s);
                    }
                    Err(e) => {
                        eprintln!("Error waiting for tail: {}", e);
                    }
                    _ => {}
                }
            }
            _ = tokio::signal::ctrl_c() => {
                child.kill().await.ok();
                println!("\nStopped tailing logs.");
            }
        }
    } else {
        let content = std::fs::read_to_string(&log_file)
            .map_err(|e| AgentError::Io(format!("Failed to read log file: {}", e)))?;
        let all_lines: Vec<&str> = content.lines().collect();
        let start = all_lines.len().saturating_sub(lines as usize);
        for line in &all_lines[start..] {
            println!("{}", line);
        }
        println!(
            "\n(Showing last {} of {} lines from {})",
            all_lines.len() - start,
            all_lines.len(),
            log_file.display()
        );
    }
    Ok(())
}

/// Handle `hermes profile [action] [name]`.
async fn run_profile(
    cli: Cli,
    action: Option<String>,
    name: Option<String>,
) -> Result<(), AgentError> {
    let config_dir = cli
        .config_dir
        .as_deref()
        .map(std::path::PathBuf::from)
        .unwrap_or_else(hermes_config::hermes_home);
    let profiles_dir = config_dir.join("profiles");

    match action.as_deref() {
        None => {
            // Show current profile
            let config = load_config(cli.config_dir.as_deref())
                .map_err(|e| AgentError::Config(e.to_string()))?;
            println!("Current profile:");
            println!("  Model:       {}", config.model.as_deref().unwrap_or("gpt-4o"));
            println!(
                "  Personality: {}",
                config.personality.as_deref().unwrap_or("default")
            );
            println!("  Max turns:   {}", config.max_turns);
            println!(
                "\nUse `hermes profile list` to see all profiles."
            );
        }
        Some("list") => {
            if !profiles_dir.exists() {
                println!("No profiles directory found. Run `hermes setup` first.");
                return Ok(());
            }
            let entries: Vec<String> = std::fs::read_dir(&profiles_dir)
                .map(|rd| {
                    rd.filter_map(|e| e.ok())
                        .filter(|e| {
                            e.path()
                                .extension()
                                .map(|ext| ext == "yaml" || ext == "yml")
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
                println!("No profiles found. Create one with `hermes profile create <name>`.");
            } else {
                println!("Available profiles:");
                for name in &entries {
                    println!("  • {}", name);
                }
            }
        }
        Some("create") => {
            let name = name.ok_or_else(|| {
                AgentError::Config("Missing profile name. Usage: hermes profile create <name>".into())
            })?;

            std::fs::create_dir_all(&profiles_dir)
                .map_err(|e| AgentError::Io(format!("Failed to create profiles dir: {}", e)))?;

            let profile_path = profiles_dir.join(format!("{}.yaml", name));
            if profile_path.exists() {
                println!("Profile '{}' already exists at {}", name, profile_path.display());
                return Ok(());
            }

            let content = format!(
                "# Hermes Profile: {}\nname: {}\nmodel: openai:gpt-4o\npersonality: default\nmax_turns: 50\n",
                name, name
            );
            std::fs::write(&profile_path, content)
                .map_err(|e| AgentError::Io(format!("Failed to write profile: {}", e)))?;
            println!("Created profile '{}' at {}", name, profile_path.display());
            println!("Edit it to customize, then switch with `hermes profile switch {}`.", name);
        }
        Some("switch") => {
            let name = name.ok_or_else(|| {
                AgentError::Config("Missing profile name. Usage: hermes profile switch <name>".into())
            })?;

            let profile_path = profiles_dir.join(format!("{}.yaml", &name));
            if !profile_path.exists() {
                // Also try .yml
                let alt = profiles_dir.join(format!("{}.yml", &name));
                if !alt.exists() {
                    println!(
                        "Profile '{}' not found. Available profiles:",
                        name
                    );
                    if let Ok(rd) = std::fs::read_dir(&profiles_dir) {
                        for entry in rd.filter_map(|e| e.ok()) {
                            if let Some(stem) = entry.path().file_stem() {
                                println!("  • {}", stem.to_string_lossy());
                            }
                        }
                    }
                    return Ok(());
                }
            }
            println!("Switched to profile: {}", name);
            println!("(Profile loading will be applied on next `hermes` session)");
        }
        Some(other) => {
            println!(
                "Unknown profile action: '{}'. Use 'list', 'create', or 'switch'.",
                other
            );
        }
    }
    Ok(())
}
