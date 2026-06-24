//! Gateway runtime — start/stop handler, setup wizard, and inbound message loops.

use std::collections::HashMap;
use std::sync::Arc;

use crate::app::{bridge_tool_registry, build_provider};
use crate::cli::Cli;
use crate::cron_delivery::GatewayCronDeliveryBackend;
use crate::gateway_runtime_defaults;
use crate::runtime_tool_wiring::{
    wire_cron_scheduler_backend, wire_gateway_clarify_backend, wire_gateway_messaging_backend,
};
use crate::startup_metrics::StartupMetrics;
use crate::terminal_backend::build_terminal_backend;
use hermes_config::{
    GatewayConfig, PlatformConfig, hermes_home, load_config, load_user_config_file,
    save_config_yaml, validate_config,
};
use hermes_core::AgentError;
use hermes_core::PlatformAdapter;
use hermes_cron::{CronCompletionEvent, CronRunner, CronScheduler, FileJobPersistence};
use hermes_gateway::Gateway;
use hermes_gateway::gateway::GatewayConfig as RuntimeGatewayConfig;
use hermes_gateway::gateway::IncomingMessage as GatewayIncomingMessage;
use hermes_gateway::hooks::HookRegistry;
use hermes_gateway::platforms::api_server::ApiInboundRequest;
use hermes_gateway::platforms::telegram::TelegramAdapter;
use hermes_gateway::platforms::webhook::WebhookPayload;
use hermes_gateway::platforms::whatsapp::{WhatsAppConfig, is_paired};
use hermes_gateway::tool_backends::ClarifyDispatcher;
use hermes_tools::ToolRegistry;
use tokio::sync::{broadcast, mpsc};

use crate::gateway_handlers;
use crate::gateway_main::{
    GATEWAY_PLATFORM_CATALOG, GatewayAgentCache, GatewayPlatformEntry, build_gateway_dm_manager,
    build_gateway_platform_access_policies, configure_gateway_platform, gateway_requirement_issues,
    gateway_session_manager_with_persistence, platform_token_or_extra, register_gateway_adapters,
    run_sessions_db_auto_maintenance, spawn_gateway_route,
};
use crate::gateway_process::{
    cleanup_stale_gateway_metadata, gateway_pid_is_alive, gateway_pid_terminate,
    gateway_service_status, install_gateway_service, migrate_legacy_gateway_services,
    read_gateway_pid, try_restart_gateway_service, try_start_gateway_service,
    try_stop_gateway_service, uninstall_gateway_service,
};
use crate::oneshot::start_gateway_keepawake_guard;
use crate::paths::CliStateRoot;
use crate::state_paths::hermes_state_root;
use crate::webhook_delivery::run_cron_webhook_delivery_loop;

/// Handle `hermes gateway [action]`.
#[allow(clippy::too_many_arguments)]
pub async fn run_gateway(
    cli: Cli,
    action: Option<String>,
    _system: bool,
    all: bool,
    force: bool,
    _run_as_user: Option<String>,
    _replace: bool,
    dry_run: bool,
    yes: bool,
    _deep: bool,
) -> Result<(), AgentError> {
    let config =
        load_config(cli.config_dir.as_deref()).map_err(|e| AgentError::Config(e.to_string()))?;

    match action.as_deref() {
        Some("install") => {
            install_gateway_service(force, dry_run)?;
            return Ok(());
        }
        Some("uninstall") => {
            uninstall_gateway_service(dry_run)?;
            return Ok(());
        }
        Some("migrate-legacy") => {
            migrate_legacy_gateway_services(dry_run, yes)?;
            return Ok(());
        }
        Some("restart") => {
            if try_restart_gateway_service()? {
                println!("Gateway service restarted.");
                return Ok(());
            }
            let pid_path = CliStateRoot::from_state_root(&hermes_state_root(&cli)).gateway_pid();
            if let Some(pid) = read_gateway_pid(&pid_path) {
                if gateway_pid_is_alive(pid) {
                    let _ = gateway_pid_terminate(pid);
                    cleanup_stale_gateway_metadata(&pid_path);
                    println!("Stopped existing gateway process {}.", pid);
                }
            }
            return Box::pin(run_gateway(
                cli,
                Some("run".to_string()),
                false,
                all,
                force,
                None,
                false,
                false,
                yes,
                false,
            ))
            .await;
        }
        Some("setup") => {
            run_gateway_setup(&cli).await?;
        }
        None | Some("run") | Some("start") => {
            if matches!(action.as_deref(), Some("start")) && try_start_gateway_service()? {
                println!("Gateway service started.");
                return Ok(());
            }
            let mut _metrics = StartupMetrics::begin();
            // Phase 1: config & preflight (critical — must pass before proceeding).
            let _p1 = _metrics.phase("config_preflight");
            println!("Starting Hermes Gateway...");
            run_sessions_db_auto_maintenance(&config);

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
            drop(_p1);

            let _p2 = _metrics.phase("requirements_check");
            let requirement_issues = gateway_requirement_issues(&config);
            if !requirement_issues.is_empty() {
                let mut msg = String::from("Gateway requirement check failed:\n");
                for issue in requirement_issues {
                    msg.push_str("  - ");
                    msg.push_str(&issue);
                    msg.push('\n');
                }
                msg.push_str("请先执行 `hermes gateway setup` 或 `hermes auth login <provider>` 修复后再启动。");
                return Err(AgentError::Config(msg));
            }
            let missing_runtime_deps =
                hermes_config::dep_missing(hermes_config::dep_check::all_deps());
            if !missing_runtime_deps.is_empty() {
                let labels: Vec<String> = missing_runtime_deps
                    .iter()
                    .map(|dep| format!("{} ({})", dep, hermes_config::dep_check::description(*dep)))
                    .collect();
                tracing::info!(
                    deps = %labels.join(", "),
                    "runtime dependencies missing; starting background install"
                );
                if crate::runtime_dep_install::auto_ensure_enabled() {
                    hermes_config::spawn_background_install(missing_runtime_deps);
                } else {
                    tracing::warn!(
                        deps = %labels.join(", "),
                        "HERMES_AUTO_ENSURE_DEPS disabled; missing deps will block tools at use time"
                    );
                }
            }
            drop(_p2);

            let _p3 = _metrics.phase("pid_check");
            let pid_path = CliStateRoot::from_state_root(&hermes_state_root(&cli)).gateway_pid();
            if let Some(pid) = read_gateway_pid(&pid_path) {
                if gateway_pid_is_alive(pid) {
                    println!(
                        "Gateway already appears to be running (PID {}, file {}). Stop it first or remove a stale PID file.",
                        pid,
                        pid_path.display()
                    );
                    return Ok(());
                }
                cleanup_stale_gateway_metadata(&pid_path);
            }
            drop(_p3);

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

            let _p4 = _metrics.phase("gateway_core_init");
            gateway_runtime_defaults::apply_gateway_runtime_defaults();

            // Build gateway runtime and context-aware message handler.
            let runtime_gateway_config = RuntimeGatewayConfig {
                streaming_enabled: config.streaming.enabled,
                service_tier: config.agent.normalized_service_tier(),
                display: config.display.clone(),
                quick_commands: config.quick_commands.clone(),
                kanban_dispatch_in_gateway: config.kanban.dispatch_in_gateway,
                ..RuntimeGatewayConfig::default()
            };
            let session_manager = Arc::new(gateway_session_manager_with_persistence(&config));
            let dm_manager = build_gateway_dm_manager(&config);
            let gateway = Arc::new(Gateway::new(
                session_manager,
                dm_manager,
                runtime_gateway_config,
            ));
            drop(_p4);
            _metrics.mark_gateway_created();
            let platform_policies = build_gateway_platform_access_policies(&config);
            gateway
                .set_platform_access_policies(platform_policies)
                .await;

            let _p5 = _metrics.phase("hooks");
            // Defer hook discovery to after "Gateway is ready" to avoid I/O delay.
            let hooks_dir = hermes_home().join("hooks");
            let gw_hook = gateway.clone();
            let enabled_for_hook: Vec<String> = enabled.iter().map(|s| (*s).clone()).collect();
            tokio::spawn(async move {
                let mut hr = HookRegistry::new();
                hr.register_builtins();
                hr.discover_and_load(&hooks_dir);
                gw_hook.set_hook_registry(Arc::new(hr)).await;
                gw_hook
                    .emit_hook_event(
                        "gateway:startup",
                        serde_json::json!({
                            "enabled_platforms": enabled_for_hook
                        }),
                    )
                    .await;
                tracing::debug!("gateway hooks initialized (deferred)");
            });
            drop(_p5);

            let _p6 = _metrics.phase("tools_and_backends");
            let tool_registry = Arc::new(ToolRegistry::new());
            let terminal_backend = build_terminal_backend(&config);
            let skills_runtime = crate::skills_runtime::build_skill_provider(true)
                .map_err(|e| hermes_core::AgentError::Config(e.to_string()))?;
            let skill_provider = skills_runtime.provider.clone();
            crate::gateway_inbound_wiring::wire_gateway_inbound_vision(
                &gateway,
                &tool_registry,
                &config,
                terminal_backend.clone(),
                skill_provider.clone(),
            )
            .await;
            drop(_p6);

            // Defer CUA daemon start: spawn in background, don't block gateway ready.
            let _p7 = _metrics.phase("messaging_clarify");
            tokio::spawn(async move {
                if hermes_tools::check_computer_use_requirements() {
                    match hermes_tools::ensure_cua_driver_daemon_running().await {
                        Ok(()) => tracing::info!("Computer Use desktop service ready"),
                        Err(err) => tracing::warn!(
                            error = %err,
                            "Computer Use desktop service not ready at gateway startup"
                        ),
                    }
                }
            });
            let messaging_session = hermes_tools::tools::messaging::MessagingSessionContext::new();
            gateway
                .set_messaging_session_context(messaging_session.clone())
                .await;
            let clarify_dispatcher = ClarifyDispatcher::new();
            gateway
                .set_clarify_dispatcher(clarify_dispatcher.clone())
                .await;
            drop(_p7);

            let _p8 = _metrics.phase("handler_wiring");
            let agent_tools_for_cron = Arc::new(bridge_tool_registry(&tool_registry));
            let config_arc = Arc::new(config.clone());
            crate::moa_wiring::wire_mixture_of_agents_backend(&tool_registry, config_arc.clone());
            let gateway_agent_cache: GatewayAgentCache =
                Arc::new(tokio::sync::Mutex::new(HashMap::new()));
            let handler_deps = gateway_handlers::GatewayHandlerDeps {
                config: config_arc.clone(),
                runtime_tools: tool_registry.clone(),
                gateway_for_review: gateway.clone(),
                clarify: clarify_dispatcher.clone(),
                gateway_agent_cache: gateway_agent_cache.clone(),
            };
            let handler_deps_stream = handler_deps.clone();
            let handler_deps_plan_mode = handler_deps.clone();
            gateway
                .set_session_teardown_handler(
                    gateway_handlers::make_gateway_session_teardown_handler(handler_deps.clone()),
                )
                .await;
            gateway
                .set_message_handler_with_context(Arc::new(move |messages, ctx| {
                    let deps = handler_deps.clone();
                    Box::pin(gateway_handlers::gateway_handle_message_non_streaming(
                        messages, ctx, deps,
                    ))
                }))
                .await;
            gateway
                .set_streaming_handler_with_context(Arc::new(move |messages, ctx, on_chunk| {
                    let deps = handler_deps_stream.clone();
                    Box::pin(gateway_handlers::gateway_handle_message_streaming(
                        messages, ctx, on_chunk, deps,
                    ))
                }))
                .await;
            let gateway_for_plan_mode = gateway.clone();
            gateway
                .set_plan_mode_slash_handler(Arc::new(move |incoming, session_key, args| {
                    let gw = gateway_for_plan_mode.clone();
                    let deps = handler_deps_plan_mode.clone();
                    Box::pin(async move {
                        crate::gateway_plan_mode::execute_plan_mode_slash_command(
                            gw,
                            &incoming,
                            &session_key,
                            &args,
                            deps,
                        )
                        .await
                    })
                }))
                .await;
            drop(_p8);

            let _p9 = _metrics.phase("cron_init");
            // Cron: same on-disk dir as `hermes cron` + real LLM/tools as the gateway agent.
            let cron_dir = hermes_state_root(&cli).join("cron");
            std::fs::create_dir_all(&cron_dir)
                .map_err(|e| AgentError::Io(format!("cron dir {}: {}", cron_dir.display(), e)))?;
            let default_model = config.model.clone().unwrap_or_else(|| "gpt-4o".to_string());
            let cron_persistence = Arc::new(FileJobPersistence::with_dir(cron_dir.clone()));
            let cron_llm = build_provider(&config, &default_model);
            let cron_runner = Arc::new(
                CronRunner::new(cron_llm, agent_tools_for_cron)
                    .with_delivery(Arc::new(GatewayCronDeliveryBackend::new(gateway.clone()))),
            );
            let mut cron_scheduler = CronScheduler::new(cron_persistence, cron_runner);
            let (cron_tx, cron_rx) = broadcast::channel::<CronCompletionEvent>(64);
            cron_scheduler.set_completion_broadcast(cron_tx);
            cron_scheduler
                .load_persisted_jobs()
                .await
                .map_err(|e| AgentError::Config(format!("cron load: {e}")))?;
            cron_scheduler.start().await;
            let cron_scheduler = Arc::new(cron_scheduler);
            wire_cron_scheduler_backend(
                &tool_registry,
                cron_scheduler.clone(),
                messaging_session.clone(),
            );
            wire_gateway_messaging_backend(
                &tool_registry,
                gateway.clone(),
                messaging_session.clone(),
            );
            wire_gateway_clarify_backend(&tool_registry, clarify_dispatcher);
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
            drop(_p9);

            let _p10 = _metrics.phase("adapters");
            let mut sidecar_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
            let webhooks_path_clone = webhooks_path.clone();
            sidecar_tasks.push(tokio::spawn(async move {
                run_cron_webhook_delivery_loop(cron_rx, webhooks_path_clone).await;
            }));

            register_gateway_adapters(&config, gateway.clone(), &mut sidecar_tasks).await?;
            drop(_p10);

            let _p11 = _metrics.phase("adapter_start");
            if gateway.adapter_names().await.is_empty() {
                if enabled.is_empty() {
                    println!("No chat adapters enabled; cron + webhooks still active.");
                } else {
                    return Err(AgentError::Config(
                        "Gateway startup failed: platforms are enabled but no adapters registered."
                            .to_string(),
                    ));
                }
            }

            gateway.start_all().await?;
            drop(_p11);

            let _p12 = _metrics.phase("watchers_and_pid");
            {
                let gw_reconnect = gateway.clone();
                sidecar_tasks.push(tokio::spawn(async move {
                    gw_reconnect.platform_reconnect_watcher(20).await;
                }));
                let gw_expiry = gateway.clone();
                sidecar_tasks.push(tokio::spawn(async move {
                    gw_expiry.session_expiry_watcher(300).await;
                }));
            }
            let own_pid = std::process::id();
            std::fs::write(&pid_path, format!("{}\n", own_pid)).map_err(|e| {
                AgentError::Io(format!("failed to write {}: {}", pid_path.display(), e))
            })?;
            drop(_p12);

            // Emit structured startup summary before "ready" message.
            let _summary = _metrics.finish();
            _summary.print_summary();
            println!("Gateway is ready. Press Ctrl+C to stop.");
            #[cfg(target_os = "windows")]
            let _gateway_keepawake_guard = start_gateway_keepawake_guard();
            #[cfg(not(target_os = "windows"))]
            start_gateway_keepawake_guard();
            // Keep gateway alive for future adapter/event wiring.
            // Wait for Ctrl+C
            tokio::signal::ctrl_c()
                .await
                .map_err(|e| AgentError::Io(format!("Failed to listen for Ctrl+C: {}", e)))?;

            println!("\nShutting down gateway...");
            gateway.teardown_all_sessions("shutdown").await;
            cron_scheduler.stop().await;
            gateway.stop_all().await?;
            let _ = std::fs::remove_file(&pid_path);
            for task in sidecar_tasks {
                task.abort();
            }
            println!("Gateway stopped.");
        }
        Some("status") => {
            if let Some(service_state) = gateway_service_status()? {
                println!("{service_state}");
            }
            let pid_path = CliStateRoot::from_state_root(&hermes_state_root(&cli)).gateway_pid();
            if !pid_path.exists() {
                println!(
                    "Gateway status: not running (no PID file; start with `hermes gateway start`)"
                );
                return Ok(());
            }
            match read_gateway_pid(&pid_path) {
                Some(pid) if gateway_pid_is_alive(pid) => {
                    println!(
                        "Gateway status: running (PID {}, file {})",
                        pid,
                        pid_path.display()
                    );
                }
                Some(pid) => {
                    cleanup_stale_gateway_metadata(&pid_path);
                    println!(
                        "Gateway status: not running (stale metadata for PID {} in {})",
                        pid,
                        pid_path.display()
                    );
                }
                None => {
                    cleanup_stale_gateway_metadata(&pid_path);
                    println!("Gateway status: invalid PID file at {}", pid_path.display());
                }
            }
        }
        Some("stop") => {
            if try_stop_gateway_service()? {
                println!("Gateway service stopped.");
                return Ok(());
            }
            let pid_path = CliStateRoot::from_state_root(&hermes_state_root(&cli)).gateway_pid();
            let Some(pid) = read_gateway_pid(&pid_path) else {
                println!("Gateway stop: no PID file (nothing to stop).");
                return Ok(());
            };
            if !gateway_pid_is_alive(pid) {
                cleanup_stale_gateway_metadata(&pid_path);
                println!(
                    "Gateway stop: process {} not running; removed stale PID/lock metadata for {}.",
                    pid,
                    pid_path.display()
                );
                return Ok(());
            }
            match gateway_pid_terminate(pid) {
                Ok(()) => {
                    println!("Sent SIGTERM to gateway PID {}.", pid);
                    cleanup_stale_gateway_metadata(&pid_path);
                    println!("Removed {}.", pid_path.display());
                }
                Err(e) => println!("Gateway stop: failed to signal PID {}: {}", pid, e),
            }
        }
        Some(other) => {
            println!(
                "Unknown gateway action: {}. Use 'run', 'start', 'stop', 'restart', 'status', 'install', 'uninstall', 'setup', or 'migrate-legacy'.",
                other
            );
        }
    }
    Ok(())
}

fn telegram_platform_has_allowlist(platform: &PlatformConfig) -> bool {
    platform.allowed_users.iter().any(|u| !u.trim().is_empty())
        || platform
            .extra
            .get("allow_from")
            .and_then(|v| serde_json::from_value::<Vec<String>>(v.clone()).ok())
            .is_some_and(|users| users.iter().any(|u| !u.trim().is_empty()))
}

fn platform_extra_nonempty(platform: &PlatformConfig, key: &str) -> bool {
    platform
        .extra
        .get(key)
        .and_then(|v| v.as_str())
        .map(str::trim)
        .is_some_and(|s| !s.is_empty())
}

fn gateway_platform_is_configured(key: &str, platform: Option<&PlatformConfig>) -> bool {
    let Some(platform) = platform else {
        return false;
    };
    if !platform.enabled {
        return false;
    }
    match key {
        "telegram" => {
            platform_token_or_extra(platform).is_some() && telegram_platform_has_allowlist(platform)
        }
        "discord" | "slack" | "signal" | "matrix" | "mattermost" | "bluebubbles" | "email"
        | "homeassistant" => platform_token_or_extra(platform).is_some(),
        "whatsapp" => {
            platform.enabled
                && is_paired(&WhatsAppConfig::from_platform_config(platform).session_path())
        }
        "weixin" => {
            platform_token_or_extra(platform).is_some()
                && platform_extra_nonempty(platform, "account_id")
        }
        "qqbot" => {
            platform_extra_nonempty(platform, "app_id")
                && platform_extra_nonempty(platform, "client_secret")
        }
        "wecom" => {
            platform_extra_nonempty(platform, "bot_id")
                && platform_extra_nonempty(platform, "secret")
        }
        "wecom_callback" => {
            platform_extra_nonempty(platform, "corp_id")
                && platform_extra_nonempty(platform, "corp_secret")
                && platform_extra_nonempty(platform, "agent_id")
                && platform_extra_nonempty(platform, "encoding_aes_key")
        }
        "dingtalk" => {
            platform_extra_nonempty(platform, "client_id")
                && platform_extra_nonempty(platform, "client_secret")
        }
        "feishu" => {
            platform_extra_nonempty(platform, "app_id")
                && platform_extra_nonempty(platform, "app_secret")
        }
        "sms" => {
            platform_extra_nonempty(platform, "account_sid")
                && platform_extra_nonempty(platform, "auth_token")
        }
        "webhook" => platform_extra_nonempty(platform, "secret"),
        "api_server" => true,
        _ => false,
    }
}

pub fn gateway_platform_menu_label(
    entry: &GatewayPlatformEntry,
    platform: Option<&PlatformConfig>,
) -> String {
    let status = if entry.key == "whatsapp" {
        crate::whatsapp_wizard::whatsapp_gateway_menu_status(platform)
    } else if gateway_platform_is_configured(entry.key, platform) {
        "configured"
    } else {
        "not configured"
    };
    format!("{} {}  ({status})", entry.emoji, entry.label)
}

pub async fn run_gateway_setup(cli: &Cli) -> Result<(), AgentError> {
    println!("Gateway setup wizard");
    println!("--------------------");
    let cfg_path = hermes_state_root(cli).join("config.yaml");
    let mut disk =
        load_user_config_file(&cfg_path).map_err(|e| AgentError::Config(e.to_string()))?;

    loop {
        let mut menu_items: Vec<String> = GATEWAY_PLATFORM_CATALOG
            .iter()
            .map(|entry| gateway_platform_menu_label(entry, disk.platforms.get(entry.key)))
            .collect();
        menu_items.push("Done".to_string());
        let done_index = menu_items.len() - 1;

        let pick = crate::prompt_choice(
            "Messaging Platforms",
            "Select a platform to configure:",
            &menu_items,
            done_index,
        );
        if !pick.confirmed || pick.index == done_index {
            break;
        }

        let Some(entry) = GATEWAY_PLATFORM_CATALOG.get(pick.index) else {
            println!("Invalid platform selection.");
            continue;
        };

        println!();
        println!("Configuring {}...", entry.label);
        configure_gateway_platform(cli, &mut disk, &cfg_path, entry.key).await?;
        validate_config(&disk).map_err(|e| AgentError::Config(e.to_string()))?;
        save_config_yaml(&cfg_path, &disk).map_err(|e| AgentError::Config(e.to_string()))?;
    }

    println!();
    println!("Gateway setup complete.");
    println!("Config saved: {}", cfg_path.display());
    println!("Next step: `hermes gateway start`");
    Ok(())
}

pub(crate) async fn run_api_server_inbound_loop(
    gateway: Arc<Gateway>,
    mut rx: mpsc::Receiver<ApiInboundRequest>,
) {
    while let Some(req) = rx.recv().await {
        gateway
            .merge_request_runtime_overrides(
                "api_server",
                &req.session_id,
                &req.user_id,
                req.model.clone(),
                req.provider.clone(),
                req.personality.clone(),
            )
            .await;
        let incoming = GatewayIncomingMessage {
            platform: "api_server".to_string(),
            chat_id: req.session_id.clone(),
            user_id: req.user_id.clone(),
            text: req.prompt.clone(),
            media_urls: vec![],
            media_types: vec![],
            message_id: Some(req.request_id.clone()),
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
            role_ids: vec![],
            parent_channel_id: None,
            channel_prompt: None,
            channel_skills: vec![],
            channel_topic: None,
            message_thread_id: None,
        };
        if let Err(err) = gateway.route_message(&incoming).await {
            tracing::warn!("Failed to route api_server message: {}", err);
        }
    }
}

pub(crate) async fn run_webhook_inbound_loop(
    gateway: Arc<Gateway>,
    mut rx: mpsc::Receiver<WebhookPayload>,
) {
    while let Some(payload) = rx.recv().await {
        let incoming = GatewayIncomingMessage {
            platform: "webhook".to_string(),
            chat_id: payload.chat_id,
            user_id: payload
                .user_id
                .unwrap_or_else(|| "webhook-client".to_string()),
            text: payload.text,
            media_urls: vec![],
            media_types: vec![],
            message_id: None,
            is_dm: true,
            interaction_id: None,
            interaction_token: None,
            role_ids: vec![],
            parent_channel_id: None,
            channel_prompt: None,
            channel_skills: vec![],
            channel_topic: None,
            message_thread_id: None,
        };
        if let Err(err) = gateway.route_message(&incoming).await {
            tracing::warn!("Failed to route webhook message: {}", err);
        }
    }
}

pub(crate) async fn run_gateway_incoming_loop(
    gateway: Arc<Gateway>,
    mut rx: mpsc::Receiver<GatewayIncomingMessage>,
    platform: &'static str,
) {
    while let Some(incoming) = rx.recv().await {
        spawn_gateway_route(gateway.clone(), incoming, platform);
    }
}

pub(crate) async fn run_telegram_poll_loop(gateway: Arc<Gateway>, adapter: Arc<TelegramAdapter>) {
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
                        media_urls: vec![],
                        media_types: vec![],
                        message_id: Some(msg.message_id.to_string()),
                        is_dm: msg.chat_id > 0,
                        interaction_id: None,
                        interaction_token: None,
                        role_ids: vec![],
                        parent_channel_id: None,
                        channel_prompt: None,
                        channel_skills: vec![],
                        channel_topic: None,
                        message_thread_id: msg.message_thread_id.map(|id| id.to_string()),
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

/// Embedded gateway startup for `hermes talk` (non-blocking; no PID file).
pub struct EmbeddedGatewayParts {
    pub gateway: Arc<Gateway>,
    pub sidecar_tasks: Vec<tokio::task::JoinHandle<()>>,
    pub cron_rx: broadcast::Receiver<CronCompletionEvent>,
}

pub async fn spawn_embedded_gateway_core(
    config: &GatewayConfig,
    state_root: &std::path::Path,
    tool_registry: Arc<ToolRegistry>,
) -> Result<EmbeddedGatewayParts, AgentError> {
    gateway_runtime_defaults::apply_gateway_runtime_defaults();

    let runtime_gateway_config = RuntimeGatewayConfig {
        streaming_enabled: config.streaming.enabled,
        service_tier: config.agent.normalized_service_tier(),
        display: config.display.clone(),
        quick_commands: config.quick_commands.clone(),
        kanban_dispatch_in_gateway: config.kanban.dispatch_in_gateway,
        ..RuntimeGatewayConfig::default()
    };
    let session_manager = Arc::new(gateway_session_manager_with_persistence(config));
    let dm_manager = build_gateway_dm_manager(config);
    let gateway = Arc::new(Gateway::new(
        session_manager,
        dm_manager,
        runtime_gateway_config,
    ));
    let platform_policies = build_gateway_platform_access_policies(config);
    gateway
        .set_platform_access_policies(platform_policies)
        .await;

    let hooks_dir = hermes_home().join("hooks");
    let gw_hook = gateway.clone();
    tokio::spawn(async move {
        let mut hr = HookRegistry::new();
        hr.register_builtins();
        hr.discover_and_load(&hooks_dir);
        gw_hook.set_hook_registry(Arc::new(hr)).await;
    });

    let terminal_backend = build_terminal_backend(config);
    let skills_runtime = crate::skills_runtime::build_skill_provider(true)
        .map_err(|e| AgentError::Config(e.to_string()))?;
    let skill_provider = skills_runtime.provider.clone();
    crate::gateway_inbound_wiring::wire_gateway_inbound_vision(
        &gateway,
        &tool_registry,
        config,
        terminal_backend,
        skill_provider,
    )
    .await;

    let messaging_session = hermes_tools::tools::messaging::MessagingSessionContext::new();
    gateway
        .set_messaging_session_context(messaging_session.clone())
        .await;
    let clarify_dispatcher = ClarifyDispatcher::new();
    gateway
        .set_clarify_dispatcher(clarify_dispatcher.clone())
        .await;

    let agent_tools_for_cron = Arc::new(bridge_tool_registry(&tool_registry));
    let config_arc = Arc::new(config.clone());
    crate::moa_wiring::wire_mixture_of_agents_backend(&tool_registry, config_arc.clone());
    let gateway_agent_cache: GatewayAgentCache = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    let handler_deps = gateway_handlers::GatewayHandlerDeps {
        config: config_arc,
        runtime_tools: tool_registry.clone(),
        gateway_for_review: gateway.clone(),
        clarify: clarify_dispatcher.clone(),
        gateway_agent_cache,
    };
    let handler_deps_stream = handler_deps.clone();
    let handler_deps_plan_mode = handler_deps.clone();
    gateway
        .set_session_teardown_handler(gateway_handlers::make_gateway_session_teardown_handler(
            handler_deps.clone(),
        ))
        .await;
    gateway
        .set_message_handler_with_context(Arc::new(move |messages, ctx| {
            let deps = handler_deps.clone();
            Box::pin(gateway_handlers::gateway_handle_message_non_streaming(
                messages, ctx, deps,
            ))
        }))
        .await;
    gateway
        .set_streaming_handler_with_context(Arc::new(move |messages, ctx, on_chunk| {
            let deps = handler_deps_stream.clone();
            Box::pin(gateway_handlers::gateway_handle_message_streaming(
                messages, ctx, on_chunk, deps,
            ))
        }))
        .await;
    let gateway_for_plan_mode = gateway.clone();
    gateway
        .set_plan_mode_slash_handler(Arc::new(move |incoming, session_key, args| {
            let gw = gateway_for_plan_mode.clone();
            let deps = handler_deps_plan_mode.clone();
            Box::pin(async move {
                crate::gateway_plan_mode::execute_plan_mode_slash_command(
                    gw,
                    &incoming,
                    &session_key,
                    &args,
                    deps,
                )
                .await
            })
        }))
        .await;

    let cron_dir = state_root.join("cron");
    std::fs::create_dir_all(&cron_dir)
        .map_err(|e| AgentError::Io(format!("cron dir {}: {}", cron_dir.display(), e)))?;
    let default_model = config.model.clone().unwrap_or_else(|| "gpt-4o".to_string());
    let cron_persistence = Arc::new(FileJobPersistence::with_dir(cron_dir));
    let cron_llm = build_provider(config, &default_model);
    let cron_runner = Arc::new(
        CronRunner::new(cron_llm, agent_tools_for_cron)
            .with_delivery(Arc::new(GatewayCronDeliveryBackend::new(gateway.clone()))),
    );
    let mut cron_scheduler = CronScheduler::new(cron_persistence, cron_runner);
    let (cron_tx, cron_rx) = broadcast::channel::<CronCompletionEvent>(64);
    cron_scheduler.set_completion_broadcast(cron_tx.clone());
    cron_scheduler
        .load_persisted_jobs()
        .await
        .map_err(|e| AgentError::Config(format!("cron load: {e}")))?;
    cron_scheduler.start().await;
    let cron_scheduler = Arc::new(cron_scheduler);
    wire_cron_scheduler_backend(
        &tool_registry,
        cron_scheduler.clone(),
        messaging_session.clone(),
    );
    wire_gateway_messaging_backend(&tool_registry, gateway.clone(), messaging_session);
    wire_gateway_clarify_backend(&tool_registry, clarify_dispatcher);

    let webhooks_path = state_root.join("webhooks.json");
    let mut sidecar_tasks: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    sidecar_tasks.push(tokio::spawn(async move {
        run_cron_webhook_delivery_loop(cron_tx.subscribe(), webhooks_path).await;
    }));

    register_gateway_adapters(config, gateway.clone(), &mut sidecar_tasks).await?;

    let enabled: Vec<&String> = config
        .platforms
        .iter()
        .filter(|(_, pc)| pc.enabled)
        .map(|(name, _)| name)
        .collect();
    if gateway.adapter_names().await.is_empty() {
        if enabled.is_empty() {
            tracing::info!("talk embedded gateway: no chat adapters enabled; cron still active");
        } else {
            tracing::warn!(
                enabled = ?enabled,
                "talk embedded gateway: platforms enabled in config but no adapters registered \
                 (missing credentials or setup); continuing with cron + in-process Hermes only"
            );
        }
    }

    gateway.start_all().await?;

    let gw_reconnect = gateway.clone();
    sidecar_tasks.push(tokio::spawn(async move {
        gw_reconnect.platform_reconnect_watcher(20).await;
    }));
    let gw_expiry = gateway.clone();
    sidecar_tasks.push(tokio::spawn(async move {
        gw_expiry.session_expiry_watcher(300).await;
    }));

    Ok(EmbeddedGatewayParts {
        gateway,
        sidecar_tasks,
        cron_rx,
    })
}
