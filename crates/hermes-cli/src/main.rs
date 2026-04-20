//! Hermes Agent — binary entry point.
//!
//! Initializes logging, parses CLI arguments, and dispatches to the
//! appropriate subcommand handler.

use clap::CommandFactory;
use clap::Parser;
use clap_complete::{generate, Shell as CompletionShell};
use hermes_agent::session_persistence::SessionPersistence;
use hermes_agent::{leading_system_prompt_for_persist, AgentLoop};
use hermes_auth::{AuthManager, FileTokenStore, OAuthCredential};
use hermes_cli::app::{
    bridge_tool_registry, build_agent_config, build_provider, provider_api_key_from_env,
};
use hermes_cli::cli::{Cli, CliCommand};
use hermes_cli::App;
use hermes_config::{
    apply_user_config_patch, gateway_pid_path_in, hermes_home, load_config, load_user_config_file,
    save_config_yaml, state_dir, user_config_field_display, validate_config, ConfigError,
    PlatformConfig,
};
use hermes_core::AgentError;
use hermes_core::PlatformAdapter;
use hermes_core::{MessageRole, StreamChunk};
use hermes_cron::{
    cron_scheduler_for_data_dir, CronCompletionEvent, CronError, CronRunner, CronScheduler,
    FileJobPersistence,
};
use hermes_environments::LocalBackend;
use hermes_gateway::gateway::GatewayConfig as RuntimeGatewayConfig;
use hermes_gateway::gateway::IncomingMessage as GatewayIncomingMessage;
use hermes_gateway::platforms::api_server::{ApiInboundRequest, ApiServerAdapter, ApiServerConfig};
use hermes_gateway::platforms::bluebubbles::{BlueBubblesAdapter, BlueBubblesConfig};
use hermes_gateway::platforms::dingtalk::{DingTalkAdapter, DingTalkConfig};
use hermes_gateway::platforms::discord::{DiscordAdapter, DiscordConfig};
use hermes_gateway::platforms::email::{EmailAdapter, EmailConfig};
use hermes_gateway::platforms::feishu::{FeishuAdapter, FeishuConfig};
use hermes_gateway::platforms::homeassistant::{HomeAssistantAdapter, HomeAssistantConfig};
use hermes_gateway::platforms::matrix::{MatrixAdapter, MatrixConfig};
use hermes_gateway::platforms::mattermost::{MattermostAdapter, MattermostConfig};
use hermes_gateway::platforms::signal::{SignalAdapter, SignalConfig};
use hermes_gateway::platforms::slack::{SlackAdapter, SlackConfig};
use hermes_gateway::platforms::sms::{SmsAdapter, SmsConfig};
use hermes_gateway::platforms::telegram::{TelegramAdapter, TelegramConfig};
use hermes_gateway::platforms::webhook::{WebhookAdapter, WebhookConfig, WebhookPayload};
use hermes_gateway::platforms::wecom::{WeComAdapter, WeComConfig};
use hermes_gateway::platforms::weixin::{WeChatAdapter, WeixinConfig};
use hermes_gateway::platforms::whatsapp::{WhatsAppAdapter, WhatsAppConfig};
use hermes_gateway::{DmManager, Gateway, GatewayRuntimeContext, SessionManager};
use hermes_skills::{FileSkillStore, SkillManager};
use hermes_telemetry::init_telemetry_from_env;
use hermes_tools::ToolRegistry;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc};
#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    // Initialize tracing
    init_tracing(cli.verbose);
    if let Err(err) = hydrate_provider_env_from_vault_for_cli(&cli).await {
        tracing::warn!("Secret-vault hydration skipped: {}", err);
    }

    tracing::debug!("Hermes Agent starting");

    let result = match cli.effective_command() {
        CliCommand::Hermes => run_interactive(cli).await,
        CliCommand::Chat {
            query,
            preload_skill,
            yolo,
        } => hermes_cli::commands::handle_cli_chat(query, preload_skill, yolo).await,
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
        CliCommand::Auth {
            action,
            provider,
            qr,
        } => run_auth(cli, action, provider, qr).await,
        CliCommand::Secrets {
            action,
            provider,
            value,
            show,
        } => run_secrets(cli, action, provider, value, show).await,
        CliCommand::Skills {
            action,
            name,
            extra,
        } => hermes_cli::commands::handle_cli_skills(action, name, extra).await,
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
        CliCommand::Memory { action } => hermes_cli::commands::handle_cli_memory(action).await,
        CliCommand::Mcp { action, server } => {
            hermes_cli::commands::handle_cli_mcp(action, server).await
        }
        CliCommand::Sessions { action, id, name } => {
            hermes_cli::commands::handle_cli_sessions(action, id, name).await
        }
        CliCommand::Insights { days, source } => {
            hermes_cli::commands::handle_cli_insights(days, source).await
        }
        CliCommand::Login { provider } => hermes_cli::commands::handle_cli_login(provider).await,
        CliCommand::Logout { provider } => hermes_cli::commands::handle_cli_logout(provider).await,
        CliCommand::Whatsapp { action } => hermes_cli::commands::handle_cli_whatsapp(action).await,
        CliCommand::Pairing { action, device_id } => {
            hermes_cli::commands::handle_cli_pairing(action, device_id).await
        }
        CliCommand::Claw { action } => hermes_cli::commands::handle_cli_claw(action).await,
        CliCommand::Acp { action } => hermes_cli::commands::handle_cli_acp(action).await,
        CliCommand::Backup { output } => hermes_cli::commands::handle_cli_backup(output).await,
        CliCommand::Import { path } => hermes_cli::commands::handle_cli_import(path).await,
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
        CliCommand::Lumio { action, model } => run_lumio(action, model).await,
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
    let config =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;

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
async fn run_tools(_cli: Cli, action: Option<String>) -> Result<(), AgentError> {
    let registry = hermes_tools::ToolRegistry::new();
    let tools = registry.list_tools();

    match action.as_deref() {
        None | Some("list") => {
            if tools.is_empty() {
                println!("No tools registered (tools are loaded at runtime).");
                println!("\nBuilt-in tool categories:");
                let categories = [
                    "web",
                    "terminal",
                    "file",
                    "browser",
                    "vision",
                    "image_gen",
                    "skills",
                    "memory",
                    "session_search",
                    "todo",
                    "clarify",
                    "code_execution",
                    "delegation",
                    "cronjob",
                    "messaging",
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
    let config =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;

    match action.as_deref() {
        None => {
            // Show full config as JSON
            let json = serde_json::to_string_pretty(&config)
                .map_err(|e| AgentError::Config(e.to_string()))?;
            println!("{}", json);
        }
        Some("get") => {
            let key = key.ok_or_else(|| {
                AgentError::Config("Missing key. Usage: hermes config get <key>".into())
            })?;
            match user_config_field_display(&config, &key) {
                Ok(s) => println!("{}", s),
                Err(ConfigError::NotFound(_)) => println!("Unknown config key: {}", key),
                Err(e) => return Err(AgentError::Config(e.to_string())),
            }
        }
        Some("set") => {
            let key = key.ok_or_else(|| {
                AgentError::Config("Missing key. Usage: hermes config set <key> <value>".into())
            })?;
            let value = value.ok_or_else(|| {
                AgentError::Config("Missing value. Usage: hermes config set <key> <value>".into())
            })?;
            let base: PathBuf = cli
                .config_dir
                .as_ref()
                .map(PathBuf::from)
                .unwrap_or_else(hermes_home);
            let cfg_path = base.join("config.yaml");
            let mut disk =
                load_user_config_file(&cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;
            apply_user_config_patch(&mut disk, &key, &value)
                .map_err(|e| AgentError::Config(e.to_string()))?;
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
            let status = std::process::Command::new(&editor).arg(&cfg_path).status();
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
    let config =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;

    match action.as_deref() {
        Some("setup") => {
            run_gateway_setup(&cli).await?;
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
                    enabled
                        .iter()
                        .map(|s| s.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
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
                        let effective_model = resolve_model_for_gateway(
                            config.model.as_deref().unwrap_or("gpt-4o"),
                            &ctx,
                        );
                        let agent =
                            build_agent_for_gateway_context(config.as_ref(), &ctx, agent_tools);
                        let result = agent
                            .run(messages, None)
                            .await
                            .map_err(|e| hermes_gateway::GatewayError::Platform(e.to_string()))?;
                        let home = ctx
                            .home
                            .as_deref()
                            .or(config.home_dir.as_deref())
                            .map(str::trim)
                            .filter(|s| !s.is_empty());
                        if let Some(h) = home {
                            if !ctx.session_key.trim().is_empty() {
                                let sp = SessionPersistence::new(Path::new(h));
                                let sys = leading_system_prompt_for_persist(&result.messages);
                                let _ = sp.persist_session(
                                    &ctx.session_key,
                                    &result.messages,
                                    Some(&effective_model),
                                    Some(ctx.platform.as_str()),
                                    None,
                                    sys.as_deref(),
                                );
                            }
                        }
                        Ok(extract_last_assistant_reply(&result.messages))
                    })
                }))
                .await;
            gateway
                .set_streaming_handler_with_context(Arc::new(move |messages, ctx, on_chunk| {
                    let config = config_arc_stream.clone();
                    let agent_tools = agent_tools_for_stream.clone();
                    Box::pin(async move {
                        let effective_model = resolve_model_for_gateway(
                            config.model.as_deref().unwrap_or("gpt-4o"),
                            &ctx,
                        );
                        let agent =
                            build_agent_for_gateway_context(config.as_ref(), &ctx, agent_tools);
                        let emit = on_chunk.clone();
                        let ui_state = Arc::new(Mutex::new((false, false))); // (muted, needs_break)
                        let ui_state_cb = ui_state.clone();
                        let stream_cb: Box<dyn Fn(StreamChunk) + Send + Sync> =
                            Box::new(move |chunk: StreamChunk| {
                                if let Some(delta) = chunk.delta {
                                    if let Some(extra) = delta.extra.as_ref() {
                                        if let Some(control) =
                                            extra.get("control").and_then(|v| v.as_str())
                                        {
                                            if control == "mute_post_response" {
                                                let enabled = extra
                                                    .get("enabled")
                                                    .and_then(|v| v.as_bool())
                                                    .unwrap_or(false);
                                                if let Ok(mut st) = ui_state_cb.lock() {
                                                    st.0 = enabled;
                                                }
                                            } else if control == "stream_break" {
                                                if let Ok(mut st) = ui_state_cb.lock() {
                                                    st.1 = true;
                                                }
                                            }
                                        }
                                    }
                                    if let Some(text) = delta.content {
                                        if let Ok(mut st) = ui_state_cb.lock() {
                                            if st.0 {
                                                return;
                                            }
                                            if st.1 {
                                                emit("\n\n".to_string());
                                                st.1 = false;
                                            }
                                        }
                                        emit(text);
                                    }
                                }
                            });

                        let result = agent
                            .run_stream(messages, None, Some(stream_cb))
                            .await
                            .map_err(|e| hermes_gateway::GatewayError::Platform(e.to_string()))?;
                        let home = ctx
                            .home
                            .as_deref()
                            .or(config.home_dir.as_deref())
                            .map(str::trim)
                            .filter(|s| !s.is_empty());
                        if let Some(h) = home {
                            if !ctx.session_key.trim().is_empty() {
                                let sp = SessionPersistence::new(Path::new(h));
                                let sys = leading_system_prompt_for_persist(&result.messages);
                                let _ = sp.persist_session(
                                    &ctx.session_key,
                                    &result.messages,
                                    Some(&effective_model),
                                    Some(ctx.platform.as_str()),
                                    None,
                                    sys.as_deref(),
                                );
                            }
                        }
                        Ok(extract_last_assistant_reply(&result.messages))
                    })
                }))
                .await;

            // Cron: same on-disk dir as `hermes cron` + real LLM/tools as the gateway agent.
            let cron_dir = hermes_state_root(&cli).join("cron");
            std::fs::create_dir_all(&cron_dir)
                .map_err(|e| AgentError::Io(format!("cron dir {}: {}", cron_dir.display(), e)))?;
            let default_model = config.model.clone().unwrap_or_else(|| "gpt-4o".to_string());
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
                    if let Some(token) = platform_cfg.token.clone().filter(|t| !t.trim().is_empty())
                    {
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
                        println!(
                            "Telegram is enabled but token is missing; skipping telegram adapter.\n  Fix: run `hermes auth login telegram` or set `platforms.telegram.token` in config.yaml."
                        );
                    }
                }
            }
            if let Some(platform_cfg) = config.platforms.get("weixin") {
                if platform_cfg.enabled {
                    let account_id_missing = platform_cfg
                        .extra
                        .get("account_id")
                        .and_then(|v| v.as_str())
                        .map(str::trim)
                        .map(|s| s.is_empty())
                        .unwrap_or(true);
                    let token_missing = platform_cfg
                        .token
                        .as_deref()
                        .map(str::trim)
                        .map(|s| s.is_empty())
                        .unwrap_or(true)
                        && platform_cfg
                            .extra
                            .get("token")
                            .and_then(|v| v.as_str())
                            .map(str::trim)
                            .map(|s| s.is_empty())
                            .unwrap_or(true);
                    if account_id_missing {
                        println!(
                            "Weixin is enabled but account_id is missing; skipping weixin adapter.\n  Fix: run `hermes auth login weixin --qr` (recommended) or set `platforms.weixin.extra.account_id`."
                        );
                    } else if token_missing {
                        println!(
                            "Weixin is enabled but token is missing; skipping weixin adapter.\n  Fix: run `hermes auth login weixin --qr` or set `platforms.weixin.token`."
                        );
                    } else {
                        let wx_cfg = WeixinConfig::from_platform_config(platform_cfg);
                        match WeChatAdapter::new(wx_cfg) {
                            Ok(adapter) => {
                                gateway.register_adapter("weixin", Arc::new(adapter)).await;
                            }
                            Err(e) => {
                                println!(
                                    "Weixin is enabled but failed to initialize: {}\n  Hint: rerun `hermes auth login weixin --qr` and check account file under ~/.hermes/weixin/accounts/.",
                                    e
                                );
                            }
                        }
                    }
                }
            }

            if let Some(platform_cfg) = config.platforms.get("api-server") {
                if platform_cfg.enabled {
                    let api_config = build_api_server_config(platform_cfg);
                    let api_adapter = Arc::new(ApiServerAdapter::new(api_config.clone()));
                    let (api_tx, api_rx) = mpsc::channel::<ApiInboundRequest>(256);
                    api_adapter.set_inbound_sender(api_tx).await;
                    gateway
                        .register_adapter("api-server", api_adapter.clone())
                        .await;
                    let gw_clone = gateway.clone();
                    sidecar_tasks.push(tokio::spawn(async move {
                        run_api_server_inbound_loop(gw_clone, api_rx).await;
                    }));
                    println!(
                        "API server adapter enabled on {}:{}",
                        api_config.host, api_config.port
                    );
                }
            }

            if let Some(platform_cfg) = config.platforms.get("webhook") {
                if platform_cfg.enabled {
                    let secret = platform_cfg
                        .token
                        .clone()
                        .filter(|s| !s.trim().is_empty())
                        .or_else(|| platform_extra_string(platform_cfg, "secret"));
                    if let Some(secret) = secret {
                        let webhook_config = build_webhook_config(platform_cfg, secret);
                        let webhook_adapter = Arc::new(WebhookAdapter::new(webhook_config.clone()));
                        let (webhook_tx, webhook_rx) = mpsc::channel::<WebhookPayload>(512);
                        webhook_adapter.set_inbound_sender(webhook_tx).await;
                        gateway
                            .register_adapter("webhook", webhook_adapter.clone())
                            .await;
                        let gw_clone = gateway.clone();
                        sidecar_tasks.push(tokio::spawn(async move {
                            run_webhook_inbound_loop(gw_clone, webhook_rx).await;
                        }));
                    } else {
                        println!(
                            "Webhook is enabled but secret/token is missing; skipping webhook adapter."
                        );
                    }
                }
            }

            if let Some(platform_cfg) = config.platforms.get("dingtalk") {
                if platform_cfg.enabled {
                    let dingtalk_config = DingTalkConfig::from_platform_config(platform_cfg);
                    match DingTalkAdapter::new(dingtalk_config.clone()) {
                        Ok(adapter) => {
                            let adapter = Arc::new(adapter);
                            let (tx, rx) = mpsc::channel::<GatewayIncomingMessage>(512);
                            adapter.set_inbound_sender(tx).await;
                            gateway.register_adapter("dingtalk", adapter.clone()).await;
                            let gw_clone = gateway.clone();
                            sidecar_tasks.push(tokio::spawn(async move {
                                run_gateway_incoming_loop(gw_clone, rx, "dingtalk").await;
                            }));
                        }
                        Err(err) => {
                            println!(
                                "DingTalk is enabled but config is invalid ({err}); skipping."
                            );
                        }
                    }
                }
            }

            if let Some(platform_cfg) = config.platforms.get("weixin") {
                if platform_cfg.enabled {
                    let weixin_config = WeixinConfig::from_platform_config(platform_cfg);
                    match WeChatAdapter::new(weixin_config.clone()) {
                        Ok(adapter) => {
                            let adapter = Arc::new(adapter);
                            let (tx, rx) = mpsc::channel::<GatewayIncomingMessage>(512);
                            adapter.set_inbound_sender(tx).await;
                            gateway.register_adapter("weixin", adapter.clone()).await;
                            let gw_clone = gateway.clone();
                            sidecar_tasks.push(tokio::spawn(async move {
                                run_gateway_incoming_loop(gw_clone, rx, "weixin").await;
                            }));
                        }
                        Err(err) => {
                            println!("Weixin is enabled but config is invalid ({err}); skipping.");
                        }
                    }
                }
            }

            if let Some(platform_cfg) = config.platforms.get("discord") {
                if platform_cfg.enabled {
                    if let Some(token) = platform_cfg.token.clone().filter(|t| !t.trim().is_empty())
                    {
                        let cfg = DiscordConfig {
                            token,
                            application_id: platform_extra_string(platform_cfg, "application_id"),
                            proxy: Default::default(),
                            require_mention: platform_cfg.require_mention.unwrap_or(false),
                            intents: platform_extra_u64(platform_cfg, "intents")
                                .unwrap_or((1 << 0) | (1 << 9) | (1 << 15)),
                        };
                        match DiscordAdapter::new(cfg) {
                            Ok(adapter) => {
                                gateway
                                    .register_adapter("discord", Arc::new(adapter))
                                    .await;
                            }
                            Err(err) => println!(
                                "Discord is enabled but could not initialize adapter ({err}); skipping."
                            ),
                        }
                    } else {
                        println!(
                            "Discord is enabled but token is missing; skipping discord adapter."
                        );
                    }
                }
            }

            if let Some(platform_cfg) = config.platforms.get("slack") {
                if platform_cfg.enabled {
                    if let Some(token) = platform_cfg.token.clone().filter(|t| !t.trim().is_empty())
                    {
                        let cfg = SlackConfig {
                            token,
                            app_token: platform_extra_string(platform_cfg, "app_token"),
                            socket_mode: platform_extra_bool(platform_cfg, "socket_mode")
                                .unwrap_or(false),
                            proxy: Default::default(),
                        };
                        match SlackAdapter::new(cfg) {
                            Ok(adapter) => {
                                gateway.register_adapter("slack", Arc::new(adapter)).await;
                            }
                            Err(err) => println!(
                                "Slack is enabled but could not initialize adapter ({err}); skipping."
                            ),
                        }
                    } else {
                        println!("Slack is enabled but token is missing; skipping slack adapter.");
                    }
                }
            }

            if let Some(platform_cfg) = config.platforms.get("whatsapp") {
                if platform_cfg.enabled {
                    if let Some(token) = platform_cfg.token.clone().filter(|t| !t.trim().is_empty())
                    {
                        let cfg = WhatsAppConfig {
                            token,
                            phone_number_id: platform_extra_string(platform_cfg, "phone_number_id"),
                            business_account_id: platform_extra_string(
                                platform_cfg,
                                "business_account_id",
                            ),
                            verify_token: platform_extra_string(platform_cfg, "verify_token"),
                            proxy: Default::default(),
                        };
                        match WhatsAppAdapter::new(cfg) {
                            Ok(adapter) => {
                                gateway
                                    .register_adapter("whatsapp", Arc::new(adapter))
                                    .await;
                            }
                            Err(err) => println!(
                                "WhatsApp is enabled but could not initialize adapter ({err}); skipping."
                            ),
                        }
                    } else {
                        println!(
                            "WhatsApp is enabled but token is missing; skipping whatsapp adapter."
                        );
                    }
                }
            }

            if let Some(platform_cfg) = config.platforms.get("signal") {
                if platform_cfg.enabled {
                    let phone_number = platform_extra_string(platform_cfg, "phone_number")
                        .or_else(|| platform_cfg.token.clone())
                        .unwrap_or_default();
                    if phone_number.trim().is_empty() {
                        println!(
                            "Signal is enabled but phone_number/token is missing; skipping signal adapter."
                        );
                    } else {
                        let cfg = SignalConfig {
                            phone_number,
                            api_url: platform_extra_string(platform_cfg, "api_url")
                                .unwrap_or_else(|| "http://localhost:8080".to_string()),
                            proxy: Default::default(),
                        };
                        match SignalAdapter::new(cfg) {
                            Ok(adapter) => {
                                gateway.register_adapter("signal", Arc::new(adapter)).await;
                            }
                            Err(err) => println!(
                                "Signal is enabled but could not initialize adapter ({err}); skipping."
                            ),
                        }
                    }
                }
            }

            if let Some(platform_cfg) = config.platforms.get("matrix") {
                if platform_cfg.enabled {
                    let homeserver_url =
                        platform_extra_string(platform_cfg, "homeserver_url").unwrap_or_default();
                    let user_id =
                        platform_extra_string(platform_cfg, "user_id").unwrap_or_default();
                    let access_token = platform_cfg.token.clone().unwrap_or_default();
                    if homeserver_url.trim().is_empty()
                        || user_id.trim().is_empty()
                        || access_token.trim().is_empty()
                    {
                        println!(
                            "Matrix is enabled but homeserver_url/user_id/token is missing; skipping matrix adapter."
                        );
                    } else {
                        let cfg = MatrixConfig {
                            homeserver_url,
                            user_id,
                            access_token,
                            room_id: platform_extra_string(platform_cfg, "room_id"),
                            proxy: Default::default(),
                        };
                        match MatrixAdapter::new(cfg) {
                            Ok(adapter) => {
                                gateway.register_adapter("matrix", Arc::new(adapter)).await;
                            }
                            Err(err) => println!(
                                "Matrix is enabled but could not initialize adapter ({err}); skipping."
                            ),
                        }
                    }
                }
            }

            if let Some(platform_cfg) = config.platforms.get("mattermost") {
                if platform_cfg.enabled {
                    let server_url =
                        platform_extra_string(platform_cfg, "server_url").unwrap_or_default();
                    let token = platform_cfg.token.clone().unwrap_or_default();
                    if server_url.trim().is_empty() || token.trim().is_empty() {
                        println!(
                            "Mattermost is enabled but server_url/token is missing; skipping mattermost adapter."
                        );
                    } else {
                        let cfg = MattermostConfig {
                            server_url,
                            token,
                            team_id: platform_extra_string(platform_cfg, "team_id"),
                            proxy: Default::default(),
                        };
                        match MattermostAdapter::new(cfg) {
                            Ok(adapter) => {
                                gateway
                                    .register_adapter("mattermost", Arc::new(adapter))
                                    .await;
                            }
                            Err(err) => println!(
                                "Mattermost is enabled but could not initialize adapter ({err}); skipping."
                            ),
                        }
                    }
                }
            }

            if let Some(platform_cfg) = config.platforms.get("feishu") {
                if platform_cfg.enabled {
                    let app_id = platform_extra_string(platform_cfg, "app_id").unwrap_or_default();
                    let app_secret =
                        platform_extra_string(platform_cfg, "app_secret").unwrap_or_default();
                    if app_id.trim().is_empty() || app_secret.trim().is_empty() {
                        println!(
                            "Feishu is enabled but app_id/app_secret is missing; skipping feishu adapter."
                        );
                    } else {
                        let cfg = FeishuConfig {
                            app_id,
                            app_secret,
                            verification_token: platform_extra_string(
                                platform_cfg,
                                "verification_token",
                            ),
                            encrypt_key: platform_extra_string(platform_cfg, "encrypt_key"),
                            proxy: Default::default(),
                        };
                        match FeishuAdapter::new(cfg) {
                            Ok(adapter) => {
                                gateway.register_adapter("feishu", Arc::new(adapter)).await;
                            }
                            Err(err) => println!(
                                "Feishu is enabled but could not initialize adapter ({err}); skipping."
                            ),
                        }
                    }
                }
            }

            if let Some(platform_cfg) = config.platforms.get("wecom") {
                if platform_cfg.enabled {
                    let corp_id =
                        platform_extra_string(platform_cfg, "corp_id").unwrap_or_default();
                    let agent_id =
                        platform_extra_string(platform_cfg, "agent_id").unwrap_or_default();
                    let secret = platform_extra_string(platform_cfg, "secret").unwrap_or_default();
                    if corp_id.trim().is_empty()
                        || agent_id.trim().is_empty()
                        || secret.trim().is_empty()
                    {
                        println!(
                            "WeCom is enabled but corp_id/agent_id/secret is missing; skipping wecom adapter."
                        );
                    } else {
                        let cfg = WeComConfig {
                            corp_id,
                            agent_id,
                            secret,
                            proxy: Default::default(),
                        };
                        match WeComAdapter::new(cfg) {
                            Ok(adapter) => {
                                gateway.register_adapter("wecom", Arc::new(adapter)).await;
                            }
                            Err(err) => println!(
                                "WeCom is enabled but could not initialize adapter ({err}); skipping."
                            ),
                        }
                    }
                }
            }

            if let Some(platform_cfg) = config.platforms.get("bluebubbles") {
                if platform_cfg.enabled {
                    let server_url =
                        platform_extra_string(platform_cfg, "server_url").unwrap_or_default();
                    let password = platform_cfg
                        .token
                        .clone()
                        .filter(|s| !s.trim().is_empty())
                        .or_else(|| platform_extra_string(platform_cfg, "password"))
                        .unwrap_or_default();
                    if server_url.trim().is_empty() || password.trim().is_empty() {
                        println!(
                            "BlueBubbles is enabled but server_url/password is missing; skipping bluebubbles adapter."
                        );
                    } else {
                        let cfg = BlueBubblesConfig {
                            server_url,
                            password,
                            proxy: Default::default(),
                        };
                        match BlueBubblesAdapter::new(cfg) {
                            Ok(adapter) => {
                                gateway
                                    .register_adapter("bluebubbles", Arc::new(adapter))
                                    .await;
                            }
                            Err(err) => println!(
                                "BlueBubbles is enabled but could not initialize adapter ({err}); skipping."
                            ),
                        }
                    }
                }
            }

            if let Some(platform_cfg) = config.platforms.get("email") {
                if platform_cfg.enabled {
                    let imap_host =
                        platform_extra_string(platform_cfg, "imap_host").unwrap_or_default();
                    let smtp_host =
                        platform_extra_string(platform_cfg, "smtp_host").unwrap_or_default();
                    let username =
                        platform_extra_string(platform_cfg, "username").unwrap_or_default();
                    let password = platform_cfg
                        .token
                        .clone()
                        .filter(|s| !s.trim().is_empty())
                        .or_else(|| platform_extra_string(platform_cfg, "password"))
                        .unwrap_or_default();
                    if imap_host.trim().is_empty()
                        || smtp_host.trim().is_empty()
                        || username.trim().is_empty()
                        || password.trim().is_empty()
                    {
                        println!(
                            "Email is enabled but imap_host/smtp_host/username/password is missing; skipping email adapter."
                        );
                    } else {
                        let cfg = EmailConfig {
                            imap_host,
                            imap_port: platform_extra_u16(platform_cfg, "imap_port").unwrap_or(993),
                            smtp_host,
                            smtp_port: platform_extra_u16(platform_cfg, "smtp_port").unwrap_or(587),
                            username,
                            password,
                            poll_interval_secs: platform_extra_u64(
                                platform_cfg,
                                "poll_interval_secs",
                            )
                            .unwrap_or(60),
                            proxy: Default::default(),
                        };
                        match EmailAdapter::new(cfg) {
                            Ok(adapter) => {
                                gateway.register_adapter("email", Arc::new(adapter)).await;
                            }
                            Err(err) => println!(
                                "Email is enabled but could not initialize adapter ({err}); skipping."
                            ),
                        }
                    }
                }
            }

            if let Some(platform_cfg) = config.platforms.get("sms") {
                if platform_cfg.enabled {
                    let account_sid =
                        platform_extra_string(platform_cfg, "account_sid").unwrap_or_default();
                    let auth_token = platform_cfg
                        .token
                        .clone()
                        .filter(|s| !s.trim().is_empty())
                        .or_else(|| platform_extra_string(platform_cfg, "auth_token"))
                        .unwrap_or_default();
                    let from_number =
                        platform_extra_string(platform_cfg, "from_number").unwrap_or_default();
                    if account_sid.trim().is_empty()
                        || auth_token.trim().is_empty()
                        || from_number.trim().is_empty()
                    {
                        println!(
                            "SMS is enabled but account_sid/auth_token/from_number is missing; skipping sms adapter."
                        );
                    } else {
                        let cfg = SmsConfig {
                            provider: platform_extra_string(platform_cfg, "provider")
                                .unwrap_or_else(|| "twilio".to_string()),
                            account_sid,
                            auth_token,
                            from_number,
                            proxy: Default::default(),
                        };
                        match SmsAdapter::new(cfg) {
                            Ok(adapter) => {
                                gateway.register_adapter("sms", Arc::new(adapter)).await;
                            }
                            Err(err) => println!(
                                "SMS is enabled but could not initialize adapter ({err}); skipping."
                            ),
                        }
                    }
                }
            }

            if let Some(platform_cfg) = config.platforms.get("homeassistant") {
                if platform_cfg.enabled {
                    let base_url =
                        platform_extra_string(platform_cfg, "base_url").unwrap_or_default();
                    let long_lived_token = platform_cfg
                        .token
                        .clone()
                        .filter(|s| !s.trim().is_empty())
                        .or_else(|| platform_extra_string(platform_cfg, "long_lived_token"))
                        .unwrap_or_default();
                    if base_url.trim().is_empty() || long_lived_token.trim().is_empty() {
                        println!(
                            "HomeAssistant is enabled but base_url/long_lived_token is missing; skipping homeassistant adapter."
                        );
                    } else {
                        let cfg = HomeAssistantConfig {
                            base_url,
                            long_lived_token,
                            webhook_id: platform_extra_string(platform_cfg, "webhook_id"),
                            proxy: Default::default(),
                        };
                        match HomeAssistantAdapter::new(cfg) {
                            Ok(adapter) => {
                                gateway
                                    .register_adapter("homeassistant", Arc::new(adapter))
                                    .await;
                            }
                            Err(err) => println!(
                                "HomeAssistant is enabled but could not initialize adapter ({err}); skipping."
                            ),
                        }
                    }
                }
            }

            if gateway.adapter_names().await.is_empty() {
                println!(
                    "No platform adapters started (e.g. missing required platform config). Cron + webhook delivery still active."
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
                        println!("Gateway status: invalid PID file at {}", pid_path.display());
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
            println!(
                "Unknown gateway action: {}. Use 'start', 'stop', or 'status'.",
                other
            );
        }
    }
    Ok(())
}

async fn prompt_yes_no(question: &str, default_yes: bool) -> Result<bool, AgentError> {
    let hint = if default_yes { "[Y/n]" } else { "[y/N]" };
    let ans = prompt_line(format!("{question} {hint}: ")).await?;
    if ans.trim().is_empty() {
        return Ok(default_yes);
    }
    let v = ans.trim().to_ascii_lowercase();
    Ok(matches!(v.as_str(), "y" | "yes" | "1" | "true" | "on"))
}

fn parse_csv_list(raw: &str) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

async fn run_gateway_setup(cli: &Cli) -> Result<(), AgentError> {
    println!("Gateway setup wizard");
    println!("--------------------");
    println!("This wizard configures chat platforms in config.yaml.");

    let setup_weixin = prompt_yes_no("Configure Weixin / WeChat now?", true).await?;
    let setup_telegram = prompt_yes_no("Configure Telegram now?", false).await?;
    if !setup_weixin && !setup_telegram {
        println!("No platform selected. You can rerun with `hermes gateway setup`.");
        return Ok(());
    }

    if setup_weixin {
        run_auth(
            cli.clone(),
            Some("login".to_string()),
            Some("weixin".to_string()),
            true,
        )
        .await?;
    } else {
        println!("Skipped Weixin setup.");
    }

    if setup_telegram {
        run_auth(
            cli.clone(),
            Some("login".to_string()),
            Some("telegram".to_string()),
            false,
        )
        .await?;
    } else {
        println!("Skipped Telegram setup.");
    }

    let cfg_path = hermes_state_root(cli).join("config.yaml");
    let mut disk =
        load_user_config_file(&cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;
    if setup_weixin {
        let wx = disk
            .platforms
            .entry("weixin".to_string())
            .or_insert_with(PlatformConfig::default);
        wx.enabled = true;

        println!();
        println!("Direct message policy:");
        println!("  1) pairing (recommended)");
        println!("  2) open");
        println!("  3) allowlist");
        println!("  4) disabled");
        let dm_choice = prompt_line("Choose [1-4] (default 1): ").await?;
        match dm_choice.trim() {
            "2" => {
                wx.extra.insert(
                    "dm_policy".to_string(),
                    serde_json::Value::String("open".to_string()),
                );
                wx.extra.insert(
                    "allow_from".to_string(),
                    serde_json::Value::Array(Vec::new()),
                );
            }
            "3" => {
                let current = wx
                    .extra
                    .get("allow_from")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_default();
                let line = prompt_line(format!(
                    "Allowed Weixin user IDs (comma-separated, current: {}): ",
                    if current.is_empty() {
                        "(none)"
                    } else {
                        &current
                    }
                ))
                .await?;
                let ids = parse_csv_list(&line);
                wx.extra.insert(
                    "dm_policy".to_string(),
                    serde_json::Value::String("allowlist".to_string()),
                );
                wx.extra.insert(
                    "allow_from".to_string(),
                    serde_json::Value::Array(
                        ids.into_iter().map(serde_json::Value::String).collect(),
                    ),
                );
            }
            "4" => {
                wx.extra.insert(
                    "dm_policy".to_string(),
                    serde_json::Value::String("disabled".to_string()),
                );
                wx.extra.insert(
                    "allow_from".to_string(),
                    serde_json::Value::Array(Vec::new()),
                );
            }
            _ => {
                wx.extra.insert(
                    "dm_policy".to_string(),
                    serde_json::Value::String("pairing".to_string()),
                );
                wx.extra.insert(
                    "allow_from".to_string(),
                    serde_json::Value::Array(Vec::new()),
                );
            }
        }

        println!();
        println!("Group message policy:");
        println!("  1) disabled (recommended)");
        println!("  2) open");
        println!("  3) allowlist");
        let group_choice = prompt_line("Choose [1-3] (default 1): ").await?;
        match group_choice.trim() {
            "2" => {
                wx.extra.insert(
                    "group_policy".to_string(),
                    serde_json::Value::String("open".to_string()),
                );
                wx.extra.insert(
                    "group_allow_from".to_string(),
                    serde_json::Value::Array(Vec::new()),
                );
            }
            "3" => {
                let current = wx
                    .extra
                    .get("group_allow_from")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|v| v.as_str())
                            .collect::<Vec<_>>()
                            .join(",")
                    })
                    .unwrap_or_default();
                let line = prompt_line(format!(
                    "Allowed Weixin group IDs (comma-separated, current: {}): ",
                    if current.is_empty() {
                        "(none)"
                    } else {
                        &current
                    }
                ))
                .await?;
                let ids = parse_csv_list(&line);
                wx.extra.insert(
                    "group_policy".to_string(),
                    serde_json::Value::String("allowlist".to_string()),
                );
                wx.extra.insert(
                    "group_allow_from".to_string(),
                    serde_json::Value::Array(
                        ids.into_iter().map(serde_json::Value::String).collect(),
                    ),
                );
            }
            _ => {
                wx.extra.insert(
                    "group_policy".to_string(),
                    serde_json::Value::String("disabled".to_string()),
                );
                wx.extra.insert(
                    "group_allow_from".to_string(),
                    serde_json::Value::Array(Vec::new()),
                );
            }
        }

        let home_default = wx.home_channel.clone().unwrap_or_default();
        let home_input = prompt_line(format!(
            "Weixin home channel (optional, current: {}): ",
            if home_default.is_empty() {
                "(none)"
            } else {
                &home_default
            }
        ))
        .await?;
        if !home_input.trim().is_empty() {
            wx.home_channel = Some(home_input.trim().to_string());
        }
    }

    if setup_telegram {
        let tg = disk
            .platforms
            .entry("telegram".to_string())
            .or_insert_with(PlatformConfig::default);
        tg.enabled = true;

        let use_polling = prompt_yes_no("Telegram use polling mode?", true).await?;
        tg.extra
            .insert("polling".to_string(), serde_json::Value::Bool(use_polling));
        if use_polling {
            tg.webhook_url = None;
        } else {
            let webhook_default = tg.webhook_url.clone().unwrap_or_default();
            let webhook_input = prompt_line(format!(
                "Telegram webhook URL (current: {}): ",
                if webhook_default.is_empty() {
                    "(none)"
                } else {
                    &webhook_default
                }
            ))
            .await?;
            if !webhook_input.trim().is_empty() {
                tg.webhook_url = Some(webhook_input.trim().to_string());
            }
        }

        let tg_home_default = tg.home_channel.clone().unwrap_or_default();
        let tg_home = prompt_line(format!(
            "Telegram home channel (optional, current: {}): ",
            if tg_home_default.is_empty() {
                "(none)"
            } else {
                &tg_home_default
            }
        ))
        .await?;
        if !tg_home.trim().is_empty() {
            tg.home_channel = Some(tg_home.trim().to_string());
        }
    }

    validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
    save_config_yaml(&cfg_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;

    println!();
    println!("Gateway setup complete.");
    println!("Config saved: {}", cfg_path.display());
    println!("Next step: `hermes gateway start`");
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
    let effective_model =
        resolve_model_for_gateway(config.model.as_deref().unwrap_or("gpt-4o"), ctx);
    let provider = build_provider(config, &effective_model);
    let mut agent_config = build_agent_config(config, &effective_model);
    if let Some(personality) = ctx.personality.clone() {
        agent_config.personality = Some(personality);
    }
    if !ctx.platform.trim().is_empty() {
        agent_config.platform = Some(ctx.platform.clone());
    }
    if let Some(provider) = ctx.provider.clone() {
        if !provider.trim().is_empty() {
            agent_config.provider = Some(provider);
        }
    }
    if !ctx.session_key.trim().is_empty() {
        agent_config.session_id = Some(ctx.session_key.clone());
    }
    let home = ctx
        .home
        .as_deref()
        .or(config.home_dir.as_deref())
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if let Some(h) = home {
        let _ = AgentLoop::hydrate_stored_system_prompt_from_hermes_home(
            &mut agent_config,
            Path::new(h),
        );
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

fn build_api_server_config(
    platform_cfg: &hermes_config::platform::PlatformConfig,
) -> ApiServerConfig {
    ApiServerConfig {
        host: platform_extra_string(platform_cfg, "host").unwrap_or_else(|| "0.0.0.0".to_string()),
        port: platform_extra_u16(platform_cfg, "port").unwrap_or(8090),
        auth_token: platform_cfg
            .token
            .clone()
            .filter(|s| !s.trim().is_empty())
            .or_else(|| platform_extra_string(platform_cfg, "auth_token")),
    }
}

fn build_webhook_config(
    platform_cfg: &hermes_config::platform::PlatformConfig,
    secret: String,
) -> WebhookConfig {
    WebhookConfig {
        port: platform_extra_u16(platform_cfg, "port").unwrap_or(9000),
        path: platform_extra_string(platform_cfg, "path").unwrap_or_else(|| "/webhook".to_string()),
        secret,
    }
}

fn platform_extra_string(
    platform_cfg: &hermes_config::platform::PlatformConfig,
    key: &str,
) -> Option<String> {
    platform_cfg
        .extra
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn platform_extra_u16(
    platform_cfg: &hermes_config::platform::PlatformConfig,
    key: &str,
) -> Option<u16> {
    platform_cfg
        .extra
        .get(key)
        .and_then(|v| v.as_u64())
        .and_then(|v| u16::try_from(v).ok())
}

fn platform_extra_u64(
    platform_cfg: &hermes_config::platform::PlatformConfig,
    key: &str,
) -> Option<u64> {
    platform_cfg.extra.get(key).and_then(|v| v.as_u64())
}

fn platform_extra_bool(
    platform_cfg: &hermes_config::platform::PlatformConfig,
    key: &str,
) -> Option<bool> {
    platform_cfg.extra.get(key).and_then(|v| v.as_bool())
}

async fn run_api_server_inbound_loop(
    gateway: Arc<Gateway>,
    mut rx: mpsc::Receiver<ApiInboundRequest>,
) {
    while let Some(req) = rx.recv().await {
        gateway
            .merge_request_runtime_overrides(
                "api-server",
                &req.session_id,
                &req.user_id,
                req.model.clone(),
                req.provider.clone(),
                req.personality.clone(),
            )
            .await;
        let incoming = GatewayIncomingMessage {
            platform: "api-server".to_string(),
            chat_id: req.session_id.clone(),
            user_id: req.user_id.clone(),
            text: req.prompt.clone(),
            message_id: Some(req.request_id.clone()),
            is_dm: true,
        };
        if let Err(err) = gateway.route_message(&incoming).await {
            tracing::warn!("Failed to route api-server message: {}", err);
        }
    }
}

async fn run_webhook_inbound_loop(gateway: Arc<Gateway>, mut rx: mpsc::Receiver<WebhookPayload>) {
    while let Some(payload) = rx.recv().await {
        let incoming = GatewayIncomingMessage {
            platform: "webhook".to_string(),
            chat_id: payload.chat_id,
            user_id: payload
                .user_id
                .unwrap_or_else(|| "webhook-client".to_string()),
            text: payload.text,
            message_id: None,
            is_dm: true,
        };
        if let Err(err) = gateway.route_message(&incoming).await {
            tracing::warn!("Failed to route webhook message: {}", err);
        }
    }
}

async fn run_gateway_incoming_loop(
    gateway: Arc<Gateway>,
    mut rx: mpsc::Receiver<GatewayIncomingMessage>,
    platform: &'static str,
) {
    while let Some(incoming) = rx.recv().await {
        if let Err(err) = gateway.route_message(&incoming).await {
            tracing::warn!("Failed to route {} message: {}", platform, err);
        }
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

fn normalize_secret_provider(provider: &str) -> String {
    let p = provider.trim().to_ascii_lowercase();
    match p.as_str() {
        "github-copilot" => "copilot".to_string(),
        _ => p,
    }
}

fn secret_provider_aliases(provider: &str) -> Vec<String> {
    match normalize_secret_provider(provider).as_str() {
        "moonshot" => vec!["moonshot".to_string(), "kimi".to_string()],
        "kimi" => vec!["kimi".to_string(), "moonshot".to_string()],
        "copilot" => vec!["copilot".to_string(), "github-copilot".to_string()],
        p => vec![p.to_string()],
    }
}

fn provider_env_var(provider: &str) -> Option<&'static str> {
    match normalize_secret_provider(provider).as_str() {
        "openai" => Some("OPENAI_API_KEY"),
        "anthropic" => Some("ANTHROPIC_API_KEY"),
        "openrouter" => Some("OPENROUTER_API_KEY"),
        "qwen" => Some("DASHSCOPE_API_KEY"),
        "moonshot" | "kimi" => Some("MOONSHOT_API_KEY"),
        "minimax" => Some("MINIMAX_API_KEY"),
        "nous" => Some("NOUS_API_KEY"),
        "copilot" => Some("GITHUB_COPILOT_TOKEN"),
        _ => None,
    }
}

fn secret_vault_path_for_cli(cli: &Cli) -> PathBuf {
    hermes_state_root(cli).join("auth").join("tokens.json")
}

async fn lookup_secret_from_vault(
    token_store: &FileTokenStore,
    provider: &str,
) -> Option<(String, String)> {
    for candidate in secret_provider_aliases(provider) {
        if let Some(cred) = token_store.get(&candidate).await {
            if !cred.access_token.trim().is_empty() {
                return Some((candidate, cred.access_token));
            }
        }
    }
    None
}

async fn hydrate_provider_env_from_vault_for_cli(cli: &Cli) -> Result<(), AgentError> {
    let path = secret_vault_path_for_cli(cli);
    if !path.exists() {
        return Ok(());
    }
    let store = FileTokenStore::new(path).await?;
    let env_bindings = [
        ("OPENAI_API_KEY", "openai"),
        ("ANTHROPIC_API_KEY", "anthropic"),
        ("OPENROUTER_API_KEY", "openrouter"),
        ("DASHSCOPE_API_KEY", "qwen"),
        ("MOONSHOT_API_KEY", "moonshot"),
        ("MINIMAX_API_KEY", "minimax"),
        ("NOUS_API_KEY", "nous"),
        ("GITHUB_COPILOT_TOKEN", "copilot"),
    ];

    for (env_var, provider) in env_bindings {
        if std::env::var(env_var)
            .ok()
            .is_some_and(|v| !v.trim().is_empty())
        {
            continue;
        }
        if let Some((_provider, secret)) = lookup_secret_from_vault(&store, provider).await {
            std::env::set_var(env_var, secret);
        }
    }
    Ok(())
}

fn mask_secret(secret: &str) -> String {
    if secret.is_empty() {
        return "(empty)".to_string();
    }
    if secret.len() <= 8 {
        return "*".repeat(secret.len());
    }
    format!(
        "{}***{}",
        &secret[..4],
        &secret[secret.len().saturating_sub(4)..]
    )
}

fn is_weixin_provider(provider: &str) -> bool {
    matches!(provider, "weixin" | "wechat" | "wx")
}

fn is_truthy(v: &str) -> bool {
    matches!(
        v.trim().to_ascii_lowercase().as_str(),
        "1" | "true" | "yes" | "on"
    )
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
            "Telegram bot token cannot be empty (set TELEGRAM_BOT_TOKEN or paste token)".into(),
        ));
    }
    Ok(t)
}

async fn weixin_account_id_from_env_or_prompt() -> Result<String, AgentError> {
    if let Ok(v) = std::env::var("WEIXIN_ACCOUNT_ID") {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Ok(v);
        }
    }
    let line = tokio::task::spawn_blocking(|| {
        use std::io::{self, Write};
        print!("Enter Weixin account_id (个人号 wxid/账号标识): ");
        let _ = io::stdout().flush();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).map(|_| buf)
    })
    .await
    .map_err(|e| AgentError::Io(format!("weixin account_id prompt: {e}")))?
    .map_err(|e| AgentError::Io(format!("stdin: {e}")))?;
    let v = line.trim().to_string();
    if v.is_empty() {
        return Err(AgentError::Config(
            "Weixin account_id cannot be empty (set WEIXIN_ACCOUNT_ID or input manually)".into(),
        ));
    }
    Ok(v)
}

fn weixin_account_file_path(account_id: &str) -> PathBuf {
    hermes_home()
        .join("weixin")
        .join("accounts")
        .join(format!("{account_id}.json"))
}

fn load_persisted_weixin_token(account_id: &str) -> Option<String> {
    let p = weixin_account_file_path(account_id);
    let s = std::fs::read_to_string(p).ok()?;
    let v: serde_json::Value = serde_json::from_str(&s).ok()?;
    v.get("token")
        .and_then(|x| x.as_str())
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(String::from)
}

fn save_persisted_weixin_account(
    account_id: &str,
    token: &str,
    base_url: Option<&str>,
    user_id: Option<&str>,
) -> Result<(), AgentError> {
    let p = weixin_account_file_path(account_id);
    if let Some(parent) = p.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| AgentError::Io(format!("create weixin account dir: {e}")))?;
    }
    let payload = serde_json::json!({
        "token": token,
        "base_url": base_url.unwrap_or(""),
        "user_id": user_id.unwrap_or(""),
        "saved_at": chrono::Utc::now().to_rfc3339(),
    });
    std::fs::write(&p, payload.to_string())
        .map_err(|e| AgentError::Io(format!("write weixin account file {}: {e}", p.display())))?;
    Ok(())
}

async fn weixin_token_from_env_or_prompt(account_id: &str) -> Result<String, AgentError> {
    if let Ok(v) = std::env::var("WEIXIN_TOKEN") {
        let v = v.trim().to_string();
        if !v.is_empty() {
            return Ok(v);
        }
    }
    if let Some(v) = load_persisted_weixin_token(account_id) {
        return Ok(v);
    }
    let line = tokio::task::spawn_blocking(|| {
        use std::io::{self, Write};
        print!("Enter Weixin iLink token (WEIXIN_TOKEN): ");
        let _ = io::stdout().flush();
        let mut buf = String::new();
        io::stdin().read_line(&mut buf).map(|_| buf)
    })
    .await
    .map_err(|e| AgentError::Io(format!("weixin token prompt: {e}")))?
    .map_err(|e| AgentError::Io(format!("stdin: {e}")))?;
    let v = line.trim().to_string();
    if v.is_empty() {
        return Err(AgentError::Config(
            "Weixin token cannot be empty (set WEIXIN_TOKEN / saved account file / input manually)"
                .into(),
        ));
    }
    Ok(v)
}

fn weixin_login_base_url_from_disk(disk: &hermes_config::GatewayConfig) -> String {
    if let Some(wx) = disk.platforms.get("weixin") {
        if let Some(v) = wx.extra.get("base_url").and_then(|v| v.as_str()) {
            let s = v.trim();
            if !s.is_empty() {
                return s.to_string();
            }
        }
    }
    if let Ok(v) = std::env::var("WEIXIN_BASE_URL") {
        let s = v.trim();
        if !s.is_empty() {
            return s.to_string();
        }
    }
    "https://ilinkai.weixin.qq.com".to_string()
}

fn weixin_login_endpoints_from_disk(disk: &hermes_config::GatewayConfig) -> (String, String) {
    let mut start_ep = "ilink/bot/get_bot_qrcode".to_string();
    let mut poll_ep = "ilink/bot/get_qrcode_status".to_string();
    if let Some(wx) = disk.platforms.get("weixin") {
        if let Some(v) = wx
            .extra
            .get("qr_get_bot_qrcode_endpoint")
            .or_else(|| wx.extra.get("qr_start_endpoint"))
            .and_then(|v| v.as_str())
        {
            let s = v.trim();
            if !s.is_empty() {
                start_ep = s.to_string();
            }
        }
        if let Some(v) = wx
            .extra
            .get("qr_get_qrcode_status_endpoint")
            .or_else(|| wx.extra.get("qr_poll_endpoint"))
            .and_then(|v| v.as_str())
        {
            let s = v.trim();
            if !s.is_empty() {
                poll_ep = s.to_string();
            }
        }
    }
    (start_ep, poll_ep)
}

fn weixin_extract_string(v: &serde_json::Value, keys: &[&str]) -> Option<String> {
    for key in keys {
        if let Some(s) = v.get(*key).and_then(|x| x.as_str()) {
            let s = s.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    None
}

fn render_qr_to_terminal(data: &str) {
    let len = data.len();
    let side = (len as f64).sqrt().ceil() as usize;
    if side == 0 {
        println!("(empty QR data)");
        return;
    }
    let bytes = data.as_bytes();
    let is_dark = |row: usize, col: usize| -> bool {
        let idx = row * side + col;
        if idx < bytes.len() {
            bytes[idx] % 2 == 1
        } else {
            false
        }
    };
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

async fn weixin_qr_login_flow(
    base_url: &str,
    start_ep: &str,
    poll_ep: &str,
    _account_id_hint: Option<&str>,
) -> Result<(String, String, String, String), AgentError> {
    let initial_base = base_url.trim_end_matches('/').to_string();
    let client = reqwest::Client::new();
    async fn fetch_weixin_qr(
        client: &reqwest::Client,
        base: &str,
        start_ep: &str,
    ) -> Result<serde_json::Value, AgentError> {
        let url = format!(
            "{}/{}",
            base.trim_end_matches('/'),
            start_ep.trim_start_matches('/')
        );
        let resp = client
            .get(&url)
            .query(&[("bot_type", "3")])
            .timeout(std::time::Duration::from_secs(35))
            .send()
            .await
            .map_err(|e| AgentError::Io(format!("weixin qr get_bot_qrcode request: {e}")))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(AgentError::Config(format!(
                "weixin qr get_bot_qrcode failed ({}): {}",
                status, body
            )));
        }
        resp.json::<serde_json::Value>()
            .await
            .map_err(|e| AgentError::Io(format!("weixin qr get_bot_qrcode parse: {e}")))
    }

    let mut current_base = initial_base.clone();
    let mut qr_json = fetch_weixin_qr(&client, &current_base, start_ep).await?;
    let mut qrcode_value = weixin_extract_string(&qr_json, &["qrcode"])
        .ok_or_else(|| AgentError::Config("weixin qr response missing qrcode".to_string()))?;
    let mut qrcode_url =
        weixin_extract_string(&qr_json, &["qrcode_img_content"]).unwrap_or_default();
    let qr_scan_data = if !qrcode_url.trim().is_empty() {
        qrcode_url.clone()
    } else {
        qrcode_value.clone()
    };
    println!();
    if !qrcode_url.trim().is_empty() {
        println!("{}", qrcode_url);
    }
    render_qr_to_terminal(&qr_scan_data);
    println!();
    println!("请使用微信扫描二维码，并在手机端确认登录。");

    let poll_interval = std::time::Duration::from_secs(1);
    let timeout = std::time::Duration::from_secs(480);
    let started = std::time::Instant::now();
    let mut refresh_count = 0u8;
    loop {
        if started.elapsed() >= timeout {
            return Err(AgentError::Config(
                "weixin qr login timed out after 480s".to_string(),
            ));
        }
        tokio::time::sleep(poll_interval).await;
        let poll_url = format!(
            "{}/{}",
            current_base.trim_end_matches('/'),
            poll_ep.trim_start_matches('/')
        );
        let poll_resp = match client
            .get(&poll_url)
            .query(&[("qrcode", qrcode_value.as_str())])
            .timeout(std::time::Duration::from_secs(35))
            .send()
            .await
        {
            Ok(v) => v,
            Err(_) => continue,
        };
        if !poll_resp.status().is_success() {
            continue;
        }
        let poll_json: serde_json::Value = match poll_resp.json().await {
            Ok(v) => v,
            Err(_) => continue,
        };
        let status = weixin_extract_string(&poll_json, &["status"])
            .unwrap_or_else(|| "wait".to_string())
            .to_ascii_lowercase();
        match status.as_str() {
            "wait" => {}
            "scaned" => {
                println!("已扫码，请在微信里确认...");
            }
            "scaned_but_redirect" => {
                if let Some(redirect_host) =
                    weixin_extract_string(&poll_json, &["redirect_host"]).filter(|s| !s.is_empty())
                {
                    current_base = format!("https://{}", redirect_host);
                }
            }
            "expired" => {
                refresh_count = refresh_count.saturating_add(1);
                if refresh_count > 3 {
                    return Err(AgentError::Config(
                        "weixin qr expired too many times".to_string(),
                    ));
                }
                println!("二维码已过期，正在刷新... ({}/3)", refresh_count);
                qr_json = fetch_weixin_qr(&client, &initial_base, start_ep).await?;
                qrcode_value = weixin_extract_string(&qr_json, &["qrcode"]).ok_or_else(|| {
                    AgentError::Config("weixin qr refresh missing qrcode".to_string())
                })?;
                qrcode_url =
                    weixin_extract_string(&qr_json, &["qrcode_img_content"]).unwrap_or_default();
                let refreshed_qr = if !qrcode_url.trim().is_empty() {
                    qrcode_url.clone()
                } else {
                    qrcode_value.clone()
                };
                if !qrcode_url.trim().is_empty() {
                    println!("{}", qrcode_url);
                }
                render_qr_to_terminal(&refreshed_qr);
            }
            "confirmed" => {
                let account_id = weixin_extract_string(&poll_json, &["ilink_bot_id", "account_id"])
                    .unwrap_or_default();
                let token =
                    weixin_extract_string(&poll_json, &["bot_token", "token"]).unwrap_or_default();
                let resolved_base_url =
                    weixin_extract_string(&poll_json, &["baseurl"]).unwrap_or(initial_base.clone());
                let user_id = weixin_extract_string(&poll_json, &["ilink_user_id", "user_id"])
                    .unwrap_or_default();
                if account_id.trim().is_empty() || token.trim().is_empty() {
                    return Err(AgentError::Config(
                        "weixin qr confirmed but payload missing ilink_bot_id/bot_token"
                            .to_string(),
                    ));
                }
                return Ok((account_id, token, resolved_base_url, user_id));
            }
            _ => {}
        }
    }
}

async fn run_auth(
    cli: Cli,
    action: Option<String>,
    provider: Option<String>,
    qr: bool,
) -> Result<(), AgentError> {
    let provider = resolve_auth_provider(provider);
    let auth_store_path = secret_vault_path_for_cli(&cli);
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
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "Telegram: token saved and platform enabled in {}",
                    cfg_path.display()
                );
                return Ok(());
            }
            if is_weixin_provider(&provider) {
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let qr_preferred = qr
                    || std::env::var("HERMES_WEIXIN_QR_LOGIN")
                        .ok()
                        .map(|v| is_truthy(&v))
                        .unwrap_or(false);
                let mut account_id_opt = disk
                    .platforms
                    .get("weixin")
                    .and_then(|p| p.extra.get("account_id"))
                    .and_then(|v| v.as_str())
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(String::from);
                let (account_id, token, qr_base_url, qr_user_id) = if qr_preferred {
                    let base_url = weixin_login_base_url_from_disk(&disk);
                    let (start_ep, poll_ep) = weixin_login_endpoints_from_disk(&disk);
                    match weixin_qr_login_flow(
                        &base_url,
                        &start_ep,
                        &poll_ep,
                        account_id_opt.as_deref(),
                    )
                    .await
                    {
                        Ok(pair) => pair,
                        Err(e) => {
                            println!("Weixin QR 登录失败，将回退到手动 token 输入: {}", e);
                            let fallback_account_id = if let Some(v) = account_id_opt.take() {
                                v
                            } else {
                                weixin_account_id_from_env_or_prompt().await?
                            };
                            let fallback_token =
                                weixin_token_from_env_or_prompt(&fallback_account_id).await?;
                            (fallback_account_id, fallback_token, base_url, String::new())
                        }
                    }
                } else {
                    let manual_account_id = if let Some(v) = account_id_opt.take() {
                        v
                    } else {
                        weixin_account_id_from_env_or_prompt().await?
                    };
                    let manual_token = weixin_token_from_env_or_prompt(&manual_account_id).await?;
                    let base_url = weixin_login_base_url_from_disk(&disk);
                    (manual_account_id, manual_token, base_url, String::new())
                };
                let wx = disk
                    .platforms
                    .entry("weixin".to_string())
                    .or_insert_with(PlatformConfig::default);
                wx.enabled = true;
                wx.token = Some(token.clone());
                wx.extra.insert(
                    "account_id".to_string(),
                    serde_json::Value::String(account_id.clone()),
                );
                if !qr_base_url.trim().is_empty() {
                    wx.extra.insert(
                        "base_url".to_string(),
                        serde_json::Value::String(qr_base_url.clone()),
                    );
                }
                save_persisted_weixin_account(
                    &account_id,
                    &token,
                    Some(qr_base_url.as_str()),
                    Some(qr_user_id.as_str()),
                )?;
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "Weixin: account_id/token saved and platform enabled in {}",
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
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "Telegram: token cleared and platform disabled in {}",
                    cfg_path.display()
                );
                return Ok(());
            }
            if is_weixin_provider(&provider) {
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let mut disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                if let Some(wx) = disk.platforms.get_mut("weixin") {
                    wx.token = None;
                    wx.enabled = false;
                }
                validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
                save_config_yaml(&cfg_path, &disk)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                println!(
                    "Weixin: token cleared and platform disabled in {} (account file retained)",
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
            if is_weixin_provider(&provider) {
                let cfg_path = hermes_state_root(&cli).join("config.yaml");
                let disk = load_user_config_file(&cfg_path)
                    .map_err(|e| AgentError::Config(e.to_string()))?;
                let (account_id, has_cfg_token, enabled) = disk
                    .platforms
                    .get("weixin")
                    .map(|p| {
                        let account_id = p
                            .extra
                            .get("account_id")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string();
                        let has_cfg_token = p
                            .token
                            .as_deref()
                            .map(|t| !t.trim().is_empty())
                            .unwrap_or(false);
                        (account_id, has_cfg_token, p.enabled)
                    })
                    .unwrap_or_else(|| ("".to_string(), false, false));
                let has_saved_token = if account_id.is_empty() {
                    false
                } else {
                    load_persisted_weixin_token(&account_id).is_some()
                };
                println!(
                    "Weixin ({}): account_id={} cfg_token_present={} saved_token_present={} enabled={}",
                    cfg_path.display(),
                    if account_id.is_empty() {
                        "(none)"
                    } else {
                        account_id.as_str()
                    },
                    has_cfg_token,
                    has_saved_token,
                    enabled
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

async fn run_secrets(
    cli: Cli,
    action: Option<String>,
    provider: Option<String>,
    value: Option<String>,
    show: bool,
) -> Result<(), AgentError> {
    let path = secret_vault_path_for_cli(&cli);
    let store = FileTokenStore::new(&path).await?;
    let manager = AuthManager::new(store.clone());

    match action.as_deref().unwrap_or("list") {
        "list" | "status" => {
            let providers = store.list_providers().await;
            println!("Secret vault: {}", path.display());
            if providers.is_empty() {
                println!("  (empty)");
            } else {
                println!("Stored providers ({}):", providers.len());
                for p in providers {
                    if let Some(env_var) = provider_env_var(&p) {
                        println!("  - {p} (env: {env_var})");
                    } else {
                        println!("  - {p}");
                    }
                }
            }
            println!("Tip: runtime automatically hydrates env vars from this vault.");
        }
        "set" => {
            let provider_input = provider.ok_or_else(|| {
                AgentError::Config("secrets set: usage `hermes secrets set <provider>`".into())
            })?;
            let provider = normalize_secret_provider(&provider_input);
            let secret = match value {
                Some(v) => v.trim().to_string(),
                None => prompt_line(format!("Enter secret for provider '{provider}': ")).await?,
            };
            if secret.is_empty() {
                return Err(AgentError::Config("Secret cannot be empty.".into()));
            }
            manager
                .save_credential(OAuthCredential {
                    provider: provider.clone(),
                    access_token: secret,
                    refresh_token: None,
                    token_type: "bearer".to_string(),
                    scope: None,
                    expires_at: None,
                })
                .await?;
            println!("Saved secret for provider '{provider}' in {}", path.display());
            if let Some(env_var) = provider_env_var(&provider) {
                println!("Mapped runtime env: {env_var}");
            }
        }
        "get" => {
            let provider_input = provider.ok_or_else(|| {
                AgentError::Config("secrets get: usage `hermes secrets get <provider>`".into())
            })?;
            let provider = normalize_secret_provider(&provider_input);
            if let Some((stored_provider, secret)) = lookup_secret_from_vault(&store, &provider).await {
                if show {
                    println!("{secret}");
                } else {
                    println!("{}", mask_secret(&secret));
                }
                if stored_provider != provider {
                    println!(
                        "(resolved via provider alias '{}')",
                        stored_provider
                    );
                }
            } else {
                return Err(AgentError::Config(format!(
                    "No secret stored for provider '{}'",
                    provider
                )));
            }
        }
        "remove" | "delete" | "rm" => {
            let provider_input = provider.ok_or_else(|| {
                AgentError::Config("secrets remove: usage `hermes secrets remove <provider>`".into())
            })?;
            let provider = normalize_secret_provider(&provider_input);
            let mut removed = false;
            for candidate in secret_provider_aliases(&provider) {
                if store.get(&candidate).await.is_some() {
                    store.remove(&candidate).await?;
                    removed = true;
                }
            }
            if removed {
                println!("Removed secret for provider '{}'.", provider);
            } else {
                println!("No secret found for provider '{}'.", provider);
            }
        }
        other => {
            return Err(AgentError::Config(format!(
                "Unknown secrets action: {} (use list|status|get|set|remove)",
                other
            )));
        }
    }
    Ok(())
}

fn cron_cli_error(e: CronError) -> AgentError {
    AgentError::Config(e.to_string())
}

fn build_live_cron_scheduler(cli: &Cli, data_dir: &Path) -> Result<CronScheduler, AgentError> {
    let config =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;
    let current_model = config.model.clone().unwrap_or_else(|| "gpt-4o".to_string());
    let provider = build_provider(&config, &current_model);

    let tool_registry = Arc::new(ToolRegistry::new());
    let terminal_backend: Arc<dyn hermes_core::TerminalBackend> = Arc::new(LocalBackend::default());
    let skill_store = Arc::new(FileSkillStore::new(FileSkillStore::default_dir()));
    let skill_provider: Arc<dyn hermes_core::SkillProvider> =
        Arc::new(SkillManager::new(skill_store));
    hermes_tools::register_builtin_tools(&tool_registry, terminal_backend, skill_provider);

    let runner = Arc::new(CronRunner::new(
        provider,
        Arc::new(bridge_tool_registry(&tool_registry)),
    ));
    let persistence = Arc::new(FileJobPersistence::with_dir(data_dir.to_path_buf()));
    Ok(CronScheduler::new(persistence, runner))
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
            let prompt = prompt
                .ok_or_else(|| AgentError::Config("cron create: use --prompt \"...\"".into()))?;
            let job = hermes_cron::CronJob::new(schedule, prompt);
            let jid = sched.create_job(job).await.map_err(cron_cli_error)?;
            println!(
                "Created cron job id={} (persisted under {})",
                jid,
                data_dir.display()
            );
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
                    let live_sched = build_live_cron_scheduler(&cli, &data_dir)?;
                    let result = live_sched.run_job(&jid).await.map_err(cron_cli_error)?;
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
    let vault_path = secret_vault_path_for_cli(cli);
    if vault_path.exists() {
        let store = FileTokenStore::new(vault_path).await?;
        if let Some((_provider, token)) = lookup_secret_from_vault(&store, provider).await {
            return Ok(token);
        }
    }
    let cfg =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;
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
         Set {} (or `hermes secrets set {}`; plaintext fallback: `hermes config set llm.{}.api_key ...`) or paste key now: ",
        provider, fallback_var, provider, provider
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

        if let Err(e) = hermes_cli::webhook_delivery::deliver_cron_completion_to_webhooks(
            &webhooks_json,
            &ev,
            &client,
        )
        .await
        {
            tracing::warn!("cron webhook delivery: {e}");
        }
    }
}

async fn run_dump(
    cli: Cli,
    session: Option<String>,
    output: Option<String>,
) -> Result<(), AgentError> {
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
    std::fs::write(
        &out,
        serde_json::to_string_pretty(&payload).unwrap_or_default(),
    )
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
    generate(sh, &mut cmd, "hermes-agent-ultra", &mut std::io::stdout());
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

/// Handle `hermes lumio [action]`.
async fn run_lumio(action: Option<String>, model: Option<String>) -> Result<(), AgentError> {
    match action.as_deref() {
        None | Some("login") => {
            hermes_cli::lumio::setup(model.as_deref(), true).await?;
        }
        Some("logout") => {
            hermes_cli::lumio::clear_token();
            println!("✅ Lumio token removed.");
        }
        Some("status") => match hermes_cli::lumio::load_token() {
            Some(t) => {
                let user = if t.username.is_empty() {
                    "(unknown)"
                } else {
                    &t.username
                };
                println!("Lumio: logged in as {}", user);
                println!("  API: {}", t.base_url);
                println!(
                    "  Token: {}...{}",
                    &t.token[..8.min(t.token.len())],
                    &t.token[t.token.len().saturating_sub(4)..]
                );
            }
            None => {
                println!("Lumio: not logged in");
                println!("  Run `hermes lumio` to login.");
            }
        },
        Some(other) => {
            println!(
                "Unknown lumio action: '{}'. Use: login, logout, status.",
                other
            );
        }
    }
    Ok(())
}

fn discover_setup_env_sources() -> Vec<PathBuf> {
    let mut candidates: Vec<PathBuf> = Vec::new();

    if let Ok(explicit) = std::env::var("HERMES_SETUP_IMPORT_ENV_PATH") {
        if !explicit.trim().is_empty() {
            candidates.push(PathBuf::from(explicit));
        }
    }
    if let Ok(py_home) = std::env::var("HERMES_PYTHON_HOME") {
        if !py_home.trim().is_empty() {
            candidates.push(PathBuf::from(py_home).join(".env"));
        }
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join("Documents/Projects/hermes-agent/.env"));
        candidates.push(home.join("Projects/hermes-agent/.env"));
        candidates.push(home.join("Documents/Projects/hermes-agent-python/.env"));
    }
    if let Some(claw_dir) = hermes_cli::claw_migrate::find_openclaw_dir(None) {
        candidates.push(claw_dir.join(".env"));
    }
    if let Ok(cwd) = std::env::current_dir() {
        if let Some(parent) = cwd.parent() {
            candidates.push(parent.join("hermes-agent/.env"));
        }
    }

    let mut seen = std::collections::HashSet::new();
    candidates
        .into_iter()
        .filter(|p| p.is_file())
        .filter(|p| seen.insert(p.clone()))
        .collect()
}

fn parse_env_assignment(line: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    if trimmed.is_empty() || trimmed.starts_with('#') {
        return None;
    }
    let (key, value) = trimmed.split_once('=')?;
    let key = key.trim();
    if key.is_empty() {
        return None;
    }
    Some((key.to_string(), value.trim().to_string()))
}

fn read_env_key(path: &Path, key: &str) -> Option<String> {
    let raw = std::fs::read_to_string(path).ok()?;
    for line in raw.lines() {
        if let Some((k, v)) = parse_env_assignment(line) {
            if k == key {
                return Some(v.trim_matches('"').trim_matches('\'').to_string());
            }
        }
    }
    None
}

fn merge_missing_env_keys(src: &Path, dst: &Path, label: &str) -> Result<usize, AgentError> {
    let src_content = std::fs::read_to_string(src)
        .map_err(|e| AgentError::Io(format!("read {}: {}", src.display(), e)))?;
    let existing = std::fs::read_to_string(dst).unwrap_or_default();

    let existing_keys: std::collections::HashSet<String> = existing
        .lines()
        .filter_map(parse_env_assignment)
        .map(|(k, _)| k)
        .collect();

    let mut to_import = Vec::new();
    for line in src_content.lines() {
        if let Some((k, _)) = parse_env_assignment(line) {
            if !existing_keys.contains(&k) {
                to_import.push(line.trim().to_string());
            }
        }
    }

    if to_import.is_empty() {
        return Ok(0);
    }

    let mut out = existing;
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!("# Imported by `hermes setup` from {label}\n"));
    for line in &to_import {
        out.push_str(line);
        out.push('\n');
    }
    std::fs::write(dst, out)
        .map_err(|e| AgentError::Io(format!("write {}: {}", dst.display(), e)))?;
    Ok(to_import.len())
}

fn maybe_import_legacy_env(
    reader: &mut dyn std::io::BufRead,
    env_path: &Path,
) -> Result<(), AgentError> {
    use std::io::Write;

    let sources: Vec<PathBuf> = discover_setup_env_sources()
        .into_iter()
        .filter(|p| p != env_path)
        .collect();
    if sources.is_empty() {
        return Ok(());
    }

    println!("\nDetected legacy environment file(s):");
    for (idx, src) in sources.iter().enumerate() {
        println!("  {}) {}", idx + 1, src.display());
    }

    print!(
        "Import missing keys into {} from the first source? [Y/n]: ",
        env_path.display()
    );
    std::io::stdout().flush().ok();
    let mut answer = String::new();
    reader.read_line(&mut answer).ok();
    if matches!(answer.trim().to_ascii_lowercase().as_str(), "n" | "no") {
        println!("Skipped legacy .env import.");
        return Ok(());
    }

    let source = &sources[0];
    let imported = merge_missing_env_keys(
        source,
        env_path,
        &source
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("legacy source"),
    )?;
    if imported == 0 {
        println!("No new keys to import from {}.", source.display());
    } else {
        println!(
            "Imported {} key(s) from {} into {}.",
            imported,
            source.display(),
            env_path.display()
        );
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
            std::fs::create_dir_all(&dir).map_err(|e| {
                AgentError::Io(format!("Failed to create {}: {}", dir.display(), e))
            })?;
            println!("  ✓ Created {}", dir.display());
        }
    }

    let config_path = config_dir.join("config.yaml");
    let env_path = config_dir.join(".env");
    let stdin = io::stdin();
    let mut reader = stdin.lock();

    // 2. Optional import from legacy Python/OpenClaw .env files
    maybe_import_legacy_env(&mut reader, &env_path)?;

    // 2. Prompt for API key
    let has_env_openai_key = read_env_key(&env_path, "OPENAI_API_KEY").is_some();
    if has_env_openai_key {
        print!(
            "\nOpenAI API key (leave blank to keep OPENAI_API_KEY from {}): ",
            env_path.display()
        );
    } else {
        print!("\nOpenAI API key (leave blank to skip): ");
    }
    io::stdout().flush().ok();
    let mut api_key = String::new();
    reader.read_line(&mut api_key).ok();
    let api_key = api_key.trim().to_string();
    let mut stored_openai_secret_in_vault = false;
    if !api_key.is_empty() {
        print!("Store OpenAI key in encrypted vault (recommended) [Y/n]: ");
        io::stdout().flush().ok();
        let mut answer = String::new();
        reader.read_line(&mut answer).ok();
        let use_vault = !matches!(answer.trim().to_ascii_lowercase().as_str(), "n" | "no");
        if use_vault {
            let store = FileTokenStore::new(config_dir.join("auth").join("tokens.json")).await?;
            let manager = AuthManager::new(store);
            manager
                .save_credential(OAuthCredential {
                    provider: "openai".to_string(),
                    access_token: api_key.clone(),
                    refresh_token: None,
                    token_type: "bearer".to_string(),
                    scope: None,
                    expires_at: None,
                })
                .await?;
            stored_openai_secret_in_vault = true;
        }
    }

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
    let personality = if personality.is_empty() {
        "default"
    } else {
        personality
    };

    // 5. Write config.yaml
    if config_path.exists() {
        print!("\nconfig.yaml already exists. Overwrite? [y/N]: ");
        io::stdout().flush().ok();
        let mut answer = String::new();
        reader.read_line(&mut answer).ok();
        if !answer.trim().eq_ignore_ascii_case("y") {
            println!("Keeping existing config.yaml.");
            println!(
                "\nSetup complete! Run `hermes-agent-ultra` (or `hermes`) to start an interactive session."
            );
            return Ok(());
        }
    }

    let mut config_content = String::from("# Hermes Agent Configuration\n\n");
    config_content.push_str(&format!("model: {}\n", model));
    config_content.push_str(&format!("personality: {}\n", personality));
    config_content.push_str("max_turns: 50\n\n");

    if !api_key.is_empty() && !stored_openai_secret_in_vault {
        config_content.push_str("llm_providers:\n");
        config_content.push_str("  openai:\n");
        config_content.push_str(&format!("    api_key: {}\n", api_key));
    } else if stored_openai_secret_in_vault {
        println!(
            "  ✓ Stored OPENAI_API_KEY in encrypted vault: {}",
            config_dir.join("auth").join("tokens.json").display()
        );
    } else if has_env_openai_key {
        println!(
            "  ✓ Keeping OPENAI_API_KEY from {} for runtime auth",
            env_path.display()
        );
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

    println!("\nSetup complete! Run `hermes-agent-ultra` (or `hermes`) to start an interactive session.");
    println!("Run `hermes-agent-ultra doctor` (or `hermes doctor`) to check system requirements.");
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
    let tool_checks = [("docker", "Docker"), ("ssh", "SSH"), ("git", "Git")];

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
            println!(
                "  Model: {}",
                config.model.as_deref().unwrap_or("(default)")
            );
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

    let config =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;

    println!(
        "Model:   {}",
        config.model.as_deref().unwrap_or("(default: gpt-4o)")
    );
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
            println!(
                "  Model:       {}",
                config.model.as_deref().unwrap_or("gpt-4o")
            );
            println!(
                "  Personality: {}",
                config.personality.as_deref().unwrap_or("default")
            );
            println!("  Max turns:   {}", config.max_turns);
            println!("\nUse `hermes profile list` to see all profiles.");
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
                AgentError::Config(
                    "Missing profile name. Usage: hermes profile create <name>".into(),
                )
            })?;

            std::fs::create_dir_all(&profiles_dir)
                .map_err(|e| AgentError::Io(format!("Failed to create profiles dir: {}", e)))?;

            let profile_path = profiles_dir.join(format!("{}.yaml", name));
            if profile_path.exists() {
                println!(
                    "Profile '{}' already exists at {}",
                    name,
                    profile_path.display()
                );
                return Ok(());
            }

            let content = format!(
                "# Hermes Profile: {}\nname: {}\nmodel: openai:gpt-4o\npersonality: default\nmax_turns: 50\n",
                name, name
            );
            std::fs::write(&profile_path, content)
                .map_err(|e| AgentError::Io(format!("Failed to write profile: {}", e)))?;
            println!("Created profile '{}' at {}", name, profile_path.display());
            println!(
                "Edit it to customize, then switch with `hermes profile switch {}`.",
                name
            );
        }
        Some("switch") => {
            let name = name.ok_or_else(|| {
                AgentError::Config(
                    "Missing profile name. Usage: hermes profile switch <name>".into(),
                )
            })?;

            let profile_path = profiles_dir.join(format!("{}.yaml", &name));
            if !profile_path.exists() {
                // Also try .yml
                let alt = profiles_dir.join(format!("{}.yml", &name));
                if !alt.exists() {
                    println!("Profile '{}' not found. Available profiles:", name);
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
